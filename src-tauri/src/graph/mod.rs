//! Microsoft Graph (spec 6.2).
//!
//! - [`auth`]: OAuth 2.0 authorization code flow with PKCE and a loopback
//!   redirect, token refresh, and [`GraphAuth`](auth::GraphAuth), the token
//!   source the Graph provider calls.
//! - [`http`]: the small blocking HTTP abstraction ([`HttpClient`](http::HttpClient))
//!   and its `ureq` implementation, so tests can inject canned responses.
//! - [`provider`][]: [`GraphCalendarProvider`](provider::GraphCalendarProvider),
//!   the `CalendarProvider` over `/me/calendars` and `calendarView`.
//! - [`factory`][]: [`GraphProviderFactory`](factory::GraphProviderFactory),
//!   which shares one `GraphAuth` per account between the sync engine and the
//!   sign-in commands, and [`CombinedProviderFactory`](factory::CombinedProviderFactory).

pub mod auth;
pub mod factory;
pub mod http;
mod loopback;
pub mod provider;
