//! The Microsoft Graph [`CalendarProvider`] (spec 6.2).
//!
//! # Requests
//!
//! * `list_calendars`: `GET /me/calendars`, following `@odata.nextLink`.
//! * `fetch_events`: per calendar, `GET /me/calendars/{id}/calendarView` for
//!   the window, following `@odata.nextLink`. `calendarView` expands
//!   recurring series into instances, so each returned item is one
//!   occurrence and its `id` (the instance id) is the [`EventId`].
//! * `test_connection`: `GET /me/calendars?$top=1`.
//!
//! Every request sends `Prefer: outlook.timezone="UTC"`, so timed events come
//! back as UTC wall-clock times. `$select` asks only for the fields that are
//! mapped.
//!
//! # Mapping
//!
//! * Items with `isCancelled: true` are omitted.
//! * Timed events: `start`/`end` `dateTime` read as UTC.
//! * All-day events (`isAllDay: true`): Graph reports them midnight to
//!   midnight in the event's own time zone. With the UTC preference the
//!   wall-clock times may be shifted by that zone's offset (e.g.
//!   `2026-11-15T23:00` for an all-day event on 16 Nov in UTC+1), so each
//!   bound is rounded to the *nearest* midnight: the date of `dateTime + 12h`.
//!   That is exact for every real offset (all are below 12 h, except +13/+14
//!   which only Pacific islands use). `end` is exclusive, as in the domain; a
//!   degenerate `end <= start` becomes a one-day event.
//! * `responseStatus.response`: `organizer`, `accepted`,
//!   `tentativelyAccepted`, `declined`, `notResponded`, `none` map to the
//!   matching [`ResponseStatus`]; `none`, a missing value or an unknown value
//!   map to [`ResponseStatus::Unknown`]. Declined events are kept.
//! * `subject` (missing or `null` gives `""`), `location.displayName`
//!   (empty gives `None`).
//!
//! # Errors
//!
//! * HTTP 401: [`GraphAuth::invalidate`] with the rejected token, then one
//!   retry with a fresh token; a second 401 is [`Error::Auth`].
//! * HTTP 403 (e.g. consent missing for `Calendars.Read`): [`Error::Auth`].
//! * HTTP 404: [`Error::NotFound`] (e.g. a deleted calendar).
//! * HTTP 429 and 503 with a `Retry-After` of at most
//!   [`GraphCalendarProvider::DEFAULT_MAX_RETRY_WAIT`] seconds: wait and
//!   retry once. Otherwise, and for every other 5xx: [`Error::Network`]
//!   (retried by the sync engine with backoff).
//! * Other statuses and malformed JSON: [`Error::Protocol`].
//! * Transport failures: [`Error::Network`] (from the [`HttpClient`]).
//!
//! Error messages contain only status codes and Graph error codes (e.g.
//! `ErrorAccessDenied`), never tokens, Graph error messages or event data.
//! `@odata.nextLink` is followed only when it points at the Graph host, so
//! the access token is never sent anywhere else.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Deserialize;

use super::auth::GraphAuth;
use super::http::{HttpClient, HttpResponse};
use crate::domain::{
    AccountId, CalendarId, CalendarInfo, CalendarProvider, Error, Event, EventId, EventTime,
    ResponseStatus, Result,
};

/// Base URL of the Graph v1.0 API. `@odata.nextLink` must start with it.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Page size requested from `calendarView`.
const EVENTS_PAGE_SIZE: u32 = 100;
/// Page size requested from `/me/calendars`.
const CALENDARS_PAGE_SIZE: u32 = 100;
/// Upper bound on pages followed for one listing, against a server that
/// keeps returning `@odata.nextLink`.
const MAX_PAGES: usize = 200;

const EVENT_FIELDS: &str = "id,subject,start,end,isAllDay,location,responseStatus,isCancelled";
const CALENDAR_FIELDS: &str = "id,name,hexColor";
const PREFER_UTC: &str = "outlook.timezone=\"UTC\"";

/// Read-only calendar access for one Microsoft Graph account.
pub struct GraphCalendarProvider {
    auth: Arc<GraphAuth>,
    http: Arc<dyn HttpClient>,
    max_retry_wait: Duration,
}

