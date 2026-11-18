use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc;

use super::*;
use crate::domain::{Error, Result};
use crate::graph::http::HttpResponse;

// --- Version comparison -------------------------------------------------

#[test]
fn parses_versions_with_and_without_v() {
    let v = Version::parse("v1.2.3").unwrap();
    assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
    assert!(v.pre.is_empty());
    assert_eq!(Version::parse("1.2.3"), Some(v.clone()));
    assert_eq!(Version::parse("V1.2.3+build.7"), Some(v));
    assert_eq!(
        Version::parse("1.0.0-rc.1").unwrap().pre,
        vec!["rc".to_string(), "1".to_string()]
    );
}

#[test]
fn rejects_malformed_versions() {
    for bad in [
        "",
        "v",
        "1",
        "1.2",
        "1.2.3.4",
        "1..3",
        "a.b.c",
        "1.2.x",
        "1.2.3-",
        "1.2.3-a..b",
        "-1.2.3",
        "1.2.3 beta",
    ] {
        assert_eq!(Version::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn newer_only_when_strictly_greater() {
    assert!(is_newer("v0.2.0", "0.1.0"));
    assert!(is_newer("v0.1.1", "0.1.0"));
    assert!(is_newer("v1.0.0", "0.9.9"));
    assert!(is_newer("v0.10.0", "0.9.0"), "numeric, not lexical");
    assert!(!is_newer("v0.1.0", "0.1.0"), "equal");
    assert!(!is_newer("v0.0.9", "0.1.0"), "older");
    assert!(!is_newer("garbage", "0.1.0"));
    assert!(!is_newer("v9.9.9", "not-a-version"));
}

#[test]
fn pre_release_precedence() {
    assert!(is_newer("v1.0.0", "1.0.0-rc.1"), "release beats its rc");
    assert!(!is_newer("v1.0.0-rc.1", "1.0.0"));
    assert!(is_newer("v1.0.0-rc.2", "1.0.0-rc.1"));
    assert!(is_newer("v1.0.0-rc.10", "1.0.0-rc.9"), "numeric ids");
    assert!(is_newer("v1.0.0-beta", "1.0.0-alpha"));
    assert!(
        is_newer("v1.0.0-alpha.1", "1.0.0-alpha"),
        "longer wins a tie"
    );
    assert!(
        is_newer("v1.0.0-alpha", "1.0.0-1"),
        "alphanumeric beats numeric"
    );
}

#[test]
fn release_page_urls_are_built_from_safe_tags_only() {
    assert_eq!(
        release_page_url("v1.2.3").as_deref(),
        Some("https://github.com/Anras573/AIchemist-desktop-buddy/releases/tag/v1.2.3")
    );
    for bad in ["", "v1/../../x", "v1?x=1", "v1#a", "v 1", "https://evil"] {
        assert_eq!(release_page_url(bad), None, "{bad:?}");
    }
}

// --- One check against a fake HTTP client --------------------------------

/// A recorded request: URL and headers.
type Request = (String, Vec<(String, String)>);

/// Canned HTTP client that records the requests.
struct FakeHttp {
    response: Mutex<Result<HttpResponse>>,
    calls: Mutex<Vec<Request>>,
    count: AtomicUsize,
}

impl FakeHttp {
    fn new(response: Result<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            response: Mutex::new(response),
            calls: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        })
    }

    fn ok(body: &str) -> Arc<Self> {
        Self::new(Ok(HttpResponse::new(200, body)))
    }

    fn count(&self) -> usize {
        self.count.load(AtomicOrdering::SeqCst)
    }
}

impl HttpClient for FakeHttp {
    fn post_form(&self, _url: &str, _form: &[(&str, &str)]) -> Result<HttpResponse> {
        panic!("the update check never posts");
    }

    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse> {
        self.count.fetch_add(1, AtomicOrdering::SeqCst);
        lock(&self.calls).push((
            url.to_owned(),
            headers
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        ));
        lock(&self.response).clone()
    }
}

fn release(tag: &str) -> String {
    format!(r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"html_url":"https://x"}}"#)
}

