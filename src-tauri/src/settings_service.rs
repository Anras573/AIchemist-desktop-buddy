//! The settings service (spec 8): owns the current [`Settings`], saves them
//! and applies changes live.
//!
//! - At startup [`init`] resolves `app_config_dir()/settings.json`, loads it
//!   (never fails, see [`crate::storage::load_settings`]) and keeps the
//!   result in managed state.
//! - [`SettingsService::update`] merges the `reminders`, `appearance` and
//!   `system` sections of an [`UpdateSettingsRequest`] into the current
//!   settings (accounts are untouched; they change only through the account
//!   commands, task 3.5), validates, saves atomically and then notifies
//!   listeners. Invalid input returns [`Error::Validation`] and changes
//!   nothing.
//! - **Live apply:** listeners registered with [`on_settings_changed`] get
//!   `(old, new)` after every successful change. [`init`] registers one that
//!   re-places the buddy window when the appearance changed. The scheduler
//!   (2.2: lead times, snooze, filters, quiet hours), the sync engine (sync
//!   interval) and the idle timer (2.1: sleep-after-idle) subscribe the same
//!   way and compare the sections they care about.
//!
//! The core ([`SettingsService`]) has no Tauri dependency, so it is unit
//! tested directly.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tauri::{AppHandle, Manager, Runtime};

use crate::domain::ipc::UpdateSettingsRequest;
use crate::domain::{Error, Result, Settings};
use crate::storage::{load_settings, save_settings, SETTINGS_FILE_NAME};

/// A settings listener: called with the old and the new settings after a
/// successful change. Runs on the thread that made the change (an async
/// runtime worker for the `update_settings` command); keep it short and do
/// not call [`SettingsService::update`] from it.
pub type Listener = Arc<dyn Fn(&Settings, &Settings) + Send + Sync>;

/// Current settings, the file they live in, and the change listeners.
pub struct SettingsService {
    /// `None` if the config directory could not be resolved; saving then
    /// fails with a storage error.
    path: Option<PathBuf>,
    current: Mutex<Settings>,
    listeners: Mutex<Vec<Listener>>,
    /// Serializes updates, so listeners see changes in the order they were
    /// saved.
    update_lock: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SettingsService {
    /// Loads the settings from `path`.
    pub fn load(path: &Path) -> Self {
        Self::with_settings(Some(path.to_path_buf()), load_settings(path))
    }

    fn with_settings(path: Option<PathBuf>, settings: Settings) -> Self {
        Self {
            path,
            current: Mutex::new(settings),
            listeners: Mutex::new(Vec::new()),
            update_lock: Mutex::new(()),
        }
    }

    /// A copy of the current settings.
    pub fn current(&self) -> Settings {
        lock(&self.current).clone()
    }

    /// Registers a listener for successful changes.
    pub fn on_change(&self, listener: impl Fn(&Settings, &Settings) + Send + Sync + 'static) {
        lock(&self.listeners).push(Arc::new(listener));
    }

    /// Merges `request` into the current settings, validates, saves and
    /// notifies listeners. Returns the saved settings. If nothing changed,
    /// nothing is written and no listener is called.
    pub fn update(&self, request: UpdateSettingsRequest) -> Result<Settings> {
        let _serial = lock(&self.update_lock);
        let old = self.current();
        let new = merge(&old, request);
        new.validate()
            .map_err(|errors| Error::Validation { errors })?;
        if new == old {
            return Ok(new);
        }
        let path = self.path.as_deref().ok_or_else(|| Error::Storage {
            message: "the settings directory is not available".into(),
        })?;
        save_settings(path, &new)?;
        *lock(&self.current) = new.clone();
        // Copy the list so a listener may register another one.
        let listeners = lock(&self.listeners).clone();
        for listener in &listeners {
            listener(&old, &new);
        }
        Ok(new)
    }
}

/// `current` with the three non-account sections replaced.
pub fn merge(current: &Settings, request: UpdateSettingsRequest) -> Settings {
    Settings {
        reminders: request.reminders,
        appearance: request.appearance,
        system: request.system,
        ..current.clone()
    }
}

/// Loads the settings, keeps the service in managed state and wires the
/// appearance to the buddy window and the idle timeout to the buddy state
/// runtime. Returns the loaded settings. Call once during setup, after
/// `buddy_state::init` and before `buddy_window::init`.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> Settings {
    let service = match app.path().app_config_dir() {
        Ok(dir) => SettingsService::load(&dir.join(SETTINGS_FILE_NAME)),
        Err(e) => {
            log::warn!("cannot resolve the config directory ({e}); using default settings");
            SettingsService::with_settings(None, Settings::default())
        }
    };
    let handle = app.clone();
    service.on_change(move |old, new| {
        if old.appearance != new.appearance {
            crate::buddy_window::set_appearance(&handle, new.appearance.clone());
        }
        let idle = new.system.sleep_after_idle_minutes;
        if old.system.sleep_after_idle_minutes != idle {
            crate::buddy_state::set_sleep_after_idle_minutes(&handle, idle);
        }
    });
    let settings = service.current();
    // Debug builds keep `AICHEMIST_SLEEP_AFTER_IDLE_SECS` (see buddy_state)
    // for testing; otherwise the saved idle timeout applies from the start.
    if !(cfg!(debug_assertions) && std::env::var_os("AICHEMIST_SLEEP_AFTER_IDLE_SECS").is_some()) {
        crate::buddy_state::set_sleep_after_idle_minutes(
            app,
            settings.system.sleep_after_idle_minutes,
        );
    }
    app.manage(service);
    settings
}

