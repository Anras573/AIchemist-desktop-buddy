//! Local one-off tasks (spec section 7.2).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::TaskId;

/// A one-off task with a due time. Task titles must not be logged at default
/// log levels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Unique id, assigned by the owning [`TaskSource`](super::TaskSource).
    pub id: TaskId,
    /// Title. Non-empty, at most [`Task::MAX_TITLE_LEN`] characters.
    pub title: String,
    /// When the task reminder fires.
    pub due_at: DateTime<Utc>,
    /// Whether the task is done.
    pub done: bool,
    /// When it was marked done. `Some` exactly when `done` is true.
    pub done_at: Option<DateTime<Utc>>,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

impl Task {
    /// Maximum title length in characters.
    pub const MAX_TITLE_LEN: usize = 200;

    /// Whether the task is not done and its due time is at or before `now`.
    /// Used for the "overdue" label in the popover.
    pub fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        !self.done && self.due_at <= now
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    #[test]
    fn overdue_only_when_open_and_past_due() {
        let now = Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap();
        let mut task = Task {
            id: TaskId::new("1"),
            title: "Water plants".into(),
            due_at: now - Duration::minutes(1),
            done: false,
            done_at: None,
            created_at: now - Duration::days(1),
        };
        assert!(task.is_overdue(now));
        task.done = true;
        task.done_at = Some(now);
        assert!(!task.is_overdue(now));
        task.done = false;
        task.due_at = now + Duration::minutes(1);
        assert!(!task.is_overdue(now));
    }
}
