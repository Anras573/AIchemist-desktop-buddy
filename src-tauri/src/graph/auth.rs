//! Microsoft Graph sign-in and tokens (spec 6.2, 9).
//!
//! # Flow
//!
//! [`GraphAuth::sign_in`] runs the OAuth 2.0 authorization code flow with
//! PKCE (RFC 7636, `S256`) for a public client, with a loopback redirect
//! (RFC 8252):
//!
//! 1. Bind `127.0.0.1` on an ephemeral port. The redirect URI is
//!    `http://localhost:<port>`; Entra ID ignores the port when matching a
//!    `localhost` redirect URI, so the app registration only lists
//!    `http://localhost` (see `docs/graph-setup.md`).
//! 2. Generate `state` and the PKCE verifier from OS randomness, open the
//!    system browser at `https://login.microsoftonline.com/<tenant>/oauth2/v2.0/authorize`.
//! 3. Wait (default [`GraphAuth::DEFAULT_SIGN_IN_TIMEOUT`], cancellable with
//!    a [`CancelToken`]) for the redirect, check `state`, serve a "you can
//!    close this tab" page.
//! 4. Redeem the code at the token endpoint, store the refresh token in the
//!    [`SecretStore`] ([`SecretKind::GraphRefreshToken`]), keep the access
//!    token in memory only.
//!
//! [`GraphAuth::access_token`] returns the cached access token, refreshing it
//! when it expires within [`GraphAuth::REFRESH_MARGIN`]. Rotated refresh
//! tokens are persisted. On HTTP 401 from Graph, call
//! [`GraphAuth::invalidate`] with the rejected token and ask again.
//!
//! # Scopes
//!
//! `https://graph.microsoft.com/Calendars.Read` (read-only calendars) and
//! `offline_access` (refresh token) as the spec requires, plus `openid
//! profile` on the interactive sign-in only. The latter two grant no Graph
//! API access and need no admin consent; they make the token endpoint return
//! an ID token with the user's name and username, which is shown in Settings
//! so the user can tell which Microsoft account an entry is signed in to.
//! Reading the profile from Graph instead would need `User.Read`.
//!
//! # Errors
//!
//! - Transport failures, HTTP 5xx/429, `temporarily_unavailable`: [`Error::Network`].
//! - `invalid_grant` (expired/revoked refresh token, consent withdrawn),
//!   `interaction_required`, a missing refresh token, any other OAuth error,
//!   a cancelled or timed-out sign-in, a `state` mismatch: [`Error::Auth`]
//!   (the user must sign in again or fix the app registration).
//! - Malformed responses: [`Error::Protocol`].
//! - Keychain failures: [`Error::Keychain`].
//!
//! Tokens, codes and verifiers are only held as [`SecretString`] and never
//! logged or put in error messages. Error messages carry the OAuth error code
//! and the `AADSTS` number, never `error_description` (it can contain the
//! user's name).
//!
//! # Threading
//!
//! Everything blocks (network, keychain, waiting for the browser). Call from
//! a worker thread or `spawn_blocking`, never on the main thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::http::{HttpClient, HttpResponse};
use super::loopback::LoopbackServer;
use crate::domain::{
    AccountConfig, AccountConnection, AccountId, Clock, Error, Result, SecretString,
};
use crate::secrets::{SecretKind, SecretStore};

/// The Microsoft identity platform host.
pub const AUTHORITY_HOST: &str = "https://login.microsoftonline.com";
/// Scope of the Graph API permission (read-only calendars).
pub const GRAPH_CALENDARS_READ: &str = "https://graph.microsoft.com/Calendars.Read";

/// Scopes requested on the interactive sign-in.
const SIGN_IN_SCOPES: &str =
    "openid profile offline_access https://graph.microsoft.com/Calendars.Read";
/// Scopes requested on refresh: no `openid`, no ID token needed.
const REFRESH_SCOPES: &str = "offline_access https://graph.microsoft.com/Calendars.Read";

/// base64url without padding (RFC 7636 appendix A); decoding also accepts
/// padding.
const B64URL: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    base64::engine::GeneralPurposeConfig::new()
        .with_encode_padding(false)
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
);

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Who signed in, from the ID token of the interactive sign-in. Shown in
/// Settings. Personal data: do not log it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedInIdentity {
    /// The user's display name (`name` claim), if present.
    pub display_name: Option<String>,
    /// The sign-in name, usually the email address (`preferred_username`
    /// claim), if present.
    pub username: Option<String>,
}

/// Cancels an interactive [`GraphAuth::sign_in`] from another thread.
/// Clones share the flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// Creates a token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels. A running sign-in fails with [`Error::Auth`] within about
    /// 100 ms.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether [`CancelToken::cancel`] was called.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Opens a URL in the user's browser.
