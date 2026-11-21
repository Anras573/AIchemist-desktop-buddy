//! CalDAV calendar provider (spec 6.3).
//!
//! [`CalDavProvider`] implements [`CalendarProvider`] for one CalDAV account
//! (iCloud, Nextcloud, any RFC 4791 server) with HTTP Basic auth.
//! [`CalDavProviderFactory`] builds one per
//! [`AccountConnection::CalDav`] account for the sync engine, reading the
//! password from the [`SecretStore`].
//!
//! # Flow
//!
//! * **Discovery** (RFC 6764, RFC 4791 6.2.1): from the configured URL,
//!   `/.well-known/caldav` (bare host only), `PROPFIND current-user-principal`,
//!   then `calendar-home-set` and `calendar-user-address-set` on the
//!   principal. The result is cached in the provider; [`list_calendars`]
//!   and [`test_connection`] always rediscover.
//! * **Listing**: `PROPFIND Depth: 1` on the home; collections whose
//!   `resourcetype` has `calendar` and whose
//!   `supported-calendar-component-set` (if reported) has `VEVENT`.
//! * **Fetching**: one `REPORT calendar-query` per calendar with a VEVENT
//!   `time-range`. Each returned `calendar-data` is parsed on its own by
//!   [`ical::parse_events`], which expands recurrences (no server-side
//!   `expand`; iCloud ignores it). A resource that fails to parse is skipped
//!   and counted (logged as a count only), so one malformed event cannot
//!   block a calendar.
//!
//! # Calendar ids
//!
//! A [`CalendarId`] is the collection's absolute path on the server (e.g.
//! `/remote.php/dav/calendars/jane/personal/`), the percent-encoded `href`
//! resolved against the response URL. Fetches resolve it against the
//! discovered calendar home's origin, so credentials only go to that origin
//! (an id cannot name another host).
//!
//! # Where credentials go
//!
//! Every request carries Basic auth, so `client::Client::send` sends one
//! only to an `https` URL on the configured server's site: the configured
//! host itself, its subdomains, or (for a host with three or more labels)
//! the other hosts under its parent domain. That covers iCloud, whose
//! `caldav.icloud.com` redirects and points discovery to a partition host
//! such as `p42-caldav.icloud.com`, while a redirect or `href` to an
//! unrelated host fails with [`Error::Protocol`] before anything is sent
//! there (task 4.4). Without a public-suffix list the parent-domain rule is
//! a heuristic: a server configured as `example.co.uk` would also accept
//! `*.co.uk`.
//!
//! # Errors
//!
//! | Cause | Error |
//! |---|---|
//! | DNS, connect, TLS, timeout; HTTP 408, 429, 5xx | [`Error::Network`] (retried) |
//! | HTTP 401, 403; no password in the keychain | [`Error::Auth`] |
//! | Malformed XML, unexpected `REPORT` status, redirect or href to non-https or to another site, redirect loop | [`Error::Protocol`] |
//! | No WebDAV resource at the URL (`PROPFIND` answered with anything but 207) | [`Error::NotFound`] |
//! | Unusable server URL, bad calendar id, non-CalDAV account | [`Error::InvalidInput`] |
//!
//! A calendar that no longer exists (404/410 on its `REPORT`) yields no
//! events instead of failing the account.
//!
//! # Privacy
//!
//! Nothing here logs credentials, URLs or event contents. Error messages
//! are fixed strings plus status codes.
//!
//! [`list_calendars`]: CalendarProvider::list_calendars
//! [`test_connection`]: CalendarProvider::test_connection
//! [`ical::parse_events`]: crate::ical::parse_events

mod client;
mod http;
mod xml;

#[cfg(test)]
mod tests;

pub use http::{
    HttpRequest, HttpResponse, Transport, UreqTransport, CONNECT_TIMEOUT, MAX_BODY_BYTES,
    READ_TIMEOUT,
};

use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Utc};
use url::Url;

use client::{Client, Discovery};

use crate::domain::{
    AccountConfig, AccountConnection, AccountId, CalendarId, CalendarInfo, CalendarProvider, Clock,
    Error, Event, Result, SecretString,
};
use crate::ical::{parse_events, ParseOptions};
use crate::secrets::{SecretKind, SecretStore};
use crate::sync::ProviderFactory;

