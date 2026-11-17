//! WebDAV requests with redirect handling, status mapping and discovery.

use std::sync::Arc;

use url::Url;

use super::http::{basic_auth, HttpRequest, HttpResponse, Transport};
use super::xml::{self, MsResponse};
use crate::domain::{Error, Result, SecretString};

/// Most redirects followed for one request.
pub(crate) const MAX_REDIRECTS: usize = 5;

/// What discovery found out about the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Discovery {
    /// The calendar home collection (after redirects).
    pub home: Url,
    /// The user's email addresses (`calendar-user-address-set` `mailto:`
    /// entries, plus the user name if it looks like an email address).
    pub user_addresses: Vec<String>,
}

/// Talks WebDAV to one server with one set of credentials.
pub(crate) struct Client {
    transport: Arc<dyn Transport>,
    username: String,
    password: SecretString,
}

impl Client {
    pub(crate) fn new(
        transport: Arc<dyn Transport>,
        username: String,
        password: SecretString,
    ) -> Self {
        Self {
            transport,
            username,
            password,
        }
    }

    /// Sends a request, following redirects to `https` targets only, and
    /// maps authentication and server failures to errors. Returns the final
    /// URL (the base for relative hrefs) and the response, whose status is
    /// neither 3xx, 401/403, 408/429 nor 5xx.
    pub(crate) fn send(
        &self,
        method: &str,
        url: &Url,
        depth: Option<&str>,
        body: &str,
    ) -> Result<(Url, HttpResponse)> {
        let mut url = url.clone();
        for _ in 0..=MAX_REDIRECTS {
            // Credentials only ever travel over https: the configured URL is
            // https (settings validation), and a redirect elsewhere is refused.
            if url.scheme() != "https" {
                return Err(Error::protocol(
                    "the server redirected to a non-https URL; refusing to follow",
                ));
            }
            let authorization = basic_auth(&self.username, &self.password);
            let response = self.transport.send(&HttpRequest {
                method,
                url: &url,
                depth,
                body,
                authorization: Some(&authorization),
            })?;
            match response.status {
                301 | 302 | 303 | 307 | 308 => {
                    let location = response.location.as_deref().ok_or_else(|| {
                        Error::protocol("the server sent a redirect without a Location")
                    })?;
                    url = url
                        .join(location)
                        .map_err(|_| Error::protocol("the server sent an invalid redirect"))?;
                }
                401 => return Err(Error::auth("the server rejected the user name or password")),
                403 => return Err(Error::auth("the server denied access to the account")),
                408 | 429 | 500..=599 => {
                    return Err(Error::network(format!(
                        "the server is unavailable (HTTP {})",
                        response.status
                    )))
                }
                _ => return Ok((url, response)),
            }
        }
        Err(Error::protocol("the server redirected too many times"))
    }

    /// `PROPFIND` expecting `207 Multi-Status`. Any other status left by
    /// [`Client::send`] (e.g. 404, 405, or a 200 web page) means there is
    /// no WebDAV resource at the URL: [`Error::NotFound`].
    pub(crate) fn propfind(
        &self,
        url: &Url,
        depth: &str,
        body: &str,
    ) -> Result<(Url, Vec<MsResponse>)> {
        let (url, response) = self.send("PROPFIND", url, Some(depth), body)?;
        match response.status {
            207 => Ok((url, xml::parse_multistatus(&response.body)?)),
            s => Err(Error::not_found(format!(
                "no CalDAV resource at this URL (HTTP {s})"
            ))),
        }
    }

    /// Finds the calendar home and the user's addresses, starting from the
    /// configured server URL (RFC 6764 section 6, RFC 4791 section 6.2.1):
    ///
    /// 1. A bare host (path `/`) is tried as `/.well-known/caldav` first,
    ///    then as `/`. Any other URL is used as given.
    /// 2. `PROPFIND current-user-principal` on it. Without one, the URL
    ///    itself is taken as the principal.
    /// 3. `PROPFIND calendar-home-set` and `calendar-user-address-set` on the
    ///    principal. Without a home set, the principal URL is the home (so a
    ///    calendar home URL can be configured directly).
    pub(crate) fn discover(&self, server: &Url, username: &str) -> Result<Discovery> {
        let mut candidates = Vec::with_capacity(2);
        if server.path() == "/" {
            candidates.push(
                server
                    .join("/.well-known/caldav")
                    .map_err(|_| Error::internal("could not build the well-known URL"))?,
            );
        }
        candidates.push(server.clone());

        let mut context = None;
        let mut last_error = None;
        for candidate in &candidates {
            match self.propfind(candidate, "0", xml::PROPFIND_PRINCIPAL) {
                Ok(found) => {
                    context = Some(found);
                    break;
                }
                // Wrong place: try the next candidate. Auth, network and
                // protocol errors (including a refused non-https redirect)
                // are final.
                Err(e @ Error::NotFound { .. }) => last_error = Some(e),
                Err(e) => return Err(e),
            }
        }
        let Some((context_url, responses)) = context else {
            return Err(
                last_error.unwrap_or_else(|| Error::protocol("the server is not a CalDAV server"))
            );
        };

        let principal = match responses
            .iter()
            .find_map(|r| r.props.current_user_principal.as_deref())
        {
            Some(href) => join(&context_url, href)?,
            None => context_url,
        };

        let (principal_url, responses) = self.propfind(&principal, "0", xml::PROPFIND_HOME)?;
        let props = responses
            .into_iter()
            .next()
            .map(|r| r.props)
            .unwrap_or_default();
        let home = match props.calendar_home_set.as_deref() {
            Some(href) => join(&principal_url, href)?,
            None => principal_url,
        };
        Ok(Discovery {
            home,
            user_addresses: user_addresses(&props.calendar_user_addresses, username),
        })
    }
}

/// Resolves a server-sent href against the URL of the response it came in.
pub(crate) fn join(base: &Url, href: &str) -> Result<Url> {
    base.join(href)
        .map_err(|_| Error::protocol("the server sent an invalid href"))
}

/// The `mailto:` addresses from a `calendar-user-address-set`, plus the user
/// name if it looks like an email address, without duplicates.
fn user_addresses(address_set: &[String], username: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let candidates = address_set
        .iter()
        .filter_map(|a| {
            let (scheme, rest) = a.split_once(':')?;
            scheme.eq_ignore_ascii_case("mailto").then_some(rest)
        })
        .chain(username.contains('@').then_some(username));
    for address in candidates {
        let address = address.trim();
        if !address.is_empty() && !out.iter().any(|a| a.eq_ignore_ascii_case(address)) {
            out.push(address.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::user_addresses;

    #[test]
    fn user_addresses_keep_mailto_and_email_like_user_names() {
        let set = vec![
            "mailto:Jane@Example.com".to_owned(),
            "https://p42-caldav.icloud.com/123/principal/".to_owned(),
            "urn:uuid:0000".to_owned(),
            "MAILTO:jane.alt@example.com".to_owned(),
        ];
        assert_eq!(
            user_addresses(&set, "jane@example.com"),
            ["Jane@Example.com", "jane.alt@example.com"]
        );
        assert_eq!(user_addresses(&[], "jane"), Vec::<String>::new());
        assert_eq!(
            user_addresses(&[], "jane@example.com"),
            ["jane@example.com"]
        );
    }
}
