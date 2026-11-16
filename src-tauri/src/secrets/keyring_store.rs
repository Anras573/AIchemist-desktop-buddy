//! [`KeyringSecretStore`]: the OS keychain behind [`SecretStore`].
//!
//! # Entries
//!
//! Every entry uses the service name [`KEYCHAIN_SERVICE`] and a user name of
//! `<account_id>:<kind>` (e.g. `acc-1:graph-refresh-token`). In Windows
//! Credential Manager this shows up as a generic credential named
//! `<account_id>:<kind>.io.github.anras573.aichemistbuddy`, persisted for the
//! local machine only (it does not roam with a domain profile). On macOS it is
//! a generic password in the login keychain.
//!
//! # Large secrets
//!
//! A Windows credential blob holds at most 2560 bytes, and a Graph refresh
//! token can be larger. Values are therefore stored as UTF-8 bytes in chunks
//! of at most [`CHUNK_BYTES`]: the first chunk goes in the entry above, behind
//! a two-byte header (format version, chunk count); chunk `n >= 2` goes in an
//! entry with user name `<account_id>:<kind>#<n>`. The same layout is used on
//! every platform so there is one tested code path. Values over
//! [`MAX_SECRET_BYTES`] are rejected.
//!
//! Writing a multi-chunk value is not atomic: a crash part way through can
//! leave a mix of old and new chunks, which reads back as a wrong value (the
//! next sign-in replaces it) or as an "incomplete" error.

use std::fmt;

use super::{keychain_error, SecretKind, SecretStore};
use crate::domain::{AccountId, Result, SecretString};

/// Keychain service name for every entry: the app's bundle identifier.
pub const KEYCHAIN_SERVICE: &str = "io.github.anras573.aichemistbuddy";

/// Maximum payload bytes per keychain entry. The Windows limit is 2560 bytes
/// per blob; this leaves room for the header with a margin.
const CHUNK_BYTES: usize = 2048;

/// Maximum number of entries one secret is split into.
const MAX_CHUNKS: usize = 16;

/// Largest secret, in UTF-8 bytes, that [`KeyringSecretStore`] can store
/// (32 KiB).
pub const MAX_SECRET_BYTES: usize = CHUNK_BYTES * MAX_CHUNKS;

/// First byte of the first chunk. Bump when the layout changes.
const FORMAT_VERSION: u8 = 1;

/// Length of the header in front of the first chunk.
const HEADER_LEN: usize = 2;

/// A failed backend call. The text must not contain secret material.
type BackendError = String;

/// Raw blob storage, one blob per entry user name, all under
/// [`KEYCHAIN_SERVICE`]. The seam that lets the chunking be tested without
/// an OS keychain.
trait Backend: Send + Sync {
    /// Returns the blob, or `None` if the entry does not exist.
    fn read(&self, user: &str) -> std::result::Result<Option<Vec<u8>>, BackendError>;
    /// Creates or replaces the entry.
    fn write(&self, user: &str, blob: &[u8]) -> std::result::Result<(), BackendError>;
    /// Deletes the entry. Succeeds if it does not exist.
    fn remove(&self, user: &str) -> std::result::Result<(), BackendError>;
}

/// [`SecretStore`] backed by the OS keychain: Windows Credential Manager or
/// the macOS login keychain.
pub struct KeyringSecretStore {
    backend: Box<dyn Backend>,
}

impl fmt::Debug for KeyringSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringSecretStore")
            .field("service", &KEYCHAIN_SERVICE)
            .finish_non_exhaustive()
    }
}

impl KeyringSecretStore {
    /// Opens the platform keychain.
    ///
    /// Fails with [`Error::Keychain`] on platforms without a supported
    /// keychain (currently everything except Windows and macOS).
    pub fn new() -> Result<Self> {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            Ok(Self {
                backend: Box::new(native::NativeBackend::new()?),
            })
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Err(crate::domain::Error::keychain(
                "no supported OS keychain on this platform (only Windows and macOS)",
            ))
        }
    }

    #[cfg(test)]
    fn with_backend(backend: impl Backend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
        }
    }
}

