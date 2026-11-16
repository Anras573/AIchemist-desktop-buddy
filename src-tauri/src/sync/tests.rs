//! Engine and worker tests against the fake provider and a fake clock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::engine::backoff_delay;
use super::worker::clock_jumped;
use super::*;
use crate::domain::ipc::SyncState;
use crate::domain::{
    AccountConnection, CalendarId, CalendarInfo, Clock, EventId, EventTime, FakeCalendarProvider,
    FakeClock, ResponseStatus, Settings,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 8, 0, 0).unwrap()
}

fn id(s: &str) -> AccountId {
    AccountId::new(s)
}

fn account(acc: &str, calendars: &[&str]) -> AccountConfig {
    AccountConfig {
        id: id(acc),
        display_name: acc.into(),
        connection: AccountConnection::CalDav {
            server_url: format!("https://{acc}.example.com/"),
            username: "me".into(),
        },
        included_calendars: calendars.iter().map(|c| CalendarId::new(*c)).collect(),
    }
}

fn settings(accounts: Vec<AccountConfig>, interval: u32) -> Settings {
    let mut s = Settings {
        accounts,
        ..Settings::default()
    };
    s.reminders.sync_interval_minutes = interval;
    s
}

fn event(acc: &str, cal: &str, ev: &str, title: &str, start: DateTime<Utc>) -> Event {
    Event {
        id: EventId::new(ev),
        account_id: id(acc),
        calendar_id: CalendarId::new(cal),
        title: title.into(),
        time: EventTime::Timed {
            start,
            end: start + Duration::minutes(30),
        },
        location: None,
        response: ResponseStatus::Accepted,
    }
}

fn fake(acc: &str, events: Vec<Event>) -> Arc<FakeCalendarProvider> {
    Arc::new(
        FakeCalendarProvider::new(acc)
            .with_calendars(vec![CalendarInfo {
                id: CalendarId::new("c"),
                account_id: id(acc),
                name: "Calendar".into(),
                color: None,
            }])
            .with_events(events),
    )
}

#[derive(Default)]
struct Factory {
    providers: Mutex<HashMap<AccountId, Arc<dyn CalendarProvider>>>,
    created: AtomicUsize,
}

impl Factory {
    fn with(providers: Vec<Arc<dyn CalendarProvider>>) -> Arc<Self> {
        let f = Self::default();
        for p in providers {
            f.providers
                .lock()
                .unwrap()
                .insert(p.account_id().clone(), p);
        }
        Arc::new(f)
    }

    fn created(&self) -> usize {
        self.created.load(Ordering::SeqCst)
    }
}

impl ProviderFactory for Factory {
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
        self.created.fetch_add(1, Ordering::SeqCst);
        self.providers
            .lock()
            .unwrap()
            .get(&account.id)
            .cloned()
            .ok_or_else(|| Error::keychain("no secret"))
    }
}

#[derive(Default)]
struct Recorder {
    updates: Mutex<Vec<SyncUpdate>>,
    tx: Mutex<Option<Sender<SyncUpdate>>>,
}

impl Recorder {
    fn channel() -> (Arc<Self>, Receiver<SyncUpdate>) {
        let (tx, rx) = mpsc::channel();
        let r = Self {
            tx: Mutex::new(Some(tx)),
            ..Self::default()
        };
        (Arc::new(r), rx)
    }

    fn take(&self) -> Vec<SyncUpdate> {
        std::mem::take(&mut self.updates.lock().unwrap())
    }
}

impl SyncListener for Recorder {
    fn on_update(&self, update: SyncUpdate) {
        if let Some(tx) = &*self.tx.lock().unwrap() {
            let _ = tx.send(update.clone());
        }
        self.updates.lock().unwrap().push(update);
    }
}

struct Fixture {
    clock: Arc<FakeClock>,
    sink: Arc<InMemorySink>,
    factory: Arc<Factory>,
    recorder: Arc<Recorder>,
    engine: SyncEngine,
}

