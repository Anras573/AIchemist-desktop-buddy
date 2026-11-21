# Shared contracts

The shared contracts live in `src-tauri/src/domain/`. Tasks that run in parallel all build
against them: scheduler, state machine, storage, iCalendar parser, sync engine, keychain and the
frontends. The rustdoc on each item is the full reference. This page is the summary.

## The freeze rule

**`src-tauri/src/domain/` is frozen** (spec section 15). Feature tasks must not change it.
If you need a change (a new field, variant, command or event), raise it with the orchestrator,
who makes the change in its own PR. Downstream code adds its own modules next to `domain/` and
imports from it. The compiler enforces rustdoc on every public item in `domain` through
`#[deny(missing_docs)]`.

### Approved changes

| Date | Change | Task |
|---|---|---|
| 2026-11-17 | Command `list_monitors` (settings window; no request; response `Vec<MonitorInfo>` with `name`, `primary`, `width`, `height` in physical pixels, primary first) and type `ipc::MonitorInfo`. Additive. First approved contract change. | 2.4 (PR #19) |
| 2026-11-17 | Field `ipc::AccountStatus::signed_in_as: Option<String>` (JSON `signedInAs`, `null` when unknown or for CalDAV): which Microsoft account a Graph account is signed in to, shown in Settings. Additive (`#[serde(default)]`). Persisted outside the keychain and outside `settings.json`, in `account-identities.json` next to it (see `src-tauri/src/accounts/identities.rs`). | 3.5 |
| 2026-11-21 | Least privilege (owner decision): `snooze_all`, `list_tasks`, `update_task`, `get_paused`, `set_paused` and `open_settings` removed from the buddy window's set and from `ipc::COMMANDS`, and no longer registered. The buddy page never called them; the cat menu and tray do snooze all, pause and open settings in Rust. Command names and request types stay in `domain::ipc`. Narrowing, not additive. | 4.4 |

## Contents

| Module | Items |
|---|---|
| `ids` | `AccountId`, `CalendarId`, `EventId`, `TaskId` (opaque string newtypes), `BubbleId(u64)` |
| `event` | `CalendarInfo`, `Event`, `EventKey` (account, calendar, event), `EventTime` (`Timed` with UTC instants or `AllDay` with dates, end exclusive), `ResponseStatus` |
| `task` | `Task` (spec 7.2 fields), `Task::is_overdue` |
| `reminder` | `ReminderKey` (`Meeting { event, lead_minutes }` or `Task { task_id }`), `ReminderKind`, `ReminderStatus` (`Pending`, `Snoozed { until }`, `Handled { at, outcome }`), `ReminderOutcome`, `ReminderInstance` |
| `animation` | `BuddyState` (`idle`, `sleeping`, `thinking`, `alert`), `BuddyEvent` (one variant per row of the spec 5 table). `next_state` belongs to task 1.C and lives outside `domain`. |
| `provider` | `CalendarProvider` (`account_id`, `list_calendars`, `fetch_events`, `test_connection`), `TaskSource` (`fetch_tasks`) |
| `clock` | `Clock` (`now`, `utc_to_local`, `local_to_utc`), `SystemClock`, `FakeClock` |
| `fake_provider` | `FakeCalendarProvider` (configurable calendars and events, `fail_with`, `fetch_count`) |
| `error` | `Error` (`auth`, `network`, `protocol`, `notFound`, `storage`, `keychain`, `invalidInput`, `validation`, `internal`), `FieldError`, `Result<T>` |
| `secret` | `SecretString`: redacted `Debug`, can be deserialized but not serialized |
| `settings` | `Settings` { `schemaVersion`, `accounts`, `reminders`, `appearance`, `system` }, `Settings::validate()` returns field-level errors |
| `ipc` | Command and event names, `COMMANDS` and `EVENTS` with the windows allowed to use each, request and response types |

## Key rules

- **Time.** Instants are `DateTime<Utc>`. All-day events stay as dates and become instants only
  through a `Clock`, at local midnight. Code that needs "now" or local time takes a `Clock`, so
  tests can inject `FakeClock`.
- **Blocking traits.** `CalendarProvider` and `TaskSource` are synchronous, `Send + Sync` and
  object-safe. Run them on a worker thread or through `tauri::async_runtime::spawn_blocking`.
  Never call them on the main thread. See the module docs in `provider.rs` for why.
- **Provider semantics.** `fetch_events` returns occurrences that overlap `[from, to)`. Recurring
  events are expanded and each occurrence gets its own `EventId`. Cancelled occurrences are
  omitted. Declined events are included, and the caller filters them out. On failure the whole
  call fails, never part of it.
- **Reminder identity.** Each `ReminderKey` fires at most once unless snoozed. On-screen and
  queued reminders stay `Pending`, because that state is not persisted.
- **Errors.** Messages never contain secrets, tokens, or event or task contents. Only `Network`
  errors are retried.
- **Settings.** Settings are JSON with camelCase keys. Every section uses `#[serde(default)]`, so
  missing fields fall back to defaults. Loading does not validate. Callers run `validate()`.
  Secrets never go in settings. They are stored in the keychain under the `AccountId`.

## IPC

Every command takes no argument or exactly one argument named `request`. For example:
`invoke("create_task", { request: { title, dueAt } })`. Payloads use camelCase. Instants use
RFC 3339 UTC. A failed command rejects with a serialized `Error`.

| Window | Commands |
|---|---|
| buddy (including its popover) | `get_buddy_state`, `report_interaction`, `get_appearance`, `get_current_bubble`, `dismiss_bubble`, `snooze_bubble`, `list_upcoming`, `create_task`, `set_task_done`, `delete_task`, `show_cat_menu` |
| none (names and request types kept in `domain::ipc::command`; not registered) | `snooze_all`, `list_tasks`, `update_task`, `get_paused`, `set_paused`, `open_settings` |
| settings | `get_settings`, `update_settings`, `list_accounts`, `add_account`, `update_account`, `remove_account`, `list_account_calendars`, `test_connection`, `sign_in_account`, `sync_now`, `list_monitors` |

| Event | Payload | To |
|---|---|---|
| `buddy-state-changed` | `BuddyState` | buddy |
| `reminder-shown` | `BubbleView` | buddy |
| `reminder-cleared` | `null` | buddy |
| `appearance-changed` | `AppearanceSettings` | buddy |
| `paused-changed` | `bool` | buddy |
| `upcoming-changed` | `null` (refetch) | buddy |
| `open-popover` | `PopoverView` (`upcoming` or `addTask`) | buddy |
| `accounts-changed` | `null` (refetch) | settings |

Capability files must grant exactly the commands listed for each window. The tray menu and the
native right-click menu are handled in Rust and need no IPC.
