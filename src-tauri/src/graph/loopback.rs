//! The loopback redirect listener (RFC 8252 section 7.3).
//!
//! Binds `127.0.0.1` on an ephemeral port and waits for the browser to be
//! redirected to `http://localhost:<port>/?code=...&state=...`. Only the
//! authorization response is processed: stray requests (`/favicon.ico`,
//! speculative connections that never send a request) are answered or dropped
//! and the wait goes on. The first request that carries `code` or `error`
//! ends the wait, whatever its outcome, and is answered with a static page
//! that never echoes request data.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use super::auth::CancelToken;
use crate::domain::{Error, Result, SecretString};

/// How often the non-blocking listener checks for a connection, the deadline
/// and cancellation. Only runs during an interactive sign-in.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long a connected browser has to send its request line and headers.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest request head read. The authorization response is a GET whose URL
/// holds a code of about 1-2 KB.
const MAX_REQUEST_BYTES: usize = 16 * 1024;

const SUCCESS_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Signed in</title></head><body style=\"font-family:sans-serif\">\
<h1>You are signed in</h1><p>You can close this tab and return to AIchemist Desktop Buddy.</p>\
</body></html>";

const FAILURE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\">\
<title>Sign-in failed</title></head><body style=\"font-family:sans-serif\">\
<h1>Sign-in failed</h1><p>You can close this tab. AIchemist Desktop Buddy shows the details.</p>\
</body></html>";

/// A bound loopback listener waiting for one authorization response.
pub(crate) struct LoopbackServer {
    listener: TcpListener,
    port: u16,
}

impl LoopbackServer {
    /// Binds `127.0.0.1` on a port chosen by the OS.
    pub(crate) fn bind() -> Result<Self> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(|e| Error::internal(format!("could not open the sign-in listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::internal(format!("could not open the sign-in listener: {e}")))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| Error::internal(format!("could not open the sign-in listener: {e}")))?;
        Ok(Self { listener, port })
    }

    /// The bound port.
    #[cfg(test)]
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    /// The redirect URI to send in the authorize and token requests:
    /// `http://localhost:<port>`. Entra ID ignores the port when matching a
    /// `localhost` redirect URI, so the registration only needs
    /// `http://localhost`.
    pub(crate) fn redirect_uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// Waits for the authorization response and returns the code.
    ///
    /// Fails with [`Error::Auth`] on a `state` mismatch, an error response
    /// (e.g. the user declined consent), timeout or cancellation, and with
    /// [`Error::Network`] for `server_error` / `temporarily_unavailable`.
    pub(crate) fn wait_for_code(
        self,
        expected_state: &str,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<SecretString> {
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::auth("sign-in was cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(Error::auth(
                    "sign-in timed out waiting for the browser; try again",
                ));
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(outcome) = handle_connection(stream, expected_state) {
                        return outcome;
                    }
                }
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(e) => {
                    return Err(Error::internal(format!("sign-in listener failed: {e}")));
                }
            }
        }
    }
}

