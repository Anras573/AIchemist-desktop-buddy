//! System integration (spec 8, System section): launch at login
//! ([`autostart`]) and the opt-in update check ([`update_check`]).
//!
//! [`init`] is the one setup call: it applies both settings at startup and
//! registers a settings listener that re-applies them when they change.
//! With `check_for_updates` off (the default) no request is made and no
//! update thread runs.

pub mod autostart;
pub mod update_check;

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::domain::Settings;
use crate::graph::http::UreqHttpClient;
use autostart::{Autostart, OsAutostart};
use update_check::{CheckOutcome, UpdateChecker};

/// What the settings listener drives. Lets tests swap the OS side.
pub struct SystemIntegration {
    autostart: Box<dyn Autostart>,
    updates: UpdateChecker,
}

impl SystemIntegration {
    /// Wraps the two parts.
    pub fn new(autostart: Box<dyn Autostart>, updates: UpdateChecker) -> Self {
        Self { autostart, updates }
    }

    /// Applies `settings` from scratch (startup): the login entry is written
    /// or removed to match, and the update check is turned on or off.
    pub fn apply_all(&self, settings: &Settings) {
        self.apply_autostart(settings.system.launch_at_login);
        self.updates.set_enabled(settings.system.check_for_updates);
    }

    /// Applies what changed between `old` and `new`. Returns `true` if the
    /// update check was turned off (the caller clears the notice).
    pub fn apply_change(&self, old: &Settings, new: &Settings) -> bool {
        if old.system.launch_at_login != new.system.launch_at_login {
            self.apply_autostart(new.system.launch_at_login);
        }
        let (was, now) = (old.system.check_for_updates, new.system.check_for_updates);
        if was != now {
            self.updates.set_enabled(now);
        }
        was && !now
    }

    fn apply_autostart(&self, enabled: bool) {
        match self.autostart.apply(enabled) {
            Ok(()) => log::info!("launch at login: {}", if enabled { "on" } else { "off" }),
            Err(e) => log::warn!("cannot change launch at login: {e}"),
        }
    }
}

/// Opens a release page in the default browser. Only
/// [`update_check::RELEASES_PAGE_PREFIX`] URLs are opened. No shell is
/// involved (same launchers as the Graph sign-in).
pub fn open_release_page(url: &str) {
    if !url.starts_with(update_check::RELEASES_PAGE_PREFIX) {
        log::warn!("refusing to open a non-release URL");
        return;
    }
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    // Detached: the menu handler must not wait for the browser.
    if let Err(e) = command.spawn() {
        log::info!("cannot open the release page: {e}");
    }
}

/// Debug builds only: `AICHEMIST_UPDATE_CHECK_DELAY_SECS` shortens the
/// startup delay and `AICHEMIST_UPDATE_CHECK_FAKE_LATEST=v9.9.9` replaces
/// the network with a canned "latest release" (manual testing).
fn debug_override(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok()
    } else {
        None
    }
}

/// A canned client for `AICHEMIST_UPDATE_CHECK_FAKE_LATEST` (debug only).
struct FakeLatest(String);

impl crate::graph::http::HttpClient for FakeLatest {
    fn post_form(
        &self,
        _url: &str,
        _form: &[(&str, &str)],
    ) -> crate::domain::Result<crate::graph::http::HttpResponse> {
        Err(crate::domain::Error::internal("not supported"))
    }

    fn get(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
    ) -> crate::domain::Result<crate::graph::http::HttpResponse> {
        log::info!("update check (fake): GET {url}");
        let body = serde_json::json!({ "tag_name": self.0 }).to_string();
        Ok(crate::graph::http::HttpResponse::new(200, body))
    }
}

