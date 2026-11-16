//! Local storage: the SQLite database and the settings file.
//!
//! The schema and its conventions are documented in `docs/storage.md`.
//! [`open`] and [`open_in_memory`] open databases and keep their schema
//! current. [`Storage`] is the thread-safe handle the rest of the app uses;
//! its methods are grouped by table in the submodules:
//!
//! * [`tasks`]: local tasks, and the local [`TaskSource`](crate::domain::TaskSource);
//! * [`events`]: the event cache, replaced per account after each sync;
//! * [`reminders`]: the reminder ledger;
//! * [`sync_state`]: per-account sync status;
//! * [`prune`]: removal of old events and reminders.
//!
//! [`settings`] loads and saves the JSON settings file.
//!
//! # Migrations
//!
//! [`MIGRATIONS`] is an append-only list of SQL scripts. Migration `n`
//! (1-based) brings the schema from version `n - 1` to `n`, and
//! `PRAGMA user_version` records the version a database is at. [`open`] and
//! [`open_in_memory`] apply the missing migrations in one transaction, so a
//! database is either fully migrated or untouched. Re-opening a current
//! database is a no-op. **Never edit a migration that has shipped**; add a
//! new one.

pub mod codec;
pub mod events;
mod handle;
pub mod prune;
pub mod reminders;
pub mod settings;
pub mod sync_state;
pub mod tasks;

pub use handle::{whole_seconds, Storage};
pub use prune::{PruneStats, DEFAULT_RETENTION};
pub use settings::{load_settings, save_settings, SETTINGS_FILE_NAME};
pub use sync_state::AccountSyncRecord;

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};

use crate::domain::{Error, Result};

/// The schema migrations, in order. Index `i` holds migration `i + 1`.
pub const MIGRATIONS: &[&str] = &[include_str!("migrations/0001_initial.sql")];

/// The schema version a fully migrated database has (`PRAGMA user_version`).
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// How long a statement waits for a lock held by another connection before
/// failing with `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

impl From<rusqlite::Error> for Error {
    /// Maps any SQLite failure to [`Error::Storage`]. rusqlite messages name
    /// tables, columns and constraints but never bound values, so they carry no
    /// task or event contents. A decoding error raised inside a row mapper
    /// (see [`codec::row_error`]) comes back as the original error.
    fn from(e: rusqlite::Error) -> Self {
        if let rusqlite::Error::FromSqlConversionFailure(_, _, inner) = &e {
            if let Some(inner) = inner.downcast_ref::<Error>() {
                return inner.clone();
            }
        }
        Error::storage(e.to_string())
    }
}

/// Opens (creating if needed) the database file at `path`, configures the
/// connection and migrates the schema to [`SCHEMA_VERSION`].
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    prepare(conn)
}

/// Opens a fresh, fully migrated in-memory database. Each call returns an
/// independent database. Intended for tests.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    prepare(conn)
}

fn prepare(mut conn: Connection) -> Result<Connection> {
    configure(&conn)?;
    migrate(&mut conn)?;
    Ok(conn)
}

/// Per-connection settings. These are not persisted in the file, so they are
/// applied on every open.
///
/// The journal mode is left at SQLite's default rollback journal (`DELETE`):
/// one process, small and infrequent writes, and no concurrent readers that
/// would benefit from WAL, which would add `-wal`/`-shm` files and
/// checkpointing for no gain.
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn user_version(conn: &Connection) -> Result<u32> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Applies the migrations the database is missing, in one immediate
/// transaction (which also keeps a second process from migrating at the same
/// time). Fails without changes if the database is newer than this build.
fn migrate(conn: &mut Connection) -> Result<()> {
    apply_migrations(conn, MIGRATIONS)
}

fn apply_migrations(conn: &mut Connection, migrations: &[&str]) -> Result<()> {
    let target = migrations.len() as u32;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current = user_version(&tx)?;
    if current > target {
        return Err(Error::storage(format!(
            "database schema version {current} is newer than this app supports ({target})"
        )));
    }
    for (i, sql) in migrations.iter().enumerate().skip(current as usize) {
        tx.execute_batch(sql)
            .map_err(|e| Error::storage(format!("migration {} failed: {e}", i + 1)))?;
    }
    if current != target {
        tx.pragma_update(None, "user_version", target)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod repo_tests;
#[cfg(test)]
mod tests;