pub trait BrowserOpener: Send + Sync {
    /// Opens `url` (always an `https://login.microsoftonline.com/...` URL).
    fn open(&self, url: &str) -> Result<()>;
}

/// [`BrowserOpener`] that starts the system default browser with the OS
/// launcher (`rundll32 url.dll,FileProtocolHandler` on Windows, `open` on
/// macOS, `xdg-open` elsewhere). No shell is involved, so the URL is never
/// interpreted by `cmd.exe` or `sh`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemBrowser;

impl BrowserOpener for SystemBrowser {
    fn open(&self, url: &str) -> Result<()> {
        if !url.starts_with(AUTHORITY_HOST) {
            return Err(Error::internal(
                "refusing to open a non-Microsoft sign-in URL",
            ));
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
        match command.status() {
            Ok(status) if status.success() || cfg!(windows) => Ok(()),
            // rundll32 reports no meaningful exit code; elsewhere a failure
            // means no browser could be started.
            Ok(status) => Err(Error::internal(format!(
                "could not open the browser for sign-in ({status})"
            ))),
            Err(e) => Err(Error::internal(format!(
                "could not open the browser for sign-in: {e}"
            ))),
        }
    }
}

/// Signs a Graph account in and hands out access tokens for it.
///
/// One instance per Graph account; share it as `Arc<GraphAuth>` between the
/// provider (which calls [`GraphAuth::access_token`]) and the
/// `sign_in_account` command. All methods block.
pub struct GraphAuth {
    account: AccountId,
    client_id: String,
    tenant: String,
    secrets: Arc<dyn SecretStore>,
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
    sign_in_timeout: Duration,
    /// The in-memory access token. The lock is held across a refresh, so
    /// concurrent callers wait for one refresh instead of each rotating the
    /// refresh token.
    cache: Mutex<Option<CachedToken>>,
}

impl std::fmt::Debug for GraphAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphAuth")
            .field("account", &self.account)
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}

struct CachedToken {
    token: SecretString,
    expires_at: DateTime<Utc>,
}

impl GraphAuth {
    /// Refresh the access token when it expires within this margin.
    pub const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
    /// How long [`GraphAuth::sign_in`] waits for the browser by default.
    pub const DEFAULT_SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

    /// Creates the token source for a Graph account.
    ///
    /// Fails with [`Error::InvalidInput`] for a CalDAV account or an invalid
    /// client id or tenant (see [`validate_client_id`], [`validate_tenant`]).
    pub fn new(
        account: &AccountConfig,
        secrets: Arc<dyn SecretStore>,
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let AccountConnection::Graph { client_id, tenant } = &account.connection else {
            return Err(Error::invalid_input("not a Microsoft Graph account"));
        };
        Ok(Self {
            account: account.id.clone(),
            client_id: validate_client_id(client_id)?,
            tenant: validate_tenant(tenant)?,
            secrets,
            http,
            clock,
            sign_in_timeout: Self::DEFAULT_SIGN_IN_TIMEOUT,
            cache: Mutex::new(None),
        })
    }

    /// Changes how long [`GraphAuth::sign_in`] waits for the browser.
    pub fn with_sign_in_timeout(mut self, timeout: Duration) -> Self {
        self.sign_in_timeout = timeout;
        self
    }

    /// The account this instance serves.
    pub fn account_id(&self) -> &AccountId {
        &self.account
    }

    /// Runs the interactive sign-in in the system browser (via `browser`) and
    /// stores the refresh token. Blocks until the user finishes, the timeout
    /// passes or `cancel` is cancelled. Replaces any previous sign-in of this
    /// account.
    pub fn sign_in(
        &self,
        browser: &dyn BrowserOpener,
        cancel: &CancelToken,
    ) -> Result<SignedInIdentity> {
        let server = LoopbackServer::bind()?;
        let redirect_uri = server.redirect_uri();
        let pkce = Pkce::generate()?;
        let state = random_b64url(16)?;
        let url = authorize_url(
            &self.tenant,
            &self.client_id,
            &redirect_uri,
            &state,
            &pkce.challenge,
        );
        browser.open(&url)?;
        let code = server.wait_for_code(&state, self.sign_in_timeout, cancel)?;

        let tokens = self.post_token(&[
            ("client_id", &self.client_id),
            ("grant_type", "authorization_code"),
            ("code", code.expose()),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", pkce.verifier.expose()),
            ("scope", SIGN_IN_SCOPES),
        ])?;
        let refresh_token = tokens.refresh_token.ok_or_else(|| {
            Error::auth(
                "Microsoft returned no refresh token; check that offline_access is allowed \
                 (docs/graph-setup.md)",
            )
        })?;
        self.secrets
            .set(&self.account, SecretKind::GraphRefreshToken, &refresh_token)?;
        let identity = tokens
            .id_token
            .as_ref()
            .map(|t| identity_from_id_token(t.expose()))
            .unwrap_or_default();
        *self.lock_cache() = Some(self.cached(tokens.access_token, tokens.expires_in));
        Ok(identity)
    }

