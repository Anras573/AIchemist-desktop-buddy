//! Sync wiring against the fake provider, an in-memory database and a fake
//! clock: the real engine and worker, a recording outlet instead of Tauri.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::*;
use crate::domain::ipc::SyncState;
use crate::domain::{
    AccountConfig, AccountConnection, CalendarInfo, CalendarProvider, Error, EventId, EventTime,
    FakeCalendarProvider, FakeClock, ResponseStatus,
};

const TIMEOUT: StdDuration = StdDuration::from_secs(10);

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 8, 0, 0).unwrap()
}

fn id(s: &str) -> AccountId {
    AccountId::new(s)
}

fn account(acc: &str) -> AccountConfig {
    AccountConfig {
        id: id(acc),
        display_name: acc.into(),
        connection: AccountConnection::CalDav {
            server_url: format!("https://{acc}.example.com/"),
            username: "me".into(),
        },
        included_calendars: vec![CalendarId::new("c")],
    }
}

fn settings(accounts: Vec<AccountConfig>) -> Settings {
    Settings {
        accounts,
        ..Settings::default()
    }
}

fn event(acc: &str, ev: &str, start: DateTime<Utc>) -> Event {
    Event {
        id: EventId::new(ev),
        account_id: id(acc),
        calendar_id: CalendarId::new("c"),
        title: format!("Meeting {ev}"),
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

fn ids(events: &[Event]) -> Vec<&str> {
    events.iter().map(|e| e.id.as_str()).collect()
}

struct Factory(Mutex<HashMap<AccountId, Arc<dyn CalendarProvider>>>);

impl Factory {
    fn with(providers: Vec<Arc<dyn CalendarProvider>>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(
            providers
                .into_iter()
                .map(|p| (p.account_id().clone(), p))
                .collect(),
        )))
    }
}

impl ProviderFactory for Factory {
    fn create(&self, account: &AccountConfig) -> Result<Arc<dyn CalendarProvider>> {
        self.0
            .lock()
            .unwrap()
            .get(&account.id)
            .cloned()
            .ok_or_else(|| Error::keychain("no secret"))
    }
}

/// What the outlet saw, in order.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Buddy(BuddyEvent),
    InProgress(bool),
    Status(AccountSyncStatus),
    Events(Vec<Event>),
    Upcoming,
}

struct Outlet(Mutex<Sender<Call>>);

impl Outlet {
    fn channel() -> (Arc<Self>, Receiver<Call>) {
        let (tx, rx) = mpsc::channel();
        (Arc::new(Self(Mutex::new(tx))), rx)
    }

    fn send(&self, call: Call) {
        let _ = self.0.lock().unwrap().send(call);
    }
}

impl SyncOutlet for Outlet {
    fn buddy_event(&self, event: BuddyEvent) {
        self.send(Call::Buddy(event));
    }
    fn sync_in_progress(&self, running: bool) {
        self.send(Call::InProgress(running));
    }
    fn account_status(&self, status: AccountSyncStatus) {
        self.send(Call::Status(status));
    }
    fn events_changed(&self, events: Vec<Event>) {
        self.send(Call::Events(events));
    }
    fn upcoming_changed(&self) {
        self.send(Call::Upcoming);
    }
}

/// Collects calls up to and including the end of one sync run.
fn one_run(rx: &Receiver<Call>) -> Vec<Call> {
    let mut calls = Vec::new();
    loop {
        let call = rx.recv_timeout(TIMEOUT).expect("the sync run ends");
        let end = call == Call::Buddy(BuddyEvent::SyncEnded);
        calls.push(call);
        if end {
            return calls;
        }
    }
}

fn statuses<'a>(calls: &'a [Call], acc: &str) -> Vec<&'a AccountSyncStatus> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Status(s) if s.account_id == id(acc) => Some(s),
            _ => None,
        })
        .collect()
}

fn events_of(calls: &[Call]) -> Option<&Vec<Event>> {
    calls.iter().rev().find_map(|c| match c {
        Call::Events(e) => Some(e),
        _ => None,
    })
}

struct Fixture {
    clock: Arc<FakeClock>,
    storage: Storage,
}

fn fixture() -> Fixture {
    let clock = Arc::new(FakeClock::new(t0()));
    let storage = Storage::open_in_memory(clock.clone()).unwrap();
    Fixture { clock, storage }
}

fn all_accounts() -> AccountFilter {
    Arc::new(|_| true)
}

