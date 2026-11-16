//! SQLite storage: connection setup and schema migrations.
//!
//! The schema and its conventions are documented in `docs/storage.md`.
//! This module only opens databases and keeps their schema current; the
//! repositories that read and write rows are built on top of it.
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
    /// task or event contents.
    fn from(e: rusqlite::Error) -> Self {
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
mod tests;
