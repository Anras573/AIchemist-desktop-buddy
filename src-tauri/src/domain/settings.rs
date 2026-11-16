//! User settings (spec section 8), persisted as JSON in the OS app-config
//! directory. Secrets never appear here; they live in the OS keychain keyed by
//! [`AccountId`].
//!
//! Every struct is `#[serde(default)]`, so a settings file missing fields (an
//! older version, or hand-edited) loads with defaults for the missing parts.
//! Unknown fields are ignored. Loading does not validate: call
//! [`Settings::validate`] and fall back to defaults for invalid sections.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

use super::{AccountId, CalendarId, FieldError};

/// All user settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Format version of the settings file, for future migrations.
    /// Currently [`Settings::SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Calendar accounts (without secrets).
    pub accounts: Vec<AccountConfig>,
    /// Reminder behavior.
    pub reminders: ReminderSettings,
    /// Appearance of the cat.
    pub appearance: AppearanceSettings,
    /// System integration.
    pub system: SystemSettings,
}

impl Settings {
    /// The current settings file format version.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Checks every field. Returns all problems at once, with paths in the
    /// JSON (camelCase) form, e.g. `reminders.leadTimesMinutes[1]`.
    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        let mut errors = Vec::new();
        if self.schema_version != Self::SCHEMA_VERSION {
            errors.push(FieldError::new(
                "schemaVersion",
                format!("must be {}", Self::SCHEMA_VERSION),
            ));
        }
        let mut ids = HashSet::new();
        for (i, account) in self.accounts.iter().enumerate() {
            let path = format!("accounts[{i}]");
            if !ids.insert(&account.id) {
                errors.push(FieldError::new(
                    format!("{path}.id"),
                    "duplicate account id",
                ));
            }
            account.validate_into(&path, &mut errors);
        }
        self.reminders.validate_into("reminders", &mut errors);
        self.appearance.validate_into("appearance", &mut errors);
        self.system.validate_into("system", &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            accounts: Vec::new(),
            reminders: ReminderSettings::default(),
            appearance: AppearanceSettings::default(),
            system: SystemSettings::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/// A configured calendar account. The secret (CalDAV password, Graph refresh
/// token) is stored in the OS keychain under [`AccountConfig::id`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountConfig {
    /// Backend-assigned id. Unique among accounts.
    pub id: AccountId,
    /// Name shown in the UI, e.g. "Work". 1 to [`AccountConfig::MAX_NAME_LEN`] characters.
    pub display_name: String,
    /// How to connect.
    pub connection: AccountConnection,
    /// Calendars whose events are synced. Empty means none.
    pub included_calendars: Vec<CalendarId>,
}

impl AccountConfig {
    /// Maximum display name length in characters.
    pub const MAX_NAME_LEN: usize = 100;

    /// Appends this account's problems to `errors`, prefixing paths with `path`.
    fn validate_into(&self, path: &str, errors: &mut Vec<FieldError>) {
        if self.id.as_str().trim().is_empty() {
            errors.push(FieldError::new(format!("{path}.id"), "must not be empty"));
        }
        check_text(
            &self.display_name,
            Self::MAX_NAME_LEN,
            &format!("{path}.displayName"),
            errors,
        );
        let conn = format!("{path}.connection");
        match &self.connection {
            AccountConnection::Graph { client_id, tenant } => {
                check_text(client_id, 200, &format!("{conn}.clientId"), errors);
                check_text(tenant, 200, &format!("{conn}.tenant"), errors);
            }
            AccountConnection::CalDav {
                server_url,
                username,
            } => {
                let url = server_url.trim();
                if !url.starts_with("https://") || url.len() <= "https://".len() {
                    errors.push(FieldError::new(
                        format!("{conn}.serverUrl"),
                        "must be an https:// URL",
                    ));
                }
                check_text(username, 200, &format!("{conn}.username"), errors);
            }
        }
    }
}

/// Connection details for an account. Serialized with a `type` tag:
/// `{"type": "graph", "clientId": ..., "tenant": ...}` or
/// `{"type": "caldav", "serverUrl": ..., "username": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AccountConnection {
    /// Microsoft Graph. The user supplies their own Azure app registration.
    #[serde(rename = "graph", rename_all = "camelCase")]
    Graph {
        /// Application (client) id of the user's Azure app registration.
        client_id: String,
        /// Tenant for the authority URL: `common`, `organizations`,
        /// `consumers` or a tenant id/domain. Defaults to `common` in the UI.
        tenant: String,
    },
    /// CalDAV with Basic auth (password or app password in the keychain).
    #[serde(rename = "caldav", rename_all = "camelCase")]
    CalDav {
        /// Server or principal URL; must be `https://`.
        server_url: String,
        /// User name.
        username: String,
    },
}

