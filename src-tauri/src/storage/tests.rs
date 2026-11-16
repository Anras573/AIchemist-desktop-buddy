//! Schema and migration tests.
//!
//! They use the repositories' row mappers to prove the schema can hold every
//! domain value. Repository behaviour is tested next to each repository.

use std::path::PathBuf;

use chrono::{NaiveDate, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::codec::*;
use super::events::{event_from_row, insert_event};
use super::reminders::{reminder_from_row, status_columns};
use super::tasks::{insert_task, task_from_row};
use super::*;
use crate::domain::{
    AccountId, CalendarId, Event, EventId, EventTime, ReminderInstance, ReminderKey,
    ReminderOutcome, ReminderStatus, ResponseStatus, Task, TaskId,
};

/// A database file in the temp dir, deleted on drop.
struct TempDb(PathBuf);

impl TempDb {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "aichemist-storage-{}-{name}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("sqlite-journal"));
    }
}

fn tables(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn at(h: u32, m: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap()
}

fn date(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

// ---- migrations ----------------------------------------------------------

#[test]
fn schema_version_matches_migration_count() {
    assert_eq!(SCHEMA_VERSION as usize, MIGRATIONS.len());
    const { assert!(SCHEMA_VERSION >= 1) };
}

#[test]
fn migrates_empty_database() {
    let conn = open_in_memory().unwrap();
    assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    assert_eq!(
        tables(&conn),
        ["events", "reminders", "sync_state", "tasks"]
    );
    let fk: bool = conn
        .pragma_query_value(None, "foreign_keys", |r| r.get(0))
        .unwrap();
    assert!(fk);
}

#[test]
fn reopening_is_a_no_op() {
    let db = TempDb::new("reopen");
    {
        let conn = open(&db.0).unwrap();
        conn.execute(
            "INSERT INTO sync_state (account_id, last_success_at) VALUES ('a', 1)",
            [],
        )
        .unwrap();
    }
    let conn = open(&db.0).unwrap();
    assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM sync_state", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "data survives re-open");
    // Busy timeout is applied on every open.
    let timeout: i64 = conn
        .pragma_query_value(None, "busy_timeout", |r| r.get(0))
        .unwrap();
    assert_eq!(timeout, 5000);
}

#[test]
fn applies_only_missing_migrations() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&mut conn, &["CREATE TABLE a (x INTEGER) STRICT;"]).unwrap();
    assert_eq!(user_version(&conn).unwrap(), 1);
    // Migration 1 would fail if it ran again (table exists).
    apply_migrations(
        &mut conn,
        &[
            "CREATE TABLE a (x INTEGER) STRICT;",
            "CREATE TABLE b (y INTEGER) STRICT;",
        ],
    )
    .unwrap();
    assert_eq!(user_version(&conn).unwrap(), 2);
    assert_eq!(tables(&conn), ["a", "b"]);
}

#[test]
fn failed_migration_rolls_back_everything() {
    let mut conn = Connection::open_in_memory().unwrap();
    let err = apply_migrations(
        &mut conn,
        &["CREATE TABLE a (x INTEGER) STRICT;", "THIS IS NOT SQL;"],
    )
    .unwrap_err();
    assert!(matches!(err, Error::Storage { .. }));
    assert!(err.to_string().contains("migration 2"));
    assert_eq!(user_version(&conn).unwrap(), 0);
    assert!(tables(&conn).is_empty());
}

