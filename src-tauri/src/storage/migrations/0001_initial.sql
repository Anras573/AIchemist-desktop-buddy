-- Migration 1: initial schema.
--
-- SHIPPED MIGRATIONS ARE IMMUTABLE. Never edit this file once it is on main;
-- add a new migration instead (see docs/storage.md).
--
-- Conventions:
--   * Instants are INTEGER unix seconds, UTC (sub-second precision is dropped).
--   * All-day dates are TEXT 'YYYY-MM-DD' (floating, no time zone).
--   * Enums are TEXT using the camelCase names of their serde representation.
--   * Booleans are INTEGER 0/1.
--   * Tables are STRICT, so SQLite enforces column types.

-- Local one-off tasks (domain::Task, spec 7.2).
CREATE TABLE tasks (
    id         TEXT    NOT NULL PRIMARY KEY,
    title      TEXT    NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
    due_at     INTEGER NOT NULL,
    done       INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
    done_at    INTEGER,
    created_at INTEGER NOT NULL,
    -- done_at is set exactly when done is true.
    CHECK ((done = 1) = (done_at IS NOT NULL))
) STRICT;

-- Cache of upcoming event occurrences (domain::Event, spec 6.5).
-- Timed events use start_at/end_at; all-day events use start_date/end_date
-- (end exclusive). Exactly one pair is set, matching time_kind.
CREATE TABLE events (
    account_id  TEXT    NOT NULL,
    calendar_id TEXT    NOT NULL,
    event_id    TEXT    NOT NULL,
    title       TEXT    NOT NULL,
    time_kind   TEXT    NOT NULL CHECK (time_kind IN ('timed', 'allDay')),
    start_at    INTEGER,
    end_at      INTEGER,
    start_date  TEXT,
    end_date    TEXT,
    location    TEXT,
    response    TEXT    NOT NULL CHECK (response IN
                    ('organizer', 'accepted', 'tentative', 'declined', 'notResponded', 'unknown')),
    PRIMARY KEY (account_id, calendar_id, event_id),
    CHECK (
        (time_kind = 'timed'
            AND start_at IS NOT NULL AND end_at IS NOT NULL AND start_at <= end_at
            AND start_date IS NULL AND end_date IS NULL)
        OR
        (time_kind = 'allDay'
            AND start_date IS NOT NULL AND end_date IS NOT NULL AND start_date < end_date
            AND start_at IS NULL AND end_at IS NULL)
    )
) STRICT;

-- Range queries by start: one index per representation (the column of the
-- other representation is NULL).
CREATE INDEX events_start_at ON events (start_at);
CREATE INDEX events_start_date ON events (start_date);

-- Per-account sync status (spec 6.5, 8). No secrets or event contents in
-- last_error_message (spec 9).
CREATE TABLE sync_state (
    account_id         TEXT    NOT NULL PRIMARY KEY,
    last_attempt_at    INTEGER,
    last_success_at    INTEGER,
    last_error_kind    TEXT CHECK (last_error_kind IN
                           ('auth', 'network', 'protocol', 'notFound', 'storage',
                            'keychain', 'invalidInput', 'validation', 'internal')),
    last_error_message TEXT,
    last_error_at      INTEGER,
    -- The three error columns are set or cleared together.
    CHECK ((last_error_kind IS NULL) = (last_error_message IS NULL)),
    CHECK ((last_error_kind IS NULL) = (last_error_at IS NULL))
) STRICT;

-- Reminder ledger (domain::ReminderInstance, spec 7.1). One row per
-- ReminderKey; the partial unique indexes below make "fires at most once"
-- a database invariant.
--
-- No foreign keys to events or tasks on purpose: the event cache is replaced
-- on every sync and tasks may come from other TaskSources, but the ledger must
-- outlive both so a reminder never fires twice.
CREATE TABLE reminders (
    id            INTEGER NOT NULL PRIMARY KEY,
    kind          TEXT    NOT NULL CHECK (kind IN ('meeting', 'task')),
    -- kind = 'meeting'
    account_id    TEXT,
    calendar_id   TEXT,
    event_id      TEXT,
    lead_minutes  INTEGER CHECK (lead_minutes >= 0),
    -- kind = 'task'
    task_id       TEXT,
    due_at        INTEGER NOT NULL,
    status        TEXT    NOT NULL CHECK (status IN ('pending', 'snoozed', 'handled')),
    snoozed_until INTEGER,
    handled_at    INTEGER,
    outcome       TEXT CHECK (outcome IN ('dismissed', 'markedDone', 'skipped')),
    CHECK (
        (kind = 'meeting'
            AND account_id IS NOT NULL AND calendar_id IS NOT NULL
            AND event_id IS NOT NULL AND lead_minutes IS NOT NULL
            AND task_id IS NULL)
        OR
        (kind = 'task'
            AND task_id IS NOT NULL
            AND account_id IS NULL AND calendar_id IS NULL
            AND event_id IS NULL AND lead_minutes IS NULL)
    ),
    CHECK (
        (status = 'pending'
            AND snoozed_until IS NULL AND handled_at IS NULL AND outcome IS NULL)
        OR
        (status = 'snoozed'
            AND snoozed_until IS NOT NULL AND handled_at IS NULL AND outcome IS NULL)
        OR
        (status = 'handled'
            AND snoozed_until IS NULL AND handled_at IS NOT NULL AND outcome IS NOT NULL)
    )
) STRICT;

CREATE UNIQUE INDEX reminders_meeting_key
    ON reminders (account_id, calendar_id, event_id, lead_minutes)
    WHERE kind = 'meeting';
CREATE UNIQUE INDEX reminders_task_key
    ON reminders (task_id)
    WHERE kind = 'task';