// ---------------------------------------------------------------------------
// Reminders
// ---------------------------------------------------------------------------

/// Reminder behavior (spec 7 and 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReminderSettings {
    /// Minutes before a meeting starts at which to remind. Default `[10, 5, 1]`.
    /// 1 to [`ReminderSettings::MAX_LEAD_TIMES`] distinct values, each in
    /// [`ReminderSettings::LEAD_TIME_RANGE`]. `0` means "at start".
    pub lead_times_minutes: Vec<u32>,
    /// Snooze durations offered in the bubble, in minutes. Default `[5]`.
    /// 1 to [`ReminderSettings::MAX_SNOOZE_DURATIONS`] distinct values, each
    /// in [`ReminderSettings::SNOOZE_RANGE`]. The first is the default action.
    pub snooze_durations_minutes: Vec<u32>,
    /// Play the chime when a bubble appears. Default on.
    pub sound_enabled: bool,
    /// Skip all-day events. Default on.
    pub skip_all_day: bool,
    /// Skip events the user declined. Default on.
    pub skip_declined: bool,
    /// Calendar poll interval in minutes, in [`ReminderSettings::SYNC_INTERVAL_RANGE`].
    /// Default 10.
    pub sync_interval_minutes: u32,
}

impl ReminderSettings {
    /// Allowed lead time values, in minutes.
    pub const LEAD_TIME_RANGE: RangeInclusive<u32> = 0..=120;
    /// Maximum number of lead times.
    pub const MAX_LEAD_TIMES: usize = 10;
    /// Allowed snooze durations, in minutes.
    pub const SNOOZE_RANGE: RangeInclusive<u32> = 1..=120;
    /// Maximum number of snooze durations.
    pub const MAX_SNOOZE_DURATIONS: usize = 5;
    /// Allowed sync intervals, in minutes (spec 6.5).
    pub const SYNC_INTERVAL_RANGE: RangeInclusive<u32> = 5..=15;

    fn validate_into(&self, path: &str, errors: &mut Vec<FieldError>) {
        check_minutes_list(
            &self.lead_times_minutes,
            Self::LEAD_TIME_RANGE,
            Self::MAX_LEAD_TIMES,
            &format!("{path}.leadTimesMinutes"),
            errors,
        );
        check_minutes_list(
            &self.snooze_durations_minutes,
            Self::SNOOZE_RANGE,
            Self::MAX_SNOOZE_DURATIONS,
            &format!("{path}.snoozeDurationsMinutes"),
            errors,
        );
        check_range(
            self.sync_interval_minutes,
            Self::SYNC_INTERVAL_RANGE,
            &format!("{path}.syncIntervalMinutes"),
            errors,
        );
    }
}

impl Default for ReminderSettings {
    fn default() -> Self {
        Self {
            lead_times_minutes: vec![10, 5, 1],
            snooze_durations_minutes: vec![5],
            sound_enabled: true,
            skip_all_day: true,
            skip_declined: true,
            sync_interval_minutes: 10,
        }
    }
}

// ---------------------------------------------------------------------------
// Appearance
// ---------------------------------------------------------------------------

/// Screen corner the cat sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScreenCorner {
    /// Top left.
    TopLeft,
    /// Top right.
    TopRight,
    /// Bottom left.
    BottomLeft,
    /// Bottom right (default, spec 14.6).
    #[default]
    BottomRight,
}

