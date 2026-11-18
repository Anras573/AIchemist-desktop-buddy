//! Tests for [`Accounts`]: in-memory keychain, fake providers, temporary
//! settings directory; the Graph sign-in runs against a canned token
//! endpoint and a "browser" that hits the loopback listener directly.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use chrono::{TimeZone, Utc};

use super::identities::{Identities, IDENTITIES_FILE_NAME};
use super::service::{tray_message, Accounts};
use crate::domain::ipc::{AddAccountRequest, SyncState, UpdateAccountRequest};
use crate::domain::{
    AccountConfig, AccountConnection, AccountId, CalendarId, CalendarInfo, CalendarProvider, Error,
    FakeCalendarProvider, FakeClock, Result, SecretString,
};
use crate::graph::auth::{BrowserOpener, CancelToken};
use crate::graph::factory::GraphProviderFactory;
use crate::graph::http::{HttpClient, HttpResponse};
use crate::secrets::{InMemorySecretStore, SecretKind, SecretStore};
use crate::settings_service::SettingsService;
use crate::storage::{load_settings, Storage, SETTINGS_FILE_NAME};
use crate::sync::{AccountSyncStatus, ProviderFactory};

const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("aichemist-accounts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Providers: one shared fake per account id, created on demand.
#[derive(Default)]
struct FakeFactory {
    providers: Mutex<HashMap<AccountId, Arc<FakeCalendarProvider>>>,
}

impl FakeFactory {
    fn get(&self, id: &AccountId) -> Arc<FakeCalendarProvider> {
        self.providers
            .lock()
            .unwrap()
            .entry(id.clone())
            .or_insert_with(|| Arc::new(FakeCalendarProvider::new(id.clone())))
            .clone()
    }
}

impl ProviderFactory for FakeFactory {
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
        Ok(self.get(&account.id))
    }
}

/// Canned token endpoint.
#[derive(Default)]
struct FakeHttp {
    responses: Mutex<VecDeque<HttpResponse>>,
}

impl HttpClient for FakeHttp {
    fn post_form(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse> {
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP request"))
    }
    fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse> {
        panic!("no GET expected")
    }
}

struct Fixture {
    _dir: TempDir,
    dir: PathBuf,
    settings: SettingsService,
    secrets: Arc<InMemorySecretStore>,
    providers: Arc<FakeFactory>,
    http: Arc<FakeHttp>,
    accounts: Accounts,
}

fn fixture(name: &str) -> Fixture {
    let temp = TempDir::new(name);
    let dir = temp.0.clone();
    let secrets = Arc::new(InMemorySecretStore::new());
    let providers = Arc::new(FakeFactory::default());
    let http = Arc::new(FakeHttp::default());
    let graph = Arc::new(GraphProviderFactory::new(
        secrets.clone(),
        http.clone(),
        Arc::new(FakeClock::new(
            Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap(),
        )),
    ));
    let accounts = Accounts::new(
        secrets.clone(),
        providers.clone(),
        Some(graph),
        Identities::load(&dir.join(IDENTITIES_FILE_NAME)),
    );
    Fixture {
        settings: SettingsService::load(&dir.join(SETTINGS_FILE_NAME)),
        _dir: temp,
        dir,
        secrets,
        providers,
        http,
        accounts,
    }
}

impl Fixture {
    fn settings_file(&self) -> String {
        std::fs::read_to_string(self.dir.join(SETTINGS_FILE_NAME)).unwrap_or_default()
    }

