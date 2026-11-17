//! IPC contract between the Rust core and the webviews: command names,
//! event names, which window may use which, and the payload types.
//!
//! This module defines names and types only; handlers live elsewhere.
//!
//! # Conventions
//!
//! * Command and event names are listed in [`COMMANDS`] and [`EVENTS`].
//! * A command takes **no argument or exactly one argument named
//!   `request`**. From TypeScript:
//!   `invoke("create_task", { request: { title, dueAt } })`.
//! * All payloads are camelCase JSON. Instants are RFC 3339 UTC strings
//!   (`"2026-11-16T09:00:00Z"`), dates are `"YYYY-MM-DD"`.
//! * A failing command rejects with a serialized [`Error`](super::Error):
//!   `{"kind": "...", "message": "..."}` or
//!   `{"kind": "validation", "errors": [...]}`.
//! * Events with no payload (`()`) carry `null`; the listener refetches.
//! * Secrets travel only webview to core ([`SecretString`] has no
//!   `Serialize`); no response ever contains a secret.
//!
//! # Windows
//!
//! [`Window::Buddy`] covers the buddy window and the upcoming popover and
//! add-task form it shows. If the popover is later moved into its own
//! webview, that webview gets the buddy command set. The tray menu and the
//! native right-click menu are handled in Rust and need no IPC.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    AccountConfig, AccountConnection, AccountId, AppearanceSettings, BubbleId, CalendarId, Event,
    ReminderKey, ReminderSettings, SecretString, SystemSettings, Task, TaskId,
};

/// A webview window, for capability scoping (spec 9: each window gets only
/// the commands it needs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Window {
    /// The always-present buddy window (label `"buddy"`), including its popover.
    Buddy,
    /// The on-demand settings window (label `"settings"`).
    Settings,
}

impl Window {
    /// The Tauri window label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Buddy => "buddy",
            Self::Settings => "settings",
        }
    }
}

/// A command name and the windows allowed to invoke it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    /// The `#[tauri::command]` function name.
    pub name: &'static str,
    /// Windows whose capability grants this command.
    pub windows: &'static [Window],
}

/// An event name and the windows it is emitted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSpec {
    /// The Tauri event name.
    pub name: &'static str,
    /// Windows that listen to it.
    pub windows: &'static [Window],
}

/// Command names. Each constant documents its request and response types.
pub mod command {
    // ----- Buddy window -----

