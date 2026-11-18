//! Backend of the upcoming popover (spec 4.4, 7.2): the `list_upcoming`
//! assembly and the task commands of the buddy window.
//!
//! The assembly is pure ([`assemble`]) or takes a [`Storage`] with an
//! injected clock ([`list_upcoming_from`]), so it is unit-tested with
//! `FakeClock`. The commands are async and run storage calls on the blocking
//! pool ([`app_storage::blocking`]): Tauri runs *sync* commands on the main
//! thread, which must never wait on SQLite.
//!
//! Every task mutation goes through [`tasks_changed`], which emits
//! `upcoming-changed`. Hook for 2.2: tell the scheduler there as well, so a
//! new or edited due time re-arms its reminder.

use chrono::{DateTime, Duration, Utc};
use tauri::{AppHandle, Emitter, Runtime};

use crate::app_storage::{self, blocking};
use crate::domain::ipc::{
    event, CreateTaskRequest, SetTaskDoneRequest, TaskRequest, UpcomingResponse, UpdateTaskRequest,
};
use crate::domain::{Clock, Event, ReminderSettings, Result, Task};
use crate::storage::Storage;
use crate::BUDDY_WINDOW_LABEL;

/// How far ahead the popover looks for meetings: the sync window (spec 6.5).
pub const MEETING_HORIZON: Duration = Duration::days(7);

/// Builds the popover data from cached events and all tasks.
///
/// - Meetings: the skip filters from `filters` (all-day, declined), sorted by
///   start (all-day ones at local midnight via `clock`), then by key, at most
///   [`UpcomingResponse::MAX_MEETINGS`].
/// - Open tasks: by due time, then id, so overdue ones come first.
/// - Done tasks: most recently done first, then id.
pub fn assemble(
    clock: &dyn Clock,
    events: Vec<Event>,
    tasks: Vec<Task>,
    filters: &ReminderSettings,
    paused: bool,
    last_synced_at: Option<DateTime<Utc>>,
) -> UpcomingResponse {
    let mut meetings: Vec<Event> = events
        .into_iter()
        .filter(|e| !(filters.skip_all_day && e.is_all_day()))
        .filter(|e| !(filters.skip_declined && e.is_declined()))
        .collect();
    meetings.sort_by_cached_key(|e| (e.time.start_utc(clock), e.key()));
    meetings.truncate(UpcomingResponse::MAX_MEETINGS);

    let (mut done_tasks, mut open_tasks): (Vec<Task>, Vec<Task>) =
        tasks.into_iter().partition(|t| t.done);
    open_tasks.sort_by(|a, b| (a.due_at, &a.id).cmp(&(b.due_at, &b.id)));
    done_tasks.sort_by(|a, b| b.done_at.cmp(&a.done_at).then_with(|| a.id.cmp(&b.id)));

    UpcomingResponse {
        meetings,
        open_tasks,
        done_tasks,
        paused,
        last_synced_at,
    }
}

/// Reads the cache and the tasks from `storage` (meetings that have not
/// ended yet and start within [`MEETING_HORIZON`], using the storage clock)
/// and [`assemble`]s them. Blocks: call off the main thread.
pub fn list_upcoming_from(
    storage: &Storage,
    filters: &ReminderSettings,
    paused: bool,
) -> Result<UpcomingResponse> {
    let now = storage.clock().now();
    let events = storage.events_in_window(now, now + MEETING_HORIZON)?;
    let tasks = storage.list_tasks()?;
    let last_synced_at = storage
        .list_sync_states()?
        .into_iter()
        .filter_map(|s| s.last_success_at)
        .max();
    Ok(assemble(
        storage.clock(),
        events,
        tasks,
        filters,
        paused,
        last_synced_at,
    ))
}

/// Tasks changed: the popover refetches. 2.2: notify the scheduler here.
pub fn tasks_changed<R: Runtime>(app: &AppHandle<R>) {
    let _ = app.emit_to(BUDDY_WINDOW_LABEL, event::UPCOMING_CHANGED, ());
}

/// Runs a task mutation off the main thread and announces the change.
async fn mutate<R: Runtime, T: Send + 'static>(
    app: &AppHandle<R>,
    f: impl FnOnce(&Storage) -> Result<T> + Send + 'static,
) -> Result<T> {
    let storage = app_storage::storage(app)?;
    let out = blocking(move || f(&storage)).await?;
    tasks_changed(app);
    Ok(out)
}