#[test]
fn refuses_newer_database() {
    let db = TempDb::new("newer");
    {
        let conn = Connection::open(&db.0).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let err = open(&db.0).unwrap_err();
    assert!(matches!(err, Error::Storage { .. }));
}

// ---- tasks ---------------------------------------------------------------

fn task(id: &str) -> Task {
    Task {
        id: TaskId::new(id),
        title: "Water the plants".into(),
        due_at: at(9, 0),
        done: false,
        done_at: None,
        created_at: at(8, 0),
    }
}

#[test]
fn task_round_trips() {
    let conn = open_in_memory().unwrap();
    let open_task = task("1");
    let done_task = Task {
        done: true,
        done_at: Some(at(9, 5)),
        ..task("2")
    };
    for t in [&open_task, &done_task] {
        insert_task(&conn, t).unwrap();
        let back = conn
            .query_row("SELECT * FROM tasks WHERE id = ?1", [t.id.as_str()], |r| {
                task_from_row(r)
            })
            .unwrap();
        assert_eq!(&back, t);
    }
}

#[test]
fn task_constraints() {
    let conn = open_in_memory().unwrap();
    insert_task(&conn, &task("1")).unwrap();
    assert!(insert_task(&conn, &task("1")).is_err(), "duplicate id");
    let empty = Task {
        title: String::new(),
        ..task("2")
    };
    assert!(insert_task(&conn, &empty).is_err(), "empty title");
    let long = Task {
        title: "é".repeat(Task::MAX_TITLE_LEN + 1),
        ..task("3")
    };
    assert!(insert_task(&conn, &long).is_err(), "title too long");
    let max = Task {
        title: "é".repeat(Task::MAX_TITLE_LEN),
        ..task("4")
    };
    insert_task(&conn, &max).expect("max length counts characters, not bytes");
    let inconsistent = Task {
        done: true,
        done_at: None,
        ..task("5")
    };
    assert!(
        insert_task(&conn, &inconsistent).is_err(),
        "done without done_at"
    );
}

// ---- events --------------------------------------------------------------

fn timed_event(id: &str) -> Event {
    Event {
        id: EventId::new(id),
        account_id: AccountId::new("acc"),
        calendar_id: CalendarId::new("cal"),
        title: "Standup".into(),
        time: EventTime::Timed {
            start: at(9, 0),
            end: at(9, 15),
        },
        location: Some("Room 1".into()),
        response: ResponseStatus::Accepted,
    }
}

fn all_day_event(id: &str) -> Event {
    Event {
        title: String::new(),
        time: EventTime::AllDay {
            start: date(16),
            end: date(17),
        },
        location: None,
        response: ResponseStatus::Unknown,
        ..timed_event(id)
    }
}

#[test]
fn event_round_trips_timed_and_all_day() {
    let conn = open_in_memory().unwrap();
    for e in [timed_event("t"), all_day_event("d")] {
        insert_event(&conn, &e).unwrap();
        let back = conn
            .query_row(
                "SELECT * FROM events WHERE account_id = ?1 AND calendar_id = ?2 AND event_id = ?3",
                params![e.account_id.as_str(), e.calendar_id.as_str(), e.id.as_str()],
                event_from_row,
            )
            .unwrap();
        assert_eq!(back, e);
    }
}

#[test]
fn event_primary_key_is_the_event_key() {
    let conn = open_in_memory().unwrap();
    insert_event(&conn, &timed_event("e")).unwrap();
    assert!(insert_event(&conn, &timed_event("e")).is_err());
    // The same event id is fine in another calendar or account.
    let other_cal = Event {
        calendar_id: CalendarId::new("cal2"),
        ..timed_event("e")
    };
    insert_event(&conn, &other_cal).unwrap();
    let other_acc = Event {
        account_id: AccountId::new("acc2"),
        ..timed_event("e")
    };
    insert_event(&conn, &other_acc).unwrap();
}

#[test]
fn event_time_constraints() {
    let conn = open_in_memory().unwrap();
    let backwards = Event {
        time: EventTime::Timed {
            start: at(10, 0),
            end: at(9, 0),
        },
        ..timed_event("1")
    };
    assert!(insert_event(&conn, &backwards).is_err());
    let empty_day = Event {
        time: EventTime::AllDay {
            start: date(16),
            end: date(16),
        },
        ..all_day_event("2")
    };
    assert!(insert_event(&conn, &empty_day).is_err());
    // Mixed representation is rejected.
    let mixed = conn.execute(
        "INSERT INTO events (account_id, calendar_id, event_id, title, time_kind,
                             start_at, end_at, start_date, end_date, response)
         VALUES ('a', 'c', 'e', '', 'timed', 1, 2, '2026-11-16', '2026-11-17', 'unknown')",
        [],
    );
    assert!(mixed.is_err());
    let bad_response = conn.execute(
        "INSERT INTO events (account_id, calendar_id, event_id, title, time_kind,
                             start_at, end_at, response)
         VALUES ('a', 'c', 'e', '', 'timed', 1, 2, 'maybe')",
        [],
    );
    assert!(bad_response.is_err());
}

