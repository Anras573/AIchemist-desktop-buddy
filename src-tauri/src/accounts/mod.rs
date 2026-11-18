//! Calendar accounts in Settings (spec 6, 8 "Calendar accounts", 9) and the
//! tray error indicator (task 3.5).
//!
//! # Pieces
//!
//! - [`AppServices`] (managed state): the app's one [`SecretStore`] (the OS
//!   keychain; on platforms without one, a store that reports a keychain
//!   error, so the app still runs), one [`CombinedProviderFactory`] (Graph +
//!   CalDAV) and the [`SystemClock`]. The sync engine (3.6) takes its
//!   factory from here, so sign-ins and syncs share one `GraphAuth` per
//!   account.
//! - [`Accounts`] (managed state): the account logic, Tauri-free and unit
//!   tested ([`service`]).
//! - The settings-window commands below. Each runs on a blocking worker
//!   (keychain, network and the browser wait all block), never on the main
//!   thread, and emits `accounts-changed` after a change.
//!
//! # Where things are stored
//!
//! - Account configuration: `settings.json` via
//!   [`SettingsService::update_accounts`]. Never secrets.
//! - CalDAV password, Graph refresh token: the keychain only.
//! - Microsoft "signed in as" name: `account-identities.json` ([`identities`]).
//!
//! # Hooks for task 3.6 (sync engine)
//!
//! - Call [`set_sync_status`] for every `SyncUpdate::AccountStatus`: it
//!   updates the account rows (`accounts-changed`) and the tray error item.
//! - Build the engine with [`services`]`(app).providers`.
//! - Account changes reach the engine as settings changes
//!   ([`crate::settings_service::on_settings_changed`]).
//! - [`sync_now`] is a stub that only logs; wire it to `SyncHandle::sync_now`.
//! - [`list_accounts`] reads the stored `sync_state` rows and
//!   [`remove_account`] deletes cached events and the row through
//!   [`crate::app_storage`] (skipped if the database did not open); until the
//!   engine records syncs, rows show "never synced".
//!
//! # Privacy
//!
//! Nothing here logs secrets, server URLs, user names, sign-in names or
//! event data. Errors carry provider messages, which follow the same rule.

pub mod identities;
pub mod service;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewWindow, WindowEvent};

pub use service::Accounts;

use crate::caldav::CalDavProviderFactory;
use crate::domain::ipc::{
    event, AccountRequest, AccountStatus, AddAccountRequest, UpdateAccountRequest,
};
use crate::domain::{AccountId, CalendarInfo, Clock, Error, Result, SecretString, SystemClock};
use crate::graph::auth::{CancelToken, SystemBrowser};
use crate::graph::factory::{CombinedProviderFactory, GraphProviderFactory};
use crate::graph::http::UreqHttpClient;
use crate::secrets::{KeyringSecretStore, SecretKind, SecretStore};
use crate::settings_service::SettingsService;
use crate::settings_window::SETTINGS_WINDOW_LABEL;
use crate::storage::Storage;
use crate::sync::AccountSyncStatus;
use identities::{Identities, IDENTITIES_FILE_NAME};

/// Shared app services, in managed state.
#[derive(Clone)]
pub struct AppServices {
    /// The OS keychain (or [`NoKeychain`]).
    pub secrets: Arc<dyn SecretStore>,
    /// Providers for every account type.
    pub providers: Arc<CombinedProviderFactory>,
    /// The real clock.
    pub clock: Arc<dyn Clock>,
}

/// A [`SecretStore`] for platforms without a supported keychain: reads and
/// writes fail with [`Error::Keychain`], deletes succeed (nothing is stored).
#[derive(Debug)]
pub struct NoKeychain(String);

impl SecretStore for NoKeychain {
    fn get(&self, _: &AccountId, _: SecretKind) -> Result<Option<SecretString>> {
        Err(Error::keychain(self.0.clone()))
    }
    fn set(&self, _: &AccountId, _: SecretKind, _: &SecretString) -> Result<()> {
        Err(Error::keychain(self.0.clone()))
    }
    fn delete(&self, _: &AccountId, _: SecretKind) -> Result<()> {
        Ok(())
    }
}

/// Creates the shared services and the account service. Call once during
/// setup, after [`crate::settings_service::init`].
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let secrets: Arc<dyn SecretStore> = match KeyringSecretStore::new() {
        Ok(store) => Arc::new(store),
        Err(e) => {
            log::warn!("no OS keychain ({e}); calendar accounts cannot store secrets");
            Arc::new(NoKeychain(
                "no supported OS keychain on this platform".into(),
            ))
        }
    };
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let graph = Arc::new(GraphProviderFactory::new(
        secrets.clone(),
        Arc::new(UreqHttpClient::new()),
        clock.clone(),
    ));
    let caldav = Arc::new(CalDavProviderFactory::new(secrets.clone(), clock.clone()));
    let providers = Arc::new(CombinedProviderFactory::new(graph.clone(), caldav));
    let identities = match app.path().app_config_dir() {
        Ok(dir) => Identities::load(&dir.join(IDENTITIES_FILE_NAME)),
        Err(_) => Identities::in_memory(),
    };
    app.manage(Accounts::new(
        secrets.clone(),
        providers.clone(),
        Some(graph),
        identities,
    ));
    app.manage(AppServices {
        secrets,
        providers,
        clock,
    });
}