impl std::fmt::Debug for GraphCalendarProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphCalendarProvider")
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl GraphCalendarProvider {
    /// Longest `Retry-After` (on 429/503) the provider waits for before
    /// retrying once. Longer waits are left to the sync engine's backoff.
    pub const DEFAULT_MAX_RETRY_WAIT: Duration = Duration::from_secs(10);

    /// Creates a provider. `auth` is shared with the account's sign-in
    /// command (see [`GraphProviderFactory`](super::factory::GraphProviderFactory)).
    pub fn new(auth: Arc<GraphAuth>, http: Arc<dyn HttpClient>) -> Self {
        Self {
            auth,
            http,
            max_retry_wait: Self::DEFAULT_MAX_RETRY_WAIT,
        }
    }

    /// Changes the longest `Retry-After` honoured (tests use zero).
    pub fn with_max_retry_wait(mut self, max: Duration) -> Self {
        self.max_retry_wait = max;
        self
    }

    /// The token source of this provider.
    pub fn auth(&self) -> &Arc<GraphAuth> {
        &self.auth
    }

    /// Sends an authorized `GET` and returns the 2xx response, handling 401
    /// (one token refresh) and 429/503 (one wait-and-retry).
    fn get(&self, url: &str) -> Result<HttpResponse> {
        let mut auth_retried = false;
        let mut throttle_retried = false;
        loop {
            let token = self.auth.access_token()?;
            let bearer = format!("Bearer {}", token.expose());
            let response = self.http.get(
                url,
                &[
                    ("Authorization", &bearer),
                    ("Accept", "application/json"),
                    ("Prefer", PREFER_UTC),
                ],
            )?;
            match response.status {
                200..=299 => return Ok(response),
                401 if !auth_retried => {
                    auth_retried = true;
                    self.auth.invalidate(&token);
                }
                429 | 503 if !throttle_retried => match self.retry_wait(&response) {
                    Some(wait) => {
                        throttle_retried = true;
                        if !wait.is_zero() {
                            std::thread::sleep(wait);
                        }
                    }
                    None => return Err(status_error(&response)),
                },
                _ => return Err(status_error(&response)),
            }
        }
    }

    /// The `Retry-After` delay if it is given in seconds and within the cap.
    fn retry_wait(&self, response: &HttpResponse) -> Option<Duration> {
        let secs: u64 = response.header("retry-after")?.trim().parse().ok()?;
        let wait = Duration::from_secs(secs);
        (wait <= self.max_retry_wait).then_some(wait)
    }

    /// Fetches all pages of a collection starting at `url`.
    fn get_all<T: for<'de> Deserialize<'de>>(&self, url: String) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut next = Some(url);
        let mut pages = 0;
        while let Some(url) = next.take() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::protocol(format!(
                    "Microsoft Graph returned more than {MAX_PAGES} pages"
                )));
            }
            let response = self.get(&url)?;
            let page: Page<T> = serde_json::from_slice(&response.body).map_err(parse_error)?;
            items.extend(page.value);
            if let Some(link) = page.next_link {
                if !link.starts_with(&format!("{GRAPH_BASE}/")) {
                    return Err(Error::protocol(
                        "Microsoft Graph returned a next page link to another host",
                    ));
                }
                next = Some(link);
            }
        }
        Ok(items)
    }

    fn fetch_calendar(
        &self,
        calendar: &CalendarId,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        let url = format!(
            "{GRAPH_BASE}/me/calendars/{}/calendarView?{}",
            encode_path_segment(calendar.as_str()),
            query(&[
                ("startDateTime", &graph_instant(from)),
                ("endDateTime", &graph_instant(to)),
                ("$select", EVENT_FIELDS),
                ("$top", &EVENTS_PAGE_SIZE.to_string()),
            ])
        );
        let items: Vec<GraphEvent> = self.get_all(url)?;
        let mut events = Vec::with_capacity(items.len());
        for item in items {
            if let Some(event) = map_event(item, self.account_id(), calendar)? {
                events.push(event);
            }
        }
        Ok(events)
    }
}

impl CalendarProvider for GraphCalendarProvider {
    fn account_id(&self) -> &AccountId {
        self.auth.account_id()
    }

    fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        let url = format!(
            "{GRAPH_BASE}/me/calendars?{}",
            query(&[
                ("$select", CALENDAR_FIELDS),
                ("$top", &CALENDARS_PAGE_SIZE.to_string()),
            ])
        );
        let calendars: Vec<GraphCalendar> = self.get_all(url)?;
        Ok(calendars
            .into_iter()
            .map(|c| CalendarInfo {
                id: CalendarId::new(c.id),
                account_id: self.account_id().clone(),
                name: c.name.unwrap_or_default(),
                color: c.hex_color.as_deref().and_then(normalize_hex_color),
            })
            .collect())
    }

    fn fetch_events(
        &self,
        calendar_ids: &[CalendarId],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        for calendar in calendar_ids {
            events.extend(self.fetch_calendar(calendar, from, to)?);
        }
        Ok(events)
    }

    fn test_connection(&self) -> Result<()> {
        let url = format!(
            "{GRAPH_BASE}/me/calendars?{}",
            query(&[("$select", "id"), ("$top", "1")])
        );
        let response = self.get(&url)?;
        serde_json::from_slice::<Page<serde_json::Value>>(&response.body)
            .map(|_| ())
            .map_err(parse_error)
    }
}

// ---------------------------------------------------------------------------
// Graph JSON
// ---------------------------------------------------------------------------

