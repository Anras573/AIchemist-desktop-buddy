//! A string that must never be logged or sent back to a webview.

use std::fmt;

use serde::Deserialize;

/// A secret (CalDAV password, app password) received from the settings window
/// on its way to the OS keychain.
///
/// `Debug` prints `***`, there is no `Display`, and it implements
/// `Deserialize` but deliberately not `Serialize`, so it can arrive over IPC
/// but can never be returned over IPC or written to the settings file.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// Wraps a secret value.
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// Returns the secret value. Call only where the value is handed to the
    /// keychain or an authentication header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_the_value() {
        let s = SecretString::new("hunter2");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn deserializes_from_a_plain_string() {
        let s: SecretString = serde_json::from_str("\"pw\"").unwrap();
        assert_eq!(s.expose(), "pw");
    }
}