/// Sets up launch at login and the update check. Call once during setup,
/// after `settings_service::init` and `tray::init`.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let identifier = app.config().identifier.clone();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            // `OsAutostart` then refuses to register (removing still works).
            log::warn!("cannot resolve the executable path ({e})");
            std::path::PathBuf::new()
        }
    };
    let autostart = OsAutostart { identifier, exe };

    let http: Arc<dyn crate::graph::http::HttpClient> =
        match debug_override("AICHEMIST_UPDATE_CHECK_FAKE_LATEST") {
            Some(tag) => Arc::new(FakeLatest(tag)),
            None => Arc::new(UreqHttpClient::new()),
        };
    let startup_delay = debug_override("AICHEMIST_UPDATE_CHECK_DELAY_SECS")
        .and_then(|s| s.parse().ok())
        .map_or(update_check::STARTUP_DELAY, Duration::from_secs);
    let handle = app.clone();
    let updates = UpdateChecker::new(
        http,
        env!("CARGO_PKG_VERSION"),
        startup_delay,
        update_check::CHECK_INTERVAL,
        Box::new(move |outcome| match outcome {
            CheckOutcome::UpdateAvailable(update) => {
                crate::tray::set_update_available(&handle, Some((&update.tag, &update.url)));
            }
            CheckOutcome::UpToDate => crate::tray::set_update_available(&handle, None),
            // Keep whatever was shown before.
            CheckOutcome::Failed { .. } => {}
        }),
    );

    let system = Arc::new(SystemIntegration::new(Box::new(autostart), updates));
    let settings = crate::settings_service::current(app);
    system.apply_all(&settings);
    if !settings.system.check_for_updates {
        log::info!("update check is off; no update requests are made");
    }

    let listener = system.clone();
    let handle = app.clone();
    crate::settings_service::on_settings_changed(app, move |old, new| {
        if listener.apply_change(old, new) {
            crate::tray::set_update_available(&handle, None);
        }
    });
    app.manage(system);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::domain::Result;
    use crate::graph::http::{HttpClient, HttpResponse};

    #[derive(Default)]
    struct FakeAutostart(Arc<Mutex<Vec<bool>>>);

    impl Autostart for FakeAutostart {
        fn apply(&self, enabled: bool) -> std::io::Result<()> {
            self.0.lock().unwrap().push(enabled);
            Ok(())
        }
    }

    struct CountingHttp(Arc<Mutex<usize>>);

    impl HttpClient for CountingHttp {
        fn post_form(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse> {
            unreachable!()
        }
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse> {
            *self.0.lock().unwrap() += 1;
            Ok(HttpResponse::new(404, ""))
        }
    }

    type Applied = Arc<Mutex<Vec<bool>>>;

    fn system() -> (SystemIntegration, Applied, Arc<Mutex<usize>>) {
        let applied = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(Mutex::new(0));
        let updates = UpdateChecker::new(
            Arc::new(CountingHttp(requests.clone())),
            "0.1.0",
            Duration::ZERO,
            update_check::CHECK_INTERVAL,
            Box::new(|_| {}),
        );
        let system = SystemIntegration::new(Box::new(FakeAutostart(applied.clone())), updates);
        (system, applied, requests)
    }

    fn with(launch: bool, updates: bool) -> Settings {
        let mut s = Settings::default();
        s.system.launch_at_login = launch;
        s.system.check_for_updates = updates;
        s
    }

    #[test]
    fn startup_applies_the_saved_settings() {
        let (system, applied, requests) = system();
        system.apply_all(&Settings::default());
        // Settings win: the entry is removed even if it was present.
        assert_eq!(*applied.lock().unwrap(), [false]);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*requests.lock().unwrap(), 0, "no request when off");
    }

    #[test]
    fn listener_applies_only_what_changed() {
        let (system, applied, requests) = system();
        system.apply_all(&Settings::default());

        assert!(!system.apply_change(&with(false, false), &with(true, false)));
        assert!(!system.apply_change(&with(true, false), &with(true, false)));
        assert!(!system.apply_change(&with(true, false), &with(false, false)));
        assert_eq!(*applied.lock().unwrap(), [false, true, false]);

        // Turning the update check on makes a request (startup delay 0 here).
        assert!(!system.apply_change(&with(false, false), &with(false, true)));
        for _ in 0..200 {
            if *requests.lock().unwrap() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*requests.lock().unwrap(), 1);
        // Turning it off reports it, so the tray notice is cleared.
        assert!(system.apply_change(&with(false, true), &with(false, false)));
        assert_eq!(*applied.lock().unwrap(), [false, true, false]);
    }
}
