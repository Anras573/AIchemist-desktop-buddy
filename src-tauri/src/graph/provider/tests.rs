//! Tests for [`GraphCalendarProvider`]. No network: the token endpoint and
//! Graph are a canned [`FakeHttp`]; Graph responses are fixtures with fake
//! data under `tests/fixtures/graph/`.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use chrono::TimeZone;

use super::*;
use crate::domain::{AccountConfig, AccountConnection, FakeClock, SecretString};
use crate::secrets::{InMemorySecretStore, SecretKind, SecretStore};

const CALENDARS_1: &str = include_str!("../../../tests/fixtures/graph/calendars_page1.json");
const CALENDARS_2: &str = include_str!("../../../tests/fixtures/graph/calendars_page2.json");
const VIEW_1: &str = include_str!("../../../tests/fixtures/graph/calendar_view_page1.json");
const VIEW_2: &str = include_str!("../../../tests/fixtures/graph/calendar_view_page2.json");

/// One recorded GET: URL and headers.
type Get = (String, HashMap<String, String>);

/// Token endpoint: each POST issues `access-1`, `access-2`, ... Graph: GETs
/// are answered from a queue and recorded.
#[derive(Default)]
struct FakeHttp {
    gets: Mutex<VecDeque<Result<HttpResponse>>>,
    requests: Mutex<Vec<Get>>,
    token_posts: Mutex<u32>,
}

impl FakeHttp {
    fn push(&self, status: u16, body: &str) {
        self.push_response(HttpResponse::new(status, body.as_bytes()));
    }

    fn push_response(&self, response: HttpResponse) {
        self.gets.lock().unwrap().push_back(Ok(response));
    }

    fn push_err(&self, err: Error) {
        self.gets.lock().unwrap().push_back(Err(err));
    }

    fn requests(&self) -> Vec<Get> {
        self.requests.lock().unwrap().clone()
    }

    fn token_posts(&self) -> u32 {
        *self.token_posts.lock().unwrap()
    }
}

impl HttpClient for FakeHttp {
    fn post_form(&self, url: &str, _form: &[(&str, &str)]) -> Result<HttpResponse> {
        assert!(url.ends_with("/oauth2/v2.0/token"), "{url}");
        let mut n = self.token_posts.lock().unwrap();
        *n += 1;
        let body = serde_json::json!({
            "token_type": "Bearer",
            "expires_in": 3600,
            "access_token": format!("access-{n}"),
        });
        Ok(HttpResponse::new(200, body.to_string()))
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.requests.lock().unwrap().push((
            url.to_owned(),
            headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        ));
        self.gets
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected GET")
    }
}

struct Fixture {
    provider: GraphCalendarProvider,
    http: Arc<FakeHttp>,
}

fn fixture() -> Fixture {
    let account = AccountConfig {
        id: AccountId::new("acc-1"),
        display_name: "Work".into(),
        connection: AccountConnection::Graph {
            client_id: "11111111-2222-3333-4444-555555555555".into(),
            tenant: "common".into(),
        },
        included_calendars: vec![],
    };
    let http = Arc::new(FakeHttp::default());
    let secrets = Arc::new(InMemorySecretStore::new());
    secrets
        .set(
            &account.id,
            SecretKind::GraphRefreshToken,
            &SecretString::new("refresh-1"),
        )
        .unwrap();
    let clock = Arc::new(FakeClock::new(
        Utc.with_ymd_and_hms(2026, 11, 16, 7, 0, 0).unwrap(),
    ));
    let auth = Arc::new(GraphAuth::new(&account, secrets, http.clone(), clock).unwrap());
    let provider =
        GraphCalendarProvider::new(auth, http.clone()).with_max_retry_wait(Duration::ZERO);
    Fixture { provider, http }
}

fn window() -> (DateTime<Utc>, DateTime<Utc>) {
    (
        Utc.with_ymd_and_hms(2026, 11, 16, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 11, 23, 0, 0, 0).unwrap(),
    )
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn utc(d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, d, h, min, 0).unwrap()
}

fn cal1() -> CalendarId {
    CalendarId::new("AAMkCal1==")
}

fn fetch_fixture_events(f: &Fixture) -> HashMap<String, Event> {
    f.http.push(200, VIEW_1);
    f.http.push(200, VIEW_2);
    let (from, to) = window();
    f.provider
        .fetch_events(&[cal1()], from, to)
        .unwrap()
        .into_iter()
        .map(|e| (e.id.as_str().to_owned(), e))
        .collect()
}

// ---------------------------------------------------------------------------
// Calendars
// ---------------------------------------------------------------------------