    /// Current animation state. Request: none. Response: [`BuddyState`](crate::domain::BuddyState).
    pub const GET_BUDDY_STATE: &str = "get_buddy_state";
    /// The user hovered over or clicked the cat (wakes it, resets the idle
    /// timer). Request: none. Response: `()`.
    pub const REPORT_INTERACTION: &str = "report_interaction";
    /// Appearance for rendering. Request: none.
    /// Response: [`AppearanceSettings`](crate::domain::AppearanceSettings).
    pub const GET_APPEARANCE: &str = "get_appearance";
    /// The bubble on screen, if any (after a webview reload). Request: none.
    /// Response: `Option<`[`BubbleView`](super::BubbleView)`>`.
    pub const GET_CURRENT_BUBBLE: &str = "get_current_bubble";
    /// Dismiss every reminder in a bubble. Request: [`BubbleRequest`](super::BubbleRequest).
    /// Response: `()`. Unknown or stale id: `notFound`.
    pub const DISMISS_BUBBLE: &str = "dismiss_bubble";
    /// Snooze every reminder in a bubble. Request:
    /// [`SnoozeBubbleRequest`](super::SnoozeBubbleRequest). Response: `()`.
    pub const SNOOZE_BUBBLE: &str = "snooze_bubble";
    /// Snooze the current bubble and everything queued behind it.
    /// Request: [`SnoozeAllRequest`](super::SnoozeAllRequest). Response: `()`.
    pub const SNOOZE_ALL: &str = "snooze_all";
    /// Data for the upcoming popover. Request: none.
    /// Response: [`UpcomingResponse`](super::UpcomingResponse).
    pub const LIST_UPCOMING: &str = "list_upcoming";
    /// All tasks, open and done. Request: none. Response: `Vec<`[`Task`](crate::domain::Task)`>`.
    pub const LIST_TASKS: &str = "list_tasks";
    /// Create a task. Request: [`CreateTaskRequest`](super::CreateTaskRequest).
    /// Response: the new [`Task`](crate::domain::Task).
    pub const CREATE_TASK: &str = "create_task";
    /// Edit a task's title and due time; a changed due time re-arms its
    /// reminder. Request: [`UpdateTaskRequest`](super::UpdateTaskRequest).
    /// Response: the updated [`Task`](crate::domain::Task).
    pub const UPDATE_TASK: &str = "update_task";
    /// Mark a task done or undone. Marking done also handles its reminder
    /// (the bubble's "Mark done" uses this). Request:
    /// [`SetTaskDoneRequest`](super::SetTaskDoneRequest). Response: the updated
    /// [`Task`](crate::domain::Task).
    pub const SET_TASK_DONE: &str = "set_task_done";
    /// Delete a task. Request: [`TaskRequest`](super::TaskRequest). Response: `()`.
    pub const DELETE_TASK: &str = "delete_task";
    /// Whether reminders are paused. Request: none. Response: `bool`.
    pub const GET_PAUSED: &str = "get_paused";
    /// Pause or resume reminders (resuming replays missed ones, spec 7.5).
    /// Request: [`SetPausedRequest`](super::SetPausedRequest). Response: the new `bool`.
    pub const SET_PAUSED: &str = "set_paused";
    /// Pop up the native right-click menu at the cursor. Request: none. Response: `()`.
    pub const SHOW_CAT_MENU: &str = "show_cat_menu";
    /// Open (or focus) the settings window. Request: none. Response: `()`.
    pub const OPEN_SETTINGS: &str = "open_settings";

    // ----- Settings window -----

    /// All settings. Request: none. Response: [`Settings`](crate::domain::Settings).
    pub const GET_SETTINGS: &str = "get_settings";
    /// Replace the non-account sections and apply them live. Request:
    /// [`UpdateSettingsRequest`](super::UpdateSettingsRequest). Response: the saved
    /// [`Settings`](crate::domain::Settings). Invalid input: `validation`.
    pub const UPDATE_SETTINGS: &str = "update_settings";
    /// Accounts with sync status. Request: none.
    /// Response: `Vec<`[`AccountStatus`](super::AccountStatus)`>`.
    pub const LIST_ACCOUNTS: &str = "list_accounts";
    /// Add an account (stores its secret in the keychain). Request:
    /// [`AddAccountRequest`](super::AddAccountRequest). Response: [`AccountStatus`](super::AccountStatus).
    pub const ADD_ACCOUNT: &str = "add_account";
    /// Change an account. Request: [`UpdateAccountRequest`](super::UpdateAccountRequest).
    /// Response: [`AccountStatus`](super::AccountStatus).
    pub const UPDATE_ACCOUNT: &str = "update_account";
    /// Remove an account, its keychain secret and its cached events.
    /// Request: [`AccountRequest`](super::AccountRequest). Response: `()`.
    pub const REMOVE_ACCOUNT: &str = "remove_account";
    /// Live list of the account's calendars, for choosing included ones.
    /// Request: [`AccountRequest`](super::AccountRequest).
    /// Response: `Vec<`[`CalendarInfo`](crate::domain::CalendarInfo)`>`.
    pub const LIST_ACCOUNT_CALENDARS: &str = "list_account_calendars";
    /// Test a saved account's connection. Request: [`AccountRequest`](super::AccountRequest).
    /// Response: `()`; failure rejects with the provider [`Error`](crate::domain::Error).
    pub const TEST_CONNECTION: &str = "test_connection";
    /// Run the interactive Microsoft sign-in (system browser) for a Graph
    /// account; resolves when it completes. Request:
    /// [`AccountRequest`](super::AccountRequest). Response: `()`. CalDAV account: `invalidInput`.
    pub const SIGN_IN_ACCOUNT: &str = "sign_in_account";
    /// Start a sync now. Request: none. Response: `()` (returns immediately).
    pub const SYNC_NOW: &str = "sync_now";
    /// Connected monitors, for the appearance monitor selector. Request:
    /// none. Response: `Vec<`[`MonitorInfo`](super::MonitorInfo)`>`, primary
    /// first. (Added 2026-11-17, first approved contract change.)
    pub const LIST_MONITORS: &str = "list_monitors";
}

