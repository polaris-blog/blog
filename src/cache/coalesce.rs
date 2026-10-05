//! Single-flight request coalescing (stampede protection).
//!
//! When a hot cache entry expires, many concurrent requests would otherwise
//! all fall through to the loader and hammer the database. Here, the first
//! requester for a key becomes the leader; everyone else parks on a per-key
//! async mutex and re-checks the cache once the leader finishes.
//!
//! Deliberately simple: an in-process lock map, no distributed coordination,
//! no background refresh. The registry is bounded — if it ever exceeds
//! `MAX_KEYS` it is cleared wholesale, which at worst allows a brief burst
//! of duplicate loads (always safe: loads are read + cache-fill).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const MAX_KEYS: usize = 4096;

#[derive(Default)]
pub struct Coalescer {
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Coalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `fut` under a per-key lock. Concurrent callers with the same key
    /// execute one at a time; followers should re-check the cache first
    /// (the leader has typically filled it).
    pub async fn run<F>(&self, key: String, fut: F) -> F::Output
    where
        F: std::future::Future,
    {
        let lock = {
            let mut m = crate::utils::lock::lock(&self.locks);
            if m.len() >= MAX_KEYS {
                m.clear();
            }
            Arc::clone(
                m.entry(key.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        };
        let guard = lock.lock().await;
        let out = fut.await;
        drop(guard);

        // Best-effort cleanup: drop the registry entry when nobody else
        // references it. Racy removals are benign (worst case: a duplicate
        // load, never a lost update — the cache itself stays consistent).
        let mut m = crate::utils::lock::lock(&self.locks);
        if let Some(cur) = m.get(&key)
            && Arc::ptr_eq(cur, &lock)
            && Arc::strong_count(cur) == 1
        {
            m.remove(&key);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_loads_execute_once() {
        let co = Arc::new(Coalescer::new());
        let loads = Arc::new(AtomicUsize::new(0));
        // Stands in for the backend cache. Every real call site
        // (get_or_load, response cache) follows the same contract: the
        // future re-checks the cache under the lock, so followers served
        // after the leader never touch the loader.
        let cache = Arc::new(Mutex::new(None::<u32>));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let co = co.clone();
            let loads = loads.clone();
            let cache = cache.clone();
            handles.push(tokio::spawn(async move {
                co.run("hot-key".to_string(), async {
                    if let Some(v) = *cache.lock().unwrap() {
                        return v;
                    }
                    loads.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    *cache.lock().unwrap() = Some(42);
                    42
                })
                .await
            }));
        }
        for h in handles {
            assert_eq!(h.await.unwrap(), 42);
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1, "exactly one loader runs");
    }

    #[tokio::test]
    async fn different_keys_run_concurrently() {
        let co = Coalescer::new();
        let a = co.run("a".to_string(), async { 1 });
        let b = co.run("b".to_string(), async { 2 });
        let (a, b) = tokio::join!(a, b);
        assert_eq!((a, b), (1, 2));
    }
}