fn start_sync(
    f: &Fixture,
    settings: &Settings,
    providers: Vec<Arc<dyn CalendarProvider>>,
) -> (CalendarSync, Receiver<Call>) {
    let (outlet, rx) = Outlet::channel();
    let sync = start(
        settings,
        Factory::with(providers),
        Some(f.storage.clone()),
        all_accounts(),
        outlet,
        f.clock.clone(),
    )
    .unwrap();
    (sync, rx)
}

#[test]
fn a_sync_writes_the_cache_and_feeds_the_scheduler() {
    let f = fixture();
    let a = fake("a", vec![event("a", "a1", t0() + Duration::hours(1))]);
    let (_sync, rx) = start_sync(&f, &settings(vec![account("a")]), vec![a]);

    let calls = one_run(&rx);

    // The cat thinks, and the reminder service knows a sync runs, for the
    // whole run; the popover refetches at the end.
    assert_eq!(
        calls[..2],
        [Call::InProgress(true), Call::Buddy(BuddyEvent::SyncStarted)]
    );
    assert_eq!(
        calls[calls.len() - 3..],
        [
            Call::InProgress(false),
            Call::Upcoming,
            Call::Buddy(BuddyEvent::SyncEnded)
        ]
    );
    let states: Vec<SyncState> = statuses(&calls, "a").iter().map(|s| s.sync_state).collect();
    assert_eq!(states, [SyncState::Syncing, SyncState::Ok]);
    // The scheduler gets the merged events ...
    assert_eq!(ids(events_of(&calls).unwrap()), ["a1"]);
    // ... and they are in the cache, with the sync recorded.
    let cached = f
        .storage
        .events_in_window(t0(), t0() + Duration::days(7))
        .unwrap();
    assert_eq!(ids(&cached), ["a1"]);
    let record = f.storage.sync_state(&id("a")).unwrap().unwrap();
    assert_eq!(record.last_success_at, Some(t0()));
    assert_eq!(record.last_attempt_at, Some(t0()));
    assert_eq!(record.last_error, None);
}

#[test]
fn a_failed_account_keeps_its_cache_and_reports_the_error() {
    let f = fixture();
    // Synced an hour ago.
    let earlier = t0() - Duration::hours(1);
    f.clock.set(earlier);
    let cached = vec![event("a", "old", t0() + Duration::hours(2))];
    f.storage
        .replace_events(&id("a"), &[CalendarId::new("c")], &cached)
        .unwrap();
    f.storage.record_sync_success(&id("a")).unwrap();
    f.clock.set(t0());

    let a = fake("a", vec![event("a", "new", t0() + Duration::hours(1))]);
    a.fail_with(Error::network("server unreachable"));
    let b = fake("b", vec![event("b", "b1", t0() + Duration::hours(3))]);
    let (sync, rx) = start_sync(&f, &settings(vec![account("a"), account("b")]), vec![a, b]);

    let calls = one_run(&rx);

    let a_status = statuses(&calls, "a");
    let failed = a_status.last().unwrap();
    assert_eq!(failed.sync_state, SyncState::Error);
    assert_eq!(
        failed.last_error,
        Some(Error::network("server unreachable"))
    );
    // Network errors retry with backoff.
    assert_eq!(failed.next_retry_at, Some(t0() + Duration::seconds(30)));
    // The engine never synced it, so the row shows the stored time.
    assert_eq!(a_status[0].last_synced_at, Some(earlier));
    assert_eq!(failed.last_synced_at, Some(earlier));
    assert!(sync.handle().has_errors());

    // The old cache stays and still reaches the scheduler, next to b.
    assert_eq!(ids(events_of(&calls).unwrap()), ["old", "b1"]);
    let record = f.storage.sync_state(&id("a")).unwrap().unwrap();
    assert_eq!(record.last_success_at, Some(earlier));
    assert!(matches!(
        record.last_error,
        Some((Error::Network { .. }, _))
    ));
}

#[test]
fn startup_offers_cached_events_even_when_every_account_fails() {
    // Offline from the start: the cache from the last run still feeds the
    // scheduler (the reminder service also loads it before this, at
    // startup), and the row shows when the account last synced.
    let f = fixture();
    let cached = vec![
        event("a", "soon", t0() + Duration::minutes(12)),
        event("a", "later", t0() + Duration::hours(5)),
    ];
    f.clock.set(t0() - Duration::days(1));
    f.storage
        .replace_events(&id("a"), &[CalendarId::new("c")], &cached)
        .unwrap();
    f.storage.record_sync_success(&id("a")).unwrap();
    f.clock.set(t0());

    let a = fake("a", Vec::new());
    a.fail_with(Error::network("offline"));
    let (_sync, rx) = start_sync(&f, &settings(vec![account("a")]), vec![a]);

    let calls = one_run(&rx);
    assert_eq!(ids(events_of(&calls).unwrap()), ["soon", "later"]);
    for s in statuses(&calls, "a") {
        assert_eq!(s.last_synced_at, Some(t0() - Duration::days(1)));
    }
}