/// Event names (core to webview). Each constant documents its payload type.
pub mod event {
    /// The animation state changed. Payload: [`BuddyState`](crate::domain::BuddyState).
    pub const BUDDY_STATE_CHANGED: &str = "buddy-state-changed";
    /// A bubble should be shown, replacing any current one. Payload:
    /// [`BubbleView`](super::BubbleView).
    pub const REMINDER_SHOWN: &str = "reminder-shown";
    /// The bubble should be hidden (handled elsewhere, or paused). Payload: `null`.
    pub const REMINDER_CLEARED: &str = "reminder-cleared";
    /// Appearance settings changed. Payload: [`AppearanceSettings`](crate::domain::AppearanceSettings).
    pub const APPEARANCE_CHANGED: &str = "appearance-changed";
    /// Pause toggled (e.g. from the tray). Payload: `bool`.
    pub const PAUSED_CHANGED: &str = "paused-changed";
    /// Meetings or tasks changed; refetch [`LIST_UPCOMING`](super::command::LIST_UPCOMING).
    /// Payload: `null`.
    pub const UPCOMING_CHANGED: &str = "upcoming-changed";
    /// A native menu asked the buddy window to open its popover.
    /// Payload: [`PopoverView`](super::PopoverView).
    pub const OPEN_POPOVER: &str = "open-popover";
    /// Account list or sync status changed; refetch
    /// [`LIST_ACCOUNTS`](super::command::LIST_ACCOUNTS). Payload: `null`.
    pub const ACCOUNTS_CHANGED: &str = "accounts-changed";
}

const BUDDY: &[Window] = &[Window::Buddy];
const SETTINGS: &[Window] = &[Window::Settings];

/// Every command and the windows allowed to call it. Capability files must
/// grant exactly these.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: command::GET_BUDDY_STATE,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::REPORT_INTERACTION,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::GET_APPEARANCE,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::GET_CURRENT_BUBBLE,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::DISMISS_BUBBLE,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::SNOOZE_BUBBLE,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::SNOOZE_ALL,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::LIST_UPCOMING,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::LIST_TASKS,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::CREATE_TASK,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::UPDATE_TASK,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::SET_TASK_DONE,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::DELETE_TASK,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::GET_PAUSED,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::SET_PAUSED,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::SHOW_CAT_MENU,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::OPEN_SETTINGS,
        windows: BUDDY,
    },
    CommandSpec {
        name: command::GET_SETTINGS,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::UPDATE_SETTINGS,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::LIST_ACCOUNTS,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::ADD_ACCOUNT,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::UPDATE_ACCOUNT,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::REMOVE_ACCOUNT,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::LIST_ACCOUNT_CALENDARS,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::TEST_CONNECTION,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::SIGN_IN_ACCOUNT,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::SYNC_NOW,
        windows: SETTINGS,
    },
    CommandSpec {
        name: command::LIST_MONITORS,
        windows: SETTINGS,
    },
];

/// Every event and the windows it is emitted to.
pub const EVENTS: &[EventSpec] = &[
    EventSpec {
        name: event::BUDDY_STATE_CHANGED,
        windows: BUDDY,
    },
    EventSpec {
        name: event::REMINDER_SHOWN,
        windows: BUDDY,
    },
    EventSpec {
        name: event::REMINDER_CLEARED,
        windows: BUDDY,
    },
    EventSpec {
        name: event::APPEARANCE_CHANGED,
        windows: BUDDY,
    },
    EventSpec {
        name: event::PAUSED_CHANGED,
        windows: BUDDY,
    },
    EventSpec {
        name: event::UPCOMING_CHANGED,
        windows: BUDDY,
    },
    EventSpec {
        name: event::OPEN_POPOVER,
        windows: BUDDY,
    },
    EventSpec {
        name: event::ACCOUNTS_CHANGED,
        windows: SETTINGS,
    },
];