#[test]
fn reports_a_newer_release_with_its_page() {
    let http = FakeHttp::ok(&release("v0.2.0"));
    assert_eq!(
        check(http.as_ref(), "0.1.0"),
        CheckOutcome::UpdateAvailable(AvailableUpdate {
            tag: "v0.2.0".into(),
            url: format!("{RELEASES_PAGE_PREFIX}tag/v0.2.0"),
        })
    );
}

#[test]
fn equal_or_older_release_is_up_to_date() {
    for tag in ["v0.1.0", "v0.0.9"] {
        let http = FakeHttp::ok(&release(tag));
        assert_eq!(check(http.as_ref(), "0.1.0"), CheckOutcome::UpToDate);
    }
}

#[test]
fn request_carries_no_user_data() {
    let http = FakeHttp::ok(&release("v0.1.0"));
    check(http.as_ref(), "0.1.0");
    let calls = lock(&http.calls);
    assert_eq!(calls.len(), 1);
    let (url, headers) = &calls[0];
    assert_eq!(url, LATEST_RELEASE_API);
    assert_eq!(
        headers,
        &[
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            (
                "Accept".to_string(),
                "application/vnd.github+json".to_string()
            ),
        ]
    );
}

#[test]
fn prerelease_or_draft_is_ignored() {
    let http = FakeHttp::ok(r#"{"tag_name":"v9.0.0","prerelease":true}"#);
    assert_eq!(check(http.as_ref(), "0.1.0"), CheckOutcome::UpToDate);
    let http = FakeHttp::ok(r#"{"tag_name":"v9.0.0","draft":true}"#);
    assert_eq!(check(http.as_ref(), "0.1.0"), CheckOutcome::UpToDate);
}

#[test]
fn no_release_yet_is_up_to_date() {
    let http = FakeHttp::new(Ok(HttpResponse::new(404, "{}")));
    assert_eq!(check(http.as_ref(), "0.1.0"), CheckOutcome::UpToDate);
}

#[test]
fn rate_limits_keep_retry_after() {
    for status in [403, 429] {
        let mut response = HttpResponse::new(status, "");
        response.headers = vec![("retry-after".into(), "120".into())];
        let http = FakeHttp::new(Ok(response));
        assert_eq!(
            check(http.as_ref(), "0.1.0"),
            CheckOutcome::Failed {
                retry_after: Some(Duration::from_secs(120))
            }
        );
    }
}

#[test]
fn failures_are_silent_results() {
    let failed = CheckOutcome::Failed { retry_after: None };
    let http = FakeHttp::new(Err(Error::network("offline")));
    assert_eq!(check(http.as_ref(), "0.1.0"), failed);
    let http = FakeHttp::new(Ok(HttpResponse::new(500, "")));
    assert_eq!(check(http.as_ref(), "0.1.0"), failed);
    let http = FakeHttp::ok("not json");
    assert_eq!(check(http.as_ref(), "0.1.0"), failed);
    let http = FakeHttp::ok(&release("v9.9.9/../x"));
    assert_eq!(check(http.as_ref(), "0.1.0"), CheckOutcome::UpToDate);
}

#[test]
fn newer_tag_with_unsafe_characters_is_dropped() {
    let http = FakeHttp::ok(&release("v9.9.9+a/b"));
    assert_eq!(
        check(http.as_ref(), "0.1.0"),
        CheckOutcome::Failed { retry_after: None }
    );
}

// --- Schedule (pure, made-up clock) --------------------------------------

const DAY: Duration = CHECK_INTERVAL;

#[test]
fn schedule_is_off_until_enabled() {
    let t0 = Instant::now();
    let s = Schedule::new(STARTUP_DELAY, DAY);
    assert_eq!(s.wait(t0), None);
    assert!(!s.is_due(t0 + DAY * 10));
}

#[test]
fn schedule_first_check_after_startup_delay_then_daily() {
    let t0 = Instant::now();
    let mut s = Schedule::new(STARTUP_DELAY, DAY);
    s.set_enabled(true, t0);
    assert_eq!(s.wait(t0), Some(STARTUP_DELAY));
    assert!(!s.is_due(t0 + STARTUP_DELAY - Duration::from_secs(1)));
    let t1 = t0 + STARTUP_DELAY;
    assert!(s.is_due(t1));

    s.after_check(&CheckOutcome::UpToDate, t1);
    assert_eq!(s.wait(t1), Some(DAY));
    // Enabling again while on does not move the next check.
    s.set_enabled(true, t1 + Duration::from_secs(5));
    assert_eq!(s.next_due, Some(t1 + DAY));
}

#[test]
fn schedule_failure_waits_a_day_or_the_longer_retry_after() {
    let t0 = Instant::now();
    let mut s = Schedule::new(STARTUP_DELAY, DAY);
    s.set_enabled(true, t0);

    s.after_check(&CheckOutcome::Failed { retry_after: None }, t0);
    assert_eq!(s.next_due, Some(t0 + DAY));
    s.after_check(
        &CheckOutcome::Failed {
            retry_after: Some(Duration::from_secs(60)),
        },
        t0,
    );
    assert_eq!(s.next_due, Some(t0 + DAY), "never sooner than a day");
    s.after_check(
        &CheckOutcome::Failed {
            retry_after: Some(DAY * 2),
        },
        t0,
    );
    assert_eq!(s.next_due, Some(t0 + DAY * 2));
}

#[test]
fn schedule_disable_cancels_and_ignores_late_results() {
    let t0 = Instant::now();
    let mut s = Schedule::new(STARTUP_DELAY, DAY);
    s.set_enabled(true, t0);
    s.set_enabled(false, t0);
    assert_eq!(s.next_due, None);
    s.after_check(&CheckOutcome::UpToDate, t0);
    assert_eq!(s.next_due, None, "a result after disabling does not re-arm");
}

// --- Worker thread with a fake HTTP client -------------------------------

fn checker(
    http: Arc<FakeHttp>,
    startup: Duration,
    interval: Duration,
) -> (UpdateChecker, mpsc::Receiver<CheckOutcome>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let checker = UpdateChecker::new(
        http,
        "0.1.0",
        startup,
        interval,
        Box::new(move |o| {
            let _ = lock(&tx).send(o.clone());
        }),
    );
    (checker, rx)
}

#[test]
fn worker_makes_no_request_while_off() {
    let http = FakeHttp::ok(&release("v0.2.0"));
    let (checker, rx) = checker(http.clone(), Duration::ZERO, Duration::from_millis(1));
    assert!(lock(&checker.thread).is_none(), "no thread while off");
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(http.count(), 0);
}

#[test]
fn worker_checks_after_the_startup_delay_and_reports() {
    let http = FakeHttp::ok(&release("v0.2.0"));
    let (checker, rx) = checker(http.clone(), Duration::from_millis(20), DAY);
    checker.set_enabled(true);
    let outcome = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(outcome, CheckOutcome::UpdateAvailable(ref u) if u.tag == "v0.2.0"));
    // Next check is a day away: nothing more.
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(http.count(), 1);
}

#[test]
fn worker_repeats_on_the_interval() {
    let http = FakeHttp::ok(&release("v0.1.0"));
    let (checker, rx) = checker(http.clone(), Duration::ZERO, Duration::from_millis(10));
    checker.set_enabled(true);
    for _ in 0..3 {
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            CheckOutcome::UpToDate
        );
    }
}

#[test]
fn worker_disable_cancels_the_pending_check() {
    let http = FakeHttp::ok(&release("v0.2.0"));
    let (checker, rx) = checker(http.clone(), Duration::from_millis(200), DAY);
    checker.set_enabled(true);
    checker.set_enabled(false);
    assert!(rx.recv_timeout(Duration::from_millis(400)).is_err());
    assert_eq!(http.count(), 0);

    // Re-enabling restarts the startup delay on the same thread.
    checker.set_enabled(true);
    assert!(rx.recv_timeout(Duration::from_secs(5)).is_ok());
    assert_eq!(http.count(), 1);
}
