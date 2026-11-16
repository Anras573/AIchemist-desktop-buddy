//! Secrets in the OS keychain (spec 3, 6.2, 6.3, 9).
//!
//! Secrets (the CalDAV password, the Microsoft Graph refresh token) live only
//! in the OS keychain, keyed by the account's [`AccountId`] and a
//! [`SecretKind`]. Everything else about an account goes in the settings file.
//!
//! - [`SecretStore`] is the interface the rest of the app uses. It is
//!   object safe; share it as `Arc<dyn SecretStore>`.
//! - [`KeyringSecretStore`] is the real store: Windows Credential Manager or
//!   the macOS login keychain. On other platforms its constructor fails with
//!   [`Error::Keychain`].
//! - [`InMemorySecretStore`] is a process-local store for tests (this and
//!   other modules').
//!
//! # Rules
//!
//! - Values cross this API only as [`SecretString`], whose `Debug` is
//!   redacted. Never log a value or put one in an error message.
//! - Errors are [`Error::Keychain`]. Their messages name the operation, the
//!   [`SecretKind`] and the opaque [`AccountId`], and never contain secret
//!   material.
//! - Calls block on the OS (the macOS keychain may even show a prompt). Call
//!   them from a blocking context (e.g. `spawn_blocking`), not directly on an
//!   async executor thread.
//! - [`SecretStore::delete`] and [`SecretStore::delete_all`] are idempotent:
//!   deleting a missing secret succeeds.

mod keyring_store;
mod memory;

pub use keyring_store::{KeyringSecretStore, KEYCHAIN_SERVICE, MAX_SECRET_BYTES};
pub use memory::InMemorySecretStore;

use std::fmt;

use crate::domain::{AccountId, Error, Result, SecretString};

/// Which secret of an account is meant.
///
/// Add variants as new providers need them, and add them to
/// [`SecretKind::ALL`] so [`SecretStore::delete_all`] removes them too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SecretKind {
    /// CalDAV password or app password (spec 6.3).
    CalDavPassword,
    /// Microsoft Graph OAuth refresh token (spec 6.2). Can be several KB.
    GraphRefreshToken,
}

impl SecretKind {
    /// Every kind. [`SecretStore::delete_all`] deletes each of these.
    pub const ALL: &'static [SecretKind] =
        &[SecretKind::CalDavPassword, SecretKind::GraphRefreshToken];

    /// Stable name used in keychain entry names. Never change an existing
    /// value: it would orphan stored secrets.
    pub fn as_str(self) -> &'static str {
        match self {
            SecretKind::CalDavPassword => "caldav-password",
            SecretKind::GraphRefreshToken => "graph-refresh-token",
        }
    }
}

impl fmt::Display for SecretKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stores account secrets.
///
/// All methods return [`Error::Keychain`] on failure.
pub trait SecretStore: Send + Sync {
    /// Returns the secret, or `None` if none is stored.
    fn get(&self, account: &AccountId, kind: SecretKind) -> Result<Option<SecretString>>;

    /// Stores the secret, replacing any previous value.
    fn set(&self, account: &AccountId, kind: SecretKind, secret: &SecretString) -> Result<()>;

    /// Deletes the secret. Succeeds if none is stored.
    fn delete(&self, account: &AccountId, kind: SecretKind) -> Result<()>;

    /// Deletes every secret of the account (every [`SecretKind::ALL`]). Call
    /// when the account is removed. Succeeds if none is stored.
    ///
    /// Tries every kind even if one fails, and returns the first error.
    fn delete_all(&self, account: &AccountId) -> Result<()> {
        let mut first_error: Option<Error> = None;
        for &kind in SecretKind::ALL {
            if let Err(err) = self.delete(account, kind) {
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// Builds the [`Error::Keychain`] message for a failed operation. Only the
/// operation, the kind and the opaque account id appear in it.
fn keychain_error(op: &str, account: &AccountId, kind: SecretKind, detail: &str) -> Error {
    Error::keychain(format!(
        "could not {op} {kind} for account {account}: {detail}"
    ))
}

#[cfg(test)]
mod tests;
