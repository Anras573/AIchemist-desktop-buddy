//! [`Accounts`]: the account logic behind the Settings commands, without
//! Tauri, so it is unit tested with an in-memory keychain, a fake provider
//! factory and a temporary settings file.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};

use super::identities::Identities;
use crate::domain::ipc::{AccountStatus, AddAccountRequest, SyncState, UpdateAccountRequest};
use crate::domain::{
    AccountConfig, AccountConnection, AccountId, CalendarId, CalendarInfo, CalendarProvider, Error,
    FieldError, Result, SecretString, Settings,
};
use crate::graph::auth::{validate_client_id, validate_tenant, BrowserOpener, CancelToken};
use crate::graph::factory::GraphProviderFactory;
use crate::secrets::{SecretKind, SecretStore};
use crate::settings_service::SettingsService;
use crate::storage::Storage;
use crate::sync::{AccountSyncStatus, ProviderFactory};

/// Tenant used when the field is left blank.
pub const DEFAULT_TENANT: &str = "common";

/// Longest tray status text, in characters.
const MAX_TRAY_TEXT: usize = 80;

/// Account operations and runtime account state (sync status, running
/// sign-ins). One instance lives in Tauri managed state.
pub struct Accounts {
    secrets: Arc<dyn SecretStore>,
    providers: Arc<dyn ProviderFactory>,
    graph: Option<Arc<GraphProviderFactory>>,
    identities: Identities,
    statuses: Mutex<HashMap<AccountId, AccountSyncStatus>>,
    /// Running sign-ins: account to (generation, cancel token).
    sign_ins: Mutex<HashMap<AccountId, (u64, CancelToken)>>,
    next_sign_in: Mutex<u64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Accounts {
    /// Creates the service. `providers` builds providers for every account
    /// type; `graph` (the Graph half of it) runs sign-ins and forgets removed
    /// accounts' tokens.
    pub fn new(
        secrets: Arc<dyn SecretStore>,
        providers: Arc<dyn ProviderFactory>,
        graph: Option<Arc<GraphProviderFactory>>,
        identities: Identities,
    ) -> Self {
        Self {
            secrets,
            providers,
            graph,
            identities,
            statuses: Mutex::new(HashMap::new()),
            sign_ins: Mutex::new(HashMap::new()),
            next_sign_in: Mutex::new(0),
        }
    }

    // ----- Status -----

    /// Every account with its status, in settings order.
    pub fn list(&self, settings: &Settings, storage: Option<&Storage>) -> Vec<AccountStatus> {
        let records = storage
            .and_then(|s| match s.list_sync_states() {
                Ok(records) => Some(records),
                Err(e) => {
                    log::warn!("cannot read the stored sync state: {e}");
                    None
                }
            })
            .unwrap_or_default();
        let statuses = lock(&self.statuses).clone();
        settings
            .accounts
            .iter()
            .map(|account| {
                let (sync_state, last_synced_at, last_error) = match statuses.get(&account.id) {
                    Some(s) => (s.sync_state, s.last_synced_at, s.last_error.clone()),
                    None => records
                        .iter()
                        .find(|r| r.account_id == account.id)
                        .map(stored_status)
                        .unwrap_or((SyncState::NeverSynced, None, None)),
                };
                AccountStatus {
                    account: account.clone(),
                    sync_state,
                    last_synced_at,
                    last_error,
                    has_secret: self.has_secret(account),
                    signed_in_as: match account.connection {
                        AccountConnection::Graph { .. } => self.identities.get(&account.id),
                        AccountConnection::CalDav { .. } => None,
                    },
                }
            })
            .collect()
    }

    /// One account's status.
    pub fn status(
        &self,
        settings: &Settings,
        storage: Option<&Storage>,
        id: &AccountId,
    ) -> Result<AccountStatus> {
        self.list(settings, storage)
            .into_iter()
            .find(|s| s.account.id == *id)
            .ok_or_else(not_found)
    }

    fn has_secret(&self, account: &AccountConfig) -> bool {
        // A keychain failure reads as "no secret"; the row then asks for the
        // password or a sign-in, which is the useful next step anyway.
        self.secrets
            .get(&account.id, secret_kind(&account.connection))
            .ok()
            .flatten()
            .is_some()
    }

    /// Records the sync engine's view of an account (task 3.6 calls this
    /// through [`super::set_sync_status`]). Unknown accounts are kept too;
    /// they are dropped from the output by [`Accounts::list`].
    pub fn set_sync_status(&self, status: AccountSyncStatus) {
        lock(&self.statuses).insert(status.account_id.clone(), status);
    }

    /// The tray error text for the current statuses, or `None` if no account
    /// is failing. Names only the account display names; never error
    /// details, URLs or event data.
    pub fn tray_message(&self, settings: &Settings) -> Option<String> {
        tray_message(&settings.accounts, &lock(&self.statuses))
    }

    // ----- Changes -----

    /// Adds an account. CalDAV: the password goes to the keychain, never to
    /// the settings file. Graph: no secret; sign in afterwards.
    pub fn add(
        &self,
        settings: &SettingsService,
        request: AddAccountRequest,
    ) -> Result<AccountConfig> {
        let mut errors = Vec::new();
        let display_name = check_name(&request.display_name, &mut errors);
        let connection = normalize_connection(request.connection, &mut errors);
        let secret = non_empty(request.secret);
        match (&connection, &secret) {
            (AccountConnection::CalDav { .. }, None) => errors.push(FieldError::new(
                "secret",
                "enter the password or app password",
            )),
            (AccountConnection::Graph { .. }, Some(_)) => {
                return Err(Error::invalid_input(
                    "a Microsoft account takes no password; use Sign in",
                ))
            }
            _ => {}
        }
        if !errors.is_empty() {
            return Err(Error::Validation { errors });
        }
        let account = AccountConfig {
            id: new_account_id(),
            display_name,
            connection,
            included_calendars: Vec::new(),
        };
        if let Some(secret) = &secret {
            self.secrets
                .set(&account.id, SecretKind::CalDavPassword, secret)?;
        }
        let saved = settings.update_accounts(|accounts| {
            accounts.push(account.clone());
            Ok(accounts.len() - 1)
        });
        if let Err(e) = saved {
            if secret.is_some() {
                let _ = self.secrets.delete_all(&account.id);
            }
            return Err(e);
        }
        Ok(account)
    }

    /// Changes an account's name, connection (same type) and included
    /// calendars. A new CalDAV password replaces the stored one; `None` (or
    /// empty) keeps it.
    pub fn update(
        &self,
        settings: &SettingsService,
        request: UpdateAccountRequest,
    ) -> Result<AccountConfig> {
        let current = find(&settings.current(), &request.account_id)?.clone();
        if std::mem::discriminant(&current.connection)
            != std::mem::discriminant(&request.connection)
        {
            return Err(Error::invalid_input("the account type cannot be changed"));
        }
        let mut errors = Vec::new();
        let display_name = check_name(&request.display_name, &mut errors);
        let connection = normalize_connection(request.connection, &mut errors);
        let secret = non_empty(request.secret);
        if secret.is_some() && matches!(connection, AccountConnection::Graph { .. }) {
            return Err(Error::invalid_input(
                "a Microsoft account takes no password; use Sign in",
            ));
        }
        if !errors.is_empty() {
            return Err(Error::Validation { errors });
        }
        let mut included_calendars: Vec<CalendarId> = Vec::new();
        for id in request.included_calendars {
            if !included_calendars.contains(&id) {
                included_calendars.push(id);
            }
        }
        let updated = AccountConfig {
            id: current.id.clone(),
            display_name,
            connection,
            included_calendars,
        };

        // Replace the password first, remembering the old one so a failed
        // settings save can put it back.
        let previous = match &secret {
            Some(secret) => {
                let previous = self.secrets.get(&updated.id, SecretKind::CalDavPassword)?;
                self.secrets
                    .set(&updated.id, SecretKind::CalDavPassword, secret)?;
                Some(previous)
            }
            None => None,
        };
        let saved = settings.update_accounts(|accounts| {
            let slot = accounts
                .iter_mut()
                .find(|a| a.id == updated.id)
                .ok_or_else(not_found)?;
            *slot = updated.clone();
            Ok(())
        });
        if let Err(e) = saved {
            match previous {
                Some(Some(old)) => {
                    let _ = self
                        .secrets
                        .set(&updated.id, SecretKind::CalDavPassword, &old);
                }
                Some(None) => {
                    let _ = self.secrets.delete(&updated.id, SecretKind::CalDavPassword);
                }
                None => {}
            }
            return Err(e);
        }
        Ok(updated)
    }

    /// Removes an account: its keychain secrets first (so a keychain failure
    /// keeps the account and can be retried), then its settings entry, its
    /// Graph token cache, its identity, its sync status and, if `storage` is
    /// given, its cached events and sync record.
    pub fn remove(
        &self,
        settings: &SettingsService,
        storage: Option<&Storage>,
        id: &AccountId,
    ) -> Result<()> {
        find(&settings.current(), id)?;
        self.cancel_sign_in(id);
        self.secrets.delete_all(id)?;
        settings.update_accounts(|accounts| {
            accounts.retain(|a| a.id != *id);
            Ok(())
        })?;
        if let Some(graph) = &self.graph {
            graph.remove(id);
        }
        if let Err(e) = self.identities.set(id, None) {
            log::warn!("cannot forget the removed account's sign-in name: {e}");
        }
        lock(&self.statuses).remove(id);
        if let Some(storage) = storage {
            if let Err(e) = storage.remove_account(id) {
                log::warn!("cannot remove the removed account's cached events: {e}");
            }
        }
        Ok(())
    }

    // ----- Provider calls (blocking: call off the main thread) -----

    /// A provider for a saved account.
    pub fn provider(
        &self,
        settings: &Settings,
        id: &AccountId,
    ) -> Result<Arc<dyn CalendarProvider>> {
        self.providers.create(find(settings, id)?)
    }

    /// The account's calendars, live from the server.
    pub fn list_calendars(&self, settings: &Settings, id: &AccountId) -> Result<Vec<CalendarInfo>> {
        self.provider(settings, id)?.list_calendars()
    }

    /// Tests the account's connection and credentials.
    pub fn test_connection(&self, settings: &Settings, id: &AccountId) -> Result<()> {
        self.provider(settings, id)?.test_connection()
    }

    /// Runs the interactive Microsoft sign-in for a Graph account and keeps
    /// the signed-in name. Blocks until the browser comes back, the sign-in
    /// times out, or it is cancelled (by `cancel`, a new sign-in of the same
    /// account, [`Accounts::cancel_sign_ins`] or removing the account).
    pub fn sign_in(
        &self,
        settings: &Settings,
        id: &AccountId,
        browser: &dyn BrowserOpener,
        cancel: CancelToken,
    ) -> Result<()> {
        let account = find(settings, id)?;
        if !matches!(account.connection, AccountConnection::Graph { .. }) {
            return Err(Error::invalid_input(
                "only Microsoft accounts sign in; CalDAV accounts use a password",
            ));
        }
        let graph = self
            .graph
            .as_ref()
            .ok_or_else(|| Error::internal("Microsoft sign-in is not available"))?;
        let auth = graph.auth_for(account)?;

        let generation = {
            let mut next = lock(&self.next_sign_in);
            *next += 1;
            *next
        };
        if let Some((_, previous)) =
            lock(&self.sign_ins).insert(id.clone(), (generation, cancel.clone()))
        {
            previous.cancel();
        }
        let result = auth.sign_in(browser, &cancel);
        {
            let mut running = lock(&self.sign_ins);
            if running.get(id).is_some_and(|(g, _)| *g == generation) {
                running.remove(id);
            }
        }
        let identity = result?;
        let name = identity.username.or(identity.display_name);
        if let Err(e) = self.identities.set(id, name.as_deref()) {
            // Signed in all the same; only the "signed in as" label is lost.
            log::warn!("cannot save the signed-in account name: {e}");
        }
        Ok(())
    }

    /// Cancels a running sign-in of `id`, if any.
    pub fn cancel_sign_in(&self, id: &AccountId) {
        if let Some((_, token)) = lock(&self.sign_ins).remove(id) {
            token.cancel();
        }
    }

    /// Cancels every running sign-in (the Settings window closed).
    pub fn cancel_sign_ins(&self) {
        for (_, (_, token)) in lock(&self.sign_ins).drain() {
            token.cancel();
        }
    }

    /// The persisted sign-in name of `id` (tests and diagnostics).
    pub fn signed_in_as(&self, id: &AccountId) -> Option<String> {
        self.identities.get(id)
    }
}

/// The secret an account of this type keeps in the keychain.
pub fn secret_kind(connection: &AccountConnection) -> SecretKind {
    match connection {
        AccountConnection::Graph { .. } => SecretKind::GraphRefreshToken,
        AccountConnection::CalDav { .. } => SecretKind::CalDavPassword,
    }
}

fn not_found() -> Error {
    Error::not_found("no such account")
}

fn find<'a>(settings: &'a Settings, id: &AccountId) -> Result<&'a AccountConfig> {
    settings
        .accounts
        .iter()
        .find(|a| a.id == *id)
        .ok_or_else(not_found)
}

