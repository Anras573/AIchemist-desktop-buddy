//! Pruning, so the database does not grow without bound.
//!
//! Each sync replaces an account's cached events with those in the sync
//! window, so most past events leave on their own. What remains are events of
//! accounts that stopped syncing, and the reminder ledger, which deliberately
//! outlives the cache. [`Storage::prune`] removes both once they are old.
//! Call it after a sync (or at startup); it is cheap.

use chrono::Duration;
use rusqlite::named_params;

use super::codec::{date_to_sql, instant_to_sql};
use super::Storage;
use crate::domain::Result;

/// How long past events (and their meeting reminders) are kept: one day.
pub const DEFAULT_RETENTION: Duration = Duration::days(1);

/// What [`Storage::prune`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneStats {
    /// Cached events removed.
    pub events: usize,
    /// Reminder rows removed.
    pub reminders: usize,
}

impl Storage {
    /// Deletes data older than `retention`, relative to the clock's now:
    ///
    /// * cached events that ended at or before `now - retention` (all-day
    ///   events end at local midnight of their exclusive end date);
    /// * meeting reminder rows due before that cutoff whose event is no longer
    ///   cached (pruned now, or dropped by an earlier sync because it was
    ///   cancelled). A reminder of a long event that is still cached stays.
    ///
    /// Task reminder rows are left alone: their lifetime follows the task, and
    /// the scheduler drops them from the ledger it saves. Done tasks are the
    /// user's to delete.
    pub fn prune(&self, retention: Duration) -> Result<PruneStats> {
        let cutoff = self.now() - retention;
        let cutoff_date = self.clock().utc_to_local(cutoff).date();
        self.write(|tx| {
            let events = tx.execute(
                "DELETE FROM events
                 WHERE (time_kind = 'timed' AND end_at <= :cutoff)
                    OR (time_kind = 'allDay' AND end_date <= :cutoff_date)",
                named_params! {
                    ":cutoff": instant_to_sql(cutoff),
                    ":cutoff_date": date_to_sql(cutoff_date),
                },
            )?;
            let reminders = tx.execute(
                "DELETE FROM reminders
                 WHERE kind = 'meeting' AND due_at < :cutoff
                   AND NOT EXISTS (
                       SELECT 1 FROM events e
                       WHERE e.account_id = reminders.account_id
                         AND e.calendar_id = reminders.calendar_id
                         AND e.event_id = reminders.event_id)",
                named_params! { ":cutoff": instant_to_sql(cutoff) },
            )?;
            Ok(PruneStats { events, reminders })
        })
    }
}