fn fixture(settings: &Settings, providers: Vec<Arc<dyn CalendarProvider>>) -> Fixture {
    let clock = Arc::new(FakeClock::new(t0()));
    let sink = Arc::new(InMemorySink::new());
    let factory = Factory::with(providers);
    let recorder = Arc::new(Recorder::default());
    let engine = SyncEngine::new(
        settings,
        factory.clone(),
        sink.clone(),
        recorder.clone(),
        clock.clone(),
        SyncConfig::default(),
    );
    Fixture {
        clock,
        sink,
        factory,
        recorder,
        engine,
    }
}

fn status(updates: &[SyncUpdate], acc: &str) -> Vec<SyncState> {
    updates
        .iter()
        .filter_map(|u| match u {
            SyncUpdate::AccountStatus(s) if s.account_id == id(acc) => Some(s.sync_state),
            _ => None,
        })
        .collect()
}

fn merged(updates: &[SyncUpdate]) -> Option<Vec<String>> {
    updates.iter().rev().find_map(|u| match u {
        SyncUpdate::EventsChanged(events) => {
            Some(events.iter().map(|e| e.id.as_str().to_owned()).collect())
        }
        _ => None,
    })
}

fn tick(f: &mut Fixture) -> TickOutcome {
    f.engine.tick(Triggers::default())
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

#[test]
fn first_tick_syncs_every_account_and_reports_in_order() {
    let a = fake(
        "a",
        vec![
            event("a", "c", "a1", "Standup", t0() + Duration::hours(2)),
            // Outside the 7-day window.
            event("a", "c", "far", "Later", t0() + Duration::days(8)),
        ],
    );
    let b = fake(
        "b",
        vec![
            event("b", "c", "b1", "Lunch", t0() + Duration::hours(1)),
            // Same meeting as a1: de-duplicated, a1 (first account) wins.
            event("b", "c", "b-dup", "Standup", t0() + Duration::hours(2)),
        ],
    );
    let s = settings(vec![account("a", &["c"]), account("b", &["c"])], 10);
    let mut f = fixture(&s, vec![a.clone(), b.clone()]);

    assert_eq!(f.engine.next_wake(), t0());
    let out = tick(&mut f);
    assert!(out.full_sync);
    assert_eq!(out.next_wake, t0() + Duration::minutes(10));

    let updates = f.recorder.take();
    let synced = |acc: &str, state| {
        SyncUpdate::AccountStatus(AccountSyncStatus {
            account_id: id(acc),
            sync_state: state,
            last_synced_at: (state == SyncState::Ok).then(t0),
            last_error: None,
            next_retry_at: None,
        })
    };
    let expected_events = merge_events(
        a.fetch_events(&[CalendarId::new("c")], t0(), t0() + Duration::days(7))
            .unwrap()
            .into_iter()
            .chain(
                b.fetch_events(&[CalendarId::new("c")], t0(), t0() + Duration::days(7))
                    .unwrap(),
            ),
        &s.accounts,
        f.clock.as_ref(),
    );
    assert_eq!(
        updates,
        [
            SyncUpdate::Started,
            synced("a", SyncState::Syncing),
            synced("a", SyncState::Ok),
            synced("b", SyncState::Syncing),
            synced("b", SyncState::Ok),
            SyncUpdate::EventsChanged(expected_events),
            SyncUpdate::Ended,
        ]
    );
    assert_eq!(merged(&updates).unwrap(), ["b1", "a1"]);
    assert_eq!(updates[0].buddy_event(), Some(BuddyEvent::SyncStarted));
    assert_eq!(updates[6].buddy_event(), Some(BuddyEvent::SyncEnded));
    assert_eq!(f.sink.events_of(&id("a")).len(), 1);
    assert_eq!(
        f.sink.last_outcome(&id("a")),
        Some(SyncOutcome {
            attempted_at: t0(),
            result: Ok(())
        })
    );

    // Nothing due before the interval: a spurious wake does nothing.
    f.clock.advance(Duration::minutes(3));
    let out = tick(&mut f);
    assert!(!out.full_sync);
    assert!(f.recorder.take().is_empty());
    assert_eq!(a.fetch_count(), 2); // one by the engine, one above

    // The interval elapses: full sync, but an unchanged list is not re-sent.
    f.clock.set(t0() + Duration::minutes(10));
    assert!(tick(&mut f).full_sync);
    let updates = f.recorder.take();
    assert_eq!(merged(&updates), None);
    assert_eq!(updates.first(), Some(&SyncUpdate::Started));
    assert_eq!(updates.last(), Some(&SyncUpdate::Ended));
}

#[test]
fn failure_keeps_the_accounts_cache_and_other_accounts_still_sync() {
    let a = fake(
        "a",
        vec![event("a", "c", "a1", "A", t0() + Duration::hours(1))],
    );
    let b = fake(
        "b",
        vec![event("b", "c", "b1", "B", t0() + Duration::hours(2))],
    );
    let s = settings(vec![account("a", &["c"]), account("b", &["c"])], 10);
    let mut f = fixture(&s, vec![a.clone(), b.clone()]);
    tick(&mut f);
    f.recorder.take();

    a.fail_with(Error::protocol("bad xml"));
    a.set_events(vec![]);
    b.set_events(vec![
        event("b", "c", "b1", "B", t0() + Duration::hours(2)),
        event("b", "c", "b2", "B2", t0() + Duration::hours(3)),
    ]);
    f.clock.advance(Duration::minutes(10));
    tick(&mut f);

    // a keeps its last good cache; b is updated.
    assert_eq!(f.sink.events_of(&id("a"))[0].id.as_str(), "a1");
    assert_eq!(f.sink.events_of(&id("b")).len(), 2);
    let updates = f.recorder.take();
    assert_eq!(
        status(&updates, "a"),
        [SyncState::Syncing, SyncState::Error]
    );
    assert_eq!(status(&updates, "b"), [SyncState::Syncing, SyncState::Ok]);
    assert_eq!(merged(&updates).unwrap(), ["a1", "b1", "b2"]);

    let a_status = f.engine.statuses().lock().unwrap()[0].clone();
    assert_eq!(a_status.sync_state, SyncState::Error);
    assert_eq!(a_status.last_error, Some(Error::protocol("bad xml")));
    assert_eq!(a_status.last_synced_at, Some(t0()));
    assert_eq!(
        f.sink.last_outcome(&id("a")).unwrap().result,
        Err(Error::protocol("bad xml"))
    );
}

#[test]
fn storage_failure_is_an_account_error() {
    let a = fake(
        "a",
        vec![event("a", "c", "a1", "A", t0() + Duration::hours(1))],
    );
    let mut f = fixture(&settings(vec![account("a", &["c"])], 10), vec![a]);
    f.sink.fail_writes(Some(Error::storage("disk full")));
    tick(&mut f);
    let st = f.engine.statuses().lock().unwrap()[0].clone();
    assert_eq!(st.last_error, Some(Error::storage("disk full")));
    assert_eq!(st.next_retry_at, None);
}

#[test]
fn retryable_errors_back_off_for_that_account_only() {
    let a = fake("a", vec![]);
    let b = fake("b", vec![]);
    a.fail_with(Error::network("offline"));
    let s = settings(vec![account("a", &["c"]), account("b", &["c"])], 15);
    let mut f = fixture(&s, vec![a.clone(), b.clone()]);

    tick(&mut f);
    let mut seen = Vec::new();
    for _ in 0..4 {
        let next = f.engine.next_wake();
        seen.push((next - f.clock.now()).num_seconds());
        f.clock.set(next);
        let out = tick(&mut f);
        assert!(!out.full_sync);
    }
    assert_eq!(seen, [30, 60, 120, 240]);
    assert_eq!(a.fetch_count(), 5);
    assert_eq!(b.fetch_count(), 1, "healthy accounts are not re-fetched");

    // A retry run still shows `thinking`.
    let updates = f.recorder.take();
    assert_eq!(updates.last(), Some(&SyncUpdate::Ended));

    // The next retry (480 s, at 15.5 min) would come after the regular
    // sync at 15 min, so the regular sync is next. Recovery clears the
    // backoff.
    assert_eq!(f.clock.now(), t0() + Duration::seconds(450));
    assert_eq!(f.engine.next_wake(), t0() + Duration::minutes(15));
    a.clear_failure();
    f.clock.set(t0() + Duration::minutes(15));
    assert!(tick(&mut f).full_sync);
    let st = f.engine.statuses().lock().unwrap()[0].clone();
    assert_eq!(st.sync_state, SyncState::Ok);
    assert_eq!(st.next_retry_at, None);
    assert_eq!(st.last_error, None);
    assert_eq!(f.engine.next_wake(), t0() + Duration::minutes(30));
}

#[test]
fn backoff_doubles_and_is_capped_at_the_interval() {
    let delays: Vec<i64> = (1..=8)
        .map(|n| backoff_delay(n, Duration::seconds(30), Duration::minutes(10)).num_seconds())
        .collect();
    assert_eq!(delays, [30, 60, 120, 240, 480, 600, 600, 600]);
    assert_eq!(
        backoff_delay(u32::MAX, Duration::seconds(30), Duration::minutes(5)),
        Duration::minutes(5)
    );
}

#[test]
fn non_retryable_errors_wait_for_the_next_full_sync() {
    let a = fake("a", vec![]);
    a.fail_with(Error::auth("expired"));
    let mut f = fixture(&settings(vec![account("a", &["c"])], 10), vec![a.clone()]);
    tick(&mut f);
    assert_eq!(f.engine.next_wake(), t0() + Duration::minutes(10));
    assert_eq!(f.engine.statuses().lock().unwrap()[0].next_retry_at, None);

    f.clock.advance(Duration::minutes(5));
    tick(&mut f);
    assert_eq!(a.fetch_count(), 1, "no fast retry");

    // The provider is re-created at the next full sync (new credentials).
    a.clear_failure();
    f.clock.set(t0() + Duration::minutes(10));
    tick(&mut f);
    assert_eq!(a.fetch_count(), 2);
    assert_eq!(f.factory.created(), 2);
    assert_eq!(
        f.engine.statuses().lock().unwrap()[0].sync_state,
        SyncState::Ok
    );
}

#[test]
fn missing_provider_is_an_account_error() {
    let mut f = fixture(&settings(vec![account("a", &["c"])], 10), vec![]);
    tick(&mut f);
    let st = f.engine.statuses().lock().unwrap()[0].clone();
    assert_eq!(st.sync_state, SyncState::Error);
    assert_eq!(st.last_error, Some(Error::keychain("no secret")));
}

#[test]
fn interval_change_applies_live() {
    let a = fake("a", vec![]);
    let mut f = fixture(&settings(vec![account("a", &["c"])], 10), vec![a.clone()]);
    tick(&mut f);

    // Longer interval: the next sync moves out.
    f.clock.advance(Duration::minutes(1));
    let out = f.engine.tick(Triggers {
        settings: Some(settings(vec![account("a", &["c"])], 15)),
        ..Triggers::default()
    });
    assert!(!out.full_sync);
    assert_eq!(out.next_wake, t0() + Duration::minutes(15));

    // Shorter than the time already elapsed: syncs now.
    f.clock.advance(Duration::minutes(5));
    let out = f.engine.tick(Triggers {
        settings: Some(settings(vec![account("a", &["c"])], 5)),
        ..Triggers::default()
    });
    assert!(out.full_sync);
    assert_eq!(out.next_wake, t0() + Duration::minutes(11));
    assert_eq!(a.fetch_count(), 2);

    // Out-of-range values are clamped.
    let out = f.engine.tick(Triggers {
        settings: Some(settings(vec![account("a", &["c"])], 60)),
        ..Triggers::default()
    });
    assert_eq!(f.engine.interval(), Duration::minutes(15));
    assert_eq!(out.next_wake, t0() + Duration::minutes(21));
}

#[test]
fn account_changes_apply_live() {
    let a = fake(
        "a",
        vec![
            event("a", "c", "a1", "A", t0() + Duration::hours(1)),
            event("a", "d", "a2", "A2", t0() + Duration::hours(2)),
        ],
    );
    let b = fake(
        "b",
        vec![event("b", "c", "b1", "B", t0() + Duration::hours(3))],
    );
    let both = vec![account("a", &["c"]), account("b", &["c"])];
    let mut f = fixture(&settings(both.clone(), 10), vec![a.clone(), b.clone()]);
    tick(&mut f);
    assert_eq!(merged(&f.recorder.take()).unwrap(), ["a1", "b1"]);

    // A rename alone does not sync.
    let mut renamed = both.clone();
    renamed[0].display_name = "Work".into();
    let out = f.engine.tick(Triggers {
        settings: Some(settings(renamed, 10)),
        ..Triggers::default()
    });
    assert!(!out.full_sync);

    // Including another calendar re-fetches.
    let mut more = both.clone();
    more[0].included_calendars.push(CalendarId::new("d"));
    let out = f.engine.tick(Triggers {
        settings: Some(settings(more.clone(), 10)),
        ..Triggers::default()
    });
    assert!(out.full_sync);
    assert_eq!(merged(&f.recorder.take()).unwrap(), ["a1", "a2", "b1"]);

    // Removing an account drops its events from the merged list.
    let out = f.engine.tick(Triggers {
        settings: Some(settings(vec![more[0].clone()], 10)),
        ..Triggers::default()
    });
    assert!(out.full_sync);
    assert_eq!(merged(&f.recorder.take()).unwrap(), ["a1", "a2"]);
    assert_eq!(f.engine.statuses().lock().unwrap().len(), 1);

    // No calendars included: nothing fetched, cache cleared, never synced.
    let fetches = a.fetch_count();
    f.engine.tick(Triggers {
        settings: Some(settings(vec![account("a", &[])], 10)),
        ..Triggers::default()
    });
    assert_eq!(a.fetch_count(), fetches);
    assert!(f.sink.events_of(&id("a")).is_empty());
    assert_eq!(merged(&f.recorder.take()).unwrap(), Vec::<String>::new());
    assert_eq!(
        f.engine.statuses().lock().unwrap()[0].sync_state,
        SyncState::NeverSynced
    );
}

#[test]
fn removing_the_last_account_clears_events_without_thinking() {
    let a = fake(
        "a",
        vec![event("a", "c", "a1", "A", t0() + Duration::hours(1))],
    );
    let mut f = fixture(&settings(vec![account("a", &["c"])], 10), vec![a]);
    tick(&mut f);
    f.recorder.take();
    f.engine.tick(Triggers {
        settings: Some(settings(vec![], 10)),
        ..Triggers::default()
    });
    assert_eq!(f.recorder.take(), [SyncUpdate::EventsChanged(vec![])]);
}

#[test]
fn sync_now_and_resume_trigger_a_full_sync() {
    let a = fake("a", vec![]);
    let mut f = fixture(&settings(vec![account("a", &["c"])], 10), vec![a.clone()]);
    tick(&mut f);
    f.clock.advance(Duration::minutes(2));
    let out = f.engine.tick(Triggers {
        sync_now: true,
        ..Triggers::default()
    });
    assert!(out.full_sync);
    assert_eq!(out.next_wake, t0() + Duration::minutes(12));
    f.clock.advance(Duration::minutes(1));
    let out = f.engine.tick(Triggers {
        resumed: true,
        ..Triggers::default()
    });
    assert!(out.full_sync);
    assert_eq!(a.fetch_count(), 3);
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// A provider whose `fetch_events` blocks until the gate opens.
struct Gated {
    inner: Arc<FakeCalendarProvider>,
    open: Mutex<bool>,
    cv: Condvar,
    entered: Mutex<Sender<()>>,
}

impl Gated {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

impl CalendarProvider for Gated {
    fn account_id(&self) -> &AccountId {
        self.inner.account_id()
    }
    fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        self.inner.list_calendars()
    }
    fn fetch_events(
        &self,
        calendar_ids: &[CalendarId],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        let _ = self.entered.lock().unwrap().send(());
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
        self.inner.fetch_events(calendar_ids, from, to)
    }
    fn test_connection(&self) -> Result<()> {
        self.inner.test_connection()
    }
}

const TIMEOUT: StdDuration = StdDuration::from_secs(10);

fn spawn(
    settings: &Settings,
    providers: Vec<Arc<dyn CalendarProvider>>,
) -> (SyncWorker, Receiver<SyncUpdate>) {
    let (recorder, rx) = Recorder::channel();
    let engine = SyncEngine::new(
        settings,
        Factory::with(providers),
        Arc::new(InMemorySink::new()),
        recorder,
        Arc::new(FakeClock::new(t0())),
        SyncConfig::default(),
    );
    (SyncWorker::spawn(engine).unwrap(), rx)
}

fn wait_for(rx: &Receiver<SyncUpdate>, wanted: &SyncUpdate) {
    loop {
        let got = rx.recv_timeout(TIMEOUT).expect("update in time");
        if &got == wanted {
            return;
        }
    }
}

#[test]
fn worker_syncs_on_start_on_demand_and_on_resume() {
    let a = fake(
        "a",
        vec![event("a", "c", "a1", "A", t0() + Duration::hours(1))],
    );
    let (worker, rx) = spawn(&settings(vec![account("a", &["c"])], 10), vec![a.clone()]);
    let handle = worker.handle();

    wait_for(&rx, &SyncUpdate::Ended);
    assert_eq!(a.fetch_count(), 1);
    assert_eq!(handle.account_statuses()[0].sync_state, SyncState::Ok);
    assert!(!handle.has_errors());

    handle.sync_now();
    wait_for(&rx, &SyncUpdate::Ended);
    assert_eq!(a.fetch_count(), 2);

    a.fail_with(Error::auth("expired"));
    handle.notify_resumed();
    wait_for(&rx, &SyncUpdate::Ended);
    assert_eq!(a.fetch_count(), 3);
    assert!(handle.has_errors());

    // Settings apply live: the new account shows up and syncs.
    handle.update_settings(settings(
        vec![account("a", &["c"]), account("b", &["c"])],
        10,
    ));
    wait_for(&rx, &SyncUpdate::Ended);
    let statuses = handle.account_statuses();
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses[1].last_error, Some(Error::keychain("no secret")));

    worker.shutdown();
}

#[test]
fn sync_now_during_a_sync_coalesces() {
    let (entered_tx, entered) = mpsc::channel();
    let gated = Arc::new(Gated {
        inner: fake("a", vec![]),
        open: Mutex::new(false),
        cv: Condvar::new(),
        entered: Mutex::new(entered_tx),
    });
    let (worker, rx) = spawn(
        &settings(vec![account("a", &["c"])], 10),
        vec![gated.clone()],
    );
    let handle = worker.handle();

    entered.recv_timeout(TIMEOUT).expect("start-up sync runs");
    handle.sync_now();
    handle.sync_now();
    handle.sync_now();
    gated.open();
    wait_for(&rx, &SyncUpdate::Ended);

    // No overlapping or follow-up sync.
    assert!(rx.recv_timeout(StdDuration::from_millis(300)).is_err());
    assert_eq!(gated.inner.fetch_count(), 1);

    // A later request runs again.
    handle.sync_now();
    wait_for(&rx, &SyncUpdate::Ended);
    assert_eq!(gated.inner.fetch_count(), 2);
    drop(worker); // Drop stops and joins the thread.
}

#[test]
fn clock_jump_detection() {
    let secs = StdDuration::from_secs;
    assert!(!clock_jumped(Duration::seconds(60), secs(60)));
    assert!(!clock_jumped(Duration::seconds(30), secs(60)));
    // Slept for an hour during a one-minute wait.
    assert!(clock_jumped(Duration::minutes(61), secs(60)));
    // Clock set back.
    assert!(clock_jumped(Duration::minutes(-5), secs(1)));
    assert!(!clock_jumped(Duration::seconds(0), StdDuration::ZERO));
}