fn non_empty(secret: Option<SecretString>) -> Option<SecretString> {
    secret.filter(|s| !s.expose().is_empty())
}

/// (state, last synced, last error) from a stored sync record.
fn stored_status(
    record: &crate::storage::AccountSyncRecord,
) -> (SyncState, Option<DateTime<Utc>>, Option<Error>) {
    let state = match (&record.last_error, record.last_success_at) {
        (Some((_, at)), Some(ok)) if ok > *at => SyncState::Ok,
        (Some(_), _) => SyncState::Error,
        (None, Some(_)) => SyncState::Ok,
        (None, None) => SyncState::NeverSynced,
    };
    let error = match state {
        SyncState::Error => record.last_error.as_ref().map(|(e, _)| e.clone()),
        _ => None,
    };
    (state, record.last_success_at, error)
}

/// The tray text for failing accounts, in settings order.
pub fn tray_message(
    accounts: &[AccountConfig],
    statuses: &HashMap<AccountId, AccountSyncStatus>,
) -> Option<String> {
    let failing: Vec<&str> = accounts
        .iter()
        .filter(|a| {
            statuses
                .get(&a.id)
                .is_some_and(|s| s.sync_state == SyncState::Error)
        })
        .map(|a| a.display_name.as_str())
        .collect();
    let first = failing.first()?;
    let text = match failing.len() {
        1 => format!("Calendar sync problem: {first}"),
        n => format!("Calendar sync problem: {first} and {} more", n - 1),
    };
    Some(if text.chars().count() > MAX_TRAY_TEXT {
        let cut: String = text.chars().take(MAX_TRAY_TEXT - 1).collect();
        format!("{cut}…")
    } else {
        text
    })
}