    /// Signs out: forgets the access token and deletes the refresh token from
    /// the keychain. Succeeds if not signed in. Does not revoke the session
    /// at Microsoft (public clients cannot); the user can do that at
    /// <https://myaccount.microsoft.com>.
    pub fn sign_out(&self) -> Result<()> {
        *self.lock_cache() = None;
        self.secrets
            .delete(&self.account, SecretKind::GraphRefreshToken)
    }

    /// Returns a valid access token, refreshing it first if it expires within
    /// [`GraphAuth::REFRESH_MARGIN`]. Fails with [`Error::Auth`] if the
    /// account is not signed in or the refresh token was rejected.
    pub fn access_token(&self) -> Result<SecretString> {
        let mut cache = self.lock_cache();
        let now = self.clock.now();
        if let Some(cached) = cache.as_ref() {
            if now + margin() < cached.expires_at {
                return Ok(cached.token.clone());
            }
        }
        *cache = None;
        let refresh_token = self
            .secrets
            .get(&self.account, SecretKind::GraphRefreshToken)?
            .ok_or_else(|| Error::auth("not signed in to Microsoft; sign in from Settings"))?;
        let tokens = self.post_token(&[
            ("client_id", &self.client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
            ("scope", REFRESH_SCOPES),
        ])?;
        if let Some(rotated) = tokens.refresh_token.filter(|t| *t != refresh_token) {
            // If the keychain write fails, keep going: the access token is
            // good and the previous refresh token stays valid until its own
            // expiry, so the next refresh still works.
            if let Err(err) =
                self.secrets
                    .set(&self.account, SecretKind::GraphRefreshToken, &rotated)
            {
                log::warn!("could not store the rotated Graph refresh token: {err}");
            }
        }
        let cached = self.cached(tokens.access_token, tokens.expires_in);
        let token = cached.token.clone();
        *cache = Some(cached);
        Ok(token)
    }

    /// Forgets the cached access token if it is `rejected`. Call when Graph
    /// answers 401 with that token, then call [`GraphAuth::access_token`]
    /// again (once) to get a refreshed one. Comparing with `rejected` means
    /// that when several requests fail at once, only one refresh happens.
    pub fn invalidate(&self, rejected: &SecretString) {
        let mut cache = self.lock_cache();
        if cache.as_ref().is_some_and(|c| c.token == *rejected) {
            *cache = None;
        }
    }

    fn cached(&self, token: SecretString, expires_in: u64) -> CachedToken {
        let lifetime =
            chrono::Duration::seconds(i64::try_from(expires_in).unwrap_or(i64::MAX).min(86_400));
        CachedToken {
            token,
            expires_at: self.clock.now() + lifetime,
        }
    }

    fn token_url(&self) -> String {
        format!("{AUTHORITY_HOST}/{}/oauth2/v2.0/token", self.tenant)
    }

    fn post_token(&self, form: &[(&str, &str)]) -> Result<TokenResponse> {
        let response = self.http.post_form(&self.token_url(), form)?;
        parse_token_response(&response)
    }

    fn lock_cache(&self) -> MutexGuard<'_, Option<CachedToken>> {
        // Every write is a single assignment, so a poisoned lock holds a
        // consistent value.
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn margin() -> chrono::Duration {
    chrono::Duration::from_std(GraphAuth::REFRESH_MARGIN).expect("small duration")
}

/// Checks an application (client) id: a GUID. Returns it trimmed.
pub fn validate_client_id(client_id: &str) -> Result<String> {
    let client_id = client_id.trim();
    if is_guid(client_id) {
        Ok(client_id.to_owned())
    } else {
        Err(Error::invalid_input(
            "the application (client) id must be a GUID like 00000000-0000-0000-0000-000000000000",
        ))
    }
}

/// Checks a tenant: `common`, `organizations`, `consumers`, a tenant id
/// (GUID) or a verified domain such as `contoso.onmicrosoft.com`. Returns it
/// trimmed and lowercased (it becomes a URL path segment).
pub fn validate_tenant(tenant: &str) -> Result<String> {
    let tenant = tenant.trim().to_ascii_lowercase();
    let is_domain = tenant.len() <= 253
        && tenant.contains('.')
        && tenant.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    if matches!(tenant.as_str(), "common" | "organizations" | "consumers")
        || is_guid(&tenant)
        || is_domain
    {
        Ok(tenant)
    } else {
        Err(Error::invalid_input(
            "the tenant must be common, organizations, consumers, a directory (tenant) id or a \
             domain like contoso.onmicrosoft.com",
        ))
    }
}

fn is_guid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// PKCE, state and the authorize URL
// ---------------------------------------------------------------------------

/// A PKCE verifier and its `S256` challenge (RFC 7636 section 4).
struct Pkce {
    verifier: SecretString,
    challenge: String,
}

impl Pkce {
    /// 32 random bytes, base64url: a 43-character verifier (the RFC's
    /// recommended form, section 4.1).
    fn generate() -> Result<Self> {
        let verifier = random_b64url(32)?;
        let challenge = s256_challenge(&verifier);
        Ok(Self {
            verifier: SecretString::new(verifier),
            challenge,
        })
    }
}

/// `BASE64URL(SHA256(ASCII(verifier)))` without padding.
fn s256_challenge(verifier: &str) -> String {
    B64URL.encode(Sha256::digest(verifier.as_bytes()))
}

/// `len` bytes from the OS CSPRNG, base64url-encoded without padding.
fn random_b64url(len: usize) -> Result<String> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::internal(format!("OS random number generator failed: {e}")))?;
    Ok(B64URL.encode(bytes))
}