/// IPC `list_upcoming`.
#[tauri::command]
pub async fn list_upcoming<R: Runtime>(app: AppHandle<R>) -> Result<UpcomingResponse> {
    let storage = app_storage::storage(&app)?;
    let paused = crate::pause::is_paused(&app);
    let reminders = crate::settings_service::current(&app).reminders;
    blocking(move || list_upcoming_from(&storage, &reminders, paused)).await
}

/// IPC `list_tasks`.
#[tauri::command]
pub async fn list_tasks<R: Runtime>(app: AppHandle<R>) -> Result<Vec<Task>> {
    let storage = app_storage::storage(&app)?;
    blocking(move || storage.list_tasks()).await
}

/// IPC `create_task`. Invalid title: `validation` on `title`.
#[tauri::command]
pub async fn create_task<R: Runtime>(
    app: AppHandle<R>,
    request: CreateTaskRequest,
) -> Result<Task> {
    mutate(&app, move |s| s.create_task(&request.title, request.due_at)).await
}

/// IPC `update_task`.
#[tauri::command]
pub async fn update_task<R: Runtime>(
    app: AppHandle<R>,
    request: UpdateTaskRequest,
) -> Result<Task> {
    mutate(&app, move |s| {
        s.update_task(&request.task_id, &request.title, request.due_at)
    })
    .await
}

/// IPC `set_task_done`.
#[tauri::command]
pub async fn set_task_done<R: Runtime>(
    app: AppHandle<R>,
    request: SetTaskDoneRequest,
) -> Result<Task> {
    mutate(&app, move |s| {
        s.set_task_done(&request.task_id, request.done)
    })
    .await
}

