//! Task repository: the local [`TaskSource`] (spec 7.2, 7.3).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::codec::{decode, instant_from_sql, instant_to_sql};
use super::handle::whole_seconds;
use super::Storage;
use crate::domain::{Error, FieldError, Result, Task, TaskId, TaskSource};

const SELECT_COLUMNS: &str = "id, title, due_at, done, done_at, created_at";

impl Storage {
    /// Creates an open task due at `due_at`, stamped `created_at = now`.
    ///
    /// The title is trimmed and must then be 1 to [`Task::MAX_TITLE_LEN`]
    /// characters, otherwise this fails with [`Error::Validation`] on the
    /// field `title`. Times are truncated to whole seconds, as stored.
    pub fn create_task(&self, title: &str, due_at: DateTime<Utc>) -> Result<Task> {
        let task = Task {
            id: new_task_id(),
            title: validate_title(title)?,
            due_at: whole_seconds(due_at),
            done: false,
            done_at: None,
            created_at: self.now(),
        };
        insert_task(&self.conn(), &task)?;
        Ok(task)
    }

    /// The task with `id`, or `None`.
    pub fn get_task(&self, id: &TaskId) -> Result<Option<Task>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {SELECT_COLUMNS} FROM tasks WHERE id = ?1"),
                [id.as_str()],
                task_from_row,
            )
            .optional()?)
    }

    /// All tasks, open and done, ordered by due time (then id).
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM tasks ORDER BY due_at, id"
        ))?;
        let tasks = stmt
            .query_map([], task_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(tasks)
    }

    /// Changes a task's title and due time (same title rules as
    /// [`Storage::create_task`]). Fails with [`Error::NotFound`] if there is
    /// no such task.
    pub fn update_task(&self, id: &TaskId, title: &str, due_at: DateTime<Utc>) -> Result<Task> {
        let title = validate_title(title)?;
        self.conn()
            .query_row(
                &format!(
                    "UPDATE tasks SET title = ?2, due_at = ?3 WHERE id = ?1
                     RETURNING {SELECT_COLUMNS}"
                ),
                params![id.as_str(), title, instant_to_sql(due_at)],
                task_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found(id))
    }

    /// Marks a task done (`done_at = now`) or undone (`done_at` cleared).
    /// Marking an already done task done again keeps its original `done_at`.
    /// Fails with [`Error::NotFound`] if there is no such task.
    pub fn set_task_done(&self, id: &TaskId, done: bool) -> Result<Task> {
        self.conn()
            .query_row(
                &format!(
                    "UPDATE tasks
                     SET done = ?2,
                         done_at = CASE WHEN ?2 THEN coalesce(done_at, ?3) ELSE NULL END
                     WHERE id = ?1
                     RETURNING {SELECT_COLUMNS}"
                ),
                params![id.as_str(), done, instant_to_sql(self.now())],
                task_from_row,
            )
            .optional()?
            .ok_or_else(|| not_found(id))
    }

    /// Deletes a task. Fails with [`Error::NotFound`] if there is no such
    /// task. Its reminder row belongs to the scheduler's ledger and goes away
    /// when the scheduler saves a ledger without it.
    pub fn delete_task(&self, id: &TaskId) -> Result<()> {
        let n = self
            .conn()
            .execute("DELETE FROM tasks WHERE id = ?1", [id.as_str()])?;
        if n == 0 {
            return Err(not_found(id));
        }
        Ok(())
    }
}

/// The local, SQLite-backed task source.
impl TaskSource for Storage {
    fn fetch_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks()
    }
}

/// Trims `title` and checks its length in characters.
pub fn validate_title(title: &str) -> Result<String> {
    let title = title.trim();
    let len = title.chars().count();
    let message = if len == 0 {
        "must not be empty".to_owned()
    } else if len > Task::MAX_TITLE_LEN {
        format!("must be at most {} characters", Task::MAX_TITLE_LEN)
    } else {
        return Ok(title.to_owned());
    };
    Err(Error::Validation {
        errors: vec![FieldError::new("title", message)],
    })
}

/// A new random task id: 128 bits as 32 lowercase hex digits.
///
/// Built from std only, to avoid a dependency: `RandomState::new()` hashers
/// are keyed with OS randomness (and each call gets fresh keys), and the
/// input mixes the time, the process id and a process-wide counter. Ids only
/// need to be unique within this database; the primary key rejects the
/// astronomically unlikely collision rather than overwriting a task.
pub fn new_task_id() -> TaskId {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut id = String::with_capacity(32);
    for half in 0u8..2 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u64(count);
        h.write_u32(std::process::id());
        h.write_u8(half);
        id.push_str(&format!("{:016x}", h.finish()));
    }
    TaskId::new(id)
}

fn not_found(id: &TaskId) -> Error {
    Error::not_found(format!("task {id} not found"))
}

/// Inserts a task row as is (no validation beyond the schema's).
pub(super) fn insert_task(conn: &Connection, t: &Task) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO tasks (id, title, due_at, done, done_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            t.id.as_str(),
            t.title,
            instant_to_sql(t.due_at),
            t.done,
            t.done_at.map(instant_to_sql),
            instant_to_sql(t.created_at),
        ],
    )
}

/// Maps a `tasks` row (selected with all columns) to a [`Task`].
pub(super) fn task_from_row(r: &Row) -> rusqlite::Result<Task> {
    let done_at: Option<i64> = r.get("done_at")?;
    Ok(Task {
        id: TaskId::new(r.get::<_, String>("id")?),
        title: r.get("title")?,
        due_at: decode(instant_from_sql(r.get("due_at")?))?,
        done: r.get("done")?,
        done_at: decode(done_at.map(instant_from_sql).transpose())?,
        created_at: decode(instant_from_sql(r.get("created_at")?))?,
    })
}
