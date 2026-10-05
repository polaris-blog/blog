//! Lock acquisition that survives poisoning.
//!
//! A panic while a lock is held marks the `RwLock`/`Mutex` poisoned. The
//! default `.unwrap()` would then turn that single panic into a *permanent*
//! outage: every later request touching the lock panics too, taking the
//! whole server down. The data guarded here (session maps, caches, version
//! counters, plugin registries) never requires cross-operation invariants,
//! so recovering the guard is safe and keeps Polaris stable — one failed
//! request must not cascade.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_locks_still_work() {
        use std::sync::Arc;
        let rw = Arc::new(RwLock::new(1u64));
        // Poison the write half from another thread.
        let poisoner = rw.clone();
        let _ = std::thread::spawn(move || {
            let _g = poisoner.write().unwrap();
            panic!("boom");
        })
        .join();
        assert_eq!(*read(&rw), 1);
        *write(&rw) = 2;
        assert_eq!(*read(&rw), 2);

        let m = Arc::new(Mutex::new(5u64));
        let poisoner = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = poisoner.lock().unwrap();
            panic!("boom");
        })
        .join();
        assert_eq!(*lock(&m), 5);
    }
}