/// Handles one connection. `None` means it was not the authorization
/// response and the wait goes on.
fn handle_connection(mut stream: TcpStream, expected_state: &str) -> Option<Result<SecretString>> {
    // Accepted sockets may inherit non-blocking mode (Windows does).
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(READ_TIMEOUT)).is_err()
    {
        return None;
    }
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let target = read_request_target(&mut stream)?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    if path != "/" {
        respond(&mut stream, "404 Not Found", "");
        return None;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match &*key {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if code.is_none() && error.is_none() {
        respond(&mut stream, "404 Not Found", "");
        return None;
    }
    let outcome = if !state
        .as_deref()
        .is_some_and(|s| constant_time_eq(s.as_bytes(), expected_state.as_bytes()))
    {
        Err(Error::auth(
            "sign-in response did not match the request (state mismatch); try again",
        ))
    } else if let Some(error) = error {
        Err(authorize_error(&error))
    } else {
        match code {
            Some(code) if !code.is_empty() => Ok(SecretString::new(code)),
            _ => Err(Error::protocol("sign-in response had an empty code")),
        }
    };
    let (status, page) = match outcome {
        Ok(_) => ("200 OK", SUCCESS_PAGE),
        Err(_) => ("400 Bad Request", FAILURE_PAGE),
    };
    respond(&mut stream, status, page);
    Some(outcome)
}

/// Reads the request head and returns the request target of a `GET`, or
/// `None` for anything else (no data, not GET, too large, malformed).
fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > MAX_REQUEST_BYTES {
            respond(stream, "431 Request Header Fields Too Large", "");
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = std::str::from_utf8(&buf).ok()?;
    let request_line = head.lines().next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?;
    let target = parts.next()?;
    if method != "GET" {
        respond(stream, "405 Method Not Allowed", "");
        return None;
    }
    Some(target.to_owned())
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Best effort: the outcome does not depend on the browser getting the page.
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// Maps an `error` from the authorization endpoint. Only the OAuth error
/// code is kept: `error_description` can contain the user's name.
fn authorize_error(error: &str) -> Error {
    // Keep the code printable and short in case it is garbage.
    let code: String = error
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(64)
        .collect();
    match code.as_str() {
        "access_denied" => {
            Error::auth("sign-in was cancelled or consent was declined (access_denied)")
        }
        "server_error" | "temporarily_unavailable" => Error::network(format!(
            "Microsoft sign-in is temporarily unavailable ({code})"
        )),
        _ => Error::auth(format!(
            "Microsoft sign-in failed ({code}); check the app registration in docs/graph-setup.md"
        )),
    }
}

/// Compares without an early exit, so timing does not reveal how much of
/// `state` an attacker guessed.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sends `request` to the server's port and returns the raw response.
    fn send(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    fn get(port: u16, target: &str) -> String {
        send(
            port,
            &format!("GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
        )
    }

    /// Runs `wait_for_code` on a thread and returns (port, join handle).
    fn spawn_server(
        timeout: Duration,
        cancel: CancelToken,
    ) -> (u16, thread::JoinHandle<Result<SecretString>>) {
        let server = LoopbackServer::bind().unwrap();
        let port = server.port();
        assert_eq!(server.redirect_uri(), format!("http://localhost:{port}"));
        let handle = thread::spawn(move || server.wait_for_code("st4te", timeout, &cancel));
        (port, handle)
    }

    #[test]
    fn returns_the_code_and_serves_the_success_page() {
        let (port, handle) = spawn_server(Duration::from_secs(10), CancelToken::new());
        // A stray favicon request and an empty connection do not end the wait.
        assert!(get(port, "/favicon.ico").starts_with("HTTP/1.1 404"));
        drop(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap());
        assert!(get(port, "/").starts_with("HTTP/1.1 404"));

        let response = get(port, "/?code=abc%2Bdef&state=st4te&session_state=x");
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.contains("You can close this tab"));
        let code = handle.join().unwrap().unwrap();
        assert_eq!(code.expose(), "abc+def");
    }

    #[test]
    fn rejects_a_wrong_or_missing_state() {
        for target in ["/?code=abc&state=other", "/?code=abc"] {
            let (port, handle) = spawn_server(Duration::from_secs(10), CancelToken::new());
            let response = get(port, target);
            assert!(response.starts_with("HTTP/1.1 400"), "{response}");
            assert!(!response.contains("abc"), "page must not echo the request");
            let err = handle.join().unwrap().unwrap_err();
            assert!(matches!(err, Error::Auth { .. }), "{err:?}");
            assert!(err.to_string().contains("state mismatch"));
        }
    }

    #[test]
    fn maps_error_responses() {
        let (port, handle) = spawn_server(Duration::from_secs(10), CancelToken::new());
        get(
            port,
            "/?error=access_denied&error_description=The+user+Jane+cancelled&state=st4te",
        );
        let err = handle.join().unwrap().unwrap_err();
        assert!(matches!(err, Error::Auth { .. }));
        assert!(!err.to_string().contains("Jane"), "description is dropped");

        let (port, handle) = spawn_server(Duration::from_secs(10), CancelToken::new());
        get(port, "/?error=temporarily_unavailable&state=st4te");
        assert!(handle.join().unwrap().unwrap_err().is_retryable());
    }

    #[test]
    fn ignores_non_get_requests() {
        let (port, handle) = spawn_server(Duration::from_secs(10), CancelToken::new());
        let response = send(
            port,
            "POST /?code=abc&state=st4te HTTP/1.1\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 405"));
        get(port, "/?code=real&state=st4te");
        assert_eq!(handle.join().unwrap().unwrap().expose(), "real");
    }

    #[test]
    fn times_out() {
        let (_port, handle) = spawn_server(Duration::from_millis(250), CancelToken::new());
        let err = handle.join().unwrap().unwrap_err();
        assert!(matches!(err, Error::Auth { .. }));
        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn can_be_cancelled() {
        let cancel = CancelToken::new();
        let (_port, handle) = spawn_server(Duration::from_secs(60), cancel.clone());
        cancel.cancel();
        let err = handle.join().unwrap().unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