// ---------------------------------------------------------------------------
// Bubble
// ---------------------------------------------------------------------------

/// One reminder inside a bubble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleItem {
    /// Which reminder this is (its `kind` tells meeting from task).
    pub key: ReminderKey,
    /// Event title or task title.
    pub title: String,
    /// Meeting start or task due time. The frontend derives "in N min".
    pub at: DateTime<Utc>,
    /// Whether the meeting is all-day (`at` is then local midnight). Always
    /// false for tasks.
    pub all_day: bool,
    /// Meeting location, if any. Always `None` for tasks.
    pub location: Option<String>,
}

/// A speech bubble. Usually one item; several when reminders missed during
/// pause or quiet hours are replayed as one batch (spec 7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleView {
    /// Id for [`BubbleRequest`] and friends.
    pub id: BubbleId,
    /// The reminders shown, at least one.
    pub items: Vec<BubbleItem>,
    /// Bubbles waiting behind this one (the "+N" badge); 0 for none.
    pub queued: u32,
    /// Whether this is a replay batch after pause or quiet hours.
    pub replay: bool,
    /// Snooze choices in minutes, from settings; the first is the default.
    pub snooze_options_minutes: Vec<u32>,
}

/// Request naming a bubble.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BubbleRequest {
    /// The bubble.
    pub bubble_id: BubbleId,
}

/// Request for [`command::SNOOZE_BUBBLE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeBubbleRequest {
    /// The bubble.
    pub bubble_id: BubbleId,
    /// Snooze duration, in [`ReminderSettings::SNOOZE_RANGE`].
    pub minutes: u32,
}

/// Request for [`command::SNOOZE_ALL`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeAllRequest {
    /// Snooze duration, in [`ReminderSettings::SNOOZE_RANGE`].
    pub minutes: u32,
}

// ---------------------------------------------------------------------------
// Upcoming and tasks
// ---------------------------------------------------------------------------

/// Data for the upcoming popover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpcomingResponse {
    /// Next meetings from the cache, after the skip filters, sorted by
    /// start, at most [`UpcomingResponse::MAX_MEETINGS`].
    pub meetings: Vec<Event>,
    /// Open tasks sorted by due time (overdue ones first by nature).
    pub open_tasks: Vec<Task>,
    /// Done tasks, most recently done first (the collapsed section).
    pub done_tasks: Vec<Task>,
    /// Whether reminders are paused.
    pub paused: bool,
    /// When the calendar cache was last successfully synced, if ever.
    pub last_synced_at: Option<DateTime<Utc>>,
}

impl UpcomingResponse {
    /// Maximum number of meetings returned.
    pub const MAX_MEETINGS: usize = 5;
}

/// Request for [`command::CREATE_TASK`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskRequest {
    /// Title, non-blank, at most [`Task::MAX_TITLE_LEN`] characters.
    pub title: String,
    /// Due time. May be in the past (fires immediately).
    pub due_at: DateTime<Utc>,
}

/// Request for [`command::UPDATE_TASK`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTaskRequest {
    /// The task.
    pub task_id: TaskId,
    /// New title (same rules as on create).
    pub title: String,
    /// New due time.
    pub due_at: DateTime<Utc>,
}

/// Request for [`command::SET_TASK_DONE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTaskDoneRequest {
    /// The task.
    pub task_id: TaskId,
    /// Done (true) or undone (false).
    pub done: bool,
}

/// Request naming a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRequest {
    /// The task.
    pub task_id: TaskId,
}

/// Request for [`command::SET_PAUSED`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPausedRequest {
    /// Pause (true) or resume (false).
    pub paused: bool,
}

/// Which popover view a native menu asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PopoverView {
    /// The upcoming list.
    Upcoming,
    /// The add-task form.
    AddTask,
}

// ---------------------------------------------------------------------------
// Settings and accounts
// ---------------------------------------------------------------------------