#[test]
fn lists_calendars_across_pages() {
    let f = fixture();
    f.http.push(200, CALENDARS_1);
    f.http.push(200, CALENDARS_2);
    let calendars = f.provider.list_calendars().unwrap();
    let account = AccountId::new("acc-1");
    assert_eq!(
        calendars,
        vec![
            CalendarInfo {
                id: CalendarId::new("AAMkCal1=="),
                account_id: account.clone(),
                name: "Calendar".into(),
                color: Some("#e07a5f".into()),
            },
            CalendarInfo {
                id: CalendarId::new("AAMkCal2/x+y"),
                account_id: account.clone(),
                name: "Team".into(),
                color: None,
            },
            CalendarInfo {
                id: CalendarId::new("AAMkCal3"),
                account_id: account,
                name: "Birthdays".into(),
                color: None,
            },
        ]
    );
    let requests = f.http.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]
        .0
        .starts_with("https://graph.microsoft.com/v1.0/me/calendars?"));
    assert!(requests[0].0.contains("%24select=id%2Cname%2ChexColor"));
    assert!(requests[1].0.contains("%24skip=2"), "follows nextLink");
}

#[test]
fn requests_carry_token_and_utc_preference() {
    let f = fixture();
    f.http.push(200, CALENDARS_2);
    f.provider.list_calendars().unwrap();
    let (_, headers) = &f.http.requests()[0];
    assert_eq!(headers["Authorization"], "Bearer access-1");
    assert_eq!(headers["Prefer"], "outlook.timezone=\"UTC\"");
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[test]
fn calendar_view_request_shape() {
    let f = fixture();
    f.http.push(200, r#"{"value": []}"#);
    let (from, to) = window();
    let events = f
        .provider
        .fetch_events(&[CalendarId::new("AAMkCal2/x+y")], from, to)
        .unwrap();
    assert!(events.is_empty());
    let url = &f.http.requests()[0].0;
    assert!(
        url.starts_with(
            "https://graph.microsoft.com/v1.0/me/calendars/AAMkCal2%2Fx%2By/calendarView?"
        ),
        "{url}"
    );
    assert!(
        url.contains("startDateTime=2026-11-16T00%3A00%3A00Z"),
        "{url}"
    );
    assert!(
        url.contains("endDateTime=2026-11-23T00%3A00%3A00Z"),
        "{url}"
    );
    assert!(url.contains("%24top=100"), "{url}");
    assert!(url.contains("isCancelled"), "{url}");
}

#[test]
fn empty_calendar_list_sends_nothing() {
    let f = fixture();
    let (from, to) = window();
    assert!(f.provider.fetch_events(&[], from, to).unwrap().is_empty());
    assert!(f.http.requests().is_empty());
}

#[test]
fn maps_calendar_view_across_pages() {
    let f = fixture();
    let events = fetch_fixture_events(&f);
    assert_eq!(f.http.requests().len(), 2, "follows nextLink");
    assert!(f.http.requests()[1].0.contains("%24skip=5"));

    let mut ids: Vec<_> = events.keys().cloned().collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "evt-allday-multi",
            "evt-allday-single",
            "evt-allday-west",
            "evt-declined",
            "evt-no-subject",
            "evt-series-instance-1",
            "evt-series-instance-2",
            "evt-timed",
        ],
        "the cancelled instance is omitted"
    );
    for e in events.values() {
        assert_eq!(e.account_id, AccountId::new("acc-1"));
        assert_eq!(e.calendar_id, cal1());
    }

    let timed = &events["evt-timed"];
    assert_eq!(timed.title, "Design review");
    assert_eq!(
        timed.time,
        EventTime::Timed {
            start: utc(16, 9, 0),
            end: utc(16, 9, 30),
        }
    );
    assert_eq!(timed.location.as_deref(), Some("Room 4"));
    assert_eq!(timed.response, ResponseStatus::Accepted);
}

#[test]
fn recurring_instances_are_separate_events() {
    let f = fixture();
    let events = fetch_fixture_events(&f);
    let first = &events["evt-series-instance-1"];
    let second = &events["evt-series-instance-2"];
    assert_eq!(first.title, "Standup");
    assert_eq!(
        first.time,
        EventTime::Timed {
            start: utc(16, 8, 0),
            end: utc(16, 8, 15),
        }
    );
    assert_eq!(
        second.time,
        EventTime::Timed {
            start: utc(17, 8, 0),
            end: utc(17, 8, 15),
        }
    );
    assert_ne!(first.key(), second.key());
}

