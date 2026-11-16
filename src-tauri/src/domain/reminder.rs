//! Reminder identity and lifecycle (spec section 7).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{EventKey, TaskId};

/// What a reminder is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReminderKind {
    /// A calendar meeting.
    Meeting,
    /// A local task.
    Task,
}

/// Identifies one reminder: an (event occurrence, lead time) pair, or a task.
///
/// Each key fires at most once unless snoozed (spec 7.1). A task has exactly
/// one reminder, at its `due_at`.
///
/// Serializes as `{"kind": "meeting", "event": {...}, "leadMinutes": 5}` or
/// `{"kind": "task", "taskId": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ReminderKey {
    /// A meeting reminder `lead_minutes` before the event starts.
    #[serde(rename_all = "camelCase")]
    Meeting {
        /// The event occurrence.
        event: EventKey,
        /// Lead time in minutes before the start (one of the configured lead times).
        lead_minutes: u32,
    },
    /// A task reminder at the task's due time.
    #[serde(rename_all = "camelCase")]
    Task {
        /// The task.
        task_id: TaskId,
    },
}

impl ReminderKey {
    /// The kind of reminder.
    pub fn kind(&self) -> ReminderKind {
        match self {
            Self::Meeting { .. } => ReminderKind::Meeting,
            Self::Task { .. } => ReminderKind::Task,
        }
    }
}

/// How a reminder stopped being pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReminderOutcome {
    /// The user dismissed the bubble.
    Dismissed,
    /// The user marked the task done (from the bubble or the popover).
    MarkedDone,
    /// Never shown: its due time had already passed when the app started
    /// (spec 7.1), or the meeting started while reminders were paused (7.5).
    Skipped,
}

/// Lifecycle state of a reminder instance. A reminder that is currently on
/// screen or queued is still `Pending`; the shown/queued state is runtime-only
/// and is not persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ReminderStatus {
    /// Waiting to fire, or firing now.
    Pending,
    /// Snoozed; fires again at `until`.
    Snoozed {
        /// When it fires again.
        until: DateTime<Utc>,
    },
    /// Done with; never fires again.
    Handled {
        /// When it was handled.
        at: DateTime<Utc>,
        /// How it was handled.
        outcome: ReminderOutcome,
    },
}

/// A concrete reminder with its due time and state, as the scheduler sees it
/// and as storage persists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderInstance {
    /// Identity.
    pub key: ReminderKey,
    /// Original due instant: event start minus lead time, or the task's `due_at`.
    pub due_at: DateTime<Utc>,
    /// Current state.
    pub status: ReminderStatus,
}

impl ReminderInstance {
    /// When this reminder should next fire, or `None` if it is handled.
    /// A snoozed reminder fires at its snooze time, otherwise at `due_at`.
    pub fn next_fire_at(&self) -> Option<DateTime<Utc>> {
        match self.status {
            ReminderStatus::Pending => Some(self.due_at),
            ReminderStatus::Snoozed { until } => Some(until),
            ReminderStatus::Handled { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AccountId, CalendarId, EventId};
    use chrono::TimeZone;

    fn meeting_key() -> ReminderKey {
        ReminderKey::Meeting {
            event: EventKey {
                account_id: AccountId::new("a"),
                calendar_id: CalendarId::new("c"),
                event_id: EventId::new("e"),
            },
            lead_minutes: 5,
        }
    }

    #[test]
    fn key_json_shape() {
        assert_eq!(
            serde_json::to_value(meeting_key()).unwrap(),
            serde_json::json!({
                "kind": "meeting",
                "event": {"accountId": "a", "calendarId": "c", "eventId": "e"},
                "leadMinutes": 5
            })
        );
        let task = ReminderKey::Task {
            task_id: TaskId::new("t"),
        };
        assert_eq!(
            serde_json::to_value(&task).unwrap(),
            serde_json::json!({"kind": "task", "taskId": "t"})
        );
        assert_eq!(task.kind(), ReminderKind::Task);
        assert_eq!(meeting_key().kind(), ReminderKind::Meeting);
    }

    #[test]
    fn next_fire_at_follows_status() {
        let due = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 11, 16, 9, 5, 0).unwrap();
        let mut r = ReminderInstance {
            key: meeting_key(),
            due_at: due,
            status: ReminderStatus::Pending,
        };
        assert_eq!(r.next_fire_at(), Some(due));
        r.status = ReminderStatus::Snoozed { until: later };
        assert_eq!(r.next_fire_at(), Some(later));
        r.status = ReminderStatus::Handled {
            at: later,
            outcome: ReminderOutcome::Dismissed,
        };
        assert_eq!(r.next_fire_at(), None);
    }
}
