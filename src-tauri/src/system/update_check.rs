//! Opt-in update check (spec 8, 9).
//!
//! Only runs while `system.check_for_updates` is on; with it off no request
//! is made and no thread exists. When on: one check [`STARTUP_DELAY`] after
//! start (or after turning it on), then one every [`CHECK_INTERVAL`]. A
//! single worker thread sleeps on a condition variable until the next check
//! is due (no polling).
//!
//! The request is a plain `GET` of the GitHub "latest release" endpoint with
//! a generic `User-Agent` and no user data or identifiers. The release tag
//! is compared with the app version; a newer one is reported to the caller
//! (the tray shows "Update available: vX.Y.Z", which opens the release page).
//! Nothing is downloaded or installed (unsigned builds, spec 2). Failures
//! are logged at info level and otherwise silent. 403/429 responses (rate
//! limits) wait for at least the `Retry-After` time and never less than the
//! normal interval.

use std::cmp::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::graph::http::HttpClient;

/// The only URL the update check requests.
pub const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Anras573/AIchemist-desktop-buddy/releases/latest";
/// Prefix of every release page URL the app opens.
pub const RELEASES_PAGE_PREFIX: &str =
    "https://github.com/Anras573/AIchemist-desktop-buddy/releases/";
/// Generic `User-Agent` (GitHub requires one). Carries no version, OS or
/// identifier.
pub const USER_AGENT: &str = "AIchemist-Buddy-update-check";
/// Delay before the first check after start or after turning the check on.
pub const STARTUP_DELAY: Duration = Duration::from_secs(60);
/// Time between checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// A semantic version (`MAJOR.MINOR.PATCH[-PRE][+BUILD]`, optional leading
/// `v`). Build metadata is ignored, as semver says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Major version.
    pub major: u64,
    /// Minor version.
    pub minor: u64,
    /// Patch version.
    pub patch: u64,
    /// Pre-release identifiers (`rc.1` in `1.0.0-rc.1`); empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3`, `1.2.3-rc.1`, `v1.2.3+build`. `None` for
    /// anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text
            .strip_prefix('v')
            .or_else(|| text.strip_prefix('V'))
            .unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(v, _)| v);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let mut number = || -> Option<u64> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let (major, minor, patch) = (number()?, number()?, number()?);
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let ids: Vec<String> = pre.split('.').map(str::to_owned).collect();
                let valid = |id: &String| {
                    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                };
                if !ids.iter().all(valid) {
                    return None;
                }
                ids
            }
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release is newer than its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare_pre(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semver pre-release precedence: identifier by identifier, numeric ones
/// numerically and below alphanumeric ones; a longer list wins a tie.
fn compare_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// `true` if the release tagged `latest_tag` is newer than `current`. An
/// unparsable version is never newer.
pub fn is_newer(latest_tag: &str, current: &str) -> bool {
    match (Version::parse(latest_tag), Version::parse(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

/// The release page for `tag`, or `None` if the tag holds characters that do
/// not belong in a version tag (the URL is built here, never taken from the
/// response).
pub fn release_page_url(tag: &str) -> Option<String> {
    let safe = !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'));
    safe.then(|| format!("{RELEASES_PAGE_PREFIX}tag/{tag}"))
}

/// A newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    /// The release tag, e.g. `v1.2.0`.
    pub tag: String,
    /// Its release page (always starts with [`RELEASES_PAGE_PREFIX`]).
    pub url: String,
}

/// Result of one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The latest release is not newer (or there is no release yet).
    UpToDate,
    /// A newer release exists.
    UpdateAvailable(AvailableUpdate),
    /// The check failed (network, unexpected response, rate limit). The
    /// previous result stays.
    Failed {
        /// Minimum wait the server asked for (rate limit), if any.
        retry_after: Option<Duration>,
    },
}

#[derive(serde::Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// Runs one check against `http`. Blocks for up to the client's timeout.
pub fn check(http: &dyn HttpClient, current_version: &str) -> CheckOutcome {
    let failed = CheckOutcome::Failed { retry_after: None };
    let response = match http.get(
        LATEST_RELEASE_API,
        &[
            ("User-Agent", USER_AGENT),
            ("Accept", "application/vnd.github+json"),
        ],
    ) {
        Ok(r) => r,
        Err(e) => {
            log::info!("update check failed: {e}");
            return failed;
        }
    };
    match response.status {
        200 => {}
        // No published release yet.
        404 => {
            log::info!("update check: no release published yet");
            return CheckOutcome::UpToDate;
        }
        403 | 429 => {
            let retry_after = response
                .header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            log::info!(
                "update check rate limited (HTTP {}); retry after {:?}",
                response.status,
                retry_after
            );
            return CheckOutcome::Failed { retry_after };
        }
        status => {
            log::info!("update check failed: HTTP {status}");
            return failed;
        }
    }
    let release: LatestRelease = match serde_json::from_slice(&response.body) {
        Ok(r) => r,
        Err(e) => {
            log::info!("update check: unexpected response ({e})");
            return failed;
        }
    };
    if release.draft || release.prerelease || !is_newer(&release.tag_name, current_version) {
        log::info!("update check: up to date (latest {})", release.tag_name);
        return CheckOutcome::UpToDate;
    }
    match release_page_url(&release.tag_name) {
        Some(url) => {
            log::info!("update check: {} is available", release.tag_name);
            CheckOutcome::UpdateAvailable(AvailableUpdate {
                tag: release.tag_name,
                url,
            })
        }
        None => {
            log::info!("update check: ignoring an unexpected tag");
            failed
        }
    }
}

/// When the next check is due. Pure: every method takes the current time,
/// so tests drive it with made-up instants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Delay before the first check after enabling.
    pub startup_delay: Duration,
    /// Time between checks.
    pub interval: Duration,
    /// `None` while the check is off.
    pub next_due: Option<Instant>,
}

impl Schedule {
    /// An off schedule.
    pub fn new(startup_delay: Duration, interval: Duration) -> Self {
        Self {
            startup_delay,
            interval,
            next_due: None,
        }
    }

    /// Turns the check on or off. Turning it on schedules the first check
    /// after the startup delay; turning it on again while on changes nothing.
    pub fn set_enabled(&mut self, enabled: bool, now: Instant) {
        match (enabled, self.next_due) {
            (true, None) => self.next_due = Some(now + self.startup_delay),
            (false, _) => self.next_due = None,
            (true, Some(_)) => {}
        }
    }

    /// Whether a check should run now.
    pub fn is_due(&self, now: Instant) -> bool {
        self.next_due.is_some_and(|due| now >= due)
    }

    /// How long to sleep: `None` means until woken (off).
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.next_due.map(|due| due.saturating_duration_since(now))
    }

    /// Schedules the next check after one finished at `now`.
    pub fn after_check(&mut self, outcome: &CheckOutcome, now: Instant) {
        if self.next_due.is_none() {
            return; // Turned off during the check.
        }
        let delay = match outcome {
            CheckOutcome::Failed {
                retry_after: Some(retry),
            } => (*retry).max(self.interval),
            _ => self.interval,
        };
        self.next_due = Some(now + delay);
    }
}