/// One page of an OData collection.
#[derive(Deserialize)]
struct Page<T> {
    value: Vec<T>,
    #[serde(rename = "@odata.nextLink")]
    next_link: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphCalendar {
    id: String,
    name: Option<String>,
    hex_color: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphEvent {
    id: String,
    subject: Option<String>,
    start: GraphDateTime,
    end: GraphDateTime,
    #[serde(default)]
    is_all_day: bool,
    #[serde(default)]
    is_cancelled: bool,
    location: Option<GraphLocation>,
    response_status: Option<GraphResponseStatus>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphDateTime {
    date_time: String,
    time_zone: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphLocation {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct GraphResponseStatus {
    response: Option<String>,
}

/// Graph's error body: `{"error": {"code": "...", "message": "..."}}`. Only
/// the code is used; the message can contain personal data.
#[derive(Deserialize)]
struct GraphErrorBody {
    error: GraphErrorDetail,
}

#[derive(Deserialize)]
struct GraphErrorDetail {
    code: Option<String>,
}

// ---------------------------------------------------------------------------
// Mapping
// ---------------------------------------------------------------------------

/// Maps one `calendarView` item, or `None` for a cancelled one.
fn map_event(
    item: GraphEvent,
    account: &AccountId,
    calendar: &CalendarId,
) -> Result<Option<Event>> {
    if item.is_cancelled {
        return Ok(None);
    }
    let time = if item.is_all_day {
        let start = nearest_date(&item.start)?;
        let end = nearest_date(&item.end)?;
        let end = if end > start {
            end
        } else {
            start.succ_opt().unwrap_or(start)
        };
        EventTime::AllDay { start, end }
    } else {
        let start = utc_instant(&item.start)?;
        let end = utc_instant(&item.end)?.max(start);
        EventTime::Timed { start, end }
    };
    let location = item
        .location
        .and_then(|l| l.display_name)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    let response = map_response(
        item.response_status
            .as_ref()
            .and_then(|r| r.response.as_deref()),
    );
    Ok(Some(Event {
        id: EventId::new(item.id),
        account_id: account.clone(),
        calendar_id: calendar.clone(),
        title: item.subject.unwrap_or_default(),
        time,
        location,
        response,
    }))
}

fn map_response(response: Option<&str>) -> ResponseStatus {
    match response {
        Some("organizer") => ResponseStatus::Organizer,
        Some("accepted") => ResponseStatus::Accepted,
        Some("tentativelyAccepted") => ResponseStatus::Tentative,
        Some("declined") => ResponseStatus::Declined,
        Some("notResponded") => ResponseStatus::NotResponded,
        _ => ResponseStatus::Unknown,
    }
}

/// Parses Graph's `dateTime` (`2026-11-16T09:00:00.0000000`, no offset).
fn wall_clock(value: &GraphDateTime) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value.date_time.trim(), "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|_| Error::protocol("Microsoft Graph returned an invalid event dateTime"))
}

/// A timed event bound as an instant. The request asks for UTC; anything
/// else means the `Prefer` header was ignored.
fn utc_instant(value: &GraphDateTime) -> Result<DateTime<Utc>> {
    let zone = value.time_zone.as_deref().unwrap_or("UTC");
    if !matches!(
        zone,
        "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" | "Coordinated Universal Time"
    ) {
        return Err(Error::protocol(
            "Microsoft Graph returned an event time that is not in UTC",
        ));
    }
    Ok(wall_clock(value)?.and_utc())
}

/// An all-day event bound as a date: the nearest midnight (see module docs).
fn nearest_date(value: &GraphDateTime) -> Result<NaiveDate> {
    Ok((wall_clock(value)? + chrono::Duration::hours(12)).date())
}

/// `#RRGGBB` (Graph's `hexColor`) to lower-case `#rrggbb`; empty or invalid
/// values (Graph sends `""` for calendars without a custom color) give `None`.
fn normalize_hex_color(value: &str) -> Option<String> {
    let hex = value.trim().strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

/// The error for an unparseable 2xx body. serde_json's own message can quote
/// the offending value (e.g. `invalid type: string "..."`), which may be
/// event data, so only the category and position are kept.
fn parse_error(err: serde_json::Error) -> Error {
    Error::protocol(format!(
        "malformed Microsoft Graph response ({:?} error at line {}, column {})",
        err.classify(),
        err.line(),
        err.column()
    ))
}

/// The error for a non-2xx status, with Graph's error code if it sent one.
fn status_error(response: &HttpResponse) -> Error {
    let status = response.status;
    let code = serde_json::from_slice::<GraphErrorBody>(&response.body)
        .ok()
        .and_then(|b| b.error.code)
        .filter(|c| c.len() <= 64 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
    let detail = match code {
        Some(code) => format!("Microsoft Graph returned HTTP {status} ({code})"),
        None => format!("Microsoft Graph returned HTTP {status}"),
    };
    match status {
        401 => Error::auth(format!("{detail}; sign in again from Settings")),
        403 => Error::auth(format!(
            "{detail}; check that Calendars.Read is consented (docs/graph-setup.md)"
        )),
        404 => Error::not_found(detail),
        429 | 500..=599 => Error::network(detail),
        _ => Error::protocol(detail),
    }
}

// ---------------------------------------------------------------------------
// URLs
// ---------------------------------------------------------------------------

/// An instant as Graph expects in `startDateTime`/`endDateTime`.
fn graph_instant(instant: DateTime<Utc>) -> String {
    instant.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A URL query string, percent-encoded.
fn query(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// Percent-encodes one path segment (Graph ids can contain `/`, `+`, `=`).
fn encode_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests;
