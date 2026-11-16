# Storage

Tasks, the event cache, per-account sync status and the reminder ledger live in one SQLite
database, accessed through `rusqlite` (spec section 3). Settings are a separate JSON file (see
[Settings file](#settings-file)). The code is in `src-tauri/src/storage/`.

## Opening a database

| Function | Use |
|---|---|
| `storage::open(path)` | Opens or creates the app database, configures the connection and migrates the schema. |
| `storage::open_in_memory()` | A fresh, migrated in-memory database. Use it in tests. |

Both return a `rusqlite::Connection`. Errors are `domain::Error::Storage`. There is a
`From<rusqlite::Error> for domain::Error` impl, so `?` works in code that returns
`domain::Result`.

Every connection gets:

- `foreign_keys = ON`, which SQLite leaves off by default. The current schema has no foreign
  keys, but later migrations may add some.
- `busy_timeout = 5000` ms, so a second connection waits for a lock instead of failing at once
  with `SQLITE_BUSY`.
- The default rollback journal (`journal_mode = DELETE`), not WAL. The app is a single process
  with small, infrequent writes (a sync every 10 minutes, the occasional task edit) and no
  concurrent readers that would benefit from WAL. WAL would add `-wal` and `-shm` files and
  checkpointing for no gain.

## The `Storage` handle

`storage::Storage` is what the rest of the app uses. It is `Clone` (an `Arc` inside), `Send` and
`Sync`: one `rusqlite::Connection` behind a `Mutex`, plus a `Clock`. Each method locks the
connection for one statement or one transaction. Writes are small and rare, so one shared
connection is enough. The methods block briefly, so call them from a worker thread or
`spawn_blocking`, not from the Tauri main thread.

`Storage::open(path, clock)` opens the app database; tests use
`Storage::open_in_memory(Arc::new(FakeClock::new(..)))`. The clock stamps `created_at`,
`done_at` and sync times, and converts query windows to local dates.

| Area | Methods |
|---|---|
| Tasks (`storage::tasks`) | `create_task(title, due_at)`, `get_task`, `list_tasks` (by due time), `update_task(id, title, due_at)`, `set_task_done(id, done)`, `delete_task`. `Storage` implements `TaskSource` (the local source). |
| Event cache (`storage::events`) | `replace_events(account, calendars, events)`, `events_in_window(from, to)`, `remove_account(account)` |
| Reminder ledger (`storage::reminders`) | `load_reminders()`, `save_reminders(&ledger)` |
| Sync state (`storage::sync_state`) | `record_sync_attempt`, `record_sync_success`, `record_sync_failure(account, &error)`, `sync_state(account)`, `list_sync_states()` |
| Pruning (`storage::prune`) | `prune(retention)`, `DEFAULT_RETENTION` (1 day) |

Rules the repositories implement:

- **Task ids** are 128 random bits as 32 lowercase hex digits, generated with std only
  (`RandomState` hashers are keyed from OS randomness). Titles are trimmed and must then be 1 to
  200 characters, otherwise `Error::Validation` on the field `title`. Times are truncated to
  whole seconds before they are returned, so a returned task equals the stored one.
  Marking a done task done again keeps its first `done_at`.
- **Event cache.** `replace_events` runs in one transaction: it deletes all of the account's
  events and inserts the new ones. Call it only after a successful sync, so a failed sync keeps
  the last good cache. Events of calendars outside the given set are dropped as well, because
  those calendars are no longer included. An event of another account or calendar is refused
  with `Error::InvalidInput`. A duplicate key keeps the last event.
- **Window queries** use the half-open window `[from, to)`. All-day events are compared on local
  dates, using the clock: `from_date` is the local date of `from`, and `to_date` is the local
  date of `to` rounded up to a whole day.
- **Ledger saves** upsert every instance against the partial unique index of its kind, with the
  index's `WHERE` clause repeated in the conflict target, then delete the rows whose key is not
  in the new ledger. All in one transaction.
- **Sync errors** keep only the error kind and message, cut to 500 characters.
- **Pruning** deletes events that ended at or before `now - retention`, and meeting reminder rows
  due before that cutoff whose event is no longer cached. Task reminder rows follow the
  scheduler's ledger. Call it after a sync or at startup.

## Settings file

`storage::load_settings(path)` and `storage::save_settings(path, &settings)`. The caller resolves
the directory with Tauri's path API (`app_config_dir()`) and joins `SETTINGS_FILE_NAME`
(`settings.json`).

- Loading never fails. A missing file gives defaults. An unreadable file or invalid JSON gives
  defaults. A section with the wrong shape or failing `Settings::validate` is reset to its
  defaults, an invalid account (including a duplicate id) is dropped, and a wrong
  `schemaVersion` is reset. Each fallback logs a warning (`log` crate) with field paths only,
  never values. Loading does not rewrite the file.
- Saving validates first (`Error::Validation`, file untouched), then writes `settings.json.tmp`,
  flushes it, and renames it over `settings.json`. A crash leaves the old or the new file.
- Settings never contain secrets. `SecretString` is not `Serialize`.

## Conventions

| Domain type | Column type | Encoding |
|---|---|---|
| `DateTime<Utc>` (instants) | `INTEGER` | Unix seconds, UTC. Sub-second precision is dropped. |
| `NaiveDate` (all-day dates) | `TEXT` | `YYYY-MM-DD`. Floating, no time zone. Sorts correctly as text. |
| Enums | `TEXT` | The camelCase serde name, e.g. `notResponded`, `markedDone`. |
| `bool` | `INTEGER` | `0` or `1`. |
| Ids (`AccountId`, `TaskId`, ...) | `TEXT` | The raw string. |

`storage::codec` implements these encodings (`instant_to_sql`, `date_from_sql`,
`response_to_sql`, ...). Use it rather than encoding values by hand.

All tables are `STRICT`, so SQLite rejects values of the wrong type. `CHECK` constraints enforce
enum values and the shape of each row, so a row that cannot map back to a domain value cannot be
stored.

## Tables (schema version 1)

### `tasks`

One row per local `Task` (spec 7.2).

| Column | Type | Notes |
|---|---|---|
| `id` | TEXT PK | `TaskId`. The repository chooses the format. |
| `title` | TEXT | 1 to 200 characters (`Task::MAX_TITLE_LEN`). SQLite counts characters, not bytes. |
| `due_at` | INTEGER | Instant. |
| `done` | INTEGER | 0 or 1. |
| `done_at` | INTEGER NULL | Instant. Non-null exactly when `done = 1`. |
| `created_at` | INTEGER | Instant. |

### `events`

The cache of upcoming event occurrences (spec 6.5). The primary key is the `EventKey`:
(`account_id`, `calendar_id`, `event_id`).

| Column | Type | Notes |
|---|---|---|
| `account_id`, `calendar_id`, `event_id` | TEXT | The `EventKey`. |
| `title` | TEXT | May be empty. |
| `time_kind` | TEXT | `timed` or `allDay`. |
| `start_at`, `end_at` | INTEGER NULL | Instants. Set only for `timed`, with `start_at <= end_at`. |
| `start_date`, `end_date` | TEXT NULL | Dates. Set only for `allDay`, end exclusive, with `start_date < end_date`. |
| `location` | TEXT NULL | |
| `response` | TEXT | `organizer`, `accepted`, `tentative`, `declined`, `notResponded`, `unknown`. |

Indexes: `events_start_at (start_at)` and `events_start_date (start_date)`. A range query covers
both representations, for example
`WHERE (start_at < :to_secs AND end_at > :from_secs) OR (start_date < :to_date AND end_date > :from_date)`.
All-day dates are floating: convert the window to local dates with the `Clock` before comparing.

### `sync_state`

One row per account that has attempted a sync (spec 6.5, 8).

| Column | Type | Notes |
|---|---|---|
| `account_id` | TEXT PK | |
| `last_attempt_at` | INTEGER NULL | When the last sync started. |
| `last_success_at` | INTEGER NULL | When the last successful sync finished. |
| `last_error_kind` | TEXT NULL | The `domain::Error` kind tag (`network`, `auth`, ...). See `codec::error_kind_to_sql`. |
| `last_error_message` | TEXT NULL | The error message. It never contains secrets or event contents (spec 9). |
| `last_error_at` | INTEGER NULL | When the error happened. |

The three `last_error_*` columns are either all set or all null. Clear them after a successful
sync.

### `reminders`

The reminder ledger: one row per `ReminderKey` that the scheduler has seen (spec 7.1).

| Column | Type | Notes |
|---|---|---|
| `id` | INTEGER PK | Row id. Internal only. |
| `kind` | TEXT | `meeting` or `task`. |
| `account_id`, `calendar_id`, `event_id`, `lead_minutes` | NULL unless `meeting` | `ReminderKey::Meeting`. All four are set for meetings. |
| `task_id` | TEXT, NULL unless `task` | `ReminderKey::Task`. |
| `due_at` | INTEGER | The original due instant. |
| `status` | TEXT | `pending`, `snoozed` or `handled`. |
| `snoozed_until` | INTEGER NULL | Set exactly when `snoozed`. |
| `handled_at`, `outcome` | NULL unless `handled` | `outcome` is `dismissed`, `markedDone` or `skipped`. |

**"Fires at most once" is enforced by the database.** Two partial unique indexes allow only one
row per key:

- `reminders_meeting_key (account_id, calendar_id, event_id, lead_minutes) WHERE kind = 'meeting'`
- `reminders_task_key (task_id) WHERE kind = 'task'`

To write a state change, upsert against the matching index. The conflict target must repeat the
index's `WHERE` clause:

```sql
INSERT INTO reminders (kind, task_id, due_at, status, snoozed_until, handled_at, outcome)
VALUES ('task', ?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT (task_id) WHERE kind = 'task' DO UPDATE SET
    status = excluded.status, snoozed_until = excluded.snoozed_until,
    handled_at = excluded.handled_at, outcome = excluded.outcome;
```

The ledger has no foreign keys to `events` or `tasks`, on purpose. The event cache is replaced on
every sync, and tasks may come from other `TaskSource`s, but a handled reminder must stay handled.
Prune old rows explicitly, for example rows whose `due_at` is older than the sync window.

A reminder that is on screen or queued stays `pending`. That state is runtime-only (see
`ReminderStatus`).

## Migrations

`storage::MIGRATIONS` is an ordered list of SQL scripts in `src-tauri/src/storage/migrations/`.
Migration `n` (1-based) moves the schema from version `n - 1` to `n`. `PRAGMA user_version`
records the current version, and `storage::SCHEMA_VERSION` is the number of migrations.

On open, the runner:

1. Starts an `IMMEDIATE` transaction, so two processes cannot migrate at the same time.
2. Reads `user_version`. If it is newer than `SCHEMA_VERSION` (the database was written by a
   newer build), it fails with `Error::Storage` and leaves the file untouched.
3. Runs every missing migration in order, sets `user_version` and commits. If any step fails,
   the whole transaction rolls back and the database stays at its old version.

Re-opening a current database runs nothing.

### Rules

- **Append-only.** Add a new file, `NNNN_description.sql`, and append it to `MIGRATIONS`.
- **Never edit or reorder a migration that has shipped** (merged to `main`). Databases in the
  field have already run it, so a change would never reach them and would make fresh and
  upgraded databases differ. Fix mistakes with a new migration.
- A migration must succeed inside a transaction. Do not use `VACUUM` or change `journal_mode`
  there.
- SQLite cannot alter most constraints in place. To change a table, create the new table, copy
  the rows, drop the old table and rename the new one, all in one migration.
- Add a test for each new migration that upgrades a database from the previous version with data
  in it.
