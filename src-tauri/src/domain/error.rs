//! The shared error type.

use serde::{Deserialize, Serialize};

/// Shorthand for `Result<T, domain::Error>`.
pub type Result<T> = std::result::Result<T, Error>;

/// One invalid field, reported by [`Settings::validate`](super::Settings::validate)
/// and by IPC commands that validate input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    /// Path of the field in the JSON (camelCase) representation, e.g.
    /// `reminders.leadTimesMinutes[1]` or `accounts[0].displayName`.
    pub field: String,
    /// Human-readable, English explanation suitable for showing next to the field.
    pub message: String,
}

impl FieldError {
    /// Creates a field error.
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Error shared by providers, task sources, storage, keychain and IPC commands.
///
/// Variants carry a plain message instead of wrapping crate-specific error
/// types, so the type stays `Clone`, serializable over IPC and independent of
/// the crates each implementation picks. **Messages must never contain
/// secrets, tokens or event/task contents** (spec section 9).
///
/// Over IPC it serializes as `{"kind": "network", "message": "..."}`, or
/// `{"kind": "validation", "errors": [{"field": ..., "message": ...}]}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Error {
    /// Credentials were rejected or a token expired and could not be
    /// refreshed. The user must re-enter credentials or sign in again. Not
    /// retried automatically.
    #[error("authentication failed: {message}")]
    Auth {
        /// Details.
        message: String,
    },
    /// The server could not be reached, timed out, or answered with a
    /// transient failure (HTTP 5xx, 429). Retried with backoff.
    #[error("network error: {message}")]
    Network {
        /// Details.
        message: String,
    },
    /// The server answered, but with something we could not understand
    /// (unexpected status, malformed XML/JSON/iCalendar).
    #[error("protocol error: {message}")]
    Protocol {
        /// Details.
        message: String,
    },
    /// The referenced item (account, calendar, task, bubble) does not exist.
    #[error("not found: {message}")]
    NotFound {
        /// Details.
        message: String,
    },
    /// Local storage (SQLite, settings file) failed.
    #[error("storage error: {message}")]
    Storage {
        /// Details.
        message: String,
    },
    /// The OS keychain failed or the secret is missing.
    #[error("keychain error: {message}")]
    Keychain {
        /// Details.
        message: String,
    },
    /// A request argument was invalid (not tied to a single settings field).
    #[error("invalid input: {message}")]
    InvalidInput {
        /// Details.
        message: String,
    },
    /// One or more fields failed validation.
    #[error("validation failed for {} field(s)", errors.len())]
    Validation {
        /// The invalid fields.
        errors: Vec<FieldError>,
    },
    /// A bug or unexpected state.
    #[error("internal error: {message}")]
    Internal {
        /// Details.
        message: String,
    },
}

impl Error {
    /// Creates [`Error::Auth`].
    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth {
            message: message.into(),
        }
    }

    /// Creates [`Error::Network`].
    pub fn network(message: impl Into<String>) -> Self {
        Self::Network {
            message: message.into(),
        }
    }

    /// Creates [`Error::Protocol`].
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol {
            message: message.into(),
        }
    }

    /// Creates [`Error::NotFound`].
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
        }
    }

    /// Creates [`Error::Storage`].
    pub fn storage(message: impl Into<String>) -> Self {
        Self::Storage {
            message: message.into(),
        }
    }

    /// Creates [`Error::Keychain`].
    pub fn keychain(message: impl Into<String>) -> Self {
        Self::Keychain {
            message: message.into(),
        }
    }

    /// Creates [`Error::InvalidInput`].
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
        }
    }

    /// Creates [`Error::Internal`].
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// Whether the sync engine should retry the failed operation with backoff.
    /// Only [`Error::Network`] is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Network { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_kind_tag() {
        let json = serde_json::to_value(Error::network("timeout")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "network", "message": "timeout"})
        );

        let json = serde_json::to_value(Error::Validation {
            errors: vec![FieldError::new("system.sleepAfterIdleMinutes", "too big")],
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "validation", "errors": [
                {"field": "system.sleepAfterIdleMinutes", "message": "too big"}
            ]})
        );
    }

    #[test]
    fn only_network_errors_are_retryable() {
        assert!(Error::network("x").is_retryable());
        assert!(!Error::auth("x").is_retryable());
        assert!(!Error::protocol("x").is_retryable());
    }
}
