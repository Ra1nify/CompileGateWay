//! Poison-tolerant lock helpers (S8).
//!
//! A panic while holding one of the shared locks poisons it. The default `std` behaviour then
//! makes every later `lock()` / `read()` / `write()` on that guard return `Err`, and the
//! prevailing `.unwrap()` turns one local panic into a cascade that takes the whole daemon down.
//!
//! The data behind these locks — job history, log ring buffers, the project / client / queue
//! maps — does not rely on cross-field invariants that a mid-panic writer could tear, so
//! continuing with the last consistent value is strictly better than aborting the process.
//! These helpers recover the guard via [`PoisonError::into_inner`] and log once per poisoning.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Take a `Mutex` guard, recovering it if a previous holder panicked.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| {
        warn_once("mutex");
        err.into_inner()
    })
}

/// Take an `RwLock` read guard, recovering it if a previous writer panicked.
pub fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|err| {
        warn_once("rwlock(read)");
        err.into_inner()
    })
}

/// Take an `RwLock` write guard, recovering it if a previous holder panicked.
pub fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|err| {
        warn_once("rwlock(write)");
        err.into_inner()
    })
}

fn warn_once(kind: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("[WARN] recovered a poisoned {kind} lock; a previous holder panicked");
    }
}

#[allow(dead_code)]
fn _assert_poison_error_type<T>(err: PoisonError<T>) -> T {
    err.into_inner()
}