#[test]
fn all_day_events_map_to_dates() {
    let f = fixture();
    let events = fetch_fixture_events(&f);
    // Midnight to midnight in UTC.
    assert_eq!(
        events["evt-allday-single"].time,
        EventTime::AllDay {
            start: date(2026, 11, 17),
            end: date(2026, 11, 18),
        }
    );
    // 18-19 Nov in UTC+1, reported shifted to 23:00 the day before.
    assert_eq!(
        events["evt-allday-multi"].time,
        EventTime::AllDay {
            start: date(2026, 11, 18),
            end: date(2026, 11, 20),
        }
    );
    // 21 Nov in UTC-5, reported shifted to 05:00.
    assert_eq!(
        events["evt-allday-west"].time,
        EventTime::AllDay {
            start: date(2026, 11, 21),
            end: date(2026, 11, 22),
        }
    );
}

#[test]
fn all_day_rounding_and_degenerate_end() {
    let dt = |s: &str| GraphDateTime {
        date_time: s.into(),
        time_zone: Some("UTC".into()),
    };
    assert_eq!(
        nearest_date(&dt("2026-11-16T00:00:00")).unwrap(),
        date(2026, 11, 16)
    );
    // UTC+11 (e.g. Sydney in summer) and UTC-10 (Hawaii).
    assert_eq!(
        nearest_date(&dt("2026-11-15T13:00:00.0000000")).unwrap(),
        date(2026, 11, 16)
    );
    assert_eq!(
        nearest_date(&dt("2026-11-16T10:00:00.0000000")).unwrap(),
        date(2026, 11, 16)
    );

    let item = GraphEvent {
        id: "x".into(),
        subject: None,
        start: dt("2026-11-16T00:00:00"),
        end: dt("2026-11-16T00:00:00"),
        is_all_day: true,
        is_cancelled: false,
        location: None,
        response_status: None,
    };
    let event = map_event(item, &AccountId::new("a"), &cal1())
        .unwrap()
        .unwrap();
    assert_eq!(
        event.time,
        EventTime::AllDay {
            start: date(2026, 11, 16),
            end: date(2026, 11, 17),
        }
    );
    assert_eq!(event.response, ResponseStatus::Unknown);
}

#[test]
fn declined_and_response_statuses() {
    let f = fixture();
    let events = fetch_fixture_events(&f);
    let declined = &events["evt-declined"];
    assert_eq!(declined.response, ResponseStatus::Declined);
    assert!(declined.is_declined());
    assert_eq!(declined.location, None);
    assert_eq!(
        events["evt-allday-single"].response,
        ResponseStatus::Organizer
    );
    assert_eq!(
        events["evt-allday-multi"].response,
        ResponseStatus::Tentative
    );
    assert_eq!(
        events["evt-no-subject"].response,
        ResponseStatus::NotResponded
    );
    assert_eq!(events["evt-allday-west"].response, ResponseStatus::Unknown);
}

#[test]
fn missing_subject_and_blank_location() {
    let f = fixture();
    let events = fetch_fixture_events(&f);
    let e = &events["evt-no-subject"];
    assert_eq!(e.title, "");
    assert_eq!(e.location, None);
    assert_eq!(events["evt-allday-single"].location, None);
}

#[test]
fn fetches_each_calendar() {
    let f = fixture();
    f.http.push(200, VIEW_2);
    f.http.push(200, r#"{"value": []}"#);
    let (from, to) = window();
    let events = f
        .provider
        .fetch_events(&[cal1(), CalendarId::new("AAMkCal3")], from, to)
        .unwrap();
    assert_eq!(events.len(), 3);
    let requests = f.http.requests();
    assert!(requests[0].0.contains("/calendars/AAMkCal1%3D%3D/"));
    assert!(requests[1].0.contains("/calendars/AAMkCal3/"));
}

#[test]
fn non_utc_times_are_a_protocol_error() {
    let f = fixture();
    f.http.push(
        200,
        r#"{"value": [{"id": "x", "isAllDay": false,
            "start": {"dateTime": "2026-11-16T09:00:00", "timeZone": "Pacific Standard Time"},
            "end": {"dateTime": "2026-11-16T10:00:00", "timeZone": "Pacific Standard Time"}}]}"#,
    );
    let (from, to) = window();
    let err = f.provider.fetch_events(&[cal1()], from, to).unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[test]
