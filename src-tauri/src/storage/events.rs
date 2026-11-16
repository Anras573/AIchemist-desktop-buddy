//! Event cache (spec 6.5): replaced per account after each successful sync,
//! queried by time window for reminders and the upcoming list.

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rusqlite::{named_params, params_from_iter, Row};

use super::codec::{
    date_from_sql, date_to_sql, decode, instant_from_sql, instant_to_sql, response_from_sql,
    response_to_sql,
};
use super::Storage;
use crate::domain::{AccountId, CalendarId, Clock, Error, Event, EventId, EventTime, Result};

impl Storage {
    /// Replaces the cached events of `account_id` with `events`, atomically.
    ///
    /// Call this only after a successful sync of the account: on failure the
    /// caller simply does not call it, and the last good cache stays.
    ///
    /// `calendar_ids` is the set of calendars that sync fetched, i.e. the
    /// account's included calendars. Afterwards the account's cache holds
    /// exactly `events`: events of calendars outside the set are dropped too,
    /// because those calendars are no longer included. Other accounts are not
    /// touched.
    ///
    /// Every event must belong to `account_id` and to one of `calendar_ids`,
    /// otherwise this fails with [`Error::InvalidInput`]. If two events share
    /// a key, the later one wins. On any failure nothing changes.
    pub fn replace_events(
        &self,
        account_id: &AccountId,
        calendar_ids: &[CalendarId],
        events: &[Event],
    ) -> Result<()> {
        let calendars: HashSet<&CalendarId> = calendar_ids.iter().collect();
        if let Some(e) = events
            .iter()
            .find(|e| &e.account_id != account_id || !calendars.contains(&e.calendar_id))
        {
            // Ids are not event contents, so they may appear in the message.
            return Err(Error::invalid_input(format!(
                "event {} of account {} calendar {} is not in the replaced set",
                e.id, e.account_id, e.calendar_id
            )));
        }
        self.write(|tx| {
            tx.execute(
                "DELETE FROM events WHERE account_id = ?1",
                [account_id.as_str()],
            )?;
            let mut stmt = tx.prepare(UPSERT_EVENT)?;
            for e in events {
                stmt.execute(params_from_iter(event_params(e).iter()))?;
            }
            Ok(())
        })
    }

    /// Cached events that overlap the half-open window `[from, to)`, sorted by
    /// start (all-day events at local midnight), then by key.
    ///
    /// Timed events overlap when `start < to` and `end > from`. A timed event
    /// with `start == end` inside the window is not returned.
    ///
    /// All-day dates are floating, so the window is converted to local dates
    /// with the handle's clock. An all-day event covers local
    /// `[start_date 00:00, end_date 00:00)` and overlaps when it starts before
    /// `to` and ends after `from` in local time.
    pub fn events_in_window(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Event>> {
        if from >= to {
            return Ok(Vec::new());
        }
        let clock = self.clock();
        let (from_date, to_date) = local_date_window(clock, from, to);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM events
             WHERE (time_kind = 'timed' AND start_at < :to AND end_at > :from)
                OR (time_kind = 'allDay' AND start_date < :to_date AND end_date > :from_date)",
        )?;
        let mut events: Vec<Event> = stmt
            .query_map(
                named_params! {
                    ":from": instant_to_sql(from),
                    ":to": instant_to_sql(to),
                    ":from_date": date_to_sql(from_date),
                    ":to_date": date_to_sql(to_date),
                },
                event_from_row,
            )?
            .collect::<rusqlite::Result<_>>()?;
        events.sort_by_cached_key(|e| (e.time.start_utc(clock), e.key()));
        Ok(events)
    }

    /// Forgets everything cached for a removed account: its events and its
    /// sync state. Reminder rows belong to the scheduler's ledger; they leave
    /// with the next ledger save or the next [`Storage::prune`].
    pub fn remove_account(&self, account_id: &AccountId) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM events WHERE account_id = ?1",
                [account_id.as_str()],
            )?;
            tx.execute(
                "DELETE FROM sync_state WHERE account_id = ?1",
                [account_id.as_str()],
            )?;
            Ok(())
        })
    }
}

