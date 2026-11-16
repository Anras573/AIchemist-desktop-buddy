//! Repository and settings-file tests.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};

use super::*;
use crate::domain::{
    AccountConfig, AccountConnection, AccountId, CalendarId, Error, Event, EventId, EventKey,
    EventTime, FakeClock, ReminderInstance, ReminderKey, ReminderOutcome, ReminderStatus,
    ResponseStatus, Settings, Task, TaskId, TaskSource,
};

fn at(d: u32, h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap()
}

fn date(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
}

/// A storage handle at 16 Nov 09:00 UTC with a UTC local zone.
fn storage() -> (Storage, Arc<FakeClock>) {
    storage_with_offset(0)
}

fn storage_with_offset(hours: i32) -> (Storage, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock::with_offset(
        at(16, 9, 0),
        FixedOffset::east_opt(hours * 3600).unwrap(),
    ));
    (Storage::open_in_memory(clock.clone()).unwrap(), clock)
}

fn assert_validation(err: Error, field: &str) {
    match err {
        Error::Validation { errors } => {
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].field, field);
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// ---- tasks ---------------------------------------------------------------

#[test]
fn create_and_get_task() {
    let (s, _) = storage();
    let due = at(17, 8, 0) + Duration::milliseconds(400);
    let task = s.create_task("  Water the plants ", due).unwrap();
    assert_eq!(task.title, "Water the plants", "title is trimmed");
    assert_eq!(task.due_at, at(17, 8, 0), "whole seconds, as stored");
    assert_eq!(task.created_at, at(16, 9, 0));
    assert!(!task.done && task.done_at.is_none());
    assert_eq!(s.get_task(&task.id).unwrap(), Some(task));
    assert_eq!(s.get_task(&TaskId::new("nope")).unwrap(), None);
}

#[test]
fn task_ids_are_unique_128_bit_hex() {
    let (s, _) = storage();
    let a = s.create_task("a", at(17, 8, 0)).unwrap().id;
    let b = s.create_task("b", at(17, 8, 0)).unwrap().id;
    assert_ne!(a, b);
    for id in [&a, &b] {
        assert_eq!(id.as_str().len(), 32);
        assert!(id
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
    let many: std::collections::HashSet<_> = (0..1000).map(|_| tasks::new_task_id()).collect();
    assert_eq!(many.len(), 1000);
}

#[test]
fn task_title_validation() {
    let (s, _) = storage();
    assert_validation(s.create_task("", at(17, 8, 0)).unwrap_err(), "title");
    assert_validation(s.create_task("   ", at(17, 8, 0)).unwrap_err(), "title");
    let too_long = "é".repeat(Task::MAX_TITLE_LEN + 1);
    assert_validation(s.create_task(&too_long, at(17, 8, 0)).unwrap_err(), "title");
    let max = "é".repeat(Task::MAX_TITLE_LEN);
    s.create_task(&max, at(17, 8, 0))
        .expect("length counts characters");
    assert_eq!(s.list_tasks().unwrap().len(), 1, "invalid tasks not stored");

    let id = s.list_tasks().unwrap()[0].id.clone();
    assert_validation(s.update_task(&id, " ", at(17, 8, 0)).unwrap_err(), "title");
}

#[test]
fn list_tasks_orders_by_due_time() {
    let (s, _) = storage();
    let late = s.create_task("late", at(18, 8, 0)).unwrap();
    let early = s.create_task("early", at(17, 8, 0)).unwrap();
    let done = s.create_task("done", at(17, 9, 0)).unwrap();
    let done = s.set_task_done(&done.id, true).unwrap();
    assert_eq!(s.list_tasks().unwrap(), vec![early, done, late]);
}

#[test]
fn update_task() {
    let (s, _) = storage();
    let task = s.create_task("old", at(17, 8, 0)).unwrap();
    let updated = s.update_task(&task.id, " new ", at(18, 7, 30)).unwrap();
    assert_eq!(
        updated,
        Task {
            title: "new".into(),
            due_at: at(18, 7, 30),
            ..task.clone()
        }
    );
    assert_eq!(s.get_task(&task.id).unwrap(), Some(updated));
    assert!(matches!(
        s.update_task(&TaskId::new("nope"), "x", at(18, 7, 30)),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn set_task_done_and_undone() {
    let (s, clock) = storage();
    let task = s.create_task("t", at(17, 8, 0)).unwrap();
    clock.set(at(16, 10, 0) + Duration::milliseconds(900));
    let done = s.set_task_done(&task.id, true).unwrap();
    assert!(done.done);
    assert_eq!(done.done_at, Some(at(16, 10, 0)));

    clock.set(at(16, 11, 0));
    let again = s.set_task_done(&task.id, true).unwrap();
    assert_eq!(
        again.done_at,
        Some(at(16, 10, 0)),
        "keeps the first done_at"
    );

    let undone = s.set_task_done(&task.id, false).unwrap();
    assert_eq!(undone, task, "undone clears done_at");
    assert_eq!(s.get_task(&task.id).unwrap(), Some(undone));
    assert!(matches!(
        s.set_task_done(&TaskId::new("nope"), true),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn delete_task() {
    let (s, _) = storage();
    let keep = s.create_task("keep", at(17, 8, 0)).unwrap();
    let gone = s.create_task("gone", at(17, 8, 0)).unwrap();
    s.delete_task(&gone.id).unwrap();
    assert_eq!(s.list_tasks().unwrap(), vec![keep]);
    assert!(matches!(
        s.delete_task(&gone.id),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn storage_is_the_local_task_source() {
    let (s, _) = storage();
    let a = s.create_task("a", at(17, 8, 0)).unwrap();
    let b = s.create_task("b", at(17, 9, 0)).unwrap();
    let b = s.set_task_done(&b.id, true).unwrap();
    let source: Box<dyn TaskSource> = Box::new(s.clone());
    assert_eq!(source.fetch_tasks().unwrap(), vec![a, b]);
}

#[test]
fn handle_is_shared_across_threads() {
    let (s, _) = storage();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || {
                for j in 0..10 {
                    s.create_task(&format!("{i}-{j}"), at(17, 8, 0)).unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.list_tasks().unwrap().len(), 40);
}

// ---- events --------------------------------------------------------------

fn timed(acc: &str, cal: &str, id: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Event {
    Event {
        id: EventId::new(id),
        account_id: AccountId::new(acc),
        calendar_id: CalendarId::new(cal),
        title: format!("Meeting {id}"),
        time: EventTime::Timed { start, end },
        location: Some("Room 1".into()),
        response: ResponseStatus::Accepted,
    }
}

fn all_day(acc: &str, cal: &str, id: &str, start: NaiveDate, end: NaiveDate) -> Event {
    Event {
        time: EventTime::AllDay { start, end },
        location: None,
        response: ResponseStatus::Unknown,
        ..timed(acc, cal, id, at(16, 0, 0), at(16, 0, 0))
    }
}

fn cals(ids: &[&str]) -> Vec<CalendarId> {
    ids.iter().map(|c| CalendarId::new(*c)).collect()
}

fn ids(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.id.as_str()).collect()
}

fn all_events(s: &Storage) -> Vec<Event> {
    s.events_in_window(at(1, 0, 0), at(30, 0, 0)).unwrap()
}

#[test]
fn replace_events_round_trips_and_replaces() {
    let (s, _) = storage();
    let acc = AccountId::new("a");
    let first = vec![
        timed("a", "c1", "1", at(16, 10, 0), at(16, 11, 0)),
        all_day("a", "c2", "2", date(17), date(18)),
    ];
    s.replace_events(&acc, &cals(&["c1", "c2"]), &first)
        .unwrap();
    assert_eq!(all_events(&s), first);

    let second = vec![timed("a", "c1", "3", at(16, 12, 0), at(16, 13, 0))];
    s.replace_events(&acc, &cals(&["c1", "c2"]), &second)
        .unwrap();
    assert_eq!(all_events(&s), second, "old events are gone");
}

#[test]
fn replace_events_is_per_account_and_drops_excluded_calendars() {
    let (s, _) = storage();
    let other = vec![timed("b", "c1", "b1", at(16, 10, 0), at(16, 11, 0))];
    s.replace_events(&AccountId::new("b"), &cals(&["c1"]), &other)
        .unwrap();
    s.replace_events(
        &AccountId::new("a"),
        &cals(&["c1", "c2"]),
        &[
            timed("a", "c1", "a1", at(16, 10, 0), at(16, 11, 0)),
            timed("a", "c2", "a2", at(16, 10, 0), at(16, 11, 0)),
        ],
    )
    .unwrap();
    // c2 is no longer included.
    s.replace_events(
        &AccountId::new("a"),
        &cals(&["c1"]),
        &[timed("a", "c1", "a1", at(16, 10, 0), at(16, 11, 0))],
    )
    .unwrap();
    assert_eq!(ids(&all_events(&s)), ["a1", "b1"]);
}

#[test]
fn failed_replace_keeps_the_last_good_cache() {
    let (s, _) = storage();
    let acc = AccountId::new("a");
    let good = vec![timed("a", "c", "1", at(16, 10, 0), at(16, 11, 0))];
    s.replace_events(&acc, &cals(&["c"]), &good).unwrap();

    // A row the schema rejects (end before start) after a valid one: the
    // transaction rolls back, including the delete.
    let bad = vec![
        timed("a", "c", "2", at(16, 10, 0), at(16, 11, 0)),
        timed("a", "c", "3", at(16, 11, 0), at(16, 10, 0)),
    ];
    assert!(matches!(
        s.replace_events(&acc, &cals(&["c"]), &bad),
        Err(Error::Storage { .. })
    ));
    assert_eq!(all_events(&s), good);

    // Events outside the replaced set are refused up front.
    for foreign in [
        timed("b", "c", "4", at(16, 10, 0), at(16, 11, 0)),
        timed("a", "x", "5", at(16, 10, 0), at(16, 11, 0)),
    ] {
        assert!(matches!(
            s.replace_events(&acc, &cals(&["c"]), &[foreign]),
            Err(Error::InvalidInput { .. })
        ));
    }
    assert_eq!(all_events(&s), good);
}

#[test]
fn replace_events_keeps_the_last_duplicate() {
    let (s, _) = storage();
    let mut dup = timed("a", "c", "1", at(16, 10, 0), at(16, 11, 0));
    let first = dup.clone();
    dup.title = "Renamed".into();
    s.replace_events(&AccountId::new("a"), &cals(&["c"]), &[first, dup.clone()])
        .unwrap();
    assert_eq!(all_events(&s), vec![dup]);
}

#[test]
fn window_query_on_timed_events() {
    let (s, _) = storage();
    let events = vec![
        timed("a", "c", "before", at(16, 8, 0), at(16, 9, 0)), // ends at `from`
        timed("a", "c", "overlap-start", at(16, 8, 30), at(16, 9, 30)),
        timed("a", "c", "inside", at(16, 10, 0), at(16, 11, 0)),
        timed("a", "c", "spanning", at(15, 0, 0), at(18, 0, 0)),
        timed("a", "c", "after", at(16, 12, 0), at(16, 13, 0)), // starts at `to`
    ];
    s.replace_events(&AccountId::new("a"), &cals(&["c"]), &events)
        .unwrap();
    let got = s.events_in_window(at(16, 9, 0), at(16, 12, 0)).unwrap();
    assert_eq!(ids(&got), ["spanning", "overlap-start", "inside"]);
    assert!(s
        .events_in_window(at(16, 12, 0), at(16, 9, 0))
        .unwrap()
        .is_empty());
}

#[test]
fn window_query_on_all_day_events_uses_local_dates() {
    // Local time is UTC+2. The window 16 Nov 22:30Z..23:30Z is local
    // 17 Nov 00:30..01:30, so it overlaps the all-day event on the 17th but
    // not the one on the 16th, although in UTC it is the other way round.
    let (s, _) = storage_with_offset(2);
    let events = vec![
        all_day("a", "c", "16th", date(16), date(17)),
        all_day("a", "c", "17th", date(17), date(18)),
        all_day("a", "c", "18th", date(18), date(19)),
        timed("a", "c", "timed", at(16, 23, 0), at(16, 23, 15)),
    ];
    s.replace_events(&AccountId::new("a"), &cals(&["c"]), &events)
        .unwrap();
    let got = s.events_in_window(at(16, 22, 30), at(16, 23, 30)).unwrap();
    // Sorted by start: the 17th starts at local midnight (16 Nov 22:00Z).
    assert_eq!(ids(&got), ["17th", "timed"]);

    // A window ending exactly at local midnight of the 18th does not reach
    // the event on the 18th; one minute later it does.
    let to = at(17, 22, 0);
    assert_eq!(
        ids(&s.events_in_window(at(17, 12, 0), to).unwrap()),
        ["17th"]
    );
    assert_eq!(
        ids(&s
            .events_in_window(at(17, 12, 0), to + Duration::minutes(1))
            .unwrap()),
        ["17th", "18th"]
    );
}

#[test]
fn remove_account_drops_its_events_and_sync_state() {
    let (s, _) = storage();
    for acc in ["a", "b"] {
        let id = AccountId::new(acc);
        s.replace_events(
            &id,
            &cals(&["c"]),
            &[timed(acc, "c", acc, at(16, 10, 0), at(16, 11, 0))],
        )
        .unwrap();
        s.record_sync_success(&id).unwrap();
    }
    s.remove_account(&AccountId::new("a")).unwrap();
    assert_eq!(ids(&all_events(&s)), ["b"]);
    assert_eq!(s.sync_state(&AccountId::new("a")).unwrap(), None);
    assert!(s.sync_state(&AccountId::new("b")).unwrap().is_some());
}

// ---- reminder ledger -----------------------------------------------------

fn meeting(event: &str, lead_minutes: u32, status: ReminderStatus) -> ReminderInstance {
    ReminderInstance {
        key: ReminderKey::Meeting {
            event: EventKey {
                account_id: AccountId::new("a"),
                calendar_id: CalendarId::new("c"),
                event_id: EventId::new(event),
            },
            lead_minutes,
        },
        due_at: at(16, 10, 0) - Duration::minutes(i64::from(lead_minutes)),
        status,
    }
}

fn task_reminder(task: &str, status: ReminderStatus) -> ReminderInstance {
    ReminderInstance {
        key: ReminderKey::Task {
            task_id: TaskId::new(task),
        },
        due_at: at(16, 12, 0),
        status,
    }
}

fn handled(outcome: ReminderOutcome) -> ReminderStatus {
    ReminderStatus::Handled {
        at: at(16, 9, 55),
        outcome,
    }
}

fn row_count(s: &Storage, table: &str) -> i64 {
    s.conn()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn ledger_round_trips_every_status() {
    let (s, _) = storage();
    assert!(s.load_reminders().unwrap().is_empty());
    let ledger = vec![
        meeting("e", 10, ReminderStatus::Pending),
        meeting(
            "e",
            5,
            ReminderStatus::Snoozed {
                until: at(16, 9, 58),
            },
        ),
        meeting("e", 1, handled(ReminderOutcome::Dismissed)),
        task_reminder("t1", ReminderStatus::Pending),
        task_reminder("t2", handled(ReminderOutcome::MarkedDone)),
        task_reminder("t3", handled(ReminderOutcome::Skipped)),
    ];
    s.save_reminders(&ledger).unwrap();
    assert_eq!(s.load_reminders().unwrap(), ledger);
}

#[test]
fn ledger_save_updates_in_place_and_deletes_missing_rows() {
    let (s, _) = storage();
    s.save_reminders(&[
        meeting("e", 10, ReminderStatus::Pending),
        meeting("e", 5, ReminderStatus::Pending),
        task_reminder("t1", ReminderStatus::Pending),
        task_reminder("t2", ReminderStatus::Pending),
    ])
    .unwrap();

    let mut moved = task_reminder("t1", handled(ReminderOutcome::MarkedDone));
    moved.due_at = at(16, 13, 0);
    let next = vec![
        meeting("e", 10, handled(ReminderOutcome::Dismissed)),
        moved,
        meeting("f", 10, ReminderStatus::Pending),
    ];
    s.save_reminders(&next).unwrap();
    assert_eq!(s.load_reminders().unwrap(), next);
    assert_eq!(row_count(&s, "reminders"), 3);

    s.save_reminders(&[]).unwrap();
    assert!(s.load_reminders().unwrap().is_empty());
}

#[test]
fn ledger_save_is_atomic() {
    let (s, _) = storage();
    let ledger = vec![
        task_reminder("t1", ReminderStatus::Pending),
        meeting("e", 5, ReminderStatus::Pending),
    ];
    s.save_reminders(&ledger).unwrap();
    // Make the insert of one particular row fail, after other rows were
    // already updated in the same transaction.
    s.conn()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_boom BEFORE INSERT ON reminders
             WHEN NEW.task_id = 'boom'
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
    let bad = vec![
        task_reminder("t1", handled(ReminderOutcome::Dismissed)),
        task_reminder("boom", ReminderStatus::Pending),
    ];
    assert!(matches!(s.save_reminders(&bad), Err(Error::Storage { .. })));
    assert_eq!(s.load_reminders().unwrap(), ledger, "nothing changed");
}

// ---- sync state ----------------------------------------------------------

#[test]
fn sync_state_records_attempts_errors_and_success() {
    let (s, clock) = storage();
    let acc = AccountId::new("a");
    assert_eq!(s.sync_state(&acc).unwrap(), None);

    s.record_sync_attempt(&acc).unwrap();
    clock.advance(Duration::seconds(5));
    let error = Error::network("timed out");
    s.record_sync_failure(&acc, &error).unwrap();
    assert_eq!(
        s.sync_state(&acc).unwrap(),
        Some(AccountSyncRecord {
            account_id: acc.clone(),
            last_attempt_at: Some(at(16, 9, 0)),
            last_success_at: None,
            last_error: Some((error, at(16, 9, 0) + Duration::seconds(5))),
        })
    );

    clock.advance(Duration::minutes(10));
    s.record_sync_attempt(&acc).unwrap();
    s.record_sync_success(&acc).unwrap();
    let record = s.sync_state(&acc).unwrap().unwrap();
    let now = at(16, 9, 10) + Duration::seconds(5);
    assert_eq!(record.last_attempt_at, Some(now));
    assert_eq!(record.last_success_at, Some(now));
    assert_eq!(record.last_error, None, "success clears the error");

    // A later failure keeps the last success.
    s.record_sync_failure(&acc, &Error::auth("rejected"))
        .unwrap();
    let record = s.sync_state(&acc).unwrap().unwrap();
    assert_eq!(record.last_success_at, Some(now));
    assert!(matches!(record.last_error, Some((Error::Auth { .. }, _))));
    assert_eq!(s.list_sync_states().unwrap(), vec![record]);
}

#[test]
fn sync_error_messages_are_bounded() {
    let (s, _) = storage();
    let acc = AccountId::new("a");
    s.record_sync_failure(&acc, &Error::protocol("x".repeat(10_000)))
        .unwrap();
    let Some((Error::Protocol { message }, _)) = s.sync_state(&acc).unwrap().unwrap().last_error
    else {
        panic!("expected a protocol error");
    };
    assert_eq!(message.chars().count(), sync_state::MAX_ERROR_MESSAGE_LEN);
}

// ---- pruning -------------------------------------------------------------

#[test]
fn prune_removes_old_events_and_their_meeting_reminders() {
    // Now is 16 Nov 09:00 local (UTC+2), the cutoff 15 Nov 09:00 local.
    let (s, _) = storage_with_offset(2);
    let events = vec![
        timed("a", "c", "old", at(14, 9, 0), at(14, 10, 0)),
        timed("a", "c", "recent", at(15, 10, 0), at(15, 11, 0)),
        all_day("a", "c", "old-day", date(13), date(15)), // ended 15th 00:00 local
        all_day("a", "c", "today", date(15), date(17)),
    ];
    s.replace_events(&AccountId::new("a"), &cals(&["c"]), &events)
        .unwrap();
    let reminder = |event: &str, due: DateTime<Utc>| ReminderInstance {
        due_at: due,
        ..meeting(event, 5, handled(ReminderOutcome::Dismissed))
    };
    s.save_reminders(&[
        reminder("old", at(14, 8, 55)),
        reminder("recent", at(15, 9, 55)),
        // Cancelled (not cached), but due recently: kept for now.
        reminder("cancelled-recent", at(15, 20, 0)),
        // Cancelled and old: pruned.
        reminder("cancelled-old", at(10, 8, 55)),
        // Task reminders are left alone.
        task_reminder("t", handled(ReminderOutcome::Skipped)),
        ReminderInstance {
            due_at: at(1, 0, 0),
            ..task_reminder("old-task", handled(ReminderOutcome::Skipped))
        },
    ])
    .unwrap();

    let stats = s.prune(DEFAULT_RETENTION).unwrap();
    assert_eq!(
        stats,
        PruneStats {
            events: 2,
            reminders: 2
        }
    );
    assert_eq!(ids(&all_events(&s)), ["today", "recent"]);
    let left: Vec<String> = s
        .load_reminders()
        .unwrap()
        .into_iter()
        .map(|r| match r.key {
            ReminderKey::Meeting { event, .. } => event.event_id.into_inner(),
            ReminderKey::Task { task_id } => task_id.into_inner(),
        })
        .collect();
    assert_eq!(left, ["recent", "cancelled-recent", "t", "old-task"]);

    assert_eq!(s.prune(DEFAULT_RETENTION).unwrap(), PruneStats::default());
}

// ---- settings file -------------------------------------------------------

/// A fresh directory in the temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("aichemist-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join(SETTINGS_FILE_NAME)
    }

    fn write(&self, text: &str) -> PathBuf {
        std::fs::create_dir_all(&self.0).unwrap();
        std::fs::write(self.file(), text).unwrap();
        self.file()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn account(id: &str, server_url: &str) -> AccountConfig {
    AccountConfig {
        id: AccountId::new(id),
        display_name: "Work".into(),
        connection: AccountConnection::CalDav {
            server_url: server_url.into(),
            username: "me".into(),
        },
        included_calendars: vec![CalendarId::new("cal")],
    }
}

fn custom_settings() -> Settings {
    let mut s = Settings {
        accounts: vec![account("a1", "https://dav.example.com/")],
        ..Default::default()
    };
    s.reminders.sync_interval_minutes = 15;
    s.appearance.opacity_percent = 80;
    s.system.sleep_after_idle_minutes = 20;
    s
}

#[test]
fn missing_settings_file_gives_defaults() {
    let dir = TempDir::new("missing");
    assert_eq!(load_settings(&dir.file()), Settings::default());
}

#[test]
fn settings_save_and_load_round_trip() {
    let dir = TempDir::new("round-trip");
    let settings = custom_settings();
    // The directory does not exist yet; save creates it.
    save_settings(&dir.file(), &settings).unwrap();
    assert_eq!(load_settings(&dir.file()), settings);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.file()).unwrap()).unwrap();
    assert_eq!(
        json["system"]["sleepAfterIdleMinutes"], 20,
        "camelCase keys"
    );
}

#[test]
fn settings_save_replaces_atomically() {
    let dir = TempDir::new("atomic");
    save_settings(&dir.file(), &Settings::default()).unwrap();
    let settings = custom_settings();
    save_settings(&dir.file(), &settings).unwrap();
    assert_eq!(load_settings(&dir.file()), settings);
    let names: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [SETTINGS_FILE_NAME], "no temp file left behind");
}

#[test]
fn invalid_settings_are_not_saved() {
    let dir = TempDir::new("invalid-save");
    save_settings(&dir.file(), &custom_settings()).unwrap();
    let mut bad = custom_settings();
    bad.appearance.opacity_percent = 5;
    assert!(matches!(
        save_settings(&dir.file(), &bad),
        Err(Error::Validation { .. })
    ));
    assert_eq!(load_settings(&dir.file()), custom_settings(), "untouched");
}

#[test]
fn malformed_settings_json_gives_defaults() {
    let dir = TempDir::new("malformed");
    for text in ["{ not json", "", "[1, 2]", "42"] {
        let path = dir.write(text);
        assert_eq!(load_settings(&path), Settings::default(), "{text:?}");
    }
}

#[test]
fn invalid_settings_sections_fall_back_to_defaults() {
    let dir = TempDir::new("invalid-sections");
    let mut json = serde_json::to_value(custom_settings()).unwrap();
    // Out of range: the section resets, the rest is kept.
    json["reminders"]["syncIntervalMinutes"] = 99.into();
    // Wrong shape: the section resets.
    json["appearance"] = "big".into();
    // A second account: one with a bad URL, one duplicating an id.
    json["accounts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(account("a2", "http://insecure")).unwrap());
    json["accounts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::to_value(account("a1", "https://other.example.com/")).unwrap());
    json["accounts"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"id": "broken"}));
    json["schemaVersion"] = 7.into();
    let path = dir.write(&json.to_string());

    let loaded = load_settings(&path);
    let expected = Settings {
        reminders: Default::default(),
        appearance: Default::default(),
        ..custom_settings()
    };
    assert_eq!(loaded, expected);
    assert!(loaded.validate().is_ok());
}

#[test]
fn partial_settings_fill_in_defaults() {
    let dir = TempDir::new("partial");
    let path = dir.write(r#"{"system": {"sleepAfterIdleMinutes": 30}, "unknown": true}"#);
    let mut expected = Settings::default();
    expected.system.sleep_after_idle_minutes = 30;
    assert_eq!(load_settings(&path), expected);
}
