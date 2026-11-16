//! Per-account sync status (spec 6.5, 8): last attempt, last success and the
//! last error, for the tray menu and the settings account rows.

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Row};

use super::codec::{
    decode, error_from_sql, error_kind_to_sql, error_message, instant_from_sql, instant_to_sql,
};
use super::Storage;
use crate::domain::{AccountId, Error, Result};

/// The longest error message kept, in characters. Longer ones are cut.
pub const MAX_ERROR_MESSAGE_LEN: usize = 500;

/// The stored sync status of one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSyncRecord {
    /// The account.
    pub account_id: AccountId,
    /// When the last sync started.
    pub last_attempt_at: Option<DateTime<Utc>>,
    /// When the last successful sync finished.
    pub last_success_at: Option<DateTime<Utc>>,
    /// The last error and when it happened; cleared by a successful sync.
    /// A stored [`Error::Validation`] comes back without its field list.
    pub last_error: Option<(Error, DateTime<Utc>)>,
}

impl Storage {
    /// Records that a sync of `account_id` started now.
    pub fn record_sync_attempt(&self, account_id: &AccountId) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sync_state (account_id, last_attempt_at) VALUES (?1, ?2)
             ON CONFLICT (account_id) DO UPDATE SET last_attempt_at = excluded.last_attempt_at",
            params![account_id.as_str(), instant_to_sql(self.now())],
        )?;
        Ok(())
    }

    /// Records that a sync of `account_id` succeeded now and clears the last
    /// error.
    pub fn record_sync_success(&self, account_id: &AccountId) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sync_state (account_id, last_success_at) VALUES (?1, ?2)
             ON CONFLICT (account_id) DO UPDATE SET
                 last_success_at = excluded.last_success_at,
                 last_error_kind = NULL, last_error_message = NULL, last_error_at = NULL",
            params![account_id.as_str(), instant_to_sql(self.now())],
        )?;
        Ok(())
    }

    /// Records that a sync of `account_id` failed now with `error`. Only the
    /// error's kind and message are kept (cut to [`MAX_ERROR_MESSAGE_LEN`]);
    /// [`Error`] messages never contain secrets or event contents (spec 9).
    pub fn record_sync_failure(&self, account_id: &AccountId, error: &Error) -> Result<()> {
        let message: String = error_message(error)
            .chars()
            .take(MAX_ERROR_MESSAGE_LEN)
            .collect();
        self.conn().execute(
            "INSERT INTO sync_state (account_id, last_error_kind, last_error_message, last_error_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account_id) DO UPDATE SET
                 last_error_kind = excluded.last_error_kind,
                 last_error_message = excluded.last_error_message,
                 last_error_at = excluded.last_error_at",
            params![
                account_id.as_str(),
                error_kind_to_sql(error),
                message,
                instant_to_sql(self.now())
            ],
        )?;
        Ok(())
    }

    /// The sync status of `account_id`, or `None` if it never synced.
    pub fn sync_state(&self, account_id: &AccountId) -> Result<Option<AccountSyncRecord>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT * FROM sync_state WHERE account_id = ?1",
                [account_id.as_str()],
                record_from_row,
            )
            .optional()?)
    }

    /// The sync status of every account that has synced, by account id.
    pub fn list_sync_states(&self) -> Result<Vec<AccountSyncRecord>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM sync_state ORDER BY account_id")?;
        let records = stmt
            .query_map([], record_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(records)
    }
}

fn record_from_row(r: &Row) -> rusqlite::Result<AccountSyncRecord> {
    let instant = |col: &str| -> rusqlite::Result<Option<DateTime<Utc>>> {
        decode(
            r.get::<_, Option<i64>>(col)?
                .map(instant_from_sql)
                .transpose(),
        )
    };
    let kind: Option<String> = r.get("last_error_kind")?;
    let last_error = match (
        kind,
        r.get("last_error_message")?,
        instant("last_error_at")?,
    ) {
        (Some(kind), Some(message), Some(at)) => {
            Some((decode(error_from_sql(&kind, message))?, at))
        }
        _ => None,
    };
    Ok(AccountSyncRecord {
        account_id: AccountId::new(r.get::<_, String>("account_id")?),
        last_attempt_at: instant("last_attempt_at")?,
        last_success_at: instant("last_success_at")?,
        last_error,
    })
}
