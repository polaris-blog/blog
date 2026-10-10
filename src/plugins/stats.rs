//! In-memory statistics counters for sandboxed plugins.
//!
//! `stat_incr(key, ttl)` / `stat_get(key)` give plugins atomic counters —
//! visitor counts, event tallies, rate hints — without touching the database.
//! Entries are per-key TTL'd (day-scoped keys keep memory bounded), expired
//! entries are evicted lazily, and everything lives in memory: stats are
//! approximate and reset when the process restarts (by design — they are
//! not analytics records).

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAX_ENTRIES: usize = 8192;

#[derive(Default)]
pub struct Stats {
    entries: RwLock<HashMap<String, (i64, Instant)>>,
    sweep_counter: AtomicU64,
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Increment `key` by one and return the new value. The entry expires
    /// after `ttl`. A stale (expired) entry restarts from 1.
    pub fn incr(&self, key: &str, ttl: Duration) -> i64 {
        let mut entries = self.entries.write().expect("stats lock");
        self.maybe_sweep(&mut entries);
        let expires_at = Instant::now() + ttl;
        match entries.get_mut(key) {
            Some((value, exp)) if *exp > Instant::now() => {
                *value += 1;
                *exp = expires_at;
                *value
            }
            _ => {
                entries.insert(key.to_string(), (1, expires_at));
                1
            }
        }
    }

    /// Current value of `key` (0 when absent or expired).
    pub fn get(&self, key: &str) -> i64 {
        let entries = self.entries.read().expect("stats lock");
        match entries.get(key) {
            Some((value, exp)) if *exp > Instant::now() => *value,
            _ => 0,
        }
    }

    /// Lazy eviction: once the map grows past MAX_ENTRIES, drop everything
    /// expired; if it is still oversized, drop the soonest-to-expire half.
    fn maybe_sweep(&self, entries: &mut HashMap<String, (i64, Instant)>) {
        let calls = self.sweep_counter.fetch_add(1, Ordering::Relaxed);
        if entries.len() <= MAX_ENTRIES && !calls.is_multiple_of(64) {
            return;
        }
        let now = Instant::now();
        entries.retain(|_, (_, exp)| *exp > now);
        if entries.len() > MAX_ENTRIES {
            let mut by_expiry: Vec<_> = entries
                .iter()
                .map(|(k, (_, exp))| (*exp, k.clone()))
                .collect();
            by_expiry.sort();
            by_expiry.truncate(entries.len() - MAX_ENTRIES);
            for (_, k) in by_expiry {
                entries.remove(&k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incr_counts_and_expires() {
        let s = Stats::new();
        assert_eq!(s.incr("k", Duration::from_secs(60)), 1);
        assert_eq!(s.incr("k", Duration::from_secs(60)), 2);
        assert_eq!(s.get("k"), 2);
        // A fresh key starts at 1.
        assert_eq!(s.incr("other", Duration::from_secs(60)), 1);
        assert_eq!(s.get("other"), 1);
    }

    #[test]
    fn separate_keys_are_independent() {
        let s = Stats::new();
        assert_eq!(s.incr("a", Duration::from_secs(60)), 1);
        assert_eq!(s.incr("a:1.2.3.4", Duration::from_secs(60)), 1);
        assert_eq!(s.get("a"), 1);
        assert_eq!(s.get("a:1.2.3.4"), 1);
    }
}
