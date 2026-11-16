use std::sync::Arc;

use super::*;

#[test]
fn kind_names_are_stable() {
    assert_eq!(SecretKind::CalDavPassword.as_str(), "caldav-password");
    assert_eq!(
        SecretKind::GraphRefreshToken.as_str(),
        "graph-refresh-token"
    );
    assert_eq!(SecretKind::ALL.len(), 2);
}

#[test]
fn in_memory_get_missing_is_none() {
    let store = InMemorySecretStore::new();
    let acc = AccountId::new("acc-1");
    assert!(store
        .get(&acc, SecretKind::CalDavPassword)
        .unwrap()
        .is_none());
    assert!(store.is_empty());
}

#[test]
fn in_memory_set_get_overwrite_delete() {
    let store = InMemorySecretStore::new();
    let acc = AccountId::new("acc-1");
    let kind = SecretKind::CalDavPassword;

    store.set(&acc, kind, &SecretString::new("one")).unwrap();
    assert_eq!(store.get(&acc, kind).unwrap().unwrap().expose(), "one");

    store.set(&acc, kind, &SecretString::new("two")).unwrap();
    assert_eq!(store.get(&acc, kind).unwrap().unwrap().expose(), "two");
    assert_eq!(store.len(), 1);

    store.delete(&acc, kind).unwrap();
    assert!(store.get(&acc, kind).unwrap().is_none());
    // Idempotent.
    store.delete(&acc, kind).unwrap();
    assert!(store.is_empty());
}

#[test]
fn in_memory_keeps_accounts_and_kinds_apart() {
    let store = InMemorySecretStore::new();
    let a = AccountId::new("a");
    let b = AccountId::new("b");
    store
        .set(&a, SecretKind::CalDavPassword, &SecretString::new("a-pw"))
        .unwrap();
    store
        .set(
            &a,
            SecretKind::GraphRefreshToken,
            &SecretString::new("a-rt"),
        )
        .unwrap();
    store
        .set(&b, SecretKind::CalDavPassword, &SecretString::new("b-pw"))
        .unwrap();

    let get = |acc: &AccountId, kind| store.get(acc, kind).unwrap().map(|s| s.expose().to_owned());
    assert_eq!(get(&a, SecretKind::CalDavPassword).as_deref(), Some("a-pw"));
    assert_eq!(
        get(&a, SecretKind::GraphRefreshToken).as_deref(),
        Some("a-rt")
    );
    assert_eq!(get(&b, SecretKind::CalDavPassword).as_deref(), Some("b-pw"));
    assert_eq!(get(&b, SecretKind::GraphRefreshToken), None);

    store.delete_all(&a).unwrap();
    assert_eq!(get(&a, SecretKind::CalDavPassword), None);
    assert_eq!(get(&a, SecretKind::GraphRefreshToken), None);
    assert_eq!(get(&b, SecretKind::CalDavPassword).as_deref(), Some("b-pw"));
    // Idempotent.
    store.delete_all(&a).unwrap();
}

#[test]
fn in_memory_handles_large_values() {
    let store = InMemorySecretStore::new();
    let acc = AccountId::new("acc-1");
    let token = "x".repeat(MAX_SECRET_BYTES * 2);
    store
        .set(
            &acc,
            SecretKind::GraphRefreshToken,
            &SecretString::new(&*token),
        )
        .unwrap();
    assert_eq!(
        store
            .get(&acc, SecretKind::GraphRefreshToken)
            .unwrap()
            .unwrap()
            .expose(),
        token
    );
}

#[test]
fn in_memory_debug_is_redacted() {
    let store = InMemorySecretStore::new();
    store
        .set(
            &AccountId::new("acc-1"),
            SecretKind::CalDavPassword,
            &SecretString::new("hunter2"),
        )
        .unwrap();
    assert!(!format!("{store:?}").contains("hunter2"));
}

#[test]
fn stores_are_usable_as_shared_trait_objects() {
    let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
    let acc = AccountId::new("acc-1");
    store
        .set(&acc, SecretKind::CalDavPassword, &SecretString::new("pw"))
        .unwrap();
    let clone = Arc::clone(&store);
    std::thread::spawn(move || {
        assert!(clone
            .get(&acc, SecretKind::CalDavPassword)
            .unwrap()
            .is_some());
    })
    .join()
    .unwrap();
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
#[test]
fn keyring_store_fails_cleanly_without_a_supported_keychain() {
    assert!(matches!(
        KeyringSecretStore::new(),
        Err(Error::Keychain { .. })
    ));
}

/// Round trip against the real OS keychain. Ignored by default because CI
/// runners may not have an unlocked keychain. Run locally with
/// `cargo test secrets::tests::real_keychain -- --ignored`.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[test]
#[ignore = "touches the real OS keychain"]
fn real_keychain_round_trip() {
    let store = KeyringSecretStore::new().expect("keychain opens");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let acc = AccountId::new(format!("test-{}-{nanos}", std::process::id()));

    // Clean up even if an assertion fails.
    struct Cleanup<'a>(&'a KeyringSecretStore, &'a AccountId);
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            let _ = self.0.delete_all(self.1);
        }
    }
    let _cleanup = Cleanup(&store, &acc);

    let pw = SecretKind::CalDavPassword;
    let rt = SecretKind::GraphRefreshToken;

    assert!(store.get(&acc, pw).unwrap().is_none());

    store.set(&acc, pw, &SecretString::new("first")).unwrap();
    assert_eq!(store.get(&acc, pw).unwrap().unwrap().expose(), "first");

    store
        .set(&acc, pw, &SecretString::new("second ünïcødé €"))
        .unwrap();
    assert_eq!(
        store.get(&acc, pw).unwrap().unwrap().expose(),
        "second ünïcødé €"
    );

    // Larger than Windows' 2560-byte blob limit.
    for len in [4 * 1024, 8 * 1024] {
        let token: String = (0..len)
            .map(|i| char::from(b'A' + ((i * 7 + i / 13) % 26) as u8))
            .collect();
        store.set(&acc, rt, &SecretString::new(&*token)).unwrap();
        assert_eq!(
            store.get(&acc, rt).unwrap().unwrap().expose(),
            token,
            "len {len}"
        );
    }
    // Shrink back to one entry.
    store.set(&acc, rt, &SecretString::new("short")).unwrap();
    assert_eq!(store.get(&acc, rt).unwrap().unwrap().expose(), "short");

    store.delete(&acc, pw).unwrap();
    assert!(store.get(&acc, pw).unwrap().is_none());
    store.delete(&acc, pw).unwrap();

    store
        .set(&acc, rt, &SecretString::new("x".repeat(6000)))
        .unwrap();
    store.delete_all(&acc).unwrap();
    assert!(store.get(&acc, pw).unwrap().is_none());
    assert!(store.get(&acc, rt).unwrap().is_none());
}