fn head_user(account: &AccountId, kind: SecretKind) -> String {
    format!("{account}:{kind}")
}

fn chunk_user(account: &AccountId, kind: SecretKind, n: usize) -> String {
    format!("{account}:{kind}#{n}")
}

/// Parses the header of the first chunk and returns the chunk count.
fn chunk_count(head: &[u8]) -> Option<usize> {
    match head {
        [FORMAT_VERSION, count, ..] if (1..=MAX_CHUNKS).contains(&usize::from(*count)) => {
            Some(usize::from(*count))
        }
        _ => None,
    }
}

impl KeyringSecretStore {
    /// Chunk count of the stored value, or `None` if there is no readable,
    /// well-formed first chunk.
    fn stored_chunk_count(&self, account: &AccountId, kind: SecretKind) -> Option<usize> {
        let head = self.backend.read(&head_user(account, kind)).ok()??;
        chunk_count(&head)
    }

    /// Removes chunk entries `from..=to` (chunk numbers, so `from >= 2`).
    fn remove_chunks(
        &self,
        account: &AccountId,
        kind: SecretKind,
        from: usize,
        to: usize,
    ) -> std::result::Result<(), BackendError> {
        for n in from..=to {
            self.backend.remove(&chunk_user(account, kind, n))?;
        }
        Ok(())
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, account: &AccountId, kind: SecretKind) -> Result<Option<SecretString>> {
        let err = |detail: &str| keychain_error("read", account, kind, detail);

        let Some(head) = self
            .backend
            .read(&head_user(account, kind))
            .map_err(|e| err(&e))?
        else {
            return Ok(None);
        };
        let count = chunk_count(&head).ok_or_else(|| err("the stored entry is malformed"))?;

        let mut bytes = Vec::with_capacity(count * CHUNK_BYTES);
        bytes.extend_from_slice(&head[HEADER_LEN..]);
        for n in 2..=count {
            let chunk = self
                .backend
                .read(&chunk_user(account, kind, n))
                .map_err(|e| err(&e))?
                .ok_or_else(|| err("the stored secret is incomplete"))?;
            bytes.extend_from_slice(&chunk);
        }

        let secret =
            String::from_utf8(bytes).map_err(|_| err("the stored secret is not valid UTF-8"))?;
        Ok(Some(SecretString::new(secret)))
    }

    fn set(&self, account: &AccountId, kind: SecretKind, secret: &SecretString) -> Result<()> {
        let err = |detail: &str| keychain_error("store", account, kind, detail);

        let bytes = secret.expose().as_bytes();
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(err(&format!(
                "the secret is longer than the {MAX_SECRET_BYTES}-byte limit"
            )));
        }
        let old_count = self.stored_chunk_count(account, kind).unwrap_or(0);

        // An empty secret is one empty chunk.
        let chunks: Vec<&[u8]> = if bytes.is_empty() {
            vec![bytes]
        } else {
            bytes.chunks(CHUNK_BYTES).collect()
        };
        let count = chunks.len();

        // Extra chunks first, the first chunk (with the count) last.
        for (i, chunk) in chunks.iter().enumerate().skip(1) {
            self.backend
                .write(&chunk_user(account, kind, i + 1), chunk)
                .map_err(|e| err(&e))?;
        }
        let mut head = Vec::with_capacity(HEADER_LEN + chunks[0].len());
        // `count <= MAX_CHUNKS`, checked above.
        head.extend_from_slice(&[FORMAT_VERSION, count as u8]);
        head.extend_from_slice(chunks[0]);
        self.backend
            .write(&head_user(account, kind), &head)
            .map_err(|e| err(&e))?;

