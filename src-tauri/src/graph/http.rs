//! A minimal blocking HTTP abstraction for the Graph code.
//!
//! [`HttpClient`] is what the auth code (and later the Graph provider) talks
//! to; [`UreqHttpClient`] is the real implementation. Tests implement the
//! trait with canned responses, so no test touches the network.

use std::time::Duration;

use crate::domain::{Error, Result};

/// Largest response body read, in bytes. Token responses are a few KB and a
/// Graph `calendarView` page is well under this.
pub const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// A response with any status code. Transport failures are an `Err` instead.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body. May contain tokens: never log it.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The body can hold tokens, so it is never printed.
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// Blocking HTTP client.
///
/// Implementations return `Ok` for every HTTP status (including 4xx and 5xx)
/// and [`Error::Network`] for transport failures (DNS, connect, TLS, timeout,
/// body too large or unreadable). Error messages must not contain request or
/// response bodies.
pub trait HttpClient: Send + Sync {
    /// Sends `POST url` with an `application/x-www-form-urlencoded` body.
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<HttpResponse>;
}

/// [`HttpClient`] over `ureq`, with TLS from the OS (SChannel on Windows,
/// Security.framework on macOS) and the OS root certificate store.
#[derive(Debug, Clone)]
pub struct UreqHttpClient {
    agent: ureq::Agent,
}

impl UreqHttpClient {
    /// Overall timeout for one request, including reading the body.
    pub const TIMEOUT: Duration = Duration::from_secs(30);

    /// Creates a client.
    pub fn new() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(Self::TIMEOUT))
            // Status codes are interpreted by the caller (OAuth errors come
            // back as 400 with a JSON body).
            .http_status_as_error(false)
            .https_only(true)
            .build()
            .new_agent();
        Self { agent }
    }
}

impl Default for UreqHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for UreqHttpClient {
    fn post_form(&self, url: &str, form: &[(&str, &str)]) -> Result<HttpResponse> {
        let mut response = self
            .agent
            .post(url)
            .send_form(form.iter().copied())
            .map_err(transport_error)?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(transport_error)?;
        Ok(HttpResponse { status, body })
    }
}

/// Maps a ureq error to [`Error::Network`]. ureq's messages describe the
/// failure (e.g. "timeout: global", "io: connection refused") and never
/// include bodies or form values.
fn transport_error(err: ureq::Error) -> Error {
    Error::network(format!("request failed: {err}"))
}
