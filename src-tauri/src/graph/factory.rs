//! Building Graph providers for the sync engine.
//!
//! [`GraphProviderFactory`] keeps one [`GraphAuth`] per Graph account, so
//! the provider used by the sync engine and the `sign_in_account` /
//! `sign_out_account` commands share the same in-memory access token. A sign
//! in through [`GraphProviderFactory::auth_for`] is seen by the next sync
//! without re-creating anything.
//!
//! [`CombinedProviderFactory`] dispatches on the account's connection type to
//! the Graph factory or a CalDAV factory supplied by the caller.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::auth::GraphAuth;
use super::http::HttpClient;
use super::provider::GraphCalendarProvider;
use crate::domain::{AccountConfig, AccountConnection, AccountId, CalendarProvider, Clock, Result};
use crate::secrets::SecretStore;
use crate::sync::ProviderFactory;

/// Creates [`GraphCalendarProvider`]s and owns the per-account [`GraphAuth`].
pub struct GraphProviderFactory {
    secrets: Arc<dyn SecretStore>,
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
    /// Account id to (the connection the auth was built for, the auth).
    auths: Mutex<HashMap<AccountId, (AccountConnection, Arc<GraphAuth>)>>,
}

impl std::fmt::Debug for GraphProviderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphProviderFactory")
            .field("accounts", &self.lock().len())
            .finish_non_exhaustive()
    }
}