/// Request for [`command::UPDATE_SETTINGS`]: every section except accounts,
/// which change only through the account commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSettingsRequest {
    /// Reminder behavior.
    pub reminders: ReminderSettings,
    /// Appearance.
    pub appearance: AppearanceSettings,
    /// System integration.
    pub system: SystemSettings,
}

/// Sync state of one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Never synced yet (just added, or no calendars included).
    NeverSynced,
    /// Syncing now.
    Syncing,
    /// Last sync succeeded.
    Ok,
    /// Last sync failed; see [`AccountStatus::last_error`].
    Error,
}

/// An account with its runtime status, for the settings account rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    /// The saved configuration.
    pub account: AccountConfig,
    /// Current sync state.
    pub sync_state: SyncState,
    /// When the last successful sync finished.
    pub last_synced_at: Option<DateTime<Utc>>,
    /// The last sync error, cleared on success.
    pub last_error: Option<super::Error>,
    /// Whether a secret (password or refresh token) is in the keychain.
    /// A Graph account without one needs [`command::SIGN_IN_ACCOUNT`].
    pub has_secret: bool,
}

/// Request for [`command::ADD_ACCOUNT`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddAccountRequest {
    /// Display name.
    pub display_name: String,
    /// Connection details.
    pub connection: AccountConnection,
    /// CalDAV password or app password. Must be `None` for Graph (which
    /// uses [`command::SIGN_IN_ACCOUNT`]).
    pub secret: Option<SecretString>,
}

/// Request for [`command::UPDATE_ACCOUNT`]. Changing the connection type
/// (Graph to CalDAV or back) is rejected with `invalidInput`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountRequest {
    /// The account.
    pub account_id: AccountId,
    /// New display name.
    pub display_name: String,
    /// New connection details, same type as before.
    pub connection: AccountConnection,
    /// New set of included calendars.
    pub included_calendars: Vec<CalendarId>,
    /// New CalDAV password; `None` keeps the stored one.
    pub secret: Option<SecretString>,
}

/// A connected monitor, for [`command::LIST_MONITORS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    /// Name as stored in [`AppearanceSettings::monitor`].
    pub name: String,
    /// Whether this is the primary monitor.
    pub primary: bool,
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
}

/// Request naming an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRequest {
    /// The account.
    pub account_id: AccountId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn command_names_are_unique_snake_case() {
        let mut seen = HashSet::new();
        for c in COMMANDS {
            assert!(seen.insert(c.name), "duplicate command {}", c.name);
            assert!(
                c.name
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{} is not snake_case",
                c.name
            );
            assert!(!c.windows.is_empty());
        }
    }

    #[test]
    fn event_names_are_unique_kebab_case() {
        let mut seen = HashSet::new();
        for e in EVENTS {
            assert!(seen.insert(e.name), "duplicate event {}", e.name);
            assert!(e
                .name
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch == '-'));
        }
    }

    #[test]
    fn buddy_label_matches_lib_constant() {
        assert_eq!(Window::Buddy.label(), crate::BUDDY_WINDOW_LABEL);
    }

    #[test]
    fn requests_use_camel_case() {
        let req: CreateTaskRequest =
            serde_json::from_str(r#"{"title": "Call mum", "dueAt": "2026-11-16T17:00:00Z"}"#)
                .unwrap();
        assert_eq!(req.title, "Call mum");

        let add: AddAccountRequest = serde_json::from_str(
            r#"{"displayName": "Home", "secret": "pw",
                "connection": {"type": "caldav", "serverUrl": "https://x", "username": "me"}}"#,
        )
        .unwrap();
        assert_eq!(add.secret.unwrap().expose(), "pw");

        let snooze: SnoozeBubbleRequest =
            serde_json::from_str(r#"{"bubbleId": 3, "minutes": 5}"#).unwrap();
        assert_eq!(snooze.bubble_id, BubbleId(3));
        assert_eq!(
            serde_json::to_value(PopoverView::AddTask).unwrap(),
            "addTask"
        );
    }
}