/// Appearance of the cat (spec 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppearanceSettings {
    /// Integer pixel scale of the sprite (nearest-neighbour), in
    /// [`AppearanceSettings::SCALE_RANGE`]. Default 2.
    pub cat_scale: u32,
    /// Screen corner. Default bottom right.
    pub corner: ScreenCorner,
    /// Monitor name as reported by Tauri's `Monitor::name()`, or `None` for
    /// the primary monitor (default). If the named monitor is not connected,
    /// the primary monitor is used.
    pub monitor: Option<String>,
    /// Window opacity in percent, in [`AppearanceSettings::OPACITY_RANGE`].
    /// Default 100.
    pub opacity_percent: u32,
    /// Sprite pack folder name. `"default"` is the bundled pack. Must be a
    /// single path component (no `/`, `\`, or `..`).
    pub sprite_pack: String,
}

impl AppearanceSettings {
    /// Allowed scale factors.
    pub const SCALE_RANGE: RangeInclusive<u32> = 1..=8;
    /// Allowed opacity in percent.
    pub const OPACITY_RANGE: RangeInclusive<u32> = 20..=100;
    /// Name of the bundled sprite pack.
    pub const DEFAULT_SPRITE_PACK: &'static str = "default";

    fn validate_into(&self, path: &str, errors: &mut Vec<FieldError>) {
        check_range(
            self.cat_scale,
            Self::SCALE_RANGE,
            &format!("{path}.catScale"),
            errors,
        );
        check_range(
            self.opacity_percent,
            Self::OPACITY_RANGE,
            &format!("{path}.opacityPercent"),
            errors,
        );
        if let Some(monitor) = &self.monitor {
            check_text(monitor, 200, &format!("{path}.monitor"), errors);
        }
        let pack = &self.sprite_pack;
        let field = format!("{path}.spritePack");
        if check_text(pack, 100, &field, errors)
            && (pack.contains(['/', '\\', ':']) || pack == "." || pack == "..")
        {
            errors.push(FieldError::new(field, "must be a plain folder name"));
        }
    }
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            cat_scale: 2,
            corner: ScreenCorner::BottomRight,
            monitor: None,
            opacity_percent: 100,
            sprite_pack: Self::DEFAULT_SPRITE_PACK.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

/// A daily do-not-disturb window in local time (spec 7.5). Bubbles that fall
/// due inside it are suppressed like while paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct QuietHours {
    /// Whether quiet hours are active. Default off (spec 14.5).
    pub enabled: bool,
    /// Local start time, inclusive. Default 22:00. JSON `"22:00:00"`
    /// (`"22:00"` is accepted on input).
    pub start: NaiveTime,
    /// Local end time, exclusive. Default 07:00. If `end < start` the window
    /// wraps past midnight. Must differ from `start`.
    pub end: NaiveTime,
}

impl QuietHours {
    /// Whether the local time `t` is inside the window (ignores `enabled`).
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.start <= self.end {
            self.start <= t && t < self.end
        } else {
            t >= self.start || t < self.end
        }
    }
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start: NaiveTime::from_hms_opt(22, 0, 0).expect("valid time"),
            end: NaiveTime::from_hms_opt(7, 0, 0).expect("valid time"),
        }
    }
}

/// System integration (spec 8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SystemSettings {
    /// Start the app at login. Default off.
    pub launch_at_login: bool,
    /// Quiet hours. Default off.
    pub quiet_hours: QuietHours,
    /// Minutes without interaction before the cat sleeps, in
    /// [`SystemSettings::SLEEP_AFTER_IDLE_RANGE`]. Default 10.
    pub sleep_after_idle_minutes: u32,
    /// Opt-in update check. Default off (no network traffic unless enabled).
    pub check_for_updates: bool,
}

impl SystemSettings {
    /// Allowed sleep-after-idle values, in minutes.
    pub const SLEEP_AFTER_IDLE_RANGE: RangeInclusive<u32> = 1..=240;