impl GraphProviderFactory {
    /// Creates a factory. `http` is used both for the token endpoint and for
    /// Graph itself.
    pub fn new(
        secrets: Arc<dyn SecretStore>,
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            secrets,
            http,
            clock,
            auths: Mutex::new(HashMap::new()),
        }
    }

    /// The shared [`GraphAuth`] for `account`, created on first use. If the
    /// account's client id or tenant changed since, a new one replaces the
    /// old (the stored refresh token belongs to the account id and is reused;
    /// Microsoft rejects it if it was issued to another app registration,
    /// which surfaces as an auth error asking to sign in again).
    ///
    /// Fails with [`Error::InvalidInput`](crate::domain::Error::InvalidInput)
    /// for a CalDAV account or an invalid client id or tenant.
    pub fn auth_for(&self, account: &AccountConfig) -> Result<Arc<GraphAuth>> {
        let mut auths = self.lock();
        if let Some((connection, auth)) = auths.get(&account.id) {
            if *connection == account.connection {
                return Ok(auth.clone());
            }
        }
        let auth = Arc::new(GraphAuth::new(
            account,
            self.secrets.clone(),
            self.http.clone(),
            self.clock.clone(),
        )?);
        auths.insert(
            account.id.clone(),
            (account.connection.clone(), auth.clone()),
        );
        Ok(auth)
    }

    /// The existing [`GraphAuth`] for `account`, if one was created.
    pub fn auth(&self, account: &AccountId) -> Option<Arc<GraphAuth>> {
        self.lock().get(account).map(|(_, auth)| auth.clone())
    }

    /// Forgets the [`GraphAuth`] of a removed account. Providers already
    /// handed out keep working until dropped.
    pub fn remove(&self, account: &AccountId) {
        self.lock().remove(account);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<AccountId, (AccountConnection, Arc<GraphAuth>)>> {
        // Every write is a single insert/remove, so a poisoned map is consistent.
        self.auths.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ProviderFactory for GraphProviderFactory {
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
        let auth = self.auth_for(account)?;
        Ok(Arc::new(GraphCalendarProvider::new(
            auth,
            self.http.clone(),
        )))
    }
}

/// Dispatches to the Graph factory or a CalDAV factory by connection type.
pub struct CombinedProviderFactory {
    graph: Arc<GraphProviderFactory>,
    caldav: Arc<dyn ProviderFactory>,
}

impl CombinedProviderFactory {
    /// Combines the Graph factory with a factory for CalDAV accounts.
    pub fn new(graph: Arc<GraphProviderFactory>, caldav: Arc<dyn ProviderFactory>) -> Self {
        Self { graph, caldav }
    }

    /// The Graph factory, e.g. for [`GraphProviderFactory::auth_for`].
    pub fn graph(&self) -> &Arc<GraphProviderFactory> {
        &self.graph
    }
}

impl ProviderFactory for CombinedProviderFactory {
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
        match account.connection {
            AccountConnection::Graph { .. } => self.graph.create(account),
            AccountConnection::CalDav { .. } => self.caldav.create(account),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::domain::{Error, FakeCalendarProvider, FakeClock};
    use crate::graph::http::HttpResponse;
    use crate::secrets::InMemorySecretStore;

    struct NoHttp;

    impl HttpClient for NoHttp {
        fn post_form(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse> {
            panic!("no HTTP expected")
        }
        fn get(&self, _: &str, _: &[(&str, &str)]) -> Result<HttpResponse> {
            panic!("no HTTP expected")
        }
    }

    fn factory() -> GraphProviderFactory {
        GraphProviderFactory::new(
            Arc::new(InMemorySecretStore::new()),
            Arc::new(NoHttp),
            Arc::new(FakeClock::new(
                Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            )),
        )
    }

    fn graph_account(id: &str, tenant: &str) -> AccountConfig {
        AccountConfig {
            id: AccountId::new(id),
            display_name: "Work".into(),
            connection: AccountConnection::Graph {
                client_id: "11111111-2222-3333-4444-555555555555".into(),
                tenant: tenant.into(),
            },
            included_calendars: vec![],
        }
    }

    fn caldav_account() -> AccountConfig {
        AccountConfig {
            id: AccountId::new("cal"),
            display_name: "iCloud".into(),
            connection: AccountConnection::CalDav {
                server_url: "https://caldav.example.com".into(),
                username: "u".into(),
            },
            included_calendars: vec![],
        }
    }

    #[test]
    fn shares_one_auth_per_account() {
        let f = factory();
        let a = graph_account("a", "common");
        assert!(f.auth(&a.id).is_none());
        let first = f.auth_for(&a).unwrap();
        let provider = f.create(&a).unwrap();
        assert_eq!(provider.account_id(), &a.id);
        assert!(Arc::ptr_eq(&first, &f.auth(&a.id).unwrap()));
        assert!(Arc::ptr_eq(&first, &f.auth_for(&a).unwrap()));

        let other = f.auth_for(&graph_account("b", "common")).unwrap();
        assert!(!Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn connection_change_replaces_the_auth() {
        let f = factory();
        let first = f.auth_for(&graph_account("a", "common")).unwrap();
        let second = f.auth_for(&graph_account("a", "organizations")).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        f.remove(&AccountId::new("a"));
        assert!(f.auth(&AccountId::new("a")).is_none());
    }

    #[test]
    fn rejects_caldav_and_invalid_accounts() {
        let f = factory();
        assert!(matches!(
            f.create(&caldav_account()).err(),
            Some(Error::InvalidInput { .. })
        ));
        let mut bad = graph_account("a", "common");
        bad.connection = AccountConnection::Graph {
            client_id: "not-a-guid".into(),
            tenant: "common".into(),
        };
        assert!(f.create(&bad).is_err());
    }

    struct CalDavFactory;

    impl ProviderFactory for CalDavFactory {
        fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
            Ok(Arc::new(FakeCalendarProvider::new(account.id.clone())))
        }
    }

    #[test]
    fn combined_dispatches_on_connection() {
        let combined = CombinedProviderFactory::new(Arc::new(factory()), Arc::new(CalDavFactory));
        let cal = caldav_account();
        assert_eq!(combined.create(&cal).unwrap().account_id(), &cal.id);
        let graph = graph_account("g", "common");
        assert_eq!(combined.create(&graph).unwrap().account_id(), &graph.id);
        assert!(combined.graph().auth(&graph.id).is_some());
    }
}