/// The local dates `(from_date, to_date)` such that an all-day event
/// `[start, end)` overlaps the instant window `[from, to)` exactly when
/// `start < to_date && end > from_date`.
///
/// * `end 00:00 > from_local` holds exactly when `end > from_local.date()`.
/// * `start 00:00 < to_local` holds exactly when `start` is before the
///   ceiling of `to_local` to a whole day: `to_local.date()` if `to_local` is
///   midnight, otherwise the day after.
fn local_date_window<C: Clock + ?Sized>(
    clock: &C,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> (NaiveDate, NaiveDate) {
    let from_date = clock.utc_to_local(from).date();
    let to_local = clock.utc_to_local(to);
    let to_date = if to_local.time() == NaiveTime::MIN {
        to_local.date()
    } else {
        to_local.date().succ_opt().unwrap_or(to_local.date())
    };
    (from_date, to_date)
}

/// `INSERT OR REPLACE`, so a duplicate key in one batch keeps the last event
/// instead of failing the whole sync.
const UPSERT_EVENT: &str = "INSERT OR REPLACE INTO events
    (account_id, calendar_id, event_id, title, time_kind,
     start_at, end_at, start_date, end_date, location, response)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";

/// The eleven parameters of an `events` insert, in column order.
fn event_params(e: &Event) -> Vec<Box<dyn rusqlite::ToSql + '_>> {
    let (kind, start_at, end_at, start_date, end_date) = match e.time {
        EventTime::Timed { start, end } => (
            "timed",
            Some(instant_to_sql(start)),
            Some(instant_to_sql(end)),
            None,
            None,
        ),
        EventTime::AllDay { start, end } => (
            "allDay",
            None,
            None,
            Some(date_to_sql(start)),
            Some(date_to_sql(end)),
        ),
    };
    vec![
        Box::new(e.account_id.as_str()),
        Box::new(e.calendar_id.as_str()),
        Box::new(e.id.as_str()),
        Box::new(e.title.as_str()),
        Box::new(kind),
        Box::new(start_at),
        Box::new(end_at),
        Box::new(start_date),
        Box::new(end_date),
        Box::new(e.location.as_deref()),
        Box::new(response_to_sql(e.response)),
    ]
}

/// Inserts one event row (plain `INSERT`: a duplicate key fails).
#[cfg(test)]
pub(super) fn insert_event(conn: &rusqlite::Connection, e: &Event) -> rusqlite::Result<usize> {
    let p = event_params(e);
    conn.execute(
        "INSERT INTO events (account_id, calendar_id, event_id, title, time_kind,
                             start_at, end_at, start_date, end_date, location, response)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params_from_iter(p.iter()),
    )
}

/// Maps an `events` row (selected with `*`) to an [`Event`].
pub(super) fn event_from_row(r: &Row) -> rusqlite::Result<Event> {
    let kind: String = r.get("time_kind")?;
    let time = if kind == "timed" {
        EventTime::Timed {
            start: decode(instant_from_sql(r.get("start_at")?))?,
            end: decode(instant_from_sql(r.get("end_at")?))?,
        }
    } else {
        EventTime::AllDay {
            start: decode(date_from_sql(&r.get::<_, String>("start_date")?))?,
            end: decode(date_from_sql(&r.get::<_, String>("end_date")?))?,
        }
    };
    Ok(Event {
        id: EventId::new(r.get::<_, String>("event_id")?),
        account_id: AccountId::new(r.get::<_, String>("account_id")?),
        calendar_id: CalendarId::new(r.get::<_, String>("calendar_id")?),
        title: r.get("title")?,
        time,
        location: r.get("location")?,
        response: decode(response_from_sql(&r.get::<_, String>("response")?))?,
    })
}
