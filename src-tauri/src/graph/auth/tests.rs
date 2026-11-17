//! Tests for [`GraphAuth`]. No network: HTTP is a canned [`FakeHttp`] and the
//! "browser" is a thread that hits the loopback listener directly.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;

use chrono::TimeZone;

use super::*;
use crate::domain::FakeClock;
use crate::secrets::InMemorySecretStore;

const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";

/// One recorded request: URL and form fields.
type Request = (String, HashMap<String, String>);

#[derive(Default)]
struct FakeHttp {
    responses: Mutex<VecDeque<Result<HttpResponse>>>,
    requests: Mutex<Vec<Request>>,
}

impl FakeHttp {
    fn push(&self, status: u16, body: &str) {
        self.responses
            .lock()
            .unwrap()
            .push_back(Ok(HttpResponse::new(status, body.as_bytes())));
    }

    fn push_err(&self, err: Error) {
        self.responses.lock().unwrap().push_back(Err(err));
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpClient for FakeHttp {
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<HttpResponse> {
        self.requests.lock().unwrap().push((
            url.to_owned(),
            form.iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ));
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP request")
    }

    fn get(&self, _url: &str, _headers: &[(&str, &str)]) -> Result<HttpResponse> {
        panic!("the auth code never sends GET")
    }
}

struct Fixture {
    auth: GraphAuth,
    http: Arc<FakeHttp>,
    secrets: Arc<InMemorySecretStore>,
    clock: Arc<FakeClock>,
    account: AccountId,
}

fn fixture() -> Fixture {
    let account = AccountConfig {
        id: AccountId::new("acc-1"),
        display_name: "Work".into(),
        connection: AccountConnection::Graph {
            client_id: format!(" {CLIENT_ID} "),
            tenant: "Common".into(),
        },
        included_calendars: vec![],
    };
    let http = Arc::new(FakeHttp::default());
    let secrets = Arc::new(InMemorySecretStore::new());
    let clock = Arc::new(FakeClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap(),
    ));
    let auth = GraphAuth::new(&account, secrets.clone(), http.clone(), clock.clone())
        .unwrap()
        .with_sign_in_timeout(Duration::from_secs(10));
    Fixture {
        auth,
        http,
        secrets,
        clock,
        account: account.id,
    }
}

fn token_json(access: &str, refresh: Option<&str>, expires_in: u64) -> String {
    let mut v = serde_json::json!({
        "token_type": "Bearer",
        "scope": "https://graph.microsoft.com/Calendars.Read",
        "expires_in": expires_in,
        "access_token": access,
    });
    if let Some(r) = refresh {
        v["refresh_token"] = r.into();
    }
    v.to_string()
}

fn stored_refresh(f: &Fixture) -> Option<String> {
    f.secrets
        .get(&f.account, SecretKind::GraphRefreshToken)
        .unwrap()
        .map(|s| s.expose().to_owned())
}

fn signed_in(f: &Fixture, refresh: &str) {
    f.secrets
        .set(
            &f.account,
            SecretKind::GraphRefreshToken,
            &SecretString::new(refresh),
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// PKCE, state, validation, URL
// ---------------------------------------------------------------------------

#[test]
fn pkce_s256_matches_rfc7636_appendix_b() {
    // The appendix's 32 octets encode (unpadded base64url, as
    // `Pkce::generate` does) to its verifier ...
    let octets: [u8; 32] = [
        116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37,
        77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
    ];
    let verifier = B64URL.encode(octets);
    assert_eq!(verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    // ... which hashes to its challenge.
    assert_eq!(
        s256_challenge(&verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn generated_pkce_and_state_are_random_and_well_formed() {
    let a = Pkce::generate().unwrap();
    let b = Pkce::generate().unwrap();
    assert_eq!(a.verifier.expose().len(), 43);
    assert_ne!(a.verifier, b.verifier);
    assert_eq!(a.challenge, s256_challenge(a.verifier.expose()));
    // RFC 7636 4.1: unreserved characters only.
    assert!(a
        .verifier
        .expose()
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"-._~".contains(&c)));
    let s1 = random_b64url(16).unwrap();
    assert_eq!(s1.len(), 22);
    assert_ne!(s1, random_b64url(16).unwrap());
}

#[test]
fn validates_tenant() {
    for ok in [
        "common",
        "ORGANIZATIONS",
        " consumers ",
        "72f988bf-86f1-41af-91ab-2d7cd011db47",
        "contoso.onmicrosoft.com",
    ] {
        assert!(validate_tenant(ok).is_ok(), "{ok}");
    }
    assert_eq!(validate_tenant(" Common ").unwrap(), "common");
    for bad in [
        "",
        "contoso",
        "../common",
        "common/oauth2",
        "a b.com",
        "x.-y.com",
        "72f988bf-86f1-41af-91ab",
        "evil.com?x=",
    ] {
        assert!(
            matches!(validate_tenant(bad), Err(Error::InvalidInput { .. })),
            "{bad}"
        );
    }
}

#[test]
fn validates_client_id_and_account_type() {
    assert_eq!(
        validate_client_id(&format!(" {CLIENT_ID}\n")).unwrap(),
        CLIENT_ID
    );
    assert!(validate_client_id("my-app").is_err());
    assert!(validate_client_id("11111111-2222-3333-4444-55555555555g").is_err());

    let caldav = AccountConfig {
        id: AccountId::new("c"),
        display_name: "Home".into(),
        connection: AccountConnection::CalDav {
            server_url: "https://example.com".into(),
            username: "me".into(),
        },
        included_calendars: vec![],
    };
    let err = GraphAuth::new(
        &caldav,
        Arc::new(InMemorySecretStore::new()),
        Arc::new(FakeHttp::default()),
        Arc::new(FakeClock::new(Utc::now())),
    )
    .unwrap_err();
    assert!(matches!(err, Error::InvalidInput { .. }));
}

#[test]
fn authorize_url_has_the_pkce_and_loopback_parameters() {
    let url = authorize_url("common", CLIENT_ID, "http://localhost:51234", "st", "chal");
    let (base, query) = url.split_once('?').unwrap();
    assert_eq!(
        base,
        "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
    );
    let q: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    assert_eq!(q["client_id"], CLIENT_ID);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["redirect_uri"], "http://localhost:51234");
    assert_eq!(q["state"], "st");
    assert_eq!(q["code_challenge"], "chal");
    assert_eq!(q["code_challenge_method"], "S256");
    let scopes: Vec<&str> = q["scope"].split(' ').collect();
    assert!(scopes.contains(&"offline_access"));
    assert!(scopes.contains(&GRAPH_CALENDARS_READ));
}

// ---------------------------------------------------------------------------
// Interactive sign-in (loopback + mocked token endpoint)
// ---------------------------------------------------------------------------

/// A browser that "signs in" by sending the redirect straight to the
/// loopback listener, optionally with a tampered state.
struct FakeBrowser {
    tamper_state: bool,
    opened: Mutex<Option<String>>,
}

impl FakeBrowser {
    fn new(tamper_state: bool) -> Self {
        Self {
            tamper_state,
            opened: Mutex::new(None),
        }
    }
}

impl BrowserOpener for FakeBrowser {
    fn open(&self, url: &str) -> Result<()> {
        *self.opened.lock().unwrap() = Some(url.to_owned());
        let query = url.split_once('?').unwrap().1;
        let q: HashMap<String, String> = form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        let port: u16 = q["redirect_uri"]
            .strip_prefix("http://localhost:")
            .unwrap()
            .parse()
            .unwrap();
        let state = if self.tamper_state {
            "forged".to_owned()
        } else {
            q["state"].clone()
        };
        std::thread::spawn(move || {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                s,
                "GET /?code=auth-code-1&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            let _ = s.read_to_string(&mut response);
        });
        Ok(())
    }
}

fn id_token(claims: serde_json::Value) -> String {
    format!(
        "{}.{}.sig",
        B64URL.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        B64URL.encode(claims.to_string())
    )
}

#[test]
fn sign_in_exchanges_the_code_and_stores_the_refresh_token() {
    let f = fixture();
    let mut body: serde_json::Value =
        serde_json::from_str(&token_json("access-1", Some("refresh-1"), 3600)).unwrap();
    body["id_token"] = id_token(serde_json::json!({
        "name": "Jane Doe", "preferred_username": "jane@example.com", "oid": "x"
    }))
    .into();
    f.http.push(200, &body.to_string());

    let browser = FakeBrowser::new(false);
    let identity = f.auth.sign_in(&browser, &CancelToken::new()).unwrap();
    assert_eq!(
        identity,
        SignedInIdentity {
            display_name: Some("Jane Doe".into()),
            username: Some("jane@example.com".into()),
        }
    );
    assert_eq!(stored_refresh(&f).as_deref(), Some("refresh-1"));

    let requests = f.http.requests();
    assert_eq!(requests.len(), 1);
    let (url, form) = &requests[0];
    assert_eq!(
        url,
        "https://login.microsoftonline.com/common/oauth2/v2.0/token"
    );
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["code"], "auth-code-1");
    assert_eq!(form["client_id"], CLIENT_ID);
    assert!(!form.contains_key("client_secret"));
    // The verifier sent matches the challenge in the authorize URL, and the
    // redirect URI is the same in both requests.
    let opened = browser.opened.lock().unwrap().clone().unwrap();
    let q: HashMap<String, String> =
        form_urlencoded::parse(opened.split_once('?').unwrap().1.as_bytes())
            .into_owned()
            .collect();
    assert_eq!(s256_challenge(&form["code_verifier"]), q["code_challenge"]);
    assert_eq!(form["redirect_uri"], q["redirect_uri"]);

    // The access token from the sign-in is cached: no further request.
    assert_eq!(f.auth.access_token().unwrap().expose(), "access-1");
    assert_eq!(f.http.requests().len(), 1);
}

#[test]
fn sign_in_rejects_a_forged_state_without_redeeming_the_code() {
    let f = fixture();
    let err = f
        .auth
        .sign_in(&FakeBrowser::new(true), &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
    assert!(f.http.requests().is_empty());
    assert_eq!(stored_refresh(&f), None);
}

#[test]
fn sign_in_without_refresh_token_is_an_auth_error() {
    let f = fixture();
    f.http.push(200, &token_json("access-1", None, 3600));
    let err = f
        .auth
        .sign_in(&FakeBrowser::new(false), &CancelToken::new())
        .unwrap_err();
    assert!(matches!(err, Error::Auth { .. }));
    assert!(err.to_string().contains("offline_access"));
    assert_eq!(stored_refresh(&f), None);
}

#[test]
fn sign_in_times_out_when_the_browser_never_returns() {
    struct NoBrowser;
    impl BrowserOpener for NoBrowser {
        fn open(&self, _url: &str) -> Result<()> {
            Ok(())
        }
    }
    let f = fixture();
    let auth = f.auth.with_sign_in_timeout(Duration::from_millis(200));
    let err = auth.sign_in(&NoBrowser, &CancelToken::new()).unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
}

#[test]
fn sign_out_deletes_the_refresh_token_and_cached_access_token() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(200, &token_json("access-1", None, 3600));
    f.auth.access_token().unwrap();
    f.auth.sign_out().unwrap();
    assert_eq!(stored_refresh(&f), None);
    let err = f.auth.access_token().unwrap_err();
    assert!(matches!(err, Error::Auth { .. }));
    // Idempotent.
    f.auth.sign_out().unwrap();
}

// ---------------------------------------------------------------------------
// Refresh and caching
// ---------------------------------------------------------------------------

#[test]
fn access_token_without_sign_in_is_an_auth_error() {
    let f = fixture();
    let err = f.auth.access_token().unwrap_err();
    assert!(matches!(err, Error::Auth { .. }));
    assert!(f.http.requests().is_empty());
}

#[test]
fn refresh_sends_the_stored_token_and_persists_rotation() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http
        .push(200, &token_json("access-1", Some("refresh-2"), 3600));
    assert_eq!(f.auth.access_token().unwrap().expose(), "access-1");
    let (url, form) = &f.http.requests()[0];
    assert!(url.ends_with("/common/oauth2/v2.0/token"));
    assert_eq!(form["grant_type"], "refresh_token");
    assert_eq!(form["refresh_token"], "refresh-1");
    assert_eq!(form["client_id"], CLIENT_ID);
    assert!(form["scope"].contains("offline_access"));
    assert_eq!(stored_refresh(&f).as_deref(), Some("refresh-2"));
}

#[test]
fn refresh_without_rotation_keeps_the_stored_token() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(200, &token_json("access-1", None, 3600));
    f.auth.access_token().unwrap();
    assert_eq!(stored_refresh(&f).as_deref(), Some("refresh-1"));
}

#[test]
fn access_token_is_cached_until_near_expiry() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(200, &token_json("access-1", None, 3600));
    assert_eq!(f.auth.access_token().unwrap().expose(), "access-1");

    // 54 minutes later: 6 minutes left, still outside the 5-minute margin.
    f.clock.advance(chrono::Duration::minutes(54));
    assert_eq!(f.auth.access_token().unwrap().expose(), "access-1");
    assert_eq!(f.http.requests().len(), 1);

    // 55 minutes 1 second: inside the margin, so refreshed proactively.
    f.clock.advance(chrono::Duration::seconds(61));
    f.http.push(200, &token_json("access-2", None, 3600));
    assert_eq!(f.auth.access_token().unwrap().expose(), "access-2");
    assert_eq!(f.http.requests().len(), 2);
}

#[test]
fn invalidate_after_401_forces_one_refresh() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(200, &token_json("access-1", None, 3600));
    let first = f.auth.access_token().unwrap();