/// Registers a listener for settings changes (for the scheduler, the sync
/// engine and the idle timer). No-op before [`init`].
pub fn on_settings_changed<R: Runtime>(
    app: &AppHandle<R>,
    listener: impl Fn(&Settings, &Settings) + Send + Sync + 'static,
) {
    if let Some(service) = app.try_state::<SettingsService>() {
        service.on_change(listener);
    }
}

/// The current settings (defaults before [`init`]).
pub fn current<R: Runtime>(app: &AppHandle<R>) -> Settings {
    app.try_state::<SettingsService>()
        .map(|s| s.current())
        .unwrap_or_default()
}

/// `get_settings` (settings window only).
#[tauri::command]
pub fn get_settings(app: AppHandle) -> Settings {
    current(&app)
}

/// `update_settings` (settings window only). Async so the file write runs
/// off the main thread.
#[tauri::command]
pub async fn update_settings(app: AppHandle, request: UpdateSettingsRequest) -> Result<Settings> {
    let service = app
        .try_state::<SettingsService>()
        .ok_or_else(|| Error::internal("settings are not loaded"))?;
    service.update(request)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::domain::{AccountConfig, AccountConnection, AccountId, ScreenCorner};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("aichemist-settings-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn file(&self) -> PathBuf {
            self.0.join(SETTINGS_FILE_NAME)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn request(settings: &Settings) -> UpdateSettingsRequest {
        UpdateSettingsRequest {
            reminders: settings.reminders.clone(),
            appearance: settings.appearance.clone(),
            system: settings.system.clone(),
        }
    }

    fn with_account() -> Settings {
        Settings {
            accounts: vec![AccountConfig {
                id: AccountId::new("a1"),
                display_name: "Home".into(),
                connection: AccountConnection::CalDav {
                    server_url: "https://caldav.example.com/".into(),
                    username: "me".into(),
                },
                included_calendars: vec![],
            }],
            ..Settings::default()
        }
    }

    #[test]
    fn merge_replaces_sections_and_keeps_accounts() {
        let current = with_account();
        let mut wanted = Settings::default();
        wanted.appearance.cat_scale = 5;
        wanted.reminders.lead_times_minutes = vec![15];
        wanted.system.sleep_after_idle_minutes = 30;

        let merged = merge(&current, request(&wanted));
        assert_eq!(merged.accounts, current.accounts);
        assert_eq!(merged.appearance.cat_scale, 5);
        assert_eq!(merged.reminders.lead_times_minutes, [15]);
        assert_eq!(merged.system.sleep_after_idle_minutes, 30);
        assert_eq!(merged.schema_version, Settings::SCHEMA_VERSION);
    }

    #[test]
    fn invalid_request_maps_to_field_errors_and_saves_nothing() {
        let dir = TempDir::new("invalid");
        let service = SettingsService::load(&dir.file());
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        service.on_change(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
        });

        let mut bad = Settings::default();
        bad.appearance.cat_scale = 9;
        bad.reminders.lead_times_minutes = vec![5, 5];
        let err = service.update(request(&bad)).unwrap_err();

        let Error::Validation { errors } = err else {
            panic!("expected a validation error, got {err:?}");
        };
        let fields: Vec<_> = errors.iter().map(|e| e.field.as_str()).collect();
        assert_eq!(
            fields,
            ["reminders.leadTimesMinutes[1]", "appearance.catScale"]
        );
        assert!(!dir.file().exists(), "nothing may be written");
        assert_eq!(service.current(), Settings::default());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn update_saves_and_reloads() {
        let dir = TempDir::new("reload");
        let service = SettingsService::load(&dir.file());
        let mut wanted = Settings::default();
        wanted.appearance.corner = ScreenCorner::TopLeft;
        wanted.appearance.monitor = Some("DISPLAY2".into());
        wanted.reminders.snooze_durations_minutes = vec![10, 5];
        wanted.system.quiet_hours.enabled = true;

        let saved = service.update(request(&wanted)).unwrap();
        assert_eq!(saved, wanted);
        assert_eq!(service.current(), wanted);
        assert_eq!(SettingsService::load(&dir.file()).current(), wanted);
    }

    #[test]
    fn update_keeps_accounts_on_disk() {
        let dir = TempDir::new("accounts");
        save_settings(&dir.file(), &with_account()).unwrap();
        let service = SettingsService::load(&dir.file());

        let mut wanted = Settings::default();
        wanted.appearance.cat_scale = 3;
        service.update(request(&wanted)).unwrap();

        let reloaded = load_settings(&dir.file());
        assert_eq!(reloaded.accounts, with_account().accounts);
        assert_eq!(reloaded.appearance.cat_scale, 3);
    }

    #[test]
    fn listeners_are_notified_only_on_a_successful_change() {
        let dir = TempDir::new("listeners");
        let service = SettingsService::load(&dir.file());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        service.on_change(move |old, new| {
            lock(&log).push((old.appearance.cat_scale, new.appearance.cat_scale));
        });

        // No change: no write, no call.
        service.update(request(&Settings::default())).unwrap();
        assert!(!dir.file().exists());

        let mut wanted = Settings::default();
        wanted.appearance.cat_scale = 4;
        service.update(request(&wanted)).unwrap();

        wanted.appearance.cat_scale = 0;
        service.update(request(&wanted)).unwrap_err();

        assert_eq!(*lock(&seen), [(2, 4)]);
    }

    #[test]
    fn missing_directory_reports_a_storage_error() {
        let service = SettingsService::with_settings(None, Settings::default());
        let mut wanted = Settings::default();
        wanted.appearance.cat_scale = 3;
        assert!(matches!(
            service.update(request(&wanted)),
            Err(Error::Storage { .. })
        ));
        assert_eq!(service.current(), Settings::default());
    }
}
