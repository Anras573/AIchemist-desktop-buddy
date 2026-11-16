//! The shared, thread-safe storage handle.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, Transaction, TransactionBehavior};

use crate::domain::{Clock, Result};

/// A cheaply cloneable handle to the app database.
///
/// # Threading
///
/// The scheduler driver, the sync engine and IPC handlers run on different
/// threads. They share one [`Connection`] behind an `Arc<Mutex<_>>`: every
/// method locks it for the duration of one call (one statement or one
/// transaction) and releases it before returning. That is enough for this app:
/// writes are small and rare, every call is short, and SQLite would serialize
/// writers anyway. Clone the handle to give each thread its own copy.
///
/// Like the provider traits, the methods block (briefly, on disk I/O and the
/// lock), so call them from a worker thread or `spawn_blocking`, not from the
/// Tauri main thread.
///
/// A panic while the lock is held poisons the mutex. The handle ignores the
/// poison: an unfinished rusqlite transaction rolls back when it is dropped
/// during the unwind, so the connection is still consistent.
///
/// # Time
///
/// The handle owns a [`Clock`]. It stamps `created_at`, `done_at` and the sync
/// state with `clock.now()`, and converts query windows to local dates for
/// all-day events. Tests inject a `FakeClock`.
#[derive(Clone)]
pub struct Storage {
    conn: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
}

impl Storage {
    /// Wraps a configured, migrated connection (from [`super::open`] or
    /// [`super::open_in_memory`]).
    pub fn new(conn: Connection, clock: Arc<dyn Clock>) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            clock,
        }
    }

    /// Opens (creating and migrating if needed) the database at `path`.
    pub fn open(path: &Path, clock: Arc<dyn Clock>) -> Result<Self> {
        Ok(Self::new(super::open(path)?, clock))
    }

    /// A fresh, migrated in-memory database. Intended for tests.
    pub fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self> {
        Ok(Self::new(super::open_in_memory()?, clock))
    }

    /// The clock this handle stamps times with.
    pub fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    /// The current instant, truncated to whole seconds as it is stored.
    pub(super) fn now(&self) -> DateTime<Utc> {
        whole_seconds(self.clock.now())
    }

    /// Locks the connection.
    pub(super) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs `f` in an immediate transaction and commits if it returns `Ok`.
    /// On `Err` (or a panic) the transaction rolls back.
    pub(super) fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Storage").finish_non_exhaustive()
    }
}

/// Drops sub-second precision, as storage does (`docs/storage.md`), so values
/// returned by a write compare equal to the same values read back later.
pub fn whole_seconds(t: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(t.timestamp(), 0).unwrap_or(t)
}
