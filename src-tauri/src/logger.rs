//! A minimal file logger for the `log` facade.
//!
//! Why not `tauri-plugin-log`: it pulls in `fern`, `time` and a plugin with
//! its own IPC permissions, for features we do not need (webview logging,
//! several targets, formatting options). This is one small `log::Log`
//! implementation with no new crates: one open file, one mutex.
//!
//! - **File:** `app_log_dir()/aichemist-buddy.log`. When it reaches
//!   [`MAX_BYTES`] it is renamed to `aichemist-buddy.old.log` (replacing an
//!   older one) and a new file is started, so at most `2 * MAX_BYTES` are
//!   kept.
//! - **Levels:** `info` and up for this crate, `warn` and up for
//!   dependencies (Tauri, tao, wry log at info for routine things).
//! - **Content:** callers must never log secrets, tokens or event/task
//!   contents (spec 9). Messages from this crate log field paths and error
//!   kinds only.
//! - Debug builds also echo to stderr.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Log file name inside the app log directory.
pub const LOG_FILE_NAME: &str = "aichemist-buddy.log";
/// Name of the rotated file.
pub const OLD_LOG_FILE_NAME: &str = "aichemist-buddy.old.log";
/// Size at which the file is rotated.
pub const MAX_BYTES: u64 = 1024 * 1024;

/// Prefix of this crate's log targets.
const OWN_TARGET: &str = "aichemist_desktop_buddy";

struct FileLogger {
    dir: PathBuf,
    file: Mutex<Option<(File, u64)>>,
}

/// Installs the logger, writing into `dir` (created if missing). Logging
/// stays a no-op if the directory cannot be created or a logger is already
/// installed. Call once, as early as possible.
pub fn init(dir: &Path) {
    let file = fs::create_dir_all(dir).ok().and_then(|()| open(dir));
    let logger = FileLogger {
        dir: dir.to_path_buf(),
        file: Mutex::new(file),
    };
    // Leaked once: the logger lives for the whole process. (`set_logger`
    // instead of `set_boxed_logger`, which needs log's `std` feature.)
    if log::set_logger(Box::leak(Box::new(logger))).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

fn open(dir: &Path) -> Option<(File, u64)> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG_FILE_NAME))
        .ok()?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some((file, len))
}

/// Whether a record with this target and level is logged.
fn enabled(target: &str, level: Level) -> bool {
    if target.starts_with(OWN_TARGET) {
        level <= Level::Info
    } else {
        level <= Level::Warn
    }
}

/// One log line, without secrets by contract of the callers.
fn format_line(
    now: chrono::DateTime<chrono::Utc>,
    level: Level,
    target: &str,
    msg: &str,
) -> String {
    format!(
        "{} {:<5} {}: {}\n",
        now.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
        level,
        target,
        msg
    )
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        enabled(metadata.target(), metadata.level())
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            chrono::Utc::now(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|(_, len)| *len >= MAX_BYTES) {
            *guard = None; // Close before renaming (required on Windows).
            let _ = fs::rename(
                self.dir.join(LOG_FILE_NAME),
                self.dir.join(OLD_LOG_FILE_NAME),
            );
            *guard = open(&self.dir);
        }
        if let Some((file, len)) = guard.as_mut() {
            if file.write_all(line.as_bytes()).is_ok() {
                *len += line.len() as u64;
            }
        }
    }

    fn flush(&self) {
        if let Some((file, _)) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_crate_logs_info_dependencies_only_warn() {
        assert!(enabled("aichemist_desktop_buddy_lib::storage", Level::Info));
        assert!(!enabled(
            "aichemist_desktop_buddy_lib::storage",
            Level::Debug
        ));
        assert!(enabled("tao::window", Level::Warn));
        assert!(!enabled("tao::window", Level::Info));
    }

    #[test]
    fn line_format() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-16T09:00:00Z")
            .unwrap()
            .to_utc();
        assert_eq!(
            format_line(at, Level::Warn, "x", "hello"),
            "2026-11-16T09:00:00.000Z WARN  x: hello\n"
        );
    }

    #[test]
    fn rotates_at_max_bytes() {
        let dir = std::env::temp_dir().join(format!("aichemist-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let logger = FileLogger {
            dir: dir.clone(),
            file: Mutex::new(open(&dir)),
        };
        // Pretend the file is full; the next record rotates it.
        logger.file.lock().unwrap().as_mut().unwrap().1 = MAX_BYTES;
        logger.log(
            &Record::builder()
                .level(Level::Warn)
                .target(OWN_TARGET)
                .args(format_args!("after rotation"))
                .build(),
        );
        logger.flush();
        drop(logger);
        assert!(dir.join(OLD_LOG_FILE_NAME).exists());
        let text = fs::read_to_string(dir.join(LOG_FILE_NAME)).unwrap();
        assert!(text.ends_with("after rotation\n"), "{text}");
        let _ = fs::remove_dir_all(&dir);
    }
}
