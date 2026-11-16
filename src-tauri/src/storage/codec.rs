//! Conversions between domain values and their column representation.
//!
//! These encode the conventions in `docs/storage.md`, so every repository
//! stores values the same way:
//!
//! * instants: `INTEGER` unix seconds, UTC; sub-second precision is dropped;
//! * all-day dates: `TEXT` `YYYY-MM-DD`;
//! * enums: `TEXT`, the camelCase name of the serde representation.

use chrono::{DateTime, NaiveDate, Utc};

use crate::domain::{Error, ReminderOutcome, ResponseStatus, Result};

/// Encodes an instant as unix seconds. Sub-second precision is dropped.
pub fn instant_to_sql(t: DateTime<Utc>) -> i64 {
    t.timestamp()
}

/// Decodes unix seconds into an instant.
pub fn instant_from_sql(secs: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp(secs, 0)
        .ok_or_else(|| Error::storage(format!("timestamp out of range: {secs}")))
}

/// Encodes an all-day date as `YYYY-MM-DD`.
pub fn date_to_sql(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// Decodes a `YYYY-MM-DD` date.
pub fn date_from_sql(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| Error::storage("malformed date in database"))
}

/// Encodes a [`ResponseStatus`] (column `events.response`).
pub fn response_to_sql(r: ResponseStatus) -> &'static str {
    match r {
        ResponseStatus::Organizer => "organizer",
        ResponseStatus::Accepted => "accepted",
        ResponseStatus::Tentative => "tentative",
        ResponseStatus::Declined => "declined",
        ResponseStatus::NotResponded => "notResponded",
        ResponseStatus::Unknown => "unknown",
    }
}

/// Decodes a [`ResponseStatus`].
pub fn response_from_sql(s: &str) -> Result<ResponseStatus> {
    Ok(match s {
        "organizer" => ResponseStatus::Organizer,
        "accepted" => ResponseStatus::Accepted,
        "tentative" => ResponseStatus::Tentative,
        "declined" => ResponseStatus::Declined,
        "notResponded" => ResponseStatus::NotResponded,
        "unknown" => ResponseStatus::Unknown,
        _ => return Err(unknown("response status", s)),
    })
}

/// Encodes a [`ReminderOutcome`] (column `reminders.outcome`).
pub fn outcome_to_sql(o: ReminderOutcome) -> &'static str {
    match o {
        ReminderOutcome::Dismissed => "dismissed",
        ReminderOutcome::MarkedDone => "markedDone",
        ReminderOutcome::Skipped => "skipped",
    }
}

/// Decodes a [`ReminderOutcome`].
pub fn outcome_from_sql(s: &str) -> Result<ReminderOutcome> {
    Ok(match s {
        "dismissed" => ReminderOutcome::Dismissed,
        "markedDone" => ReminderOutcome::MarkedDone,
        "skipped" => ReminderOutcome::Skipped,
        _ => return Err(unknown("reminder outcome", s)),
    })
}

/// The kind tag of an [`Error`] (column `sync_state.last_error_kind`), as in
/// its IPC serialization.
pub fn error_kind_to_sql(e: &Error) -> &'static str {
    match e {
        Error::Auth { .. } => "auth",
        Error::Network { .. } => "network",
        Error::Protocol { .. } => "protocol",
        Error::NotFound { .. } => "notFound",
        Error::Storage { .. } => "storage",
        Error::Keychain { .. } => "keychain",
        Error::InvalidInput { .. } => "invalidInput",
        Error::Validation { .. } => "validation",
        Error::Internal { .. } => "internal",
    }
}

fn unknown(what: &str, value: &str) -> Error {
    // `value` is an enum tag written by this app, never user content.
    Error::storage(format!("unknown {what} in database: {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn instants_round_trip_at_second_precision() {
        let t = Utc.with_ymd_and_hms(2026, 11, 16, 9, 30, 15).unwrap();
        assert_eq!(instant_from_sql(instant_to_sql(t)).unwrap(), t);
        let with_millis = t + chrono::Duration::milliseconds(750);
        assert_eq!(instant_from_sql(instant_to_sql(with_millis)).unwrap(), t);
        assert!(instant_from_sql(i64::MAX).is_err());
    }

    #[test]
    fn dates_round_trip() {
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        assert_eq!(date_to_sql(d), "2026-01-05");
        assert_eq!(date_from_sql("2026-01-05").unwrap(), d);
        assert!(date_from_sql("2026-1-5x").is_err());
    }

    #[test]
    fn enums_round_trip_and_match_serde_names() {
        for r in [
            ResponseStatus::Organizer,
            ResponseStatus::Accepted,
            ResponseStatus::Tentative,
            ResponseStatus::Declined,
            ResponseStatus::NotResponded,
            ResponseStatus::Unknown,
        ] {
            let s = response_to_sql(r);
            assert_eq!(response_from_sql(s).unwrap(), r);
            assert_eq!(serde_json::to_value(r).unwrap(), s);
        }
        for o in [
            ReminderOutcome::Dismissed,
            ReminderOutcome::MarkedDone,
            ReminderOutcome::Skipped,
        ] {
            let s = outcome_to_sql(o);
            assert_eq!(outcome_from_sql(s).unwrap(), o);
            assert_eq!(serde_json::to_value(o).unwrap(), s);
        }
        assert!(response_from_sql("maybe").is_err());
        assert!(outcome_from_sql("maybe").is_err());
    }

    #[test]
    fn error_kind_matches_serde_tag() {
        for e in [
            Error::auth("x"),
            Error::network("x"),
            Error::protocol("x"),
            Error::not_found("x"),
            Error::storage("x"),
            Error::keychain("x"),
            Error::invalid_input("x"),
            Error::Validation { errors: vec![] },
            Error::internal("x"),
        ] {
            assert_eq!(
                serde_json::to_value(&e).unwrap()["kind"],
                error_kind_to_sql(&e)
            );
        }
    }
}