#[test]
fn a_settings_change_triggers_a_resync() {
    let f = fixture();
    let a = fake("a", vec![event("a", "a1", t0() + Duration::hours(1))]);
    let b = fake("b", vec![event("b", "b1", t0() + Duration::hours(2))]);
    let (sync, rx) = start_sync(&f, &settings(vec![account("a")]), vec![a.clone(), b]);
    one_run(&rx);
    assert_eq!(a.fetch_count(), 1);

    // An account added in Settings.
    sync.handle()
        .update_settings(settings(vec![account("a"), account("b")]));
    let calls = one_run(&rx);
    assert_eq!(
        statuses(&calls, "b").last().unwrap().sync_state,
        SyncState::Ok
    );
    assert_eq!(ids(events_of(&calls).unwrap()), ["a1", "b1"]);
    assert_eq!(a.fetch_count(), 2);

    // Removed again: no fetch needed, the scheduler and popover still hear.
    sync.handle().update_settings(settings(vec![account("a")]));
    let calls = one_run(&rx);
    assert_eq!(ids(events_of(&calls).unwrap()), ["a1"]);
}

#[test]
fn sync_now_runs_a_full_sync() {
    let f = fixture();
    let a = fake("a", Vec::new());
    let (sync, rx) = start_sync(&f, &settings(vec![account("a")]), vec![a.clone()]);
    one_run(&rx);
    sync.handle().sync_now();
    one_run(&rx);
    assert_eq!(a.fetch_count(), 2);
}

/// A provider that panics on every fetch.
struct Panicky(Arc<FakeCalendarProvider>);

impl CalendarProvider for Panicky {
    fn account_id(&self) -> &AccountId {
        self.0.account_id()
    }
    fn list_calendars(&self) -> Result<Vec<CalendarInfo>> {
        self.0.list_calendars()
    }
    fn fetch_events(
        &self,
        _: &[CalendarId],
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<Event>> {
        panic!("provider bug (expected in this test)");
    }
    fn test_connection(&self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn a_panicking_provider_is_contained() {
    let f = fixture();
    let bad = Arc::new(Panicky(fake("a", Vec::new())));
    let b = fake("b", vec![event("b", "b1", t0() + Duration::hours(1))]);
    let (sync, rx) = start_sync(
        &f,
        &settings(vec![account("a"), account("b")]),
        vec![bad, b.clone()],
    );

    let calls = one_run(&rx);
    let a = statuses(&calls, "a");
    assert!(matches!(
        a.last().unwrap().last_error,
        Some(Error::Internal { .. })
    ));
    assert_eq!(ids(events_of(&calls).unwrap()), ["b1"]);

    // The sync thread is still alive.
    sync.handle().sync_now();
    one_run(&rx);
    assert_eq!(b.fetch_count(), 2);
}

#[test]
fn writes_for_a_removed_account_are_dropped() {
    let f = fixture();
    let removed: AccountFilter = Arc::new(|id| id.as_str() != "gone");
    let sink = StorageSink::new(f.storage.clone(), Duration::days(7), removed);
    let events = vec![event("gone", "x", t0() + Duration::hours(1))];
    sink.replace_events(&id("gone"), &events).unwrap();
    sink.record_sync(
        &id("gone"),
        &SyncOutcome {
            attempted_at: t0(),
            result: Ok(()),
        },
    )
    .unwrap();
    assert!(sink.cached_events().unwrap().is_empty());
    assert_eq!(f.storage.sync_state(&id("gone")).unwrap(), None);

    // A configured account is written.
    let events = vec![event("kept", "y", t0() + Duration::hours(1))];
    sink.replace_events(&id("kept"), &events).unwrap();
    assert_eq!(ids(&sink.cached_events().unwrap()), ["y"]);
}

#[test]
fn events_changed_outside_a_run_announces_the_change() {
    let (outlet, rx) = Outlet::channel();
    let listener = Listener::new(None, outlet);
    listener.on_update(SyncUpdate::EventsChanged(Vec::new()));
    assert_eq!(rx.try_recv().unwrap(), Call::Events(Vec::new()));
    assert_eq!(rx.try_recv().unwrap(), Call::Upcoming);
    assert!(rx.try_recv().is_err());
}
