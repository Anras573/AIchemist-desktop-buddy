//! The app's SQLite database at runtime (`docs/storage.md`).
//!
//! [`init`] opens `app_data_dir()/buddy.sqlite` once at startup and keeps the
//! [`Storage`] handle in managed state; [`storage`] hands out clones. Pruning
//! runs once at startup, off the main thread. The scheduler and the sync
//! engine (2.2, 3.5) reuse the same handle.
//!
//! [`blocking`] runs storage work on Tauri's blocking thread pool, so IPC
//! commands never touch SQLite on the main thread.

use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};

use crate::domain::{Error, Result, SystemClock};
use crate::storage::{Storage, DEFAULT_RETENTION};

/// File name of the database inside the app data directory.
pub const DATABASE_FILE_NAME: &str = "buddy.sqlite";

/// Managed state: the open database, or why it could not be opened.
struct AppStorage(std::result::Result<Storage, Error>);

/// Opens the database and registers it. Call once during setup. A failure
/// is logged and kept: the app still runs, and storage-backed commands
/// return the error instead of panicking.
pub fn init<R: Runtime>(app: &AppHandle<R>) {
    let opened = open(app);
    match &opened {
        Ok(storage) => {
            let storage = storage.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = storage.prune(DEFAULT_RETENTION) {
                    log::warn!("pruning the database failed: {e}");
                }
            });
        }
        Err(e) => log::error!("opening the database failed: {e}"),
    }
    app.manage(AppStorage(opened));
}

fn open<R: Runtime>(app: &AppHandle<R>) -> Result<Storage> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| Error::storage(format!("no app data directory: {e}")))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::storage(format!("creating the app data directory: {e}")))?;
    Storage::open(&dir.join(DATABASE_FILE_NAME), Arc::new(SystemClock))
}

/// A handle to the app database.
pub fn storage<R: Runtime>(app: &AppHandle<R>) -> Result<Storage> {
    match app.try_state::<AppStorage>() {
        Some(state) => state.0.clone(),
        None => Err(Error::internal("storage is not initialised")),
    }
}

/// Runs `f` on the blocking thread pool and awaits its result.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(Error::internal(format!("worker failed: {e}"))))
}
