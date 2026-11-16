//! Reminder ledger (spec 7.1): the persisted state of every reminder the
//! scheduler knows about, so a reminder never fires twice across restarts.

use std::collections::HashSet;

use rusqlite::{params, Row, Statement};

use super::codec::{decode, instant_from_sql, instant_to_sql, outcome_from_sql, outcome_to_sql};
use super::Storage;
use crate::domain::{
    AccountId, CalendarId, Error, EventId, EventKey, ReminderInstance, ReminderKey, ReminderStatus,
    Result, TaskId,
};

/// Upsert of a meeting reminder. The conflict target repeats the partial
/// unique index's `WHERE` clause, as SQLite requires to match that index.
const UPSERT_MEETING: &str = "
    INSERT INTO reminders (kind, account_id, calendar_id, event_id, lead_minutes,
                           due_at, status, snoozed_until, handled_at, outcome)
    VALUES ('meeting', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    ON CONFLICT (account_id, calendar_id, event_id, lead_minutes) WHERE kind = 'meeting'
    DO UPDATE SET due_at = excluded.due_at, status = excluded.status,
                  snoozed_until = excluded.snoozed_until,
                  handled_at = excluded.handled_at, outcome = excluded.outcome
    RETURNING id";

/// Upsert of a task reminder, against `reminders_task_key`.
const UPSERT_TASK: &str = "
    INSERT INTO reminders (kind, task_id, due_at, status, snoozed_until, handled_at, outcome)
    VALUES ('task', ?1, ?2, ?3, ?4, ?5, ?6)
    ON CONFLICT (task_id) WHERE kind = 'task'
    DO UPDATE SET due_at = excluded.due_at, status = excluded.status,
                  snoozed_until = excluded.snoozed_until,
                  handled_at = excluded.handled_at, outcome = excluded.outcome
    RETURNING id";

impl Storage {
    /// Loads the whole reminder ledger, in insertion order.
    pub fn load_reminders(&self) -> Result<Vec<ReminderInstance>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM reminders ORDER BY id")?;
        let ledger = stmt
            .query_map([], reminder_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(ledger)
    }

    /// Makes the stored ledger equal to `ledger`, in one transaction: every
    /// instance is upserted by its key, and rows whose key is not in `ledger`
    /// are deleted. This matches the scheduler's sink, which hands over the
    /// full ledger on every change. If a key appears twice, the later instance
    /// wins. On failure nothing changes.
    pub fn save_reminders(&self, ledger: &[ReminderInstance]) -> Result<()> {
        self.write(|tx| {
            let mut meeting = tx.prepare(UPSERT_MEETING)?;
            let mut task = tx.prepare(UPSERT_TASK)?;
            let mut keep = HashSet::with_capacity(ledger.len());
            for r in ledger {
                keep.insert(upsert(&mut meeting, &mut task, r)?);
            }
            let existing: Vec<i64> = tx
                .prepare("SELECT id FROM reminders")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let mut delete = tx.prepare("DELETE FROM reminders WHERE id = ?1")?;
            for id in existing.into_iter().filter(|id| !keep.contains(id)) {
                delete.execute([id])?;
            }
            Ok(())
        })
    }
}

/// Upserts one instance and returns its row id.
fn upsert(meeting: &mut Statement, task: &mut Statement, r: &ReminderInstance) -> Result<i64> {
    let (status, until, handled_at, outcome) = status_columns(&r.status);
    let due_at = instant_to_sql(r.due_at);
    let id = match &r.key {
        ReminderKey::Meeting {
            event,
            lead_minutes,
        } => meeting.query_row(
            params![
                event.account_id.as_str(),
                event.calendar_id.as_str(),
                event.event_id.as_str(),
                lead_minutes,
                due_at,
                status,
                until,
                handled_at,
                outcome
            ],
            |row| row.get(0),
        )?,
        ReminderKey::Task { task_id } => task.query_row(
            params![task_id.as_str(), due_at, status, until, handled_at, outcome],
            |row| row.get(0),
        )?,
    };
    Ok(id)
}

/// `(status, snoozed_until, handled_at, outcome)` columns of a status.
pub(super) fn status_columns(
    s: &ReminderStatus,
) -> (&'static str, Option<i64>, Option<i64>, Option<&'static str>) {
    match *s {
        ReminderStatus::Pending => ("pending", None, None, None),
        ReminderStatus::Snoozed { until } => ("snoozed", Some(instant_to_sql(until)), None, None),
        ReminderStatus::Handled { at, outcome } => (
            "handled",
            None,
            Some(instant_to_sql(at)),
            Some(outcome_to_sql(outcome)),
        ),
    }
}

/// Maps a `reminders` row (selected with `*`) to a [`ReminderInstance`].
pub(super) fn reminder_from_row(r: &Row) -> rusqlite::Result<ReminderInstance> {
    let key = match r.get::<_, String>("kind")?.as_str() {
        "meeting" => ReminderKey::Meeting {
            event: EventKey {
                account_id: AccountId::new(r.get::<_, String>("account_id")?),
                calendar_id: CalendarId::new(r.get::<_, String>("calendar_id")?),
                event_id: EventId::new(r.get::<_, String>("event_id")?),
            },
            lead_minutes: r.get("lead_minutes")?,
        },
        "task" => ReminderKey::Task {
            task_id: TaskId::new(r.get::<_, String>("task_id")?),
        },
        other => return Err(unknown("reminder kind", other)),
    };
    let status = match r.get::<_, String>("status")?.as_str() {
        "pending" => ReminderStatus::Pending,
        "snoozed" => ReminderStatus::Snoozed {
            until: decode(instant_from_sql(r.get("snoozed_until")?))?,
        },
        "handled" => ReminderStatus::Handled {
            at: decode(instant_from_sql(r.get("handled_at")?))?,
            outcome: decode(outcome_from_sql(&r.get::<_, String>("outcome")?))?,
        },
        other => return Err(unknown("reminder status", other)),
    };
    Ok(ReminderInstance {
        key,
        due_at: decode(instant_from_sql(r.get("due_at")?))?,
        status,
    })
}

fn unknown(what: &str, value: &str) -> rusqlite::Error {
    // Enum tags written by this app, never user content.
    super::codec::row_error(Error::storage(format!(
        "unknown {what} in database: {value:?}"
    )))
}
