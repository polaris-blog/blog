//! In-memory cache backend: per-entry TTL + LRU eviction, bounded by
//! `max_entries`.
//!
//! The LRU list is a slab-allocated doubly-linked list guarded by a
//! `std::sync::Mutex` — every operation is O(1) and lock hold times are
//! tiny (no I/O, no awaits inside the lock), which keeps contention low.
//!
//! Approximate memory usage is tracked (key + value bytes + fixed slot
//! overhead) and reported through the stats API, but eviction is
//! entry-count based. See `[cache.memory] max_entries` in polaris.toml.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::Cache;

/// Fixed bookkeeping cost per entry, added to the tracked byte total.
const SLOT_OVERHEAD: usize = 128;
/// Expired entries are swept opportunistically every N inserts.
const SWEEP_EVERY: u64 = 512;

struct Slot {
    key: Arc<str>,
    val: Arc<Vec<u8>>,
    /// Unix-seconds deadline; entries expire lazily on access.
    exp: i64,
    prev: Option<u32>,
    next: Option<u32>,
}

#[derive(Default)]
struct Inner {
    map: HashMap<Arc<str>, u32>,
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Most-recently-used slot.
    head: Option<u32>,
    /// Least-recently-used slot (eviction candidate).
    tail: Option<u32>,
    bytes: usize,
}

impl Inner {
    fn unlink(&mut self, idx: u32) {
        let (prev, next) = {
            let s = &self.slots[idx as usize];
            (s.prev, s.next)
        };
        match prev {
            Some(p) => self.slots[p as usize].next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.slots[n as usize].prev = prev,
            None => self.tail = prev,
        }
        let s = &mut self.slots[idx as usize];
        s.prev = None;
        s.next = None;
    }

    fn push_front(&mut self, idx: u32) {
        let old_head = self.head;
        {
            let s = &mut self.slots[idx as usize];
            s.prev = None;
            s.next = old_head;
        }
        if let Some(h) = old_head {
            self.slots[h as usize].prev = Some(idx);
        }
        self.head = Some(idx);
        if self.tail.is_none() {
            self.tail = Some(idx);
        }
    }

    fn remove(&mut self, idx: u32) {
        self.unlink(idx);
        let key = {
            let s = &mut self.slots[idx as usize];
            let sz = s.key.len() + s.val.len() + SLOT_OVERHEAD;
            self.bytes -= self.bytes.min(sz);
            std::mem::replace(&mut s.key, Arc::from(""))
        };
        let _val = std::mem::replace(&mut self.slots[idx as usize].val, Arc::new(Vec::new()));
        self.map.remove(key.as_ref());
        self.free.push(idx);
    }

    fn get_at(&mut self, key: &str, now: i64) -> Option<Arc<Vec<u8>>> {
        let idx = *self.map.get(key)?;
        if now >= self.slots[idx as usize].exp {
            self.remove(idx);
            return None;
        }
        self.unlink(idx);
        self.push_front(idx);
        Some(self.slots[idx as usize].val.clone())
    }

    fn set_at(
        &mut self,
        key: Arc<str>,
        val: Arc<Vec<u8>>,
        exp: i64,
        max_entries: usize,
        evictions: &AtomicU64,
    ) {
        if let Some(idx) = self.map.get(key.as_ref()).copied() {
            let s = &mut self.slots[idx as usize];
            let old_sz = s.key.len() + s.val.len() + SLOT_OVERHEAD;
            self.bytes -= self.bytes.min(old_sz);
            self.bytes += key.len() + val.len() + SLOT_OVERHEAD;
            s.val = val;
            s.exp = exp;
            self.unlink(idx);
            self.push_front(idx);
            return;
        }
        // Evict from the LRU tail until there is room.
        while self.map.len() >= max_entries {
            let Some(victim) = self.tail else { break };
            self.remove(victim);
            evictions.fetch_add(1, Ordering::Relaxed);
        }
        let idx = if let Some(i) = self.free.pop() {
            self.slots[i as usize] = Slot {
                key: key.clone(),
                val,
                exp,
                prev: None,
                next: None,
            };
            i
        } else {
            self.slots.push(Slot {
                key: key.clone(),
                val,
                exp,
                prev: None,
                next: None,
            });
            (self.slots.len() - 1) as u32
        };
        self.bytes += key.len() + self.slots[idx as usize].val.len() + SLOT_OVERHEAD;
        self.map.insert(key, idx);
        self.push_front(idx);
    }

