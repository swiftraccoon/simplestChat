//! Locks whose guards survive a poisoned state.
//!
//! A panic while one of these locks is held has already been reported by the
//! panic itself, and every holder keeps its critical section consistent (the
//! guard unlocks on unwind). Carrying the poison flag to the next holder would
//! only turn one failure into many, so every production lock in this crate
//! takes the inner value back and continues. Test fixtures that want a panic
//! on poisoning keep using `std::sync` directly.

use std::sync::{MutexGuard, PoisonError, RwLockReadGuard, RwLockWriteGuard, TryLockError};

/// A mutual-exclusion lock that ignores poisoning.
#[derive(Debug, Default)]
pub(crate) struct Mutex<T>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    /// Wrap a value.
    pub(crate) const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }

    /// Lock, taking the value back from a holder that panicked.
    pub(crate) fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A reader-writer lock that ignores poisoning.
#[derive(Debug, Default)]
pub(crate) struct RwLock<T>(std::sync::RwLock<T>);

impl<T> RwLock<T> {
    /// Wrap a value.
    pub(crate) const fn new(value: T) -> Self {
        Self(std::sync::RwLock::new(value))
    }

    /// Lock for reading, ignoring a writer that panicked.
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock for reading without waiting: `None` while a writer has the lock.
    pub(crate) fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        match self.0.try_read() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Lock for writing, taking the value back from a holder that panicked.
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{Mutex, RwLock};

    #[test]
    fn locks_hand_the_value_back_after_a_poisoning_panic() {
        let mutex = std::sync::Arc::new(Mutex::new(1));
        let poisoner = std::sync::Arc::clone(&mutex);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock();
            panic!("poison the lock");
        })
        .join();
        assert_eq!(*mutex.lock(), 1);

        let lock = std::sync::Arc::new(RwLock::new(vec![1]));
        let poisoner = std::sync::Arc::clone(&lock);
        let _ = std::thread::spawn(move || {
            let mut guard = poisoner.write();
            guard.push(2);
            panic!("poison the lock");
        })
        .join();
        assert_eq!(*lock.read(), vec![1, 2]);
        assert!(lock.try_read().is_some());
        {
            let _writer = lock.write();
            assert!(lock.try_read().is_none());
        }
        lock.write().push(3);
        assert_eq!(*lock.read(), vec![1, 2, 3]);
    }
}