    fn validate_into(&self, path: &str, errors: &mut Vec<FieldError>) {
        check_range(
            self.sleep_after_idle_minutes,
            Self::SLEEP_AFTER_IDLE_RANGE,
            &format!("{path}.sleepAfterIdleMinutes"),
            errors,
        );
        if self.quiet_hours.start == self.quiet_hours.end {
            errors.push(FieldError::new(
                format!("{path}.quietHours.end"),
                "must differ from the start time",
            ));
        }
    }
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            launch_at_login: false,
            quiet_hours: QuietHours::default(),
            sleep_after_idle_minutes: 10,
            check_for_updates: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn check_range(value: u32, range: RangeInclusive<u32>, field: &str, errors: &mut Vec<FieldError>) {
    if !range.contains(&value) {
        errors.push(FieldError::new(
            field,
            format!("must be between {} and {}", range.start(), range.end()),
        ));
    }
}

/// Checks that `text` is non-blank and at most `max` characters. Returns
/// whether it passed.
fn check_text(text: &str, max: usize, field: &str, errors: &mut Vec<FieldError>) -> bool {
    if text.trim().is_empty() {
        errors.push(FieldError::new(field, "must not be empty"));
        false
    } else if text.chars().count() > max {
        errors.push(FieldError::new(
            field,
            format!("must be at most {max} characters"),
        ));
        false
    } else {
        true
    }
}

fn check_minutes_list(
    values: &[u32],
    range: RangeInclusive<u32>,
    max_len: usize,
    field: &str,
    errors: &mut Vec<FieldError>,
) {
    if values.is_empty() || values.len() > max_len {
        errors.push(FieldError::new(
            field,
            format!("must have between 1 and {max_len} entries"),
        ));
    }
    let mut seen = HashSet::new();
    for (i, &v) in values.iter().enumerate() {
        let item = format!("{field}[{i}]");
        check_range(v, range.clone(), &item, errors);
        if !seen.insert(v) {
            errors.push(FieldError::new(item, "duplicate value"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(settings: &Settings) -> Vec<String> {
        settings
            .validate()
            .err()
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.field)
            .collect()
    }

    fn caldav(id: &str) -> AccountConfig {
        AccountConfig {
            id: AccountId::new(id),
            display_name: "Home".into(),
            connection: AccountConnection::CalDav {
                server_url: "https://caldav.example.com/".into(),
                username: "me".into(),
            },
            included_calendars: vec![],
        }
    }

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.schema_version, 1);
        assert!(s.accounts.is_empty());
        assert_eq!(s.reminders.lead_times_minutes, [10, 5, 1]);
        assert_eq!(s.reminders.snooze_durations_minutes, [5]);
        assert!(s.reminders.sound_enabled);
        assert!(s.reminders.skip_all_day);
        assert!(s.reminders.skip_declined);
        assert_eq!(s.reminders.sync_interval_minutes, 10);
        assert_eq!(s.appearance.corner, ScreenCorner::BottomRight);
        assert_eq!(s.appearance.monitor, None);
        assert_eq!(s.appearance.opacity_percent, 100);
        assert_eq!(s.appearance.sprite_pack, "default");
        assert!(!s.system.launch_at_login);
        assert!(!s.system.quiet_hours.enabled);
        assert_eq!(s.system.sleep_after_idle_minutes, 10);
        assert!(!s.system.check_for_updates);
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn empty_json_loads_defaults_and_round_trips() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        let partial: Settings =
            serde_json::from_str(r#"{"reminders": {"syncIntervalMinutes": 7}, "unknown": 1}"#)
                .unwrap();
        assert_eq!(partial.reminders.sync_interval_minutes, 7);
        assert_eq!(partial.reminders.lead_times_minutes, [10, 5, 1]);

        let mut full = Settings::default();
        full.accounts.push(caldav("a1"));
        let json = serde_json::to_string(&full).unwrap();
        assert!(json.contains(r#""type":"caldav""#));
        assert!(json.contains(r#""leadTimesMinutes":[10,5,1]"#));
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), full);
    }

    #[test]
    fn quiet_hours_accept_hh_mm_and_wrap_midnight() {
        let q: QuietHours =
            serde_json::from_str(r#"{"enabled": true, "start": "22:30", "end": "06:00"}"#).unwrap();
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert!(q.contains(t(23, 0)));
        assert!(q.contains(t(22, 30)));
        assert!(q.contains(t(5, 59)));
        assert!(!q.contains(t(6, 0)));
        assert!(!q.contains(t(12, 0)));
        let day = QuietHours {
            enabled: true,
            start: t(12, 0),
            end: t(13, 0),
        };
        assert!(day.contains(t(12, 30)));
        assert!(!day.contains(t(13, 0)));
    }

    #[test]
    fn sync_interval_edges() {
        let mut s = Settings::default();
        for (v, ok) in [(4, false), (5, true), (15, true), (16, false)] {
            s.reminders.sync_interval_minutes = v;
            assert_eq!(s.validate().is_ok(), ok, "sync interval {v}");
        }
        s.reminders.sync_interval_minutes = 16;
        assert_eq!(fields(&s), ["reminders.syncIntervalMinutes"]);
    }

    #[test]
    fn lead_time_and_snooze_lists() {
        let mut s = Settings::default();
        s.reminders.lead_times_minutes = vec![0, 120];
        assert!(s.validate().is_ok());

        s.reminders.lead_times_minutes = vec![];
        assert_eq!(fields(&s), ["reminders.leadTimesMinutes"]);

        s.reminders.lead_times_minutes = vec![10, 121, 10];
        assert_eq!(
            fields(&s),
            [
                "reminders.leadTimesMinutes[1]",
                "reminders.leadTimesMinutes[2]"
            ]
        );

        s.reminders.lead_times_minutes = (0..11).collect();
        assert_eq!(fields(&s), ["reminders.leadTimesMinutes"]);

        s.reminders.lead_times_minutes = vec![5];
        s.reminders.snooze_durations_minutes = vec![0];
        assert_eq!(fields(&s), ["reminders.snoozeDurationsMinutes[0]"]);
    }

    #[test]
    fn appearance_edges() {
        let mut s = Settings::default();
        s.appearance.cat_scale = 0;
        s.appearance.opacity_percent = 19;
        s.appearance.sprite_pack = "../evil".into();
        s.appearance.monitor = Some(" ".into());
        assert_eq!(
            fields(&s),
            [
                "appearance.catScale",
                "appearance.opacityPercent",
                "appearance.monitor",
                "appearance.spritePack"
            ]
        );
        s.appearance.cat_scale = 8;
        s.appearance.opacity_percent = 20;
        s.appearance.sprite_pack = "pixel-cat".into();
        s.appearance.monitor = Some("DISPLAY2".into());
        assert_eq!(s.validate(), Ok(()));
        s.appearance.sprite_pack = "..".into();
        assert_eq!(fields(&s), ["appearance.spritePack"]);
    }

    #[test]
    fn system_edges() {
        let mut s = Settings::default();
        s.system.sleep_after_idle_minutes = 0;
        s.system.quiet_hours.end = s.system.quiet_hours.start;
        assert_eq!(
            fields(&s),
            ["system.sleepAfterIdleMinutes", "system.quietHours.end"]
        );
        s.system.sleep_after_idle_minutes = 240;
        s.system.quiet_hours = QuietHours::default();
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn account_validation() {
        let mut s = Settings::default();
        let mut bad = caldav("a1");
        bad.display_name = " ".into();
        bad.connection = AccountConnection::CalDav {
            server_url: "http://insecure.example.com".into(),
            username: String::new(),
        };
        let graph = AccountConfig {
            id: AccountId::new("a1"),
            display_name: "Work".into(),
            connection: AccountConnection::Graph {
                client_id: String::new(),
                tenant: "common".into(),
            },
            included_calendars: vec![CalendarId::new("cal")],
        };
        s.accounts = vec![bad, graph];
        assert_eq!(
            fields(&s),
            [
                "accounts[0].displayName",
                "accounts[0].connection.serverUrl",
                "accounts[0].connection.username",
                "accounts[1].id",
                "accounts[1].connection.clientId"
            ]
        );
        s.accounts = vec![caldav("a1"), caldav("a2")];
        assert_eq!(s.validate(), Ok(()));
    }

    #[test]
    fn wrong_schema_version_is_reported() {
        let s = Settings {
            schema_version: 2,
            ..Settings::default()
        };
        assert_eq!(fields(&s), ["schemaVersion"]);
    }
}