    fn del_at(&mut self, key: &str) -> bool {
        let Some(idx) = self.map.get(key).copied() else {
            return false;
        };
        self.remove(idx);
        true
    }

    fn sweep_expired(&mut self, now: i64) -> usize {
        let victims: Vec<u32> = self
            .map
            .values()
            .copied()
            .filter(|i| now >= self.slots[*i as usize].exp)
            .collect();
        let n = victims.len();
        for v in victims {
            self.remove(v);
        }
        n
    }

    fn live_keys_with_prefix(&self, prefix: &str, now: i64) -> Vec<String> {
        self.map
            .iter()
            .filter(|(k, i)| k.starts_with(prefix) && now < self.slots[**i as usize].exp)
            .map(|(k, _)| k.to_string())
            .collect()
    }
}

/// Thread-safe in-memory LRU+TTL cache.
pub struct MemoryCache {
    inner: Mutex<Inner>,
    max_entries: usize,
    evictions: AtomicU64,
    inserts: AtomicU64,
}

impl MemoryCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            max_entries: max_entries.max(1),
            evictions: AtomicU64::new(0),
            inserts: AtomicU64::new(0),
        }
    }

    pub fn get_sync(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        self.get_with_now(key, crate::utils::time::now())
    }

    pub fn set_sync(&self, key: &str, val: Arc<Vec<u8>>, ttl: Duration) {
        let secs = ttl.as_secs().clamp(1, 86_400 * 365) as i64;
        let now = crate::utils::time::now();
        self.set_with_now(key, val, secs, now);
    }

    pub fn del_sync(&self, key: &str) -> bool {
        crate::utils::lock::lock(&self.inner).del_at(key)
    }

    pub fn clear_sync(&self) {
        *crate::utils::lock::lock(&self.inner) = Inner::default();
    }

    pub fn len_sync(&self) -> usize {
        crate::utils::lock::lock(&self.inner).map.len()
    }

    pub fn bytes_sync(&self) -> usize {
        crate::utils::lock::lock(&self.inner).bytes
    }

    pub fn evictions_sync(&self) -> u64 {
        self.evictions.load(Ordering::Relaxed)
    }

    /// Live (unexpired) keys starting with `prefix` — used by admin tooling.
    pub fn keys_with_prefix(&self, prefix: &str) -> Vec<String> {
        let inner = crate::utils::lock::lock(&self.inner);
        inner.live_keys_with_prefix(prefix, crate::utils::time::now())
    }

    // -- clock-injectable variants (tests) -----------------------------------

    fn get_with_now(&self, key: &str, now: i64) -> Option<Arc<Vec<u8>>> {
        crate::utils::lock::lock(&self.inner).get_at(key, now)
    }

    fn set_with_now(&self, key: &str, val: Arc<Vec<u8>>, ttl_secs: i64, now: i64) {
        let mut inner = crate::utils::lock::lock(&self.inner);
        inner.set_at(
            Arc::from(key),
            val,
            now + ttl_secs,
            self.max_entries,
            &self.evictions,
        );
        if self
            .inserts
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(SWEEP_EVERY)
        {
            inner.sweep_expired(now);
        }
    }
}

impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.get_sync(key).map(|v| v.as_ref().clone()))
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration) -> anyhow::Result<()> {
        self.set_sync(key, Arc::new(value), ttl);
        Ok(())
    }

    async fn delete(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.del_sync(key))
    }

    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.get_sync(key).is_some())
    }

    async fn clear(&self) -> anyhow::Result<()> {
        self.clear_sync();
        Ok(())
    }

    fn size(&self) -> (usize, Option<usize>) {
        (self.len_sync(), Some(self.bytes_sync()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(n: usize) -> MemoryCache {
        MemoryCache::new(n)
    }

    #[test]
    fn set_get_delete_roundtrip() {
        let c = cache(16);
        c.set_sync("a", Arc::new(b"1".to_vec()), Duration::from_secs(60));
        assert_eq!(
            c.get_sync("a").as_deref().map(|v| v.as_slice()),
            Some(&b"1"[..])
        );
        assert!(c.del_sync("a"));
        assert!(c.get_sync("a").is_none());
        assert!(!c.del_sync("a"));
    }

    #[test]
    fn ttl_expiry() {
        let c = cache(16);
        c.set_with_now("k", Arc::new(b"v".to_vec()), 10, 100);
        assert!(c.get_with_now("k", 109).is_some());
        // At or past the deadline the entry is gone.
        assert!(c.get_with_now("k", 110).is_none());
    }

    #[test]
    fn overwrite_updates_value_and_size() {
        let c = cache(16);
        c.set_sync("k", Arc::new(vec![0u8; 100]), Duration::from_secs(60));
        let b1 = c.bytes_sync();
        c.set_sync("k", Arc::new(vec![1u8; 10]), Duration::from_secs(60));
        assert!(c.bytes_sync() < b1);
        assert_eq!(c.len_sync(), 1);
        assert_eq!(
            c.get_sync("k").as_deref().map(|v| v.as_slice()),
            Some(&[1u8; 10][..])
        );
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let c = cache(2);
        c.set_sync("old", Arc::new(b"1".to_vec()), Duration::from_secs(60));
        c.set_sync("mid", Arc::new(b"2".to_vec()), Duration::from_secs(60));
        // Touch "old" so "mid" becomes the LRU tail.
        c.get_sync("old");
        c.set_sync("new", Arc::new(b"3".to_vec()), Duration::from_secs(60));
        assert!(c.get_sync("mid").is_none(), "LRU entry should be evicted");
        assert!(c.get_sync("old").is_some());
        assert!(c.get_sync("new").is_some());
        assert_eq!(c.evictions_sync(), 1);
    }

    #[test]
    fn expired_entries_swept_on_insert() {
        let c = MemoryCache::new(1024);
        // 512 inserts, every entry expiring at t=1001.
        for i in 0..512 {
            c.set_with_now(&format!("k{i}"), Arc::new(b"v".to_vec()), 1, 1000);
        }
        assert_eq!(c.len_sync(), 512);
        // The 513th insert crosses SWEEP_EVERY with the clock past every
        // deadline: the sweep drops all stale entries in one pass.
        c.set_with_now("fresh", Arc::new(b"v".to_vec()), 60, 2000);
        assert_eq!(c.len_sync(), 1);
        assert!(c.get_with_now("fresh", 2000).is_some());
    }

    #[test]
    fn clear_and_prefix() {
        let c = cache(16);
        c.set_sync(
            "post:v1:a",
            Arc::new(b"1".to_vec()),
            Duration::from_secs(60),
        );
        c.set_sync(
            "post:v1:b",
            Arc::new(b"2".to_vec()),
            Duration::from_secs(60),
        );
        c.set_sync("rss:v1:x", Arc::new(b"3".to_vec()), Duration::from_secs(60));
        let mut keys = c.keys_with_prefix("post:");
        keys.sort();
        assert_eq!(keys, vec!["post:v1:a".to_string(), "post:v1:b".to_string()]);
        c.clear_sync();
        assert_eq!(c.len_sync(), 0);
        assert_eq!(c.bytes_sync(), 0);
    }
}
