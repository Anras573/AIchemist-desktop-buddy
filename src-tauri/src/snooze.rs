//! "Snooze all" (spec 4.3): snooze the current bubble and everything queued
//! behind it.
//!
//! **Stub until task 2.2** wires the scheduler: [`snooze_all_for`] validates the
//! duration, logs and returns `Ok`. The `snooze_all` command and the cat
//! menu's "Snooze all" both go through [`snooze_all_for`], so 2.2 only
//! replaces its body.

use tauri::{AppHandle, Runtime};

use crate::domain::ipc::SnoozeAllRequest;
use crate::domain::{Error, ReminderSettings, Result};

/// Snoozes every shown and queued reminder for `minutes`.
pub fn snooze_all_for<R: Runtime>(_app: &AppHandle<R>, minutes: u32) -> Result<()> {
    if !ReminderSettings::SNOOZE_RANGE.contains(&minutes) {
        return Err(Error::invalid_input(format!(
            "snooze minutes must be in {}..={}",
            ReminderSettings::SNOOZE_RANGE.start(),
            ReminderSettings::SNOOZE_RANGE.end()
        )));
    }
    // 2.2: forward to the scheduler (snooze the current bubble and the queue).
    log::info!("snooze all for {minutes} min: no scheduler yet (task 2.2)");
    Ok(())
}

/// The default snooze duration: the first configured option (spec 7.1).
pub fn default_minutes(reminders: &ReminderSettings) -> u32 {
    reminders
        .snooze_durations_minutes
        .first()
        .copied()
        .unwrap_or(5)
}

/// IPC `snooze_all`. Async, so the scheduler call (2.2) never runs on the
/// main thread.
#[tauri::command]
pub async fn snooze_all<R: Runtime>(app: AppHandle<R>, request: SnoozeAllRequest) -> Result<()> {
    snooze_all_for(&app, request.minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_the_first_option() {
        let mut r = ReminderSettings {
            snooze_durations_minutes: vec![10, 5],
            ..ReminderSettings::default()
        };
        assert_eq!(default_minutes(&r), 10);
        r.snooze_durations_minutes.clear();
        assert_eq!(default_minutes(&r), 5);
    }
}