/// A read-only CalDAV account. Blocking; call it off the main thread.
pub struct CalDavProvider {
    account_id: AccountId,
    server_url: Url,
    username: String,
    client: Client,
    clock: Arc<dyn Clock>,
    discovery: Mutex<Option<Arc<Discovery>>>,
}

impl std::fmt::Debug for CalDavProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CalDavProvider")
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

/// The outcome of fetching one calendar.
#[derive(Debug)]
pub(crate) struct CalendarFetch {
    pub events: Vec<Event>,
    /// Resources whose `calendar-data` could not be parsed.
    pub skipped: usize,
}

impl CalDavProvider {
    /// Creates a provider. No network access happens until the first call.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] if `server_url` is not an `https://` URL or
    /// carries credentials.
    pub fn new(
        account_id: AccountId,
        server_url: &str,
        username: impl Into<String>,
        password: SecretString,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        let server_url = parse_server_url(server_url)?;
        let username = username.into();
        Ok(Self {
            account_id,
            client: Client::new(transport, &server_url, username.clone(), password),
            server_url,
            username,
            clock,
            discovery: Mutex::new(None),
        })
    }

    /// Runs discovery and caches the result.
    fn discover(&self) -> Result<Arc<Discovery>> {
        let found = Arc::new(self.client.discover(&self.server_url, &self.username)?);
        *self
            .discovery
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(found.clone());
        Ok(found)
    }

    /// The cached discovery, or a fresh one.
    fn discovery(&self) -> Result<Arc<Discovery>> {
        let cached = self
            .discovery
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match cached {
            Some(d) => Ok(d),
            None => self.discover(),
        }
    }

    /// Fetches one calendar. Transport, auth and multistatus errors fail
    /// the call; unparsable resources are skipped and counted.
    pub(crate) fn fetch_calendar(
        &self,
        discovery: &Discovery,
        calendar_id: &CalendarId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<CalendarFetch> {
        let url = calendar_url(&discovery.home, calendar_id)?;
        let body = xml::calendar_query(&ical_utc(from), &ical_utc(to));
        let (_, response) = self.client.send("REPORT", &url, Some("1"), &body)?;
        match response.status {
            207 => {}
            404 | 410 => {
                log::warn!("CalDAV: an included calendar no longer exists on the server");
                return Ok(CalendarFetch {
                    events: Vec::new(),
                    skipped: 0,
                });
            }
            s => {
                return Err(Error::protocol(format!(
                    "unexpected HTTP status {s} for REPORT"
                )))
            }
        }

        let opts = ParseOptions {
            account_id: &self.account_id,
            calendar_id,
            user_addresses: &discovery.user_addresses,
            clock: self.clock.as_ref(),
        };
        let mut fetch = CalendarFetch {
            events: Vec::new(),
            skipped: 0,
        };
        for resource in xml::parse_multistatus(&response.body)? {
            let Some(data) = resource.props.calendar_data else {
                continue;
            };
            match parse_events(&data, from, to, &opts) {
                Ok(events) => fetch.events.extend(events),
                Err(_) => fetch.skipped += 1,
            }
        }
        if fetch.skipped > 0 {
            log::warn!(
                "CalDAV: skipped {} event resource(s) with invalid iCalendar data",
                fetch.skipped
            );
        }
        Ok(fetch)
    }
}

impl CalendarProvider for CalDavProvider {
    fn account_id(&self) -> &AccountId {
        &self.account_id
    }

    fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        let discovery = self.discover()?;
        let (home_url, responses) =
            self.client
                .propfind(&discovery.home, "1", xml::PROPFIND_CALENDARS)?;
        let mut calendars = Vec::new();
        for r in responses {
            let has_events = r
                .props
                .components
                .as_ref()
                .is_none_or(|c| c.iter().any(|c| c == "VEVENT"));
            if !r.props.is_calendar || !has_events {
                continue;
            }
            let url = client::join(&home_url, &r.href)?;
            let id = url.path().to_owned();
            let name = r
                .props
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| name_from_path(&id));
            if calendars.iter().any(|c: &CalendarInfo| c.id.as_str() == id) {
                continue;
            }
            calendars.push(CalendarInfo {
                id: CalendarId::new(id),
                account_id: self.account_id.clone(),
                name,
                color: r.props.color.as_deref().and_then(normalize_color),
            });
        }
        Ok(calendars)
    }

    fn fetch_events(
        &self,
        calendar_ids: &[CalendarId],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        if calendar_ids.is_empty() || from >= to {
            return Ok(Vec::new());
        }
        let discovery = self.discovery()?;
        let mut events = Vec::new();
        for id in calendar_ids {
            events.extend(self.fetch_calendar(&discovery, id, from, to)?.events);
        }
        Ok(events)
    }

    fn test_connection(&self) -> Result<()> {
        self.discover().map(|_| ())
    }
}

/// Builds [`CalDavProvider`]s for [`AccountConnection::CalDav`] accounts.
pub struct CalDavProviderFactory {
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
    transport: Arc<dyn Transport>,
}

impl CalDavProviderFactory {
    /// A factory using the real HTTP transport ([`UreqTransport`], shared by
    /// every provider it creates).
    pub fn new(secrets: Arc<dyn SecretStore>, clock: Arc<dyn Clock>) -> Self {
        Self::with_transport(secrets, clock, Arc::new(UreqTransport::new()))
    }

    /// A factory using `transport` (tests inject canned responses).
    pub fn with_transport(
        secrets: Arc<dyn SecretStore>,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        Self {
            secrets,
            clock,
            transport,
        }
    }

    /// Builds the concrete provider (for callers that need more than the
    /// trait object).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a non-CalDAV account or a bad server URL,
    /// [`Error::Auth`] if no password is stored, [`Error::Keychain`] if the
    /// keychain fails.
    pub fn create_caldav(&self, account: &AccountConfig) -> Result<CalDavProvider> {
        let AccountConnection::CalDav {
            server_url,
            username,
        } = &account.connection
        else {
            return Err(Error::invalid_input("not a CalDAV account"));
        };
        let password = self
            .secrets
            .get(&account.id, SecretKind::CalDavPassword)?
            .ok_or_else(|| Error::auth("no CalDAV password is stored for this account"))?;
        CalDavProvider::new(
            account.id.clone(),
            server_url,
            username.clone(),
            password,
            self.clock.clone(),
            self.transport.clone(),
        )
    }
}

impl ProviderFactory for CalDavProviderFactory {
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
        Ok(Arc::new(self.create_caldav(account)?))
    }
}

fn parse_server_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw.trim())
        .map_err(|_| Error::invalid_input("the server URL is not a valid URL"))?;
    if url.scheme() != "https" || url.host_str().is_none_or(str::is_empty) {
        return Err(Error::invalid_input(
            "the server URL must be an https:// URL",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::invalid_input(
            "the server URL must not contain a user name or password",
        ));
    }
    Ok(url)
}

/// Resolves a calendar id (an absolute path) against the calendar home's
/// origin. Rejects anything that is not a plain absolute path, so an id
/// can never send credentials to another host.
fn calendar_url(home: &Url, id: &CalendarId) -> Result<Url> {
    let path = id.as_str();
    if !path.starts_with('/') || path.starts_with("//") || path.contains(['\\', '?', '#']) {
        return Err(Error::invalid_input("not a CalDAV calendar id"));
    }
    let url = client::join(home, path)?;
    if url.origin() != home.origin() {
        return Err(Error::invalid_input("not a CalDAV calendar id"));
    }
    Ok(url)
}

/// `2026-11-16T08:00:00Z` as iCalendar UTC: `20261116T080000Z`.
fn ical_utc(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

/// The last path segment, percent-decoded, as a fallback calendar name.
fn name_from_path(path: &str) -> String {
    let segment = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    percent_encoding::percent_decode_str(segment)
        .decode_utf8_lossy()
        .into_owned()
}

/// `#RRGGBB` or `#RRGGBBAA` (Apple) to lower-case `#rrggbb`.
fn normalize_color(raw: &str) -> Option<String> {
    let hex = raw.trim().strip_prefix('#')?;
    if !(hex.len() == 6 || hex.len() == 8) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("#{}", hex[..6].to_ascii_lowercase()))
}
