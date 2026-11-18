//! Which Microsoft account each Graph account is signed in to
//! ([`AccountStatus::signed_in_as`](crate::domain::ipc::AccountStatus::signed_in_as)).
//!
//! # Where it is stored
//!
//! In `account-identities.json` next to `settings.json` in the app-config
//! directory: a JSON object from account id to the sign-in name, e.g.
//! `{"3f2a...": "jane@example.com"}`.
//!
//! - Not in the keychain: it is not a secret, and reading it there on every
//!   `list_accounts` would cost a keychain round trip per account.
//! - Not in `settings.json`: the settings schema is a frozen contract, and
//!   the name is runtime state learned from Microsoft, not something the user
//!   configures.
//! - Not in the account's display name: the user chooses that one.
//!
//! The value is personal data: it is never logged. Loading is forgiving (a
//! missing or broken file means "unknown"), saving is atomic (temporary file
//! plus rename), like the settings file.

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::domain::{AccountId, Error, Result};

/// The file name inside the app-config directory.
pub const IDENTITIES_FILE_NAME: &str = "account-identities.json";

/// Longest name kept, in characters.
const MAX_LEN: usize = 200;

/// The persisted sign-in names, by account id.
#[derive(Debug)]
pub struct Identities {
    /// `None`: kept in memory only (no config directory, or tests).
    path: Option<PathBuf>,
    names: Mutex<BTreeMap<String, String>>,
}

impl Identities {
    /// Loads the file at `path` (empty if missing or unreadable).
    pub fn load(path: &Path) -> Self {
        let names = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| {
                log::warn!("{IDENTITIES_FILE_NAME} is not valid; ignoring it");
                BTreeMap::new()
            }),
            Err(e) if e.kind() == ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                log::warn!("cannot read {IDENTITIES_FILE_NAME} ({})", e.kind());
                BTreeMap::new()
            }
        };
        Self {
            path: Some(path.to_path_buf()),
            names: Mutex::new(names),
        }
    }

    /// An empty set that is never written to disk.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            names: Mutex::new(BTreeMap::new()),
        }
    }

    /// The sign-in name of `account`, if known.
    pub fn get(&self, account: &AccountId) -> Option<String> {
        self.lock().get(account.as_str()).cloned()
    }

    /// Stores (`Some`) or forgets (`None`) the sign-in name of `account` and
    /// saves the file if it changed.
    pub fn set(&self, account: &AccountId, name: Option<&str>) -> Result<()> {
        let mut names = self.lock();
        let name = name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| n.chars().take(MAX_LEN).collect::<String>());
        let changed = match name {
            Some(name) => names.insert(account.as_str().to_owned(), name.clone()) != Some(name),
            None => names.remove(account.as_str()).is_some(),
        };
        if changed {
            if let Some(path) = &self.path {
                save(path, &names)?;
            }
        }
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, String>> {
        self.names.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn save(path: &Path, names: &BTreeMap<String, String>) -> Result<()> {
    let json = serde_json::to_vec_pretty(names)
        .map_err(|e| Error::internal(format!("cannot serialize account identities: {e}")))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| io_error(&e))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let written = fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(&json).and_then(|()| f.sync_all()))
        .and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_error(&e));
    }
    Ok(())
}

fn io_error(e: &std::io::Error) -> Error {
    Error::storage(format!("cannot write {IDENTITIES_FILE_NAME}: {e}"))
}