/// A new random account id (32 hex digits), from std only like task ids.
pub fn new_account_id() -> AccountId {
    AccountId::new(crate::storage::tasks::new_task_id().into_inner())
}

fn check_name(name: &str, errors: &mut Vec<FieldError>) -> String {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 {
        errors.push(FieldError::new("displayName", "enter a name"));
    } else if len > AccountConfig::MAX_NAME_LEN {
        errors.push(FieldError::new(
            "displayName",
            format!("must be at most {} characters", AccountConfig::MAX_NAME_LEN),
        ));
    }
    name.to_owned()
}

fn invalid_message(e: Error) -> String {
    match e {
        Error::InvalidInput { message } => message,
        other => other.to_string(),
    }
}

/// Trims and checks connection fields, adding field errors with paths such
/// as `connection.serverUrl`. Graph ids are normalized as sign-in uses them
/// (trimmed, tenant lowercased; blank tenant means [`DEFAULT_TENANT`]).
pub fn normalize_connection(
    connection: AccountConnection,
    errors: &mut Vec<FieldError>,
) -> AccountConnection {
    match connection {
        AccountConnection::Graph { client_id, tenant } => {
            let client_id = validate_client_id(&client_id).unwrap_or_else(|e| {
                errors.push(FieldError::new("connection.clientId", invalid_message(e)));
                client_id.trim().to_owned()
            });
            let tenant = if tenant.trim().is_empty() {
                DEFAULT_TENANT.to_owned()
            } else {
                validate_tenant(&tenant).unwrap_or_else(|e| {
                    errors.push(FieldError::new("connection.tenant", invalid_message(e)));
                    tenant.trim().to_owned()
                })
            };
            AccountConnection::Graph { client_id, tenant }
        }
        AccountConnection::CalDav {
            server_url,
            username,
        } => {
            let server_url = server_url.trim().to_owned();
            if let Some(message) = server_url_problem(&server_url) {
                errors.push(FieldError::new("connection.serverUrl", message));
            }
            let username = username.trim().to_owned();
            if username.is_empty() {
                errors.push(FieldError::new(
                    "connection.username",
                    "enter the user name",
                ));
            } else if username.chars().count() > 200 {
                errors.push(FieldError::new(
                    "connection.username",
                    "must be at most 200 characters",
                ));
            }
            AccountConnection::CalDav {
                server_url,
                username,
            }
        }
    }
}

fn server_url_problem(raw: &str) -> Option<&'static str> {
    if raw.is_empty() {
        return Some("enter the server URL");
    }
    if !raw.starts_with("https://") {
        return Some("must be an https:// URL");
    }
    match url::Url::parse(raw) {
        Err(_) => Some("is not a valid URL"),
        Ok(url) if url.host_str().is_none_or(str::is_empty) => Some("is not a valid URL"),
        Ok(url) if !url.username().is_empty() || url.password().is_some() => {
            Some("must not contain a user name or password")
        }
        Ok(_) => None,
    }
}