/// The shared services (for the sync engine, 3.6). `None` before [`init`].
pub fn services<R: Runtime>(app: &AppHandle<R>) -> Option<AppServices> {
    app.try_state::<AppServices>().map(|s| s.inner().clone())
}

/// Records an account's sync status (called by the sync engine, 3.6):
/// refreshes the Settings rows and the tray error indicator ("Calendar sync
/// problem: <account name>", cleared when no account is failing).
pub fn set_sync_status<R: Runtime>(app: &AppHandle<R>, status: AccountSyncStatus) {
    let Some(accounts) = app.try_state::<Accounts>() else {
        return;
    };
    accounts.set_sync_status(status);
    refresh_tray(app);
    emit_changed(app);
}

fn refresh_tray<R: Runtime>(app: &AppHandle<R>) {
    if let Some(accounts) = app.try_state::<Accounts>() {
        let settings = crate::settings_service::current(app);
        crate::tray::set_error(app, accounts.tray_message(&settings).as_deref());
    }
}

fn emit_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit_to(SETTINGS_WINDOW_LABEL, event::ACCOUNTS_CHANGED, ());
}

/// The app database ([`crate::app_storage`]), if it opened.
fn storage<R: Runtime>(app: &AppHandle<R>) -> Option<Storage> {
    crate::app_storage::storage(app).ok()
}

/// Runs `f` on a blocking worker with the account service and settings.
async fn run<T, F>(app: AppHandle, f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle, &Accounts, &SettingsService) -> Result<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let accounts = app
            .try_state::<Accounts>()
            .ok_or_else(|| Error::internal("accounts are not initialized"))?;
        let settings = app
            .try_state::<SettingsService>()
            .ok_or_else(|| Error::internal("settings are not loaded"))?;
        f(&app, &accounts, &settings)
    })
    .await
    .map_err(|e| Error::internal(format!("account task failed: {e}")))?
}

/// `list_accounts` (settings window only).
#[tauri::command]
pub async fn list_accounts(app: AppHandle) -> Result<Vec<AccountStatus>> {
    run(app, |app, accounts, settings| {
        Ok(accounts.list(&settings.current(), storage(app).as_ref()))
    })
    .await
}

/// `add_account` (settings window only).
#[tauri::command]
pub async fn add_account(app: AppHandle, request: AddAccountRequest) -> Result<AccountStatus> {
    run(app, move |app, accounts, settings| {
        let account = accounts.add(settings, request)?;
        log::info!("calendar account added");
        emit_changed(app);
        accounts.status(&settings.current(), storage(app).as_ref(), &account.id)
    })
    .await
}

/// `update_account` (settings window only).
#[tauri::command]
pub async fn update_account(
    app: AppHandle,
    request: UpdateAccountRequest,
) -> Result<AccountStatus> {
    run(app, move |app, accounts, settings| {
        let account = accounts.update(settings, request)?;
        emit_changed(app);
        refresh_tray(app);
        accounts.status(&settings.current(), storage(app).as_ref(), &account.id)
    })
    .await
}

/// `remove_account` (settings window only).
#[tauri::command]
pub async fn remove_account(app: AppHandle, request: AccountRequest) -> Result<()> {
    run(app, move |app, accounts, settings| {
        accounts.remove(settings, storage(app).as_ref(), &request.account_id)?;
        log::info!("calendar account removed");
        emit_changed(app);
        refresh_tray(app);
        Ok(())
    })
    .await
}

/// `list_account_calendars` (settings window only).
#[tauri::command]
pub async fn list_account_calendars(
    app: AppHandle,
    request: AccountRequest,
) -> Result<Vec<CalendarInfo>> {
    run(app, move |_, accounts, settings| {
        accounts.list_calendars(&settings.current(), &request.account_id)
    })
    .await
}

/// `test_connection` (settings window only).
#[tauri::command]
pub async fn test_connection(app: AppHandle, request: AccountRequest) -> Result<()> {
    run(app, move |_, accounts, settings| {
        let result = accounts.test_connection(&settings.current(), &request.account_id);
        if let Err(e) = &result {
            log::info!("connection test failed: {e}");
        }
        result
    })
    .await
}

/// `sign_in_account` (settings window only): the Microsoft sign-in in the
/// system browser. Cancelled when the Settings window closes.
#[tauri::command]
pub async fn sign_in_account(
    app: AppHandle,
    window: WebviewWindow,
    request: AccountRequest,
) -> Result<()> {
    let cancel = CancelToken::new();
    let on_close = cancel.clone();
    window.on_window_event(move |e| {
        if matches!(e, WindowEvent::Destroyed) {
            on_close.cancel();
        }
    });
    run(app, move |app, accounts, settings| {
        let result = accounts.sign_in(
            &settings.current(),
            &request.account_id,
            &SystemBrowser,
            cancel,
        );
        match &result {
            Ok(()) => log::info!("Microsoft sign-in completed"),
            Err(e) => log::info!("Microsoft sign-in did not complete: {e}"),
        }
        emit_changed(app);
        result
    })
    .await
}

/// `sync_now` (settings window only). **Stub until task 3.6** wires the
/// sync engine (`SyncHandle::sync_now`); it only logs.
#[tauri::command]
pub fn sync_now() {
    log::info!("sync requested; the sync engine is not wired yet (task 3.6)");
}