    // A stale token from elsewhere does not drop the current one.
    f.auth.invalidate(&SecretString::new("old"));
    assert_eq!(f.auth.access_token().unwrap(), first);

    f.auth.invalidate(&first);
    f.http.push(200, &token_json("access-2", None, 3600));
    assert_eq!(f.auth.access_token().unwrap().expose(), "access-2");
    assert_eq!(f.http.requests().len(), 2);
}

#[test]
fn accepts_expires_in_as_a_string() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(
        200,
        r#"{"token_type":"Bearer","expires_in":"3600","access_token":"a"}"#,
    );
    assert_eq!(f.auth.access_token().unwrap().expose(), "a");
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn refresh_error(status: u16, body: &str) -> Error {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(status, body);
    let err = f.auth.access_token().unwrap_err();
    // A failed refresh never deletes or replaces the stored token.
    assert_eq!(stored_refresh(&f).as_deref(), Some("refresh-1"));
    err
}

#[test]
fn invalid_grant_is_an_auth_error() {
    let err = refresh_error(
        400,
        r#"{"error":"invalid_grant","error_description":"AADSTS70008: The refresh token for jane@example.com has expired","error_codes":[70008]}"#,
    );
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains("invalid_grant") && msg.contains("AADSTS70008"),
        "{msg}"
    );
    assert!(
        !msg.contains("jane"),
        "error_description must not leak: {msg}"
    );
    assert!(!msg.contains("refresh-1"));
}