fn unauthorized_refreshes_once_and_retries() {
    let f = fixture();
    f.http
        .push(401, r#"{"error": {"code": "InvalidAuthenticationToken"}}"#);
    f.http.push(200, CALENDARS_2);
    assert_eq!(f.provider.list_calendars().unwrap().len(), 1);
    let requests = f.http.requests();
    assert_eq!(requests[0].1["Authorization"], "Bearer access-1");
    assert_eq!(requests[1].1["Authorization"], "Bearer access-2");
    assert_eq!(f.http.token_posts(), 2);
}

#[test]
fn unauthorized_twice_is_an_auth_error() {
    let f = fixture();
    f.http
        .push(401, r#"{"error": {"code": "InvalidAuthenticationToken"}}"#);
    f.http
        .push(401, r#"{"error": {"code": "InvalidAuthenticationToken"}}"#);
    let err = f.provider.list_calendars().unwrap_err();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
    assert!(err.to_string().contains("401"));
    assert!(err.to_string().contains("InvalidAuthenticationToken"));
    assert_eq!(f.http.requests().len(), 2);
}

#[test]
fn forbidden_is_an_auth_error_with_code_only() {
    let f = fixture();
    f.http.push(
        403,
        r#"{"error": {"code": "ErrorAccessDenied", "message": "Access is denied for jane@example.com"}}"#,
    );
    let err = f.provider.test_connection().unwrap_err();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains("403") && msg.contains("ErrorAccessDenied"),
        "{msg}"
    );
    assert!(!msg.contains("jane"), "{msg}");
    assert_eq!(f.http.requests().len(), 1, "no retry on 403");
}

#[test]
fn server_errors_and_transport_failures_are_network_errors() {
    let f = fixture();
    f.http.push(500, "<html>oops</html>");
    let err = f.provider.list_calendars().unwrap_err();
    assert!(err.is_retryable(), "{err:?}");
    assert!(!err.to_string().contains("oops"));

    f.http.push(502, "");
    assert!(f.provider.list_calendars().unwrap_err().is_retryable());

    f.http.push_err(Error::network("request failed: timeout"));
    assert!(f.provider.list_calendars().unwrap_err().is_retryable());
}

fn throttled(status: u16, retry_after: Option<&str>) -> HttpResponse {
    let mut response = HttpResponse::new(
        status,
        r#"{"error": {"code": "TooManyRequests"}}"#.as_bytes(),
    );
    if let Some(value) = retry_after {
        response.headers.push(("retry-after".into(), value.into()));
    }
    response
}

#[test]
fn throttling_within_the_cap_is_retried_once() {
    let f = fixture();
    f.http.push_response(throttled(429, Some("0")));
    f.http.push(200, CALENDARS_2);
    assert_eq!(f.provider.list_calendars().unwrap().len(), 1);

    f.http.push_response(throttled(503, Some("0")));
    f.http.push_response(throttled(429, Some("0")));
    let err = f.provider.list_calendars().unwrap_err();
    assert!(err.is_retryable(), "{err:?}");
    assert!(err.to_string().contains("429"));
}

#[test]
fn throttling_beyond_the_cap_is_a_network_error() {
    let f = fixture();
    // The fixture caps the wait at zero seconds.
    f.http.push_response(throttled(429, Some("30")));
    let err = f.provider.list_calendars().unwrap_err();
    assert!(err.is_retryable(), "{err:?}");
    assert_eq!(f.http.requests().len(), 1);

    // No Retry-After, or an HTTP-date one: not waited for.
    f.http.push_response(throttled(429, None));
    assert!(f.provider.list_calendars().unwrap_err().is_retryable());
    f.http
        .push_response(throttled(503, Some("Wed, 18 Nov 2026 08:00:00 GMT")));
    assert!(f.provider.list_calendars().unwrap_err().is_retryable());
    assert_eq!(f.http.requests().len(), 3);
}

#[test]
fn not_found_and_other_statuses() {
    let f = fixture();
    f.http
        .push(404, r#"{"error": {"code": "ErrorItemNotFound"}}"#);
    let (from, to) = window();
    let err = f.provider.fetch_events(&[cal1()], from, to).unwrap_err();
    assert!(matches!(err, Error::NotFound { .. }), "{err:?}");

    f.http
        .push(400, r#"{"error": {"code": "ErrorInvalidParameter"}}"#);
    let err = f.provider.list_calendars().unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
}

#[test]
fn malformed_json_is_a_protocol_error() {
    let f = fixture();
    f.http.push(200, "{not json");
    let err = f.provider.list_calendars().unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");

    // serde_json would quote the bad value; the error must not.
    f.http.push(
        200,
        r#"{"value": [{"id": "x", "subject": "s", "isAllDay": "Secret meeting"}]}"#,
    );
    let (from, to) = window();
    let err = f.provider.fetch_events(&[cal1()], from, to).unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
    assert!(!err.to_string().contains("Secret"));

    f.http.push(200, "[]");
    let err = f.provider.test_connection().unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
}

#[test]
fn a_failing_page_fails_the_whole_fetch() {
    let f = fixture();
    f.http.push(200, VIEW_1);
    f.http.push(500, "");
    let (from, to) = window();
    assert!(f
        .provider
        .fetch_events(&[cal1()], from, to)
        .unwrap_err()
        .is_retryable());
}

#[test]
fn next_link_to_another_host_is_refused() {
    let f = fixture();
    f.http.push(
        200,
        r#"{"value": [], "@odata.nextLink": "https://evil.example.com/v1.0/me/calendars?x=1"}"#,
    );
    let err = f.provider.list_calendars().unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
    assert_eq!(f.http.requests().len(), 1);
}

#[test]
fn test_connection_is_one_small_request() {
    let f = fixture();
    f.http.push(200, CALENDARS_2);
    f.provider.test_connection().unwrap();
    let requests = f.http.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.contains("%24top=1"));
}

#[test]
fn not_signed_in_is_an_auth_error_without_requests() {
    let account = AccountConfig {
        id: AccountId::new("acc-2"),
        display_name: "Work".into(),
        connection: AccountConnection::Graph {
            client_id: "11111111-2222-3333-4444-555555555555".into(),
            tenant: "common".into(),
        },
        included_calendars: vec![],
    };
    let http = Arc::new(FakeHttp::default());
    let auth = GraphAuth::new(
        &account,
        Arc::new(InMemorySecretStore::new()),
        http.clone(),
        Arc::new(FakeClock::new(Utc::now())),
    )
    .unwrap();
    let provider = GraphCalendarProvider::new(Arc::new(auth), http.clone());
    assert!(matches!(
        provider.test_connection().unwrap_err(),
        Error::Auth { .. }
    ));
    assert!(http.requests().is_empty());
}

#[test]
fn helpers() {
    assert_eq!(normalize_hex_color("#A1B2C3").as_deref(), Some("#a1b2c3"));
    assert_eq!(normalize_hex_color(""), None);
    assert_eq!(normalize_hex_color("#abc"), None);
    assert_eq!(
        encode_path_segment("a/b+c=d_e-f.g~h"),
        "a%2Fb%2Bc%3Dd_e-f.g~h"
    );
    assert_eq!(graph_instant(utc(16, 9, 5)), "2026-11-16T09:05:00Z");
}

// ---------------------------------------------------------------------------
// Live test
// ---------------------------------------------------------------------------

/// Interactive end-to-end test against the real Microsoft Graph. Opens the
/// browser to sign in, then lists calendars and fetches the next 7 days of
/// every calendar. Prints only counts, never event data. Needs an app
/// registration set up as in docs/graph-setup.md:
///
/// ```text
/// GRAPH_CLIENT_ID=<guid> GRAPH_TENANT=common \
///   cargo test --lib graph::provider::tests::live_fetch -- --ignored --nocapture
/// ```
///
/// Uses an in-memory secret store, so nothing is written to the keychain.
#[test]
#[ignore = "interactive: opens a browser and talks to Microsoft Graph"]
fn live_fetch() {
    use crate::graph::auth::{CancelToken, SystemBrowser};
    use crate::graph::http::UreqHttpClient;

    let client_id = std::env::var("GRAPH_CLIENT_ID").expect("set GRAPH_CLIENT_ID");
    let tenant = std::env::var("GRAPH_TENANT").unwrap_or_else(|_| "common".into());
    let account = AccountConfig {
        id: AccountId::new("live-test"),
        display_name: "Live".into(),
        connection: AccountConnection::Graph { client_id, tenant },
        included_calendars: vec![],
    };
    let http: Arc<dyn HttpClient> = Arc::new(UreqHttpClient::new());
    let auth = Arc::new(
        GraphAuth::new(
            &account,
            Arc::new(InMemorySecretStore::new()),
            http.clone(),
            Arc::new(crate::domain::SystemClock),
        )
        .unwrap(),
    );
    auth.sign_in(&SystemBrowser, &CancelToken::new()).unwrap();
    let provider = GraphCalendarProvider::new(auth, http);
    provider.test_connection().unwrap();
    let calendars = provider.list_calendars().unwrap();
    println!("{} calendar(s)", calendars.len());
    let ids: Vec<_> = calendars.into_iter().map(|c| c.id).collect();
    let from = Utc::now();
    let events = provider
        .fetch_events(&ids, from, from + chrono::Duration::days(7))
        .unwrap();
    let all_day = events.iter().filter(|e| e.is_all_day()).count();
    println!("{} event(s), {all_day} all-day", events.len());
}