/// Callback with each check's result. Runs on the worker thread.
pub type OutcomeSink = Box<dyn Fn(&CheckOutcome) + Send + Sync>;

struct State {
    schedule: Schedule,
    /// Bumped by every `set_enabled`, so a result from a check that started
    /// before the check was turned off (or off and on) is dropped.
    generation: u64,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs the update check on its own thread while enabled. The thread is
/// started on the first [`UpdateChecker::set_enabled`]`(true)`, so with the
/// check off there is no thread at all.
pub struct UpdateChecker {
    shared: Arc<Shared>,
    http: Arc<dyn HttpClient>,
    current_version: String,
    sink: Arc<OutcomeSink>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl UpdateChecker {
    /// A checker that is off. `current_version` is the app version.
    pub fn new(
        http: Arc<dyn HttpClient>,
        current_version: impl Into<String>,
        startup_delay: Duration,
        interval: Duration,
        sink: OutcomeSink,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    schedule: Schedule::new(startup_delay, interval),
                    generation: 0,
                    shutdown: false,
                }),
                wake: Condvar::new(),
            }),
            http,
            current_version: current_version.into(),
            sink: Arc::new(sink),
            thread: Mutex::new(None),
        }
    }

    /// Turns the check on or off. Off cancels a pending check; a check that
    /// is running finishes but its result is dropped.
    pub fn set_enabled(&self, enabled: bool) {
        {
            let mut state = lock(&self.shared.state);
            let was = state.schedule.next_due.is_some();
            if was == enabled {
                return;
            }
            state.schedule.set_enabled(enabled, Instant::now());
            state.generation += 1;
        }
        self.shared.wake.notify_all();
        if enabled {
            self.ensure_thread();
        }
    }

    fn ensure_thread(&self) {
        let mut thread = lock(&self.thread);
        if thread.is_some() {
            return;
        }
        let shared = self.shared.clone();
        let http = self.http.clone();
        let version = self.current_version.clone();
        let sink = self.sink.clone();
        match std::thread::Builder::new()
            .name("update-check".into())
            .spawn(move || run(&shared, http.as_ref(), &version, sink.as_ref()))
        {
            Ok(handle) => *thread = Some(handle),
            Err(e) => log::info!("cannot start the update check thread: {e}"),
        }
    }
}

impl Drop for UpdateChecker {
    fn drop(&mut self) {
        lock(&self.shared.state).shutdown = true;
        self.shared.wake.notify_all();
        if let Some(handle) = lock(&self.thread).take() {
            let _ = handle.join();
        }
    }
}

fn run(shared: &Shared, http: &dyn HttpClient, version: &str, sink: &OutcomeSink) {
    let mut state = lock(&shared.state);
    loop {
        if state.shutdown {
            return;
        }
        let now = Instant::now();
        if !state.schedule.is_due(now) {
            state = match state.schedule.wait(now) {
                Some(wait) => {
                    shared
                        .wake
                        .wait_timeout(state, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0
                }
                None => shared.wake.wait(state).unwrap_or_else(|e| e.into_inner()),
            };
            continue;
        }
        let generation = state.generation;
        drop(state);
        let outcome = check(http, version);
        state = lock(&shared.state);
        if state.generation != generation || state.shutdown {
            continue;
        }
        state.schedule.after_check(&outcome, Instant::now());
        drop(state);
        sink(&outcome);
        state = lock(&shared.state);
    }
}

#[cfg(test)]
#[path = "update_check_tests.rs"]
mod tests;
