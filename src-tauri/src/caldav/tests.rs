//! CalDAV tests against recorded-style fixtures with fake data
//! (`tests/fixtures/caldav/`) and a mock [`Transport`]. No network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use super::xml::parse_multistatus;
use super::*;
use crate::domain::{EventTime, FakeClock, ResponseStatus};
use crate::secrets::InMemorySecretStore;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/caldav/", $name))
    };
}

// ---------------------------------------------------------------------------
// Mock transport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Sent {
    method: String,
    url: String,
    depth: Option<String>,
    body: String,
    authorization: Option<String>,
}

#[derive(Default)]
struct Mock {
    routes: Mutex<HashMap<(String, String), Result<HttpResponse>>>,
    sent: Mutex<Vec<Sent>>,
}

impl Mock {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn on(&self, method: &str, url: &str, response: Result<HttpResponse>) -> &Self {
        self.routes
            .lock()
            .unwrap()
            .insert((method.to_owned(), url.to_owned()), response);
        self
    }

    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn urls(&self) -> Vec<String> {
        self.sent().into_iter().map(|s| s.url).collect()
    }
}

impl Transport for Mock {
    fn send(&self, request: &HttpRequest<'_>) -> Result<HttpResponse> {
        self.sent.lock().unwrap().push(Sent {
            method: request.method.to_owned(),
            url: request.url.to_string(),
            depth: request.depth.map(str::to_owned),
            body: request.body.to_owned(),
            authorization: request.authorization.map(str::to_owned),
        });
        self.routes
            .lock()
            .unwrap()
            .get(&(request.method.to_owned(), request.url.to_string()))
            .cloned()
            .unwrap_or_else(|| Ok(status(404)))
    }
}

fn status(code: u16) -> HttpResponse {
    HttpResponse {
        status: code,
        location: None,
        body: String::new(),
    }
}

fn multistatus(body: &str) -> Result<HttpResponse> {
    Ok(HttpResponse {
        status: 207,
        location: None,
        body: body.to_owned(),
    })
}