#[test]
fn range_queries_use_the_start_indexes() {
    let conn = open_in_memory().unwrap();
    for (sql, index) in [
        (
            "SELECT * FROM events WHERE start_at >= 0 AND start_at < 10",
            "events_start_at",
        ),
        (
            "SELECT * FROM events WHERE start_date >= '2026-11-16' AND start_date < '2026-11-23'",
            "events_start_date",
        ),
    ] {
        let plan: String = conn
            .query_row(&format!("EXPLAIN QUERY PLAN {sql}"), [], |r| r.get(3))
            .unwrap();
        assert!(plan.contains(index), "{plan}");
    }
}

// ---- sync state ----------------------------------------------------------

#[test]
fn sync_state_error_columns_go_together() {
    let conn = open_in_memory().unwrap();
    conn.execute(
        "INSERT INTO sync_state (account_id, last_attempt_at, last_error_kind,
                                 last_error_message, last_error_at)
         VALUES ('a', 1, ?1, 'timeout', 1)",
        [error_kind_to_sql(&Error::network("timeout"))],
    )
    .unwrap();
    let half = conn.execute(
        "INSERT INTO sync_state (account_id, last_error_kind) VALUES ('b', 'network')",
        [],
    );
    assert!(half.is_err());
    let bad_kind = conn.execute(
        "INSERT INTO sync_state (account_id, last_error_kind, last_error_message, last_error_at)
         VALUES ('c', 'oops', 'x', 1)",
        [],
    );
    assert!(bad_kind.is_err());
    assert!(conn
        .execute("INSERT INTO sync_state (account_id) VALUES ('a')", [])
        .is_err());
}

// ---- reminders -----------------------------------------------------------

fn insert_reminder(conn: &Connection, r: &ReminderInstance) -> rusqlite::Result<usize> {
    let (kind, acc, cal, ev, lead, task_id) = match &r.key {
        ReminderKey::Meeting {
            event,
            lead_minutes,
        } => (
            "meeting",
            Some(event.account_id.as_str()),
            Some(event.calendar_id.as_str()),
            Some(event.event_id.as_str()),
            Some(*lead_minutes),
            None,
        ),
        ReminderKey::Task { task_id } => ("task", None, None, None, None, Some(task_id.as_str())),
    };
    let (status, until, handled_at, outcome) = status_columns(&r.status);
    conn.execute(
        "INSERT INTO reminders (kind, account_id, calendar_id, event_id, lead_minutes, task_id,
                                due_at, status, snoozed_until, handled_at, outcome)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            kind,
            acc,
            cal,
            ev,
            lead,
            task_id,
            instant_to_sql(r.due_at),
            status,
            until,
            handled_at,
            outcome
        ],
    )
}

fn meeting_key(lead_minutes: u32) -> ReminderKey {
    ReminderKey::Meeting {
        event: timed_event("e").key(),
        lead_minutes,
    }
}

fn task_key(id: &str) -> ReminderKey {
    ReminderKey::Task {
        task_id: TaskId::new(id),
    }
}

fn pending(key: ReminderKey) -> ReminderInstance {
    ReminderInstance {
        key,
        due_at: at(8, 50),
        status: ReminderStatus::Pending,
    }
}