fn authorize_url(
    tenant: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_mode", "query")
        .append_pair("scope", SIGN_IN_SCOPES)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        // Let the user pick which Microsoft account to connect instead of
        // silently reusing the browser's session.
        .append_pair("prompt", "select_account")
        .finish();
    format!("{AUTHORITY_HOST}/{tenant}/oauth2/v2.0/authorize?{query}")
}

// ---------------------------------------------------------------------------
// Token endpoint responses
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: SecretString,
    #[serde(deserialize_with = "number_or_string")]
    expires_in: u64,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    refresh_token: Option<SecretString>,
    #[serde(default)]
    id_token: Option<SecretString>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
    #[serde(default)]
    error_codes: Vec<u64>,
}

/// `expires_in` is a number in the v2 endpoint, but accept a numeric string
/// as older endpoints sent.
fn number_or_string<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Num(u64),
        Str(String),
    }
    match NumOrStr::deserialize(d)? {
        NumOrStr::Num(n) => Ok(n),
        NumOrStr::Str(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

fn parse_token_response(response: &HttpResponse) -> Result<TokenResponse> {
    let status = response.status;
    if (200..300).contains(&status) {
        // serde_json errors name the position, never the input text.
        let tokens: TokenResponse = serde_json::from_slice(&response.body)
            .map_err(|e| Error::protocol(format!("malformed token response: {e}")))?;
        if tokens
            .token_type
            .as_deref()
            .is_some_and(|t| !t.eq_ignore_ascii_case("bearer"))
        {
            return Err(Error::protocol("token response is not a bearer token"));
        }
        if tokens.access_token.expose().is_empty() {
            return Err(Error::protocol("token response has an empty access token"));
        }
        return Ok(tokens);
    }
    if status == 429 || status >= 500 {
        return Err(Error::network(format!(
            "Microsoft sign-in service returned HTTP {status}"
        )));
    }
    let Ok(err) = serde_json::from_slice::<ErrorResponse>(&response.body) else {
        return Err(Error::protocol(format!(
            "unexpected HTTP {status} from the Microsoft token endpoint"
        )));
    };
    Err(oauth_error(&err))
}

fn oauth_error(err: &ErrorResponse) -> Error {
    let code: String = err
        .error
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    let detail = match err.error_codes.first() {
        Some(n) => format!("{code}, AADSTS{n}"),
        None => code.clone(),
    };
    match code.as_str() {
        "invalid_grant" | "interaction_required" | "consent_required" | "login_required" => {
            Error::auth(format!(
                "Microsoft sign-in expired or was revoked ({detail}); sign in again"
            ))
        }
        "temporarily_unavailable" | "server_error" => Error::network(format!(
            "Microsoft sign-in is temporarily unavailable ({detail})"
        )),
        _ => Error::auth(format!(
            "Microsoft rejected the sign-in ({detail}); check the app registration \
             (docs/graph-setup.md) and sign in again"
        )),
    }
}

/// Reads `name` and `preferred_username` from an ID token. The token came
/// straight from the token endpoint over TLS, so its signature is not checked
/// (OpenID Connect Core 3.1.3.7); it is only used for display. Returns empty
/// fields if the token cannot be read.
fn identity_from_id_token(id_token: &str) -> SignedInIdentity {
    #[derive(Deserialize)]
    struct Claims {
        name: Option<String>,
        preferred_username: Option<String>,
    }
    id_token
        .split('.')
        .nth(1)
        .and_then(|payload| B64URL.decode(payload).ok())
        .and_then(|json| serde_json::from_slice::<Claims>(&json).ok())
        .map(|c| SignedInIdentity {
            display_name: c.name,
            username: c.preferred_username,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
