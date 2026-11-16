//! Strongly typed identifiers.
//!
//! Every identifier is an opaque string newtype so the different kinds cannot be
//! mixed up. They serialize as plain JSON strings (`#[serde(transparent)]`).

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wraps a raw identifier string.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// Returns the raw identifier string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the identifier and returns the raw string.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self(id.to_owned())
            }
        }
    };
}

string_id!(
    /// Identifies a configured calendar account (Graph or CalDAV).
    ///
    /// Assigned by the backend when the account is added and stable for the
    /// lifetime of the account. Also used as the keychain entry key.
    AccountId
);

string_id!(
    /// Identifies a calendar within one account, as reported by the provider
    /// (Graph calendar id, or the CalDAV collection href).
    ///
    /// Only unique together with its [`AccountId`].
    CalendarId
);

string_id!(
    /// Identifies one *occurrence* of a calendar event within one calendar.
    ///
    /// Providers must make this unique per occurrence, so expanded recurring
    /// events get distinct ids: Graph uses the `calendarView` instance id,
    /// CalDAV uses `UID` for single events and `UID` + `"/"` + the occurrence
    /// start as RFC 3339 UTC (e.g. `abc@host/2026-11-16T09:00:00Z`) for
    /// recurring ones. Only unique together with its account and calendar ids
    /// (see [`EventKey`](super::EventKey)).
    EventId
);

string_id!(
    /// Identifies a task. Opaque; the [`TaskSource`](super::TaskSource) that
    /// owns the task assigns it and guarantees uniqueness.
    TaskId
);

/// Identifies one speech bubble shown by the buddy window.
///
/// Assigned by the backend each time a bubble is shown; bubble actions
/// (dismiss, snooze) refer to it so a stale click cannot act on a newer
/// bubble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BubbleId(pub u64);

impl fmt::Display for BubbleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_serialize_as_plain_strings() {
        let id = AccountId::new("acc-1");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"acc-1\"");
        let back: AccountId = serde_json::from_str("\"acc-1\"").unwrap();
        assert_eq!(back, id);
        assert_eq!(id.to_string(), "acc-1");
        assert_eq!(serde_json::to_string(&BubbleId(7)).unwrap(), "7");
    }
}