        // Drop chunks the previous, longer value left behind.
        self.remove_chunks(account, kind, count + 1, old_count)
            .map_err(|e| err(&e))
    }

    fn delete(&self, account: &AccountId, kind: SecretKind) -> Result<()> {
        let err = |detail: &str| keychain_error("delete", account, kind, detail);

        // Without a readable first chunk the chunk count is unknown (e.g. a
        // write was interrupted), so sweep every possible chunk.
        let last_chunk = self.stored_chunk_count(account, kind).unwrap_or(MAX_CHUNKS);
        self.remove_chunks(account, kind, 2, last_chunk)
            .map_err(|e| err(&e))?;
        self.backend
            .remove(&head_user(account, kind))
            .map_err(|e| err(&e))
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod native {
    use std::collections::HashMap;
    use std::sync::Arc;

    use keyring_core::{CredentialStore, Entry, Error as KeyringError};

    use super::{Backend, BackendError, KEYCHAIN_SERVICE};
    use crate::domain::{Error, Result};

    pub(super) struct NativeBackend {
        store: Arc<CredentialStore>,
    }

    impl NativeBackend {
        pub(super) fn new() -> Result<Self> {
            #[cfg(target_os = "windows")]
            let store = windows_native_keyring_store::Store::new();
            #[cfg(target_os = "macos")]
            let store = apple_native_keyring_store::keychain::Store::new();

            let store: Arc<CredentialStore> = store.map_err(|e| {
                Error::keychain(format!("could not open the OS keychain: {}", describe(&e)))
            })?;
            Ok(Self { store })
        }

        fn entry(&self, user: &str) -> std::result::Result<Entry, BackendError> {
            // Windows: keep the credential on this machine instead of the
            // default "Enterprise" persistence, which roams with a domain
            // profile. The macOS store's only modifier picks the keychain;
            // the default (login keychain) is right.
            #[cfg(target_os = "windows")]
            let modifiers = Some(HashMap::from([("persistence", "Local")]));
            #[cfg(target_os = "macos")]
            let modifiers: Option<HashMap<&str, &str>> = None;

            self.store
                .build(KEYCHAIN_SERVICE, user, modifiers.as_ref())
                .map_err(|e| describe(&e))
        }
    }

    impl Backend for NativeBackend {
        fn read(&self, user: &str) -> std::result::Result<Option<Vec<u8>>, BackendError> {
            match self.entry(user)?.get_secret() {
                Ok(blob) => Ok(Some(blob)),
                Err(KeyringError::NoEntry) => Ok(None),
                Err(e) => Err(describe(&e)),
            }
        }

        fn write(&self, user: &str, blob: &[u8]) -> std::result::Result<(), BackendError> {
            self.entry(user)?.set_secret(blob).map_err(|e| describe(&e))
        }

        fn remove(&self, user: &str) -> std::result::Result<(), BackendError> {
            match self.entry(user)?.delete_credential() {
                Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
                Err(e) => Err(describe(&e)),
            }
        }
    }

    /// Describes a keyring error without secret material: `BadEncoding` and
    /// `BadDataFormat` carry the stored bytes, so their payloads are never
    /// formatted. Platform errors are OS status codes and their text.
    fn describe(err: &KeyringError) -> String {
        match err {
            KeyringError::PlatformFailure(e) => format!("keychain failure: {e}"),
            KeyringError::NoStorageAccess(e) => format!("keychain is not accessible: {e}"),
            KeyringError::NoEntry => "no such entry".to_owned(),
            KeyringError::BadEncoding(_) | KeyringError::BadDataFormat(..) => {
                "the stored data is malformed".to_owned()
            }
            KeyringError::BadStoreFormat(_) => "the keychain data is malformed".to_owned(),
            KeyringError::TooLong(name, limit) => {
                format!("'{name}' exceeds the keychain limit of {limit}")
            }
            KeyringError::Invalid(name, _) => format!("invalid keychain parameter '{name}'"),
            KeyringError::Ambiguous(_) => "several keychain entries match".to_owned(),
            KeyringError::NoDefaultStore => "no keychain store is available".to_owned(),
            KeyringError::NotSupportedByStore(_) => {
                "the operation is not supported by this keychain".to_owned()
            }
            _ => "unknown keychain error".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::domain::Error;

    /// Windows' per-entry blob limit, enforced by the fake.
    const WINDOWS_BLOB_LIMIT: usize = 2560;

    /// Fake keychain with the Windows blob limit and an optional failure.
    #[derive(Default)]
    struct FakeBackend {
        entries: Mutex<BTreeMap<String, Vec<u8>>>,
        fail_with: Mutex<Option<String>>,
    }

    impl FakeBackend {
        fn fail(&self) -> std::result::Result<(), BackendError> {
            match self.fail_with.lock().unwrap().clone() {
                Some(msg) => Err(msg),
                None => Ok(()),
            }
        }
    }

    impl Backend for std::sync::Arc<FakeBackend> {
        fn read(&self, user: &str) -> std::result::Result<Option<Vec<u8>>, BackendError> {
            self.fail()?;
            Ok(self.entries.lock().unwrap().get(user).cloned())
        }

        fn write(&self, user: &str, blob: &[u8]) -> std::result::Result<(), BackendError> {
            self.fail()?;
            assert!(
                blob.len() <= WINDOWS_BLOB_LIMIT,
                "blob of {} bytes",
                blob.len()
            );
            self.entries
                .lock()
                .unwrap()
                .insert(user.to_owned(), blob.to_vec());
            Ok(())
        }

        fn remove(&self, user: &str) -> std::result::Result<(), BackendError> {
            self.fail()?;
            self.entries.lock().unwrap().remove(user);
            Ok(())
        }
    }

    fn store() -> (KeyringSecretStore, std::sync::Arc<FakeBackend>) {
        let fake = std::sync::Arc::new(FakeBackend::default());
        (KeyringSecretStore::with_backend(fake.clone()), fake)
    }

    fn keys(fake: &FakeBackend) -> Vec<String> {
        fake.entries.lock().unwrap().keys().cloned().collect()
    }

    /// A value of `len` bytes that is not periodic in the chunk size.
    fn value(len: usize) -> String {
        (0..len)
            .map(|i| char::from(b'a' + ((i * 7 + i / 13) % 26) as u8))
            .collect()
    }

    const ACC: &str = "acc-1";
    const KIND: SecretKind = SecretKind::GraphRefreshToken;

    #[test]
    fn small_value_uses_one_entry() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        store.set(&acc, KIND, &SecretString::new("pw")).unwrap();

        assert_eq!(keys(&fake), ["acc-1:graph-refresh-token"]);
        assert_eq!(store.get(&acc, KIND).unwrap().unwrap().expose(), "pw");
    }

    #[test]
    fn empty_value_round_trips() {
        let (store, _) = store();
        let acc = AccountId::new(ACC);
        store.set(&acc, KIND, &SecretString::new("")).unwrap();
        assert_eq!(store.get(&acc, KIND).unwrap().unwrap().expose(), "");
    }

    #[test]
    fn large_values_are_chunked_under_the_windows_limit() {
        for len in [
            CHUNK_BYTES - 1,
            CHUNK_BYTES,
            CHUNK_BYTES + 1,
            4096,
            8192,
            MAX_SECRET_BYTES,
        ] {
            let (store, fake) = store();
            let acc = AccountId::new(ACC);
            let secret = value(len);
            store.set(&acc, KIND, &SecretString::new(&*secret)).unwrap();

            assert_eq!(keys(&fake).len(), len.div_ceil(CHUNK_BYTES), "len {len}");
            assert_eq!(
                store.get(&acc, KIND).unwrap().unwrap().expose(),
                secret,
                "len {len}"
            );
        }
    }

    #[test]
    fn multibyte_characters_split_across_chunks_round_trip() {
        let (store, _) = store();
        let acc = AccountId::new(ACC);
        // 3-byte characters, so chunk boundaries fall inside characters.
        let secret = "€".repeat(3000);
        store.set(&acc, KIND, &SecretString::new(&*secret)).unwrap();
        assert_eq!(store.get(&acc, KIND).unwrap().unwrap().expose(), secret);
    }

    #[test]
    fn too_large_value_is_rejected_without_writing() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        let secret = value(MAX_SECRET_BYTES + 1);
        let err = store
            .set(&acc, KIND, &SecretString::new(&*secret))
            .unwrap_err();

        assert!(matches!(err, Error::Keychain { .. }));
        assert!(keys(&fake).is_empty());
    }

    #[test]
    fn overwriting_with_a_shorter_value_removes_stale_chunks() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        store
            .set(&acc, KIND, &SecretString::new(value(8000)))
            .unwrap();
        assert_eq!(keys(&fake).len(), 4);

        store.set(&acc, KIND, &SecretString::new("short")).unwrap();
        assert_eq!(keys(&fake), ["acc-1:graph-refresh-token"]);
        assert_eq!(store.get(&acc, KIND).unwrap().unwrap().expose(), "short");
    }

    #[test]
    fn delete_removes_every_chunk_and_is_idempotent() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        store
            .set(&acc, KIND, &SecretString::new(value(5000)))
            .unwrap();

        store.delete(&acc, KIND).unwrap();
        assert!(keys(&fake).is_empty());
        assert!(store.get(&acc, KIND).unwrap().is_none());
        store.delete(&acc, KIND).unwrap();
    }

    #[test]
    fn delete_sweeps_orphan_chunks_without_a_first_chunk() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        // As if a write was interrupted before the first chunk was written.
        fake.entries
            .lock()
            .unwrap()
            .insert(chunk_user(&acc, KIND, 3), b"orphan".to_vec());

        store.delete(&acc, KIND).unwrap();
        assert!(keys(&fake).is_empty());
    }

    #[test]
    fn missing_chunk_is_an_error() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        store
            .set(&acc, KIND, &SecretString::new(value(5000)))
            .unwrap();
        fake.entries
            .lock()
            .unwrap()
            .remove(&chunk_user(&acc, KIND, 2));

        let err = store.get(&acc, KIND).unwrap_err();
        assert!(matches!(err, Error::Keychain { .. }));
    }

    #[test]
    fn malformed_first_chunk_is_an_error() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        fake.entries
            .lock()
            .unwrap()
            .insert(head_user(&acc, KIND), b"plain password".to_vec());

        let err = store.get(&acc, KIND).unwrap_err();
        assert!(matches!(err, Error::Keychain { .. }));
    }

    #[test]
    fn backend_errors_become_keychain_errors_without_the_secret() {
        let (store, fake) = store();
        let acc = AccountId::new(ACC);
        *fake.fail_with.lock().unwrap() = Some("locked".to_owned());

        let err = store
            .set(&acc, KIND, &SecretString::new("hunter2"))
            .unwrap_err();
        let Error::Keychain { message } = err else {
            panic!("expected a keychain error, got {err:?}");
        };
        assert!(message.contains("acc-1"), "{message}");
        assert!(message.contains("graph-refresh-token"), "{message}");
        assert!(message.contains("locked"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");

        assert!(matches!(store.get(&acc, KIND), Err(Error::Keychain { .. })));
        assert!(matches!(
            store.delete(&acc, KIND),
            Err(Error::Keychain { .. })
        ));
    }

    #[test]
    fn accounts_and_kinds_are_isolated_and_delete_all_removes_only_one_account() {
        let (store, fake) = store();
        let a = AccountId::new("a");
        let b = AccountId::new("b");
        store
            .set(&a, SecretKind::CalDavPassword, &SecretString::new("a-pw"))
            .unwrap();
        store
            .set(
                &a,
                SecretKind::GraphRefreshToken,
                &SecretString::new(value(3000)),
            )
            .unwrap();
        store
            .set(&b, SecretKind::CalDavPassword, &SecretString::new("b-pw"))
            .unwrap();

        store.delete_all(&a).unwrap();
        assert_eq!(keys(&fake), ["b:caldav-password"]);
        assert_eq!(
            store
                .get(&b, SecretKind::CalDavPassword)
                .unwrap()
                .unwrap()
                .expose(),
            "b-pw"
        );
    }

    #[test]
    fn debug_does_not_expose_anything_secret() {
        let (store, _) = store();
        assert!(format!("{store:?}").contains(KEYCHAIN_SERVICE));
    }
}
