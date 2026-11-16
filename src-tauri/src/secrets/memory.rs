//! [`InMemorySecretStore`]: a process-local [`SecretStore`] for tests.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use super::{SecretKind, SecretStore};
use crate::domain::{AccountId, Result, SecretString};

/// A [`SecretStore`] that keeps secrets in a map in memory. Nothing is
/// persisted. Meant for tests; always compiled so other modules' tests can
/// use it.
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<(AccountId, SecretKind), SecretString>>,
}

impl InMemorySecretStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored secrets (all accounts and kinds).
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether no secret is stored.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(AccountId, SecretKind), SecretString>> {
        // A panic while holding the lock cannot leave the map half-updated
        // (every operation is a single map call), so a poisoned lock is safe
        // to reuse.
        self.secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, account: &AccountId, kind: SecretKind) -> Result<Option<SecretString>> {
        Ok(self.lock().get(&(account.clone(), kind)).cloned())
    }

    fn set(&self, account: &AccountId, kind: SecretKind, secret: &SecretString) -> Result<()> {
        self.lock().insert((account.clone(), kind), secret.clone());
        Ok(())
    }

    fn delete(&self, account: &AccountId, kind: SecretKind) -> Result<()> {
        self.lock().remove(&(account.clone(), kind));
        Ok(())
    }

    fn delete_all(&self, account: &AccountId) -> Result<()> {
        self.lock().retain(|(a, _), _| a != account);
        Ok(())
    }
}