#[test]
fn consent_revoked_and_config_errors_are_auth_errors() {
    for body in [
        r#"{"error":"interaction_required","error_codes":[65001]}"#,
        r#"{"error":"invalid_client","error_codes":[7000218]}"#,
        r#"{"error":"unauthorized_client"}"#,
    ] {
        assert!(
            matches!(refresh_error(400, body), Error::Auth { .. }),
            "{body}"
        );
    }
}

#[test]
fn server_errors_and_throttling_are_network_errors() {
    for (status, body) in [
        (500, "<html>oops</html>"),
        (503, ""),
        (429, r#"{"error":"too_many"}"#),
        (400, r#"{"error":"temporarily_unavailable"}"#),
    ] {
        let err = refresh_error(status, body);
        assert!(matches!(err, Error::Network { .. }), "{status}: {err:?}");
        assert!(err.is_retryable());
    }
}

#[test]
fn transport_errors_pass_through_as_network_errors() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push_err(Error::network("request failed: timeout"));
    assert!(f.auth.access_token().unwrap_err().is_retryable());
}

#[test]
fn malformed_responses_are_protocol_errors() {
    for (status, body) in [
        (200, "not json"),
        (200, r#"{"access_token":"a"}"#),
        (200, r#"{"access_token":"","expires_in":3600}"#),
        (
            200,
            r#"{"access_token":"a","expires_in":3600,"token_type":"pop"}"#,
        ),
        (400, "<html>bad</html>"),
        (302, ""),
    ] {
        let err = refresh_error(status, body);
        assert!(
            matches!(err, Error::Protocol { .. }),
            "{status} {body}: {err:?}"
        );
    }
}

#[test]
fn identity_is_empty_for_unreadable_id_tokens() {
    assert_eq!(
        identity_from_id_token("garbage"),
        SignedInIdentity::default()
    );
    assert_eq!(
        identity_from_id_token("a.!!!.c"),
        SignedInIdentity::default()
    );
    let only_upn = id_token(serde_json::json!({"preferred_username": "u@x.com"}));
    assert_eq!(
        identity_from_id_token(&only_upn).username.as_deref(),
        Some("u@x.com")
    );
}

#[test]
fn debug_output_never_contains_tokens() {
    let f = fixture();
    signed_in(&f, "refresh-1");
    f.http.push(200, &token_json("access-1", None, 3600));
    f.auth.access_token().unwrap();
    let dbg = format!("{:?}", f.auth);
    assert!(
        !dbg.contains("access-1") && !dbg.contains("refresh-1"),
        "{dbg}"
    );
    let resp = HttpResponse::new(200, b"secret-body".as_slice());
    assert!(!format!("{resp:?}").contains("secret-body"));
}

// ---------------------------------------------------------------------------
// Live test
// ---------------------------------------------------------------------------

/// Interactive end-to-end test against the real Microsoft endpoints. Opens
/// the browser; sign in and close the tab. Needs an app registration set up
/// as in docs/graph-setup.md:
///
/// ```text
/// GRAPH_CLIENT_ID=<guid> GRAPH_TENANT=common \
///   cargo test --lib graph::auth::tests::live_sign_in -- --ignored --nocapture
/// ```
///
/// Uses an in-memory secret store, so nothing is written to the keychain.
#[test]
#[ignore = "interactive: opens a browser and talks to login.microsoftonline.com"]
fn live_sign_in() {
    let client_id = std::env::var("GRAPH_CLIENT_ID").expect("set GRAPH_CLIENT_ID");
    let tenant = std::env::var("GRAPH_TENANT").unwrap_or_else(|_| "common".into());
    let account = AccountConfig {
        id: AccountId::new("live-test"),
        display_name: "Live".into(),
        connection: AccountConnection::Graph { client_id, tenant },
        included_calendars: vec![],
    };
    let auth = GraphAuth::new(
        &account,
        Arc::new(InMemorySecretStore::new()),
        Arc::new(super::super::http::UreqHttpClient::new()),
        Arc::new(crate::domain::SystemClock),
    )
    .unwrap();
    let identity = auth.sign_in(&SystemBrowser, &CancelToken::new()).unwrap();
    println!("signed in as {identity:?}");
    // Force a refresh through the real token endpoint.
    let first = auth.access_token().unwrap();
    auth.invalidate(&first);
    let second = auth.access_token().unwrap();
    assert!(!second.expose().is_empty());
    println!("refresh OK");
}