/// IPC `delete_task`.
#[tauri::command]
pub async fn delete_task<R: Runtime>(app: AppHandle<R>, request: TaskRequest) -> Result<()> {
    mutate(&app, move |s| s.delete_task(&request.task_id)).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{FixedOffset, NaiveDate, TimeZone};

    use super::*;
    use crate::domain::{
        AccountId, CalendarId, EventId, EventTime, FakeClock, ResponseStatus, TaskId,
    };

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 9, 0, 0).unwrap()
    }

    fn clock() -> Arc<FakeClock> {
        Arc::new(FakeClock::with_offset(
            t0(),
            FixedOffset::east_opt(3600).unwrap(),
        ))
    }

    fn timed(id: &str, start_min: i64, response: ResponseStatus) -> Event {
        Event {
            id: EventId::new(id),
            account_id: AccountId::new("a"),
            calendar_id: CalendarId::new("c"),
            title: format!("Meeting {id}"),
            time: EventTime::Timed {
                start: t0() + Duration::minutes(start_min),
                end: t0() + Duration::minutes(start_min + 30),
            },
            location: None,
            response,
        }
    }

    fn all_day(id: &str, day: u32) -> Event {
        Event {
            time: EventTime::AllDay {
                start: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 11, day + 1).unwrap(),
            },
            ..timed(id, 0, ResponseStatus::Accepted)
        }
    }

    fn task(id: &str, due_min: i64, done_min: Option<i64>) -> Task {
        Task {
            id: TaskId::new(id),
            title: format!("Task {id}"),
            due_at: t0() + Duration::minutes(due_min),
            done: done_min.is_some(),
            done_at: done_min.map(|m| t0() + Duration::minutes(m)),
            created_at: t0() - Duration::days(1),
        }
    }

    fn ids(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.id.as_str()).collect()
    }

    fn task_ids(tasks: &[Task]) -> Vec<&str> {
        tasks.iter().map(|t| t.id.as_str()).collect()
    }

    fn filters(skip_all_day: bool, skip_declined: bool) -> ReminderSettings {
        ReminderSettings {
            skip_all_day,
            skip_declined,
            ..ReminderSettings::default()
        }
    }

    #[test]
    fn meetings_are_sorted_and_limited_to_five() {
        let c = clock();
        let events = (0..8)
            .rev()
            .map(|i| timed(&format!("e{i}"), i * 10, ResponseStatus::Accepted))
            .collect();
        let r = assemble(&*c, events, vec![], &filters(true, true), false, None);
        assert_eq!(ids(&r.meetings), ["e0", "e1", "e2", "e3", "e4"]);
    }

    #[test]
    fn skip_filters_apply_only_when_on() {
        let c = clock();
        let events = || {
            vec![
                timed("declined", 10, ResponseStatus::Declined),
                timed("accepted", 20, ResponseStatus::Accepted),
                all_day("tomorrow", 18),
            ]
        };
        let r = assemble(&*c, events(), vec![], &filters(true, true), false, None);
        assert_eq!(ids(&r.meetings), ["accepted"]);

        let r = assemble(&*c, events(), vec![], &filters(false, false), false, None);
        assert_eq!(ids(&r.meetings), ["declined", "accepted", "tomorrow"]);

        let r = assemble(&*c, events(), vec![], &filters(false, true), false, None);
        assert_eq!(ids(&r.meetings), ["accepted", "tomorrow"]);
    }

    #[test]
    fn all_day_events_sort_at_local_midnight() {
        let c = clock(); // UTC+1: 17 Nov local midnight is 16 Nov 23:00 UTC.
        let events = vec![
            timed("morning", 0, ResponseStatus::Accepted),
            all_day("today", 17),
        ];
        let r = assemble(&*c, events, vec![], &filters(false, false), false, None);
        assert_eq!(ids(&r.meetings), ["today", "morning"]);
    }

    #[test]
    fn tasks_split_into_open_by_due_and_done_by_recency() {
        let c = clock();
        let tasks = vec![
            task("later", 60, None),
            task("overdue", -30, None),
            task("done-early", -100, Some(-90)),
            task("soon", 5, None),
            task("done-late", 200, Some(-10)),
        ];
        let r = assemble(&*c, vec![], tasks, &filters(true, true), true, None);
        assert_eq!(task_ids(&r.open_tasks), ["overdue", "soon", "later"]);
        assert!(r.open_tasks[0].is_overdue(t0()));
        assert!(!r.open_tasks[1].is_overdue(t0()));
        assert_eq!(task_ids(&r.done_tasks), ["done-late", "done-early"]);
        assert!(r.paused);
    }

    #[test]
    fn from_storage_reads_window_tasks_and_last_sync() {
        let c = clock();
        let storage = Storage::open_in_memory(c.clone()).unwrap();
        let account = AccountId::new("a");
        let calendar = CalendarId::new("c");
        let past = Event {
            time: EventTime::Timed {
                start: t0() - Duration::hours(2),
                end: t0() - Duration::hours(1),
            },
            ..timed("past", 0, ResponseStatus::Accepted)
        };
        let ongoing = Event {
            time: EventTime::Timed {
                start: t0() - Duration::minutes(10),
                end: t0() + Duration::minutes(20),
            },
            ..timed("ongoing", 0, ResponseStatus::Accepted)
        };
        let far = timed("far", 8 * 24 * 60, ResponseStatus::Accepted);
        let soon = timed("soon", 15, ResponseStatus::Accepted);
        storage
            .replace_events(
                &account,
                std::slice::from_ref(&calendar),
                &[past, ongoing, far, soon],
            )
            .unwrap();
        storage.record_sync_success(&account).unwrap();

        let open = storage
            .create_task("Call mum", t0() - Duration::minutes(1))
            .unwrap();
        let done = storage
            .create_task("Water plants", t0() + Duration::hours(1))
            .unwrap();
        storage.set_task_done(&done.id, true).unwrap();

        let r = list_upcoming_from(&storage, &filters(true, true), false).unwrap();
        assert_eq!(ids(&r.meetings), ["ongoing", "soon"]);
        assert_eq!(r.open_tasks, std::slice::from_ref(&open));
        assert!(r.open_tasks[0].is_overdue(c.now()));
        assert_eq!(task_ids(&r.done_tasks), [done.id.as_str()]);
        assert_eq!(r.last_synced_at, Some(t0()));
        assert!(!r.paused);
    }

    #[test]
    fn response_is_camel_case_json() {
        let c = clock();
        let r = assemble(
            &*c,
            vec![],
            vec![task("x", 1, None)],
            &filters(true, true),
            false,
            None,
        );
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["openTasks"][0]["dueAt"].is_string());
        assert!(json["doneTasks"].as_array().unwrap().is_empty());
        assert!(json["lastSyncedAt"].is_null());
    }
}
