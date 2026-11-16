//! Shared contracts: domain types, traits and the IPC contract.
//!
//! **Frozen.** Everything under `src/domain/` is built against by tasks running
//! in parallel. Do not change it in a feature task; raise the needed change
//! with the orchestrator (spec section 15, `docs/contracts.md`).
//!
//! All items are re-exported here, so downstream code can write
//! `use crate::domain::{Event, CalendarProvider, Clock};`. The IPC contract is
//! reached as `crate::domain::ipc`.

mod animation;
mod clock;
mod error;
mod event;
mod fake_provider;
mod ids;
pub mod ipc;
mod provider;
mod reminder;
mod secret;
mod settings;
mod task;

pub use animation::{BuddyEvent, BuddyState};
pub use clock::{Clock, FakeClock, SystemClock};
pub use error::{Error, FieldError, Result};
pub use event::{CalendarInfo, Event, EventKey, EventTime, ResponseStatus};
pub use fake_provider::FakeCalendarProvider;
pub use ids::{AccountId, BubbleId, CalendarId, EventId, TaskId};
pub use provider::{CalendarProvider, TaskSource};
pub use reminder::{ReminderInstance, ReminderKey, ReminderKind, ReminderOutcome, ReminderStatus};
pub use secret::SecretString;
pub use settings::{
    AccountConfig, AccountConnection, AppearanceSettings, QuietHours, ReminderSettings,
    ScreenCorner, Settings, SystemSettings,
};
pub use task::Task;