    fn password(&self, id: &AccountId) -> Option<String> {
        self.secrets
            .get(id, SecretKind::CalDavPassword)
            .unwrap()
            .map(|s| s.expose().to_owned())
    }
}

fn caldav(url: &str, password: Option<&str>) -> AddAccountRequest {
    AddAccountRequest {
        display_name: " Home ".into(),
        connection: AccountConnection::CalDav {
            server_url: url.into(),
            username: " jane ".into(),
        },
        secret: password.map(SecretString::new),
    }
}

fn graph(client_id: &str, tenant: &str) -> AddAccountRequest {
    AddAccountRequest {
        display_name: "Work".into(),
        connection: AccountConnection::Graph {
            client_id: client_id.into(),
            tenant: tenant.into(),
        },
        secret: None,
    }
}

fn fields(err: Error) -> Vec<String> {
    match err {
        Error::Validation { errors } => errors.into_iter().map(|e| e.field).collect(),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Add
// ---------------------------------------------------------------------------

#[test]
fn add_caldav_stores_the_password_only_in_the_keychain() {
    let f = fixture("add-caldav");
    let account = f
        .accounts
        .add(
            &f.settings,
            caldav(" https://caldav.example.com/ ", Some("hunter2-app-pw")),
        )
        .unwrap();

    assert_eq!(account.display_name, "Home");
    assert_eq!(account.id.as_str().len(), 32);
    assert_eq!(
        account.connection,
        AccountConnection::CalDav {
            server_url: "https://caldav.example.com/".into(),
            username: "jane".into(),
        }
    );
    assert_eq!(f.password(&account.id).as_deref(), Some("hunter2-app-pw"));
    let file = f.settings_file();
    assert!(file.contains(account.id.as_str()));
    assert!(
        !file.contains("hunter2"),
        "the password must not be in settings"
    );
    assert_eq!(
        load_settings(&f.dir.join(SETTINGS_FILE_NAME)).accounts,
        std::slice::from_ref(&account)
    );

    let list = f.accounts.list(&f.settings.current(), None);
    assert_eq!(list.len(), 1);
    assert!(list[0].has_secret);
    assert_eq!(list[0].sync_state, SyncState::NeverSynced);
    assert_eq!(list[0].signed_in_as, None);
}

#[test]
fn add_rejects_invalid_input_and_changes_nothing() {
    let f = fixture("add-invalid");
    let err = f
        .accounts
        .add(&f.settings, caldav("http://caldav.example.com", None))
        .unwrap_err();
    assert_eq!(fields(err), ["connection.serverUrl", "secret"]);

    let mut bad = caldav("https://user:pw@caldav.example.com", Some("pw"));
    bad.display_name = "  ".into();
    bad.connection = AccountConnection::CalDav {
        server_url: "https://user:pw@caldav.example.com".into(),
        username: "".into(),
    };
    assert_eq!(
        fields(f.accounts.add(&f.settings, bad).unwrap_err()),
        ["displayName", "connection.serverUrl", "connection.username"]
    );

    assert_eq!(
        fields(
            f.accounts
                .add(&f.settings, graph("not-a-guid", "contoso"))
                .unwrap_err()
        ),
        ["connection.clientId", "connection.tenant"]
    );
    let mut with_secret = graph(CLIENT_ID, "common");
    with_secret.secret = Some(SecretString::new("x"));
    assert!(matches!(
        f.accounts.add(&f.settings, with_secret),
        Err(Error::InvalidInput { .. })
    ));

    assert!(f.secrets.is_empty());
    assert!(f.settings.current().accounts.is_empty());
    assert_eq!(f.settings_file(), "");
}

#[test]
fn add_graph_normalizes_ids_and_needs_sign_in() {
    let f = fixture("add-graph");
    let account = f
        .accounts
        .add(&f.settings, graph(&format!(" {CLIENT_ID} "), "  "))
        .unwrap();
    assert_eq!(
        account.connection,
        AccountConnection::Graph {
            client_id: CLIENT_ID.into(),
            tenant: "common".into(),
        }
    );
    let status = f
        .accounts
        .status(&f.settings.current(), None, &account.id)
        .unwrap();
    assert!(!status.has_secret);
    assert_eq!(status.signed_in_as, None);
    assert!(f.secrets.is_empty());
}

// ---------------------------------------------------------------------------
// Update and remove
// ---------------------------------------------------------------------------

fn update_of(account: &AccountConfig) -> UpdateAccountRequest {
    UpdateAccountRequest {
        account_id: account.id.clone(),
        display_name: account.display_name.clone(),
        connection: account.connection.clone(),
        included_calendars: account.included_calendars.clone(),
        secret: None,
    }
}

#[test]
fn update_replaces_the_password_only_when_given() {
    let f = fixture("update");
    let account = f
        .accounts
        .add(
            &f.settings,
            caldav("https://caldav.example.com", Some("old-pw")),
        )
        .unwrap();

    let mut request = update_of(&account);
    request.display_name = "Family".into();
    request.included_calendars = vec![
        CalendarId::new("/cal/a/"),
        CalendarId::new("/cal/b/"),
        CalendarId::new("/cal/a/"),
    ];
    request.secret = Some(SecretString::new(""));
    let updated = f.accounts.update(&f.settings, request).unwrap();
    assert_eq!(updated.display_name, "Family");
    assert_eq!(
        updated.included_calendars,
        [CalendarId::new("/cal/a/"), CalendarId::new("/cal/b/")]
    );
    assert_eq!(f.password(&account.id).as_deref(), Some("old-pw"));

    let mut request = update_of(&updated);
    request.secret = Some(SecretString::new("new-pw"));
    f.accounts.update(&f.settings, request).unwrap();
    assert_eq!(f.password(&account.id).as_deref(), Some("new-pw"));
    assert!(!f.settings_file().contains("new-pw"));
    assert_eq!(f.settings.current().accounts[0].display_name, "Family");
}

#[test]
fn update_rejects_type_change_unknown_account_and_bad_fields() {
    let f = fixture("update-bad");
    let account = f
        .accounts
        .add(
            &f.settings,
            caldav("https://caldav.example.com", Some("pw")),
        )
        .unwrap();

    let mut request = update_of(&account);
    request.connection = AccountConnection::Graph {
        client_id: CLIENT_ID.into(),
        tenant: "common".into(),
    };
    assert!(matches!(
        f.accounts.update(&f.settings, request),
        Err(Error::InvalidInput { .. })
    ));

    let mut request = update_of(&account);
    request.account_id = AccountId::new("missing");
    assert!(matches!(
        f.accounts.update(&f.settings, request),
        Err(Error::NotFound { .. })
    ));

    let mut request = update_of(&account);
    request.connection = AccountConnection::CalDav {
        server_url: "http://insecure.example.com".into(),
        username: "jane".into(),
    };
    request.secret = Some(SecretString::new("other"));
    assert_eq!(
        fields(f.accounts.update(&f.settings, request).unwrap_err()),
        ["connection.serverUrl"]
    );
    assert_eq!(f.password(&account.id).as_deref(), Some("pw"));
    assert_eq!(f.settings.current().accounts, [account]);
}

#[test]
fn remove_deletes_settings_secrets_identity_status_and_cache() {
    let f = fixture("remove");
    let keep = f
        .accounts
        .add(
            &f.settings,
            caldav("https://keep.example.com", Some("keep-pw")),
        )
        .unwrap();
    let gone = f
        .accounts
        .add(&f.settings, graph(CLIENT_ID, "common"))
        .unwrap();
    f.secrets
        .set(
            &gone.id,
            SecretKind::GraphRefreshToken,
            &SecretString::new("rt"),
        )
        .unwrap();
    f.accounts.set_sync_status(failing(&gone.id));

    let clock = Arc::new(FakeClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap(),
    ));
    let storage = Storage::open_in_memory(clock).unwrap();
    storage.record_sync_success(&gone.id).unwrap();

    f.accounts
        .remove(&f.settings, Some(&storage), &gone.id)
        .unwrap();
    assert_eq!(f.settings.current().accounts, std::slice::from_ref(&keep));
    assert!(f
        .secrets
        .get(&gone.id, SecretKind::GraphRefreshToken)
        .unwrap()
        .is_none());
    assert_eq!(f.password(&keep.id).as_deref(), Some("keep-pw"));
    assert!(storage.sync_state(&gone.id).unwrap().is_none());
    assert_eq!(f.accounts.tray_message(&f.settings.current()), None);
    assert!(matches!(
        f.accounts.remove(&f.settings, None, &gone.id),
        Err(Error::NotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// Provider calls
// ---------------------------------------------------------------------------

#[test]
fn calendars_and_connection_test_go_through_the_factory() {
    let f = fixture("provider");
    let account = f
        .accounts
        .add(
            &f.settings,
            caldav("https://caldav.example.com", Some("pw")),
        )
        .unwrap();
    let calendar = CalendarInfo {
        id: CalendarId::new("/cal/home/"),
        account_id: account.id.clone(),
        name: "Home".into(),
        color: None,
    };
    f.providers
        .get(&account.id)
        .set_calendars(vec![calendar.clone()]);
    let settings = f.settings.current();
    assert_eq!(
        f.accounts.list_calendars(&settings, &account.id).unwrap(),
        [calendar]
    );
    f.accounts.test_connection(&settings, &account.id).unwrap();

    f.providers
        .get(&account.id)
        .fail_with(Error::network("could not connect"));
    assert!(matches!(
        f.accounts.test_connection(&settings, &account.id),
        Err(Error::Network { .. })
    ));
    assert!(matches!(
        f.accounts
            .list_calendars(&settings, &AccountId::new("missing")),
        Err(Error::NotFound { .. })
    ));
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

fn failing(id: &AccountId) -> AccountSyncStatus {
    AccountSyncStatus {
        account_id: id.clone(),
        sync_state: SyncState::Error,
        last_synced_at: None,
        last_error: Some(Error::network("timed out")),
        next_retry_at: None,
    }
}

#[test]
fn status_comes_from_the_engine_then_storage_then_never_synced() {
    let f = fixture("status");
    let a = f
        .accounts
        .add(&f.settings, caldav("https://a.example.com", Some("pw")))
        .unwrap();
    let b = f
        .accounts
        .add(&f.settings, caldav("https://b.example.com", Some("pw")))
        .unwrap();
    let c = f
        .accounts
        .add(&f.settings, caldav("https://c.example.com", Some("pw")))
        .unwrap();

    let clock = Arc::new(FakeClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap(),
    ));
    let storage = Storage::open_in_memory(clock.clone()).unwrap();
    storage.record_sync_success(&b.id).unwrap();
    clock.advance(chrono::Duration::minutes(10));
    storage
        .record_sync_failure(&b.id, &Error::auth("rejected"))
        .unwrap();
    storage.record_sync_success(&c.id).unwrap();
    f.accounts.set_sync_status(failing(&a.id));

    let list = f.accounts.list(&f.settings.current(), Some(&storage));
    let states: Vec<_> = list.iter().map(|s| s.sync_state).collect();
    assert_eq!(states, [SyncState::Error, SyncState::Error, SyncState::Ok]);
    assert_eq!(list[0].last_error, Some(Error::network("timed out")));
    assert!(matches!(list[1].last_error, Some(Error::Auth { .. })));
    assert!(list[1].last_synced_at.is_some());
    assert_eq!(list[2].last_error, None);

    // Without storage, only the engine's status is known.
    let list = f.accounts.list(&f.settings.current(), None);
    assert_eq!(list[1].sync_state, SyncState::NeverSynced);
}

#[test]
fn tray_message_names_failing_accounts_only() {
    let account = |id: &str, name: &str| AccountConfig {
        id: AccountId::new(id),
        display_name: name.into(),
        connection: AccountConnection::CalDav {
            server_url: "https://x.example.com".into(),
            username: "u".into(),
        },
        included_calendars: vec![],
    };
    let accounts = [
        account("a", "Work"),
        account("b", "Home"),
        account("c", "Club"),
    ];
    let mut statuses = HashMap::new();
    assert_eq!(tray_message(&accounts, &statuses), None);

    let mut ok = failing(&AccountId::new("a"));
    ok.sync_state = SyncState::Ok;
    statuses.insert(ok.account_id.clone(), ok);
    assert_eq!(tray_message(&accounts, &statuses), None);

    statuses.insert(AccountId::new("b"), failing(&AccountId::new("b")));
    assert_eq!(
        tray_message(&accounts, &statuses).as_deref(),
        Some("Calendar sync problem: Home")
    );
    statuses.insert(AccountId::new("c"), failing(&AccountId::new("c")));
    statuses.insert(AccountId::new("gone"), failing(&AccountId::new("gone")));
    assert_eq!(
        tray_message(&accounts, &statuses).as_deref(),
        Some("Calendar sync problem: Home and 1 more")
    );
}

// ---------------------------------------------------------------------------
// Graph sign-in
// ---------------------------------------------------------------------------

/// Sends the redirect straight to the loopback listener.
struct FakeBrowser;

impl BrowserOpener for FakeBrowser {
    fn open(&self, url: &str) -> Result<()> {
        let query = url.split_once('?').unwrap().1;
        let q: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let port: u16 = q["redirect_uri"]
            .strip_prefix("http://localhost:")
            .unwrap()
            .parse()
            .unwrap();
        let state = q["state"].clone();
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                s,
                "GET /?code=code-1&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            let _ = s.read_to_string(&mut response);
        });
        Ok(())
    }
}

/// Never comes back.
struct SilentBrowser;

impl BrowserOpener for SilentBrowser {
    fn open(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

fn token_response(claims: serde_json::Value) -> HttpResponse {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let id_token = format!(
        "{}.{}.sig",
        b64.encode(br#"{"alg":"none"}"#),
        b64.encode(claims.to_string())
    );
    let body = serde_json::json!({
        "token_type": "Bearer",
        "expires_in": 3600,
        "access_token": "access-1",
        "refresh_token": "refresh-1",
        "id_token": id_token,
    });
    HttpResponse::new(200, body.to_string())
}

#[test]
fn sign_in_persists_signed_in_as() {
    let f = fixture("sign-in");
    let account = f
        .accounts
        .add(&f.settings, graph(CLIENT_ID, "common"))
        .unwrap();
    f.http
        .responses
        .lock()
        .unwrap()
        .push_back(token_response(serde_json::json!({
            "name": "Jane Doe", "preferred_username": "jane@example.com"
        })));

    f.accounts
        .sign_in(
            &f.settings.current(),
            &account.id,
            &FakeBrowser,
            CancelToken::new(),
        )
        .unwrap();
    let status = f
        .accounts
        .status(&f.settings.current(), None, &account.id)
        .unwrap();
    assert!(status.has_secret);
    assert_eq!(status.signed_in_as.as_deref(), Some("jane@example.com"));
    // Survives a restart, and stays out of settings and the keychain.
    let reloaded = Identities::load(&f.dir.join(IDENTITIES_FILE_NAME));
    assert_eq!(
        reloaded.get(&account.id).as_deref(),
        Some("jane@example.com")
    );
    assert!(!f.settings_file().contains("jane@"));

    // Without a username, the display name is used.
    f.http
        .responses
        .lock()
        .unwrap()
        .push_back(token_response(serde_json::json!({ "name": "Jane Doe" })));
    f.accounts
        .sign_in(
            &f.settings.current(),
            &account.id,
            &FakeBrowser,
            CancelToken::new(),
        )
        .unwrap();
    assert_eq!(
        f.accounts.signed_in_as(&account.id).as_deref(),
        Some("Jane Doe")
    );

    // Removing the account forgets it.
    f.accounts.remove(&f.settings, None, &account.id).unwrap();
    assert_eq!(
        Identities::load(&f.dir.join(IDENTITIES_FILE_NAME)).get(&account.id),
        None
    );
}

#[test]
fn sign_in_is_cancelled_by_cancel_sign_ins() {
    let f = Arc::new(fixture("cancel"));
    let account = f
        .accounts
        .add(&f.settings, graph(CLIENT_ID, "common"))
        .unwrap();
    let runner = {
        let f = f.clone();
        let id = account.id.clone();
        std::thread::spawn(move || {
            f.accounts.sign_in(
                &f.settings.current(),
                &id,
                &SilentBrowser,
                CancelToken::new(),
            )
        })
    };
    // Cancel until the sign-in has registered and returned.
    let started = std::time::Instant::now();
    while !runner.is_finished() {
        assert!(
            started.elapsed().as_secs() < 10,
            "sign-in was not cancelled"
        );
        f.accounts.cancel_sign_ins();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(matches!(runner.join().unwrap(), Err(Error::Auth { .. })));
    assert_eq!(f.accounts.signed_in_as(&account.id), None);
}

#[test]
fn sign_in_rejects_caldav_accounts() {
    let f = fixture("sign-in-caldav");
    let account = f
        .accounts
        .add(
            &f.settings,
            caldav("https://caldav.example.com", Some("pw")),
        )
        .unwrap();
    assert!(matches!(
        f.accounts.sign_in(
            &f.settings.current(),
            &account.id,
            &FakeBrowser,
            CancelToken::new()
        ),
        Err(Error::InvalidInput { .. })
    ));
}