#[test]
fn reminder_round_trips_every_key_and_status() {
    let conn = open_in_memory().unwrap();
    let all = [
        pending(meeting_key(10)),
        ReminderInstance {
            status: ReminderStatus::Snoozed { until: at(8, 55) },
            ..pending(meeting_key(5))
        },
        ReminderInstance {
            status: ReminderStatus::Handled {
                at: at(8, 51),
                outcome: ReminderOutcome::Dismissed,
            },
            ..pending(meeting_key(1))
        },
        ReminderInstance {
            status: ReminderStatus::Handled {
                at: at(9, 1),
                outcome: ReminderOutcome::MarkedDone,
            },
            ..pending(task_key("t1"))
        },
        ReminderInstance {
            status: ReminderStatus::Handled {
                at: at(9, 1),
                outcome: ReminderOutcome::Skipped,
            },
            ..pending(task_key("t2"))
        },
    ];
    for r in &all {
        insert_reminder(&conn, r).unwrap();
    }
    let mut stmt = conn.prepare("SELECT * FROM reminders ORDER BY id").unwrap();
    let back: Vec<ReminderInstance> = stmt
        .query_map([], reminder_from_row)
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(back, all);
}

#[test]
fn reminder_key_is_unique() {
    let conn = open_in_memory().unwrap();
    insert_reminder(&conn, &pending(meeting_key(5))).unwrap();
    assert!(
        insert_reminder(&conn, &pending(meeting_key(5))).is_err(),
        "same (event, lead) twice"
    );
    insert_reminder(&conn, &pending(meeting_key(10))).expect("other lead time");

    insert_reminder(&conn, &pending(task_key("t"))).unwrap();
    assert!(
        insert_reminder(&conn, &pending(task_key("t"))).is_err(),
        "same task twice"
    );
    insert_reminder(&conn, &pending(task_key("u"))).expect("other task");
}

#[test]
fn reminder_upsert_targets_the_partial_unique_index() {
    // The pattern repositories use to record a state change without ever
    // creating a second row for the same key.
    let conn = open_in_memory().unwrap();
    insert_reminder(&conn, &pending(task_key("t"))).unwrap();
    conn.execute(
        "INSERT INTO reminders (kind, task_id, due_at, status, handled_at, outcome)
         VALUES ('task', 't', 0, 'handled', 1, 'dismissed')
         ON CONFLICT (task_id) WHERE kind = 'task' DO UPDATE SET
             status = excluded.status, snoozed_until = excluded.snoozed_until,
             handled_at = excluded.handled_at, outcome = excluded.outcome",
        [],
    )
    .unwrap();
    let (n, status): (i64, String) = conn
        .query_row("SELECT count(*), max(status) FROM reminders", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((n, status.as_str()), (1, "handled"));
}

#[test]
fn reminder_shape_constraints() {
    let conn = open_in_memory().unwrap();
    let cases = [
        // Meeting without lead time.
        "INSERT INTO reminders (kind, account_id, calendar_id, event_id, due_at, status)
         VALUES ('meeting', 'a', 'c', 'e', 0, 'pending')",
        // Task row carrying meeting columns.
        "INSERT INTO reminders (kind, task_id, account_id, due_at, status)
         VALUES ('task', 't', 'a', 0, 'pending')",
        // Snoozed without until.
        "INSERT INTO reminders (kind, task_id, due_at, status)
         VALUES ('task', 't', 0, 'snoozed')",
        // Handled without outcome.
        "INSERT INTO reminders (kind, task_id, due_at, status, handled_at)
         VALUES ('task', 't', 0, 'handled', 1)",
        // Pending with leftover snooze.
        "INSERT INTO reminders (kind, task_id, due_at, status, snoozed_until)
         VALUES ('task', 't', 0, 'pending', 1)",
        // Unknown outcome.
        "INSERT INTO reminders (kind, task_id, due_at, status, handled_at, outcome)
         VALUES ('task', 't', 0, 'handled', 1, 'ignored')",
    ];
    for sql in cases {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }
    let n: Option<i64> = conn
        .query_row("SELECT id FROM reminders", [], |r| r.get(0))
        .optional()
        .unwrap();
    assert_eq!(n, None);
}
