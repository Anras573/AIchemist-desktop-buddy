//! The settings file (spec 8): [`Settings`] as JSON in the OS app-config
//! directory.
//!
//! The path is injected. Resolve the real directory at the call site with
//! Tauri's path API (`app.path().app_config_dir()`) and join
//! [`SETTINGS_FILE_NAME`].
//!
//! # Loading is forgiving
//!
//! [`load_settings`] never fails, so a broken file cannot keep the app from
//! starting. It keeps as much of the file as it can and logs a warning (field
//! paths and line numbers only, never values) for what it drops:
//!
//! * missing file: defaults, no warning;
//! * unreadable file or invalid JSON: defaults;
//! * a section (`reminders`, `appearance`, `system`) of the wrong shape or
//!   failing [`Settings::validate`]: that section is reset to its defaults;
//! * an account of the wrong shape or failing validation (including a
//!   duplicate id): that account is dropped;
//! * a wrong `schemaVersion`: set to the current version.
//!
//! The file itself is not rewritten on load; the next [`save_settings`] does
//! that.
//!
//! # Saving is atomic
//!
//! [`save_settings`] validates, writes a temporary file next to the target,
//! flushes it to disk and renames it over the target, so a crash leaves
//! either the old or the new file, never a partial one. Settings hold no
//! secrets: [`SecretString`](crate::domain::SecretString) is not
//! `Serialize`, so it cannot end up here.

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::domain::{
    AccountConfig, AppearanceSettings, Error, ReminderSettings, Result, Settings, SystemSettings,
};

/// The settings file name inside the app-config directory.
pub const SETTINGS_FILE_NAME: &str = "settings.json";

/// Loads the settings at `path`. See the module docs for how a missing or
/// invalid file is handled. Never fails.
pub fn load_settings(path: &Path) -> Settings {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            log::warn!(
                "cannot read the settings file ({}); using defaults",
                e.kind()
            );
            return Settings::default();
        }
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(e) => {
            log::warn!(
                "the settings file is not valid JSON (line {}, column {}); using defaults",
                e.line(),
                e.column()
            );
            return Settings::default();
        }
    };
    sanitize(parse_lenient(value))
}

/// Validates `settings` and writes them to `path` atomically, creating the
/// directory if needed. Fails with [`Error::Validation`] for invalid settings
/// (the file is not touched) or [`Error::Storage`] if writing fails.
pub fn save_settings(path: &Path, settings: &Settings) -> Result<()> {
    settings
        .validate()
        .map_err(|errors| Error::Validation { errors })?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| Error::internal(format!("cannot serialize settings: {e}")))?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).map_err(|e| io_error("create the settings directory", &e))?;
    }
    let tmp = temp_path(path);
    let written = write_synced(&tmp, &json).and_then(|()| fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_error("write the settings file", &e));
    }
    Ok(())
}

/// `settings.json` -> `settings.json.tmp`, in the same directory so the
/// rename stays on one file system.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn io_error(action: &str, e: &std::io::Error) -> Error {
    Error::storage(format!("cannot {action}: {e}"))
}

/// Deserializes `value`, falling back to defaults per section (and dropping
/// single accounts) where a part has the wrong shape.
fn parse_lenient(value: Value) -> Settings {
    if let Ok(settings) = serde_json::from_value::<Settings>(value.clone()) {
        return settings;
    }
    let Value::Object(mut map) = value else {
        log::warn!("the settings file is not a JSON object; using defaults");
        return Settings::default();
    };
    let mut settings = Settings::default();
    if let Some(v) = map.remove("schemaVersion") {
        settings.schema_version = section(v, "schemaVersion").unwrap_or(settings.schema_version);
    }
    if let Some(v) = map.remove("accounts") {
        settings.accounts = match v {
            Value::Array(items) => items
                .into_iter()
                .enumerate()
                .filter_map(|(i, item)| section::<AccountConfig>(item, &format!("accounts[{i}]")))
                .collect(),
            _ => {
                log::warn!("settings: `accounts` is not a list; dropping it");
                Vec::new()
            }
        };
    }
    if let Some(v) = map.remove("reminders") {
        settings.reminders = section::<ReminderSettings>(v, "reminders").unwrap_or_default();
    }
    if let Some(v) = map.remove("appearance") {
        settings.appearance = section::<AppearanceSettings>(v, "appearance").unwrap_or_default();
    }
    if let Some(v) = map.remove("system") {
        settings.system = section::<SystemSettings>(v, "system").unwrap_or_default();
    }
    settings
}

fn section<T: DeserializeOwned>(value: Value, path: &str) -> Option<T> {
    serde_json::from_value(value)
        .inspect_err(|_| log::warn!("settings: `{path}` has the wrong shape; using defaults"))
        .ok()
}

/// Resets the parts that fail validation. Falls back to all defaults if
/// that still does not validate.
fn sanitize(mut settings: Settings) -> Settings {
    let Err(errors) = settings.validate() else {
        return settings;
    };
    let mut bad_accounts = BTreeSet::new();
    for error in &errors {
        log::warn!("settings: `{}` is invalid; using defaults", error.field);
        let field = error.field.as_str();
        if field.starts_with("schemaVersion") {
            settings.schema_version = Settings::SCHEMA_VERSION;
        } else if field.starts_with("reminders") {
            settings.reminders = ReminderSettings::default();
        } else if field.starts_with("appearance") {
            settings.appearance = AppearanceSettings::default();
        } else if field.starts_with("system") {
            settings.system = SystemSettings::default();
        } else if let Some(i) = account_index(field) {
            bad_accounts.insert(i);
        }
    }
    let mut i = 0;
    settings.accounts.retain(|_| {
        i += 1;
        !bad_accounts.contains(&(i - 1))
    });
    if settings.validate().is_ok() {
        settings
    } else {
        log::warn!("settings are still invalid after repair; using defaults");
        Settings::default()
    }
}

/// `accounts[3].displayName` -> `Some(3)`.
fn account_index(field: &str) -> Option<usize> {
    field
        .strip_prefix("accounts[")?
        .split_once(']')?
        .0
        .parse()
        .ok()
}