fn redirect(code: u16, location: &str) -> Result<HttpResponse> {
    Ok(HttpResponse {
        status: code,
        location: Some(location.to_owned()),
        body: String::new(),
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const PASSWORD: &str = "fake-app-password";

fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn clock() -> Arc<FakeClock> {
    Arc::new(FakeClock::new(utc(2026, 11, 16, 8, 0)))
}

fn provider(server: &str, username: &str, mock: &Arc<Mock>) -> CalDavProvider {
    CalDavProvider::new(
        AccountId::new("acc-1"),
        server,
        username,
        SecretString::new(PASSWORD),
        clock(),
        mock.clone(),
    )
    .unwrap()
}

/// Window used by the fetch tests: the week of 16 Nov 2026.
fn window() -> (DateTime<Utc>, DateTime<Utc>) {
    (utc(2026, 11, 16, 0, 0), utc(2026, 11, 23, 0, 0))
}

const ICLOUD: &str = "https://caldav.icloud.com/";
const ICLOUD_P: &str = "https://p42-caldav.icloud.com";

/// iCloud: the well-known URL redirects to the root, the principal request
/// is redirected to the partition host.
fn icloud_mock() -> Arc<Mock> {
    let mock = Mock::new();
    mock.on(
        "PROPFIND",
        "https://caldav.icloud.com/.well-known/caldav",
        redirect(301, "https://caldav.icloud.com/"),
    )
    .on(
        "PROPFIND",
        ICLOUD,
        multistatus(fixture!("icloud/principal.xml")),
    )
    .on(
        "PROPFIND",
        "https://caldav.icloud.com/1234567890/principal/",
        redirect(302, "https://p42-caldav.icloud.com/1234567890/principal/"),
    )
    .on(
        "PROPFIND",
        &format!("{ICLOUD_P}/1234567890/principal/"),
        multistatus(fixture!("icloud/home.xml")),
    )
    .on(
        "PROPFIND",
        &format!("{ICLOUD_P}/1234567890/calendars/"),
        multistatus(fixture!("icloud/calendars.xml")),
    )
    .on(
        "REPORT",
        &format!("{ICLOUD_P}/1234567890/calendars/home/"),
        multistatus(fixture!("icloud/report.xml")),
    );
    mock
}

const NEXTCLOUD: &str = "https://cloud.example.com";
const NC_DAV: &str = "https://cloud.example.com/remote.php/dav/";

/// Nextcloud: the well-known URL redirects (relative Location) to the DAV
/// root; everything else lives on the same host.
fn nextcloud_mock() -> Arc<Mock> {
    let mock = Mock::new();
    mock.on(
        "PROPFIND",
        "https://cloud.example.com/.well-known/caldav",
        redirect(301, "/remote.php/dav/"),
    )
    .on(
        "PROPFIND",
        NC_DAV,
        multistatus(fixture!("nextcloud/principal.xml")),
    )
    .on(
        "PROPFIND",
        "https://cloud.example.com/remote.php/dav/principals/users/testuser/",
        multistatus(fixture!("nextcloud/home.xml")),
    )
    .on(
        "PROPFIND",
        "https://cloud.example.com/remote.php/dav/calendars/testuser/",
        multistatus(fixture!("nextcloud/calendars.xml")),
    )
    .on(
        "REPORT",
        "https://cloud.example.com/remote.php/dav/calendars/testuser/personal/",
        multistatus(fixture!("nextcloud/report.xml")),
    );
    mock
}

fn ids(calendars: &[CalendarInfo]) -> Vec<&str> {
    calendars.iter().map(|c| c.id.as_str()).collect()
}

// ---------------------------------------------------------------------------
// XML parsing
// ---------------------------------------------------------------------------

#[test]
fn parses_icloud_principal_with_default_namespaces() {
    let r = parse_multistatus(fixture!("icloud/principal.xml")).unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].href, "/");
    assert_eq!(
        r[0].props.current_user_principal.as_deref(),
        Some("/1234567890/principal/")
    );
}

#[test]
fn parses_icloud_home_set_and_addresses() {
    let r = parse_multistatus(fixture!("icloud/home.xml")).unwrap();
    let p = &r[0].props;
    assert_eq!(
        p.calendar_home_set.as_deref(),
        Some("https://p42-caldav.icloud.com:443/1234567890/calendars/")
    );
    assert_eq!(p.calendar_user_addresses.len(), 3);
    assert_eq!(
        p.calendar_user_addresses[0],
        "mailto:test.user@icloud.example"
    );
}

#[test]
fn parses_icloud_calendar_listing() {
    let r = parse_multistatus(fixture!("icloud/calendars.xml")).unwrap();
    assert_eq!(r.len(), 6);

    // The home itself: the 404 propstat's empty properties are ignored.
    assert_eq!(r[0].href, "/1234567890/calendars/");
    assert!(!r[0].props.is_calendar);
    assert_eq!(r[0].props.display_name, None);
    assert_eq!(r[0].props.components, None);

    let home = &r[1].props;
    assert!(home.is_calendar);
    assert_eq!(home.display_name.as_deref(), Some("Home"));
    assert_eq!(home.components.as_deref(), Some(&["VEVENT".to_owned()][..]));
    assert_eq!(home.color.as_deref(), Some("#FF2968FF"));

    assert_eq!(r[2].props.display_name.as_deref(), Some("Work & Projects"));
    assert_eq!(
        r[2].props.components.as_deref(),
        Some(&["VEVENT".to_owned(), "VTODO".to_owned()][..])
    );
    assert!(!r[4].props.is_calendar, "schedule inbox is not a calendar");
}

#[test]
fn parses_nextcloud_prefixed_listing() {
    let r = parse_multistatus(fixture!("nextcloud/calendars.xml")).unwrap();
    assert_eq!(r.len(), 6);
    assert_eq!(r[1].href, "/remote.php/dav/calendars/testuser/personal/");
    assert!(r[1].props.is_calendar);
    assert_eq!(r[1].props.color.as_deref(), Some("#0082c9"));
    assert_eq!(
        r[2].href,
        "/remote.php/dav/calendars/testuser/family%20stuff/"
    );
    assert_eq!(r[2].props.display_name, None);
    assert!(!r[5].props.is_calendar);
}

#[test]
fn parses_calendar_data_with_char_refs_and_cdata() {
    let r = parse_multistatus(fixture!("nextcloud/report.xml")).unwrap();
    assert_eq!(r.len(), 4);
    let first = r[0].props.calendar_data.as_deref().unwrap();
    assert!(first.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
    let cdata = r[3].props.calendar_data.as_deref().unwrap();
    assert!(cdata.contains("SUMMARY:Design review <draft>\n"));

    let icloud = parse_multistatus(fixture!("icloud/report.xml")).unwrap();
    assert!(icloud[0]
        .props
        .calendar_data
        .as_deref()
        .unwrap()
        .contains("SUMMARY:Dentist <checkup>"));
}

#[test]
fn parses_upper_case_prefixes() {
    let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:A="http://apple.com/ns/ical/">
  <D:response>
    <D:href>/cal/work/</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype><D:collection/><C:calendar/></D:resourcetype>
        <D:displayname>Work</D:displayname>
        <A:calendar-color>#112233</A:calendar-color>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
    let r = parse_multistatus(xml).unwrap();
    assert!(r[0].props.is_calendar);
    assert_eq!(r[0].props.display_name.as_deref(), Some("Work"));
    assert_eq!(r[0].props.color.as_deref(), Some("#112233"));
}

#[test]
fn ignores_elements_in_other_namespaces() {
    // `calendar` in the DAV namespace is not a CalDAV calendar.
    let xml = r#"<multistatus xmlns="DAV:"><response><href>/x/</href><propstat><prop>
        <resourcetype><collection/><calendar/></resourcetype></prop></propstat></response></multistatus>"#;
    let r = parse_multistatus(xml).unwrap();
    assert!(!r[0].props.is_calendar);
}

#[test]
fn malformed_xml_is_a_protocol_error() {
    for bad in [
        "",
        "not xml at all",
        "<html><body>Login</body></html>",
        r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/x</d:response></d:multistatus>"#,
        r#"<d:multistatus xmlns:d="DAV:"><d:response>"#,
        r#"<d:multistatus xmlns:d="DAV:">&bogus;</d:multistatus>"#,
    ] {
        let err = parse_multistatus(bad).unwrap_err();
        assert!(matches!(err, Error::Protocol { .. }), "{bad:?}: {err:?}");
    }
}

#[test]
fn request_bodies_are_well_formed() {
    let query = xml::calendar_query("20261116T000000Z", "20261123T000000Z");
    for body in [
        xml::PROPFIND_PRINCIPAL,
        xml::PROPFIND_HOME,
        xml::PROPFIND_CALENDARS,
        query.as_str(),
    ] {
        let mut reader = quick_xml::Reader::from_str(body);
        loop {
            match reader.read_event().expect("well-formed") {
                quick_xml::events::Event::Eof => break,
                _ => continue,
            }
        }
    }
    assert!(query.contains(r#"<c:time-range start="20261116T000000Z" end="20261123T000000Z"/>"#));
}

// ---------------------------------------------------------------------------
// Discovery and listing
// ---------------------------------------------------------------------------

#[test]
fn icloud_discovery_follows_redirects_to_the_partition_host() {
    let mock = icloud_mock();
    let p = provider(ICLOUD, "test.user@icloud.example", &mock);
    let calendars = p.list_calendars().unwrap();

    assert_eq!(
        ids(&calendars),
        [
            "/1234567890/calendars/home/",
            "/1234567890/calendars/A1B2C3D4-0000-4000-8000-000000000001/",
        ]
    );
    assert_eq!(calendars[0].name, "Home");
    assert_eq!(calendars[0].color.as_deref(), Some("#ff2968"));
    assert_eq!(calendars[1].name, "Work & Projects");
    assert_eq!(calendars[1].color.as_deref(), Some("#1badf8"));
    assert!(calendars.iter().all(|c| c.account_id.as_str() == "acc-1"));

    let sent = mock.sent();
    assert_eq!(
        mock.urls(),
        [
            "https://caldav.icloud.com/.well-known/caldav",
            "https://caldav.icloud.com/",
            "https://caldav.icloud.com/1234567890/principal/",
            "https://p42-caldav.icloud.com/1234567890/principal/",
            "https://p42-caldav.icloud.com/1234567890/calendars/",
        ]
    );
    let depths: Vec<_> = sent.iter().map(|s| s.depth.as_deref()).collect();
    assert_eq!(
        depths,
        [Some("0"), Some("0"), Some("0"), Some("0"), Some("1")]
    );
    let expected_auth = http::basic_auth("test.user@icloud.example", &SecretString::new(PASSWORD));
    for s in &sent {
        assert_eq!(s.method, "PROPFIND");
        assert!(s.url.starts_with("https://"));
        // Auth is re-applied after the cross-host (https) redirect.
        assert_eq!(s.authorization.as_deref(), Some(expected_auth.as_str()));
    }
    assert!(sent[4].body.contains("supported-calendar-component-set"));
}

#[test]
fn nextcloud_discovery_from_a_bare_host() {
    let mock = nextcloud_mock();
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let calendars = p.list_calendars().unwrap();

    assert_eq!(
        ids(&calendars),
        [
            "/remote.php/dav/calendars/testuser/personal/",
            "/remote.php/dav/calendars/testuser/family%20stuff/",
        ]
    );
    assert_eq!(calendars[0].name, "Personal");
    assert_eq!(calendars[0].color.as_deref(), Some("#0082c9"));
    // No displayname: the decoded last path segment.
    assert_eq!(calendars[1].name, "family stuff");
    assert_eq!(calendars[1].color, None);
    assert_eq!(
        mock.urls()[0],
        "https://cloud.example.com/.well-known/caldav"
    );
}

#[test]
fn a_configured_dav_path_skips_well_known() {
    let mock = nextcloud_mock();
    let p = provider(NC_DAV, "testuser", &mock);
    p.test_connection().unwrap();
    assert_eq!(
        mock.urls(),
        [
            NC_DAV,
            "https://cloud.example.com/remote.php/dav/principals/users/testuser/",
        ]
    );
}

#[test]
fn missing_well_known_falls_back_to_the_root() {
    let mock = Mock::new();
    mock.on(
        "PROPFIND",
        "https://dav.example.com/",
        multistatus(fixture!("nextcloud/principal.xml")),
    )
    .on(
        "PROPFIND",
        "https://dav.example.com/remote.php/dav/principals/users/testuser/",
        multistatus(fixture!("nextcloud/home.xml")),
    );
    let p = provider("https://dav.example.com/", "testuser", &mock);
    p.test_connection().unwrap();
    assert_eq!(mock.urls()[0], "https://dav.example.com/.well-known/caldav");
    assert_eq!(mock.urls()[1], "https://dav.example.com/");
}

#[test]
fn a_calendar_url_without_principal_or_home_lists_itself() {
    let listing = r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:response><d:href>/dav/cal/main/</d:href><d:propstat><d:prop>
    <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>
    <d:displayname>Main</d:displayname>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
  <d:response><d:href>/dav/cal/main/event.ics</d:href><d:propstat><d:prop>
    <d:resourcetype/>
  </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
</d:multistatus>"#;
    let url = "https://dav.example.com/dav/cal/main/";
    let mock = Mock::new();
    // All three PROPFINDs hit the same URL. The listing has neither a
    // principal nor a home set, so the URL serves as both.
    mock.on("PROPFIND", url, multistatus(listing));
    let p = provider(url, "user", &mock);
    let calendars = p.list_calendars().unwrap();
    assert_eq!(ids(&calendars), ["/dav/cal/main/"]);
    assert_eq!(calendars[0].name, "Main");
    assert_eq!(mock.sent().len(), 3);
}

#[test]
fn redirect_to_http_is_refused_without_sending_credentials() {
    let mock = Mock::new();
    mock.on(
        "PROPFIND",
        "https://cloud.example.com/.well-known/caldav",
        redirect(301, "http://cloud.example.com/remote.php/dav/"),
    );
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let err = p.test_connection().unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
    assert!(mock.urls().iter().all(|u| u.starts_with("https://")));
}

#[test]
fn redirect_loops_are_cut_off() {
    let mock = Mock::new();
    mock.on(
        "PROPFIND",
        "https://loop.example.com/a/",
        redirect(307, "/b/"),
    )
    .on(
        "PROPFIND",
        "https://loop.example.com/b/",
        redirect(308, "/a/"),
    );
    let p = provider("https://loop.example.com/a/", "u", &mock);
    let err = p.test_connection().unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
    assert_eq!(mock.sent().len(), client::MAX_REDIRECTS + 1);
}

#[test]
fn http_401_and_403_are_auth_errors() {
    for code in [401, 403] {
        let mock = Mock::new();
        mock.on("PROPFIND", NC_DAV, Ok(status(code)));
        let err = provider(NC_DAV, "testuser", &mock)
            .test_connection()
            .unwrap_err();
        assert!(matches!(err, Error::Auth { .. }), "{code}: {err:?}");
        assert!(!err.is_retryable());
    }
}

#[test]
fn auth_failure_on_well_known_is_not_masked_by_the_fallback() {
    let mock = Mock::new();
    mock.on(
        "PROPFIND",
        "https://cloud.example.com/.well-known/caldav",
        Ok(status(401)),
    );
    let err = provider(NEXTCLOUD, "testuser", &mock)
        .test_connection()
        .unwrap_err();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
    assert_eq!(mock.sent().len(), 1);
}

#[test]
fn server_errors_and_transport_failures_are_network_errors() {
    for response in [
        Ok(status(500)),
        Ok(status(503)),
        Ok(status(429)),
        Err(Error::network("the request timed out")),
    ] {
        let mock = Mock::new();
        mock.on("PROPFIND", NC_DAV, response.clone());
        let err = provider(NC_DAV, "testuser", &mock)
            .list_calendars()
            .unwrap_err();
        assert!(err.is_retryable(), "{response:?}: {err:?}");
    }
}

#[test]
fn malformed_multistatus_is_a_protocol_error() {
    let mock = nextcloud_mock();
    mock.on(
        "PROPFIND",
        "https://cloud.example.com/remote.php/dav/calendars/testuser/",
        multistatus("<d:multistatus xmlns:d=\"DAV:\"><d:response>"),
    );
    let err = provider(NEXTCLOUD, "testuser", &mock)
        .list_calendars()
        .unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");
}

#[test]
fn a_url_without_webdav_is_not_found() {
    for code in [200, 404, 405] {
        let mock = Mock::new();
        mock.on("PROPFIND", NC_DAV, Ok(status(code)));
        let err = provider(NC_DAV, "testuser", &mock)
            .test_connection()
            .unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }), "{code}: {err:?}");
    }
}

#[test]
fn error_messages_contain_no_urls_or_credentials() {
    let mock = Mock::new();
    mock.on("PROPFIND", NC_DAV, Ok(status(401)));
    let err = provider(NC_DAV, "testuser", &mock)
        .test_connection()
        .unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains("cloud.example.com"));
    assert!(!msg.contains(PASSWORD));
    assert!(!msg.contains("testuser"));
    let debug = format!("{:?}", provider(NC_DAV, "testuser", &mock));
    assert!(!debug.contains(PASSWORD));
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

#[test]
fn nextcloud_fetch_skips_a_bad_resource_and_keeps_the_rest() {
    let mock = nextcloud_mock();
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    let personal = CalendarId::new("/remote.php/dav/calendars/testuser/personal/");

    let discovery = p.discovery().unwrap();
    assert_eq!(discovery.user_addresses, ["test.user@example.com"]);
    let fetch = p.fetch_calendar(&discovery, &personal, from, to).unwrap();
    assert_eq!(fetch.skipped, 1, "broken.ics is skipped");

    let events = p
        .fetch_events(std::slice::from_ref(&personal), from, to)
        .unwrap();
    assert_eq!(events, fetch.events);
    assert_eq!(events.len(), 2, "cancelled omitted: {events:?}");

    let standup = events.iter().find(|e| e.title == "Stand-up").unwrap();
    assert_eq!(standup.id.as_str(), "declined-standup@example.com");
    assert_eq!(standup.response, ResponseStatus::Declined);
    assert_eq!(
        standup.time,
        EventTime::Timed {
            start: utc(2026, 11, 18, 8, 0),
            end: utc(2026, 11, 18, 8, 15),
        }
    );
    assert_eq!(standup.calendar_id, personal);
    assert_eq!(standup.account_id.as_str(), "acc-1");

    let review = events
        .iter()
        .find(|e| e.id.as_str() == "review@example.com")
        .unwrap();
    assert_eq!(review.title, "Design review <draft>");
    assert_eq!(review.location.as_deref(), Some("Room 4"));
    assert_eq!(review.response, ResponseStatus::Accepted);

    let report = mock
        .sent()
        .into_iter()
        .find(|s| s.method == "REPORT")
        .unwrap();
    assert_eq!(
        report.url,
        "https://cloud.example.com/remote.php/dav/calendars/testuser/personal/"
    );
    assert_eq!(report.depth.as_deref(), Some("1"));
    assert!(report.body.contains("calendar-query"));
    assert!(report.body.contains(r#"<c:comp-filter name="VEVENT">"#));
    assert!(report
        .body
        .contains(r#"start="20261116T000000Z" end="20261123T000000Z""#));
    assert!(!report.body.contains("expand"));
}

#[test]
fn icloud_fetch_expands_recurrences_and_all_day_events() {
    let mock = icloud_mock();
    let p = provider(ICLOUD, "test.user@icloud.example", &mock);
    let (from, to) = window();
    let home = CalendarId::new("/1234567890/calendars/home/");
    let mut events = p.fetch_events(&[home], from, to).unwrap();
    events.sort_by(|a, b| a.id.cmp(&b.id));

    let summary: Vec<(&str, &str)> = events
        .iter()
        .map(|e| (e.id.as_str(), e.title.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            ("0F1E2D3C-0000-4000-8000-000000000001", "Dentist <checkup>"),
            (
                "0F1E2D3C-0000-4000-8000-000000000002/2026-11-16T07:00:00Z",
                "Run"
            ),
            (
                "0F1E2D3C-0000-4000-8000-000000000002/2026-11-18T07:00:00Z",
                "Run"
            ),
            ("0F1E2D3C-0000-4000-8000-000000000003", "Day off"),
        ]
    );
    // Copenhagen is UTC+1 in November.
    assert_eq!(
        events[0].time,
        EventTime::Timed {
            start: utc(2026, 11, 17, 9, 0),
            end: utc(2026, 11, 17, 9, 30),
        }
    );
    assert_eq!(
        events[3].time,
        EventTime::AllDay {
            start: date(2026, 11, 20),
            end: date(2026, 11, 21),
        }
    );
    // The REPORT went to the partition host found by discovery.
    let report = mock
        .sent()
        .into_iter()
        .find(|s| s.method == "REPORT")
        .unwrap();
    assert_eq!(
        report.url,
        "https://p42-caldav.icloud.com/1234567890/calendars/home/"
    );
}

#[test]
fn one_failing_calendar_fails_the_whole_fetch() {
    let mock = nextcloud_mock();
    mock.on(
        "REPORT",
        "https://cloud.example.com/remote.php/dav/calendars/testuser/family%20stuff/",
        Ok(status(502)),
    );
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    let err = p
        .fetch_events(
            &[
                CalendarId::new("/remote.php/dav/calendars/testuser/personal/"),
                CalendarId::new("/remote.php/dav/calendars/testuser/family%20stuff/"),
            ],
            from,
            to,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Network { .. }), "{err:?}");
}

#[test]
fn an_unusable_report_is_a_protocol_error() {
    let mock = nextcloud_mock();
    let url = "https://cloud.example.com/remote.php/dav/calendars/testuser/personal/";
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    let personal = [CalendarId::new(
        "/remote.php/dav/calendars/testuser/personal/",
    )];

    mock.on("REPORT", url, multistatus("<html>Maintenance</html>"));
    let err = p.fetch_events(&personal, from, to).unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");

    mock.on("REPORT", url, Ok(status(200)));
    let err = p.fetch_events(&personal, from, to).unwrap_err();
    assert!(matches!(err, Error::Protocol { .. }), "{err:?}");

    mock.on("REPORT", url, Ok(status(401)));
    let err = p.fetch_events(&personal, from, to).unwrap_err();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
}

#[test]
fn a_deleted_calendar_yields_no_events() {
    let mock = nextcloud_mock();
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    let events = p
        .fetch_events(
            &[CalendarId::new("/remote.php/dav/calendars/testuser/gone/")],
            from,
            to,
        )
        .unwrap();
    assert!(events.is_empty());
}

#[test]
fn empty_calendar_list_makes_no_requests() {
    let mock = nextcloud_mock();
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    assert!(p.fetch_events(&[], from, to).unwrap().is_empty());
    assert!(mock.sent().is_empty());
}

#[test]
fn discovery_is_cached_between_fetches() {
    let mock = nextcloud_mock();
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    let personal = [CalendarId::new(
        "/remote.php/dav/calendars/testuser/personal/",
    )];
    p.fetch_events(&personal, from, to).unwrap();
    p.fetch_events(&personal, from, to).unwrap();
    let methods: Vec<_> = mock.sent().into_iter().map(|s| s.method).collect();
    assert_eq!(
        methods,
        ["PROPFIND", "PROPFIND", "PROPFIND", "REPORT", "REPORT"]
    );
}

#[test]
fn calendar_ids_cannot_point_at_another_host() {
    let mock = nextcloud_mock();
    let p = provider(NEXTCLOUD, "testuser", &mock);
    let (from, to) = window();
    for bad in [
        "//evil.example.com/steal/",
        "https://evil.example.com/steal/",
        "relative/path/",
        "/\\evil.example.com/",
        "",
    ] {
        let err = p
            .fetch_events(&[CalendarId::new(bad)], from, to)
            .unwrap_err();
        assert!(
            matches!(err, Error::InvalidInput { .. }),
            "{bad:?}: {err:?}"
        );
    }
    assert!(mock.urls().iter().all(|u| u.starts_with(NEXTCLOUD)));
    assert!(mock.sent().iter().all(|s| s.method == "PROPFIND"));
}

// ---------------------------------------------------------------------------
// Construction and factory
// ---------------------------------------------------------------------------

#[test]
fn basic_auth_header() {
    assert_eq!(
        http::basic_auth("Aladdin", &SecretString::new("open sesame")),
        "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
    );
}

#[test]
fn server_url_must_be_plain_https() {
    let mock = Mock::new();
    for bad in [
        "http://cloud.example.com/",
        "not a url",
        "https://user:pw@cloud.example.com/",
        "ftp://cloud.example.com/",
    ] {
        let err = CalDavProvider::new(
            AccountId::new("a"),
            bad,
            "u",
            SecretString::new("p"),
            clock(),
            mock.clone(),
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::InvalidInput { .. }),
            "{bad:?}: {err:?}"
        );
    }
}

#[test]
fn color_normalization() {
    assert_eq!(normalize_color("#FF2968FF").as_deref(), Some("#ff2968"));
    assert_eq!(normalize_color(" #0082c9 ").as_deref(), Some("#0082c9"));
    assert_eq!(normalize_color("red"), None);
    assert_eq!(normalize_color("#12345"), None);
    assert_eq!(normalize_color("#GGGGGG"), None);
}

fn caldav_account(server_url: &str) -> AccountConfig {
    AccountConfig {
        id: AccountId::new("acc-1"),
        display_name: "Home".into(),
        connection: AccountConnection::CalDav {
            server_url: server_url.into(),
            username: "testuser".into(),
        },
        included_calendars: vec![],
    }
}

fn factory(secrets: Arc<InMemorySecretStore>, mock: &Arc<Mock>) -> CalDavProviderFactory {
    CalDavProviderFactory::with_transport(secrets, clock(), mock.clone())
}

#[test]
fn factory_reads_the_password_from_the_secret_store() {
    let secrets = Arc::new(InMemorySecretStore::new());
    let account = caldav_account(NEXTCLOUD);
    secrets
        .set(
            &account.id,
            SecretKind::CalDavPassword,
            &SecretString::new(PASSWORD),
        )
        .unwrap();
    let mock = nextcloud_mock();
    let provider = factory(secrets, &mock).create(&account).unwrap();
    assert_eq!(provider.account_id(), &account.id);
    provider.test_connection().unwrap();
    let expected = http::basic_auth("testuser", &SecretString::new(PASSWORD));
    assert_eq!(
        mock.sent()[0].authorization.as_deref(),
        Some(expected.as_str())
    );
}

#[test]
fn factory_without_a_password_is_an_auth_error() {
    let mock = Mock::new();
    let err = factory(Arc::new(InMemorySecretStore::new()), &mock)
        .create(&caldav_account(NEXTCLOUD))
        .err()
        .unwrap();
    assert!(matches!(err, Error::Auth { .. }), "{err:?}");
}

#[test]
fn factory_rejects_other_account_types_and_bad_urls() {
    let secrets = Arc::new(InMemorySecretStore::new());
    let mut graph = caldav_account(NEXTCLOUD);
    graph.connection = AccountConnection::Graph {
        client_id: "x".into(),
        tenant: "common".into(),
    };
    let mock = Mock::new();
    let f = factory(secrets.clone(), &mock);
    let err = f.create(&graph).err().unwrap();
    assert!(matches!(err, Error::InvalidInput { .. }), "{err:?}");

    let insecure = caldav_account("http://cloud.example.com/");
    secrets
        .set(
            &insecure.id,
            SecretKind::CalDavPassword,
            &SecretString::new("pw"),
        )
        .unwrap();
    let err = f.create(&insecure).err().unwrap();
    assert!(matches!(err, Error::InvalidInput { .. }), "{err:?}");
}

// ---------------------------------------------------------------------------
// Real transport (no external network)
// ---------------------------------------------------------------------------

#[test]
fn ureq_transport_maps_connection_failures_to_network_errors() {
    let transport = UreqTransport::new();
    // Nothing listens on the discard port of the loopback interface.
    let url = Url::parse("https://127.0.0.1:9/").unwrap();
    let err = transport
        .send(&HttpRequest {
            method: "PROPFIND",
            url: &url,
            depth: Some("0"),
            body: xml::PROPFIND_PRINCIPAL,
            authorization: None,
        })
        .unwrap_err();
    assert!(matches!(err, Error::Network { .. }), "{err:?}");
    assert!(!err.to_string().contains("127.0.0.1"));
}

/// Live check against a real server. Run with
/// `CALDAV_URL=https://... CALDAV_USER=... CALDAV_PASSWORD=... cargo test caldav_live -- --ignored --nocapture`.
/// Prints only counts. Never commit credentials.
#[test]
#[ignore = "needs a real CalDAV server and credentials in env vars"]
fn caldav_live() {
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
    let p = CalDavProvider::new(
        AccountId::new("live"),
        &var("CALDAV_URL"),
        var("CALDAV_USER"),
        SecretString::new(var("CALDAV_PASSWORD")),
        Arc::new(crate::domain::SystemClock),
        Arc::new(UreqTransport::new()),
    )
    .unwrap();
    p.test_connection().unwrap();
    let calendars = p.list_calendars().unwrap();
    println!("{} calendar(s)", calendars.len());
    let now = Utc::now();
    let ids: Vec<CalendarId> = calendars.into_iter().map(|c| c.id).collect();
    let events = p
        .fetch_events(&ids, now, now + chrono::Duration::days(7))
        .unwrap();
    println!("{} event occurrence(s) in the next 7 days", events.len());
}
