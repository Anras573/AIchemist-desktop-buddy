//! The HTTP layer, behind the small [`Transport`] trait so tests can inject
//! canned responses.
//!
//! [`UreqTransport`] is the real one: `ureq` (blocking, no async runtime)
//! with `native-tls`, i.e. SChannel on Windows and Security.framework on
//! macOS, which validate certificates against the OS trust store. It never
//! follows redirects itself; [`super::client`] does that, so it controls
//! where credentials go.

use std::time::Duration;

use base64::Engine as _;
use ureq::http;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use url::Url;

use crate::domain::{Error, Result, SecretString};

/// Connection (TCP + TLS handshake) timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Timeout for sending the request, waiting for the response head, and
/// receiving the response body (each).
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest response body accepted. A week of events is far smaller.
pub const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

/// One HTTP request. `authorization` is the full header value; it must
/// never be logged.
pub struct HttpRequest<'a> {
    /// `PROPFIND` or `REPORT`.
    pub method: &'a str,
    /// Target URL (always `https`).
    pub url: &'a Url,
    /// The WebDAV `Depth` header, if any.
    pub depth: Option<&'a str>,
    /// XML request body.
    pub body: &'a str,
    /// The `Authorization` header value.
    pub authorization: Option<&'a str>,
}

impl std::fmt::Debug for HttpRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No URL (it may name the user) and no credentials.
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

/// An HTTP response. Any status, including 3xx/4xx/5xx, is an `Ok` response.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// Status code.
    pub status: u16,
    /// The `Location` header, for redirects.
    pub location: Option<String>,
    /// The body, decoded as UTF-8.
    pub body: String,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The body holds event contents.
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// Sends one HTTP request without following redirects.
///
/// Implementations return [`Error::Network`] when the server cannot be
/// reached, the connection or TLS handshake fails, or a timeout expires.
/// Error messages contain no URLs or credentials.
pub trait Transport: Send + Sync {
    /// Sends `request` and returns the response, whatever its status.
    fn send(&self, request: &HttpRequest<'_>) -> Result<HttpResponse>;
}

/// The `Authorization: Basic ...` header value.
pub(crate) fn basic_auth(username: &str, password: &SecretString) -> String {
    let raw = format!("{username}:{}", password.expose());
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

/// [`Transport`] over `ureq` with the OS TLS stack and trust store.
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl std::fmt::Debug for UreqTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UreqTransport").finish_non_exhaustive()
    }
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    /// Creates the transport. Cheap: TLS is set up on first use. Share one
    /// instance between providers so they share the connection pool.
    pub fn new() -> Self {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .tls_config(tls)
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(0)
            .allow_non_standard_methods(true)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_send_request(Some(READ_TIMEOUT))
            .timeout_send_body(Some(READ_TIMEOUT))
            .timeout_recv_response(Some(READ_TIMEOUT))
            .timeout_recv_body(Some(READ_TIMEOUT))
            .user_agent(concat!(
                "AIchemist-Desktop-Buddy/",
                env!("CARGO_PKG_VERSION")
            ))
            .build();
        Self {
            agent: config.new_agent(),
        }
    }
}

impl Transport for UreqTransport {
    fn send(&self, request: &HttpRequest<'_>) -> Result<HttpResponse> {
        let method = http::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| Error::internal("invalid HTTP method"))?;
        let mut builder = http::Request::builder()
            .method(method)
            .uri(request.url.as_str())
            .header("Content-Type", "application/xml; charset=utf-8");
        if let Some(depth) = request.depth {
            builder = builder.header("Depth", depth);
        }
        if let Some(auth) = request.authorization {
            builder = builder.header("Authorization", auth);
        }
        let http_request = builder
            .body(request.body)
            .map_err(|_| Error::protocol("could not build the HTTP request"))?;

        let mut response = self.agent.run(http_request).map_err(map_ureq_error)?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_string()
            .map_err(map_ureq_error)?;
        Ok(HttpResponse {
            status,
            location,
            body,
        })
    }
}

/// Maps a `ureq` error to a domain error. Messages are fixed strings or
/// error kinds, never URLs (`ureq` may put the URL in its own messages).
fn map_ureq_error(err: ureq::Error) -> Error {
    match err {
        ureq::Error::Timeout(_) => Error::network("the request timed out"),
        ureq::Error::HostNotFound => Error::network("the server's host name was not found"),
        ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            Error::protocol("the response is not valid UTF-8")
        }
        ureq::Error::Io(e) => Error::network(format!("connection failed ({})", e.kind())),
        ureq::Error::ConnectionFailed => Error::network("the connection failed"),
        ureq::Error::Tls(_) | ureq::Error::NativeTls(_) | ureq::Error::Der(_) => {
            Error::network("the TLS connection failed (certificate not trusted?)")
        }
        ureq::Error::BodyExceedsLimit(_) => Error::protocol("the response is too large"),
        ureq::Error::Http(_)
        | ureq::Error::BadUri(_)
        | ureq::Error::RequireHttpsOnly(_)
        | ureq::Error::LargeResponseHeader(..) => {
            Error::protocol("the request or response was invalid")
        }
        _ => Error::network("the HTTP exchange failed"),
    }
}
