# AIchemist Desktop Buddy: Specification

> Status: Ready for implementation. Derived from `docs/human.md` and Q&A rounds with the project owner.
> All earlier open questions are resolved. Section 14 records the decisions.

## 1. Purpose

A small, always-running desktop companion: an animated pixel-art gray tabby cat that sits on the desktop, reminds the user of upcoming calendar meetings and of their own task reminders, and has a settings view.

## 2. Goals and non-goals

### Goals
- Tiny resource use: **< 80 MB RAM and ~0% CPU when idle.**
- Cross-platform: **Windows and macOS are required.** Linux is best-effort (do not actively break it).
- Remind the user of upcoming meetings from **Microsoft Graph** and **CalDAV** calendars.
- Local one-off task reminders with completion tracking.
- Settings window for all configurable behavior.
- All data local. No telemetry.

### Non-goals (v1)
- No AI/LLM features. "Thinking" is a cosmetic state.
- No writing to calendars (read-only).
- No free-roaming or window-walking cat.
- No recurring tasks, no natural-language task entry, no CLI task entry.
- No external task sources (Microsoft To Do, VTODO), but leave an extension point (section 7.3).
- No signed or notarized builds.

## 3. Tech stack (decided)

| Concern | Choice |
|---|---|
| Shell | Tauri (latest stable 2.x) |
| Core / backend | Rust |
| Frontend | TypeScript + plain HTML/CSS/Canvas. **No heavy frameworks** (keeps memory down). A tiny library such as Preact or Svelte is acceptable for the settings window only, and only if it is not loaded in the buddy window. |
| Storage | SQLite (via `rusqlite`) for the event cache and tasks. JSON file for settings. |
| Secrets | OS keychain (`keyring` crate): Windows Credential Manager, macOS Keychain. |
| CI | GitHub Actions: build and test on `windows-latest` and `macos-latest`. A Linux build job is allowed to fail. |
| License | MIT |

## 4. Windows and UI surfaces

1. **Buddy window**: transparent, frameless, always-on-top, no taskbar/dock entry. Hosts the cat canvas and the speech bubble. Click-through everywhere except the cat's and bubble's opaque pixels or bounding box. Fixed position (corner and monitor chosen in settings). Only needs to be minimal and lightweight.
2. **Tray / menu bar icon** with a menu: Open Settings, Upcoming, Pause reminders (toggle), Quit.
3. **Right-click menu on the cat**: Upcoming, Add task, Snooze all, Settings, Quit.
4. **Upcoming popover** (opened by clicking the cat or from the menus): next few meetings and open tasks, with a "mark done" action on tasks and an "add task" form.
5. **Settings window**: a separate, normal window, created on demand and **destroyed when closed** (do not keep its webview alive, to save memory).

Only the buddy window may exist while the app is idle.

## 5. Animation

- **Format:** pixel-art sprite sheets, one per state, rendered on a `<canvas>` with nearest-neighbour scaling.
- **States:** `idle`, `sleeping`, `thinking`, `alert`.
- **Frame rate:** idle and sleeping run at a low rate (about 4-8 fps). Stop the render loop entirely when a state is a static frame. Use `requestAnimationFrame` only while animating.
- **Sprite pack format (documented in `docs/sprite-pack.md`, which the agent must write):**
  - A pack is a folder with `pack.json` plus one PNG per state.
  - `pack.json` declares: name, author, license, frame size, and per state: file, frame count, fps, loop (bool).
  - The default pack is bundled. Users may select a pack in Settings, Appearance.
- **Placeholders:** the agent generates simple placeholder sprites programmatically (a script in the repo that outputs the PNGs) for all four states. Real art is swapped in later by replacing the pack, with no code changes.

### State machine

| From | Event | To |
|---|---|---|
| any | reminder becomes due | `alert` |
| `alert` | bubble dismissed or snoozed | `idle` |
| `idle` | sync starts | `thinking` |
| `thinking` | sync ends | `idle` |
| `idle` | no user interaction for N minutes (default 10) | `sleeping` |
| `sleeping` | user hovers or clicks the cat, or a reminder fires | `idle` or `alert` |

`alert` has the highest priority and interrupts every other state. Implement as a pure, unit-tested function `next_state(state, event) -> state`.

## 6. Calendar integration

### 6.1 Provider abstraction
Define a trait (or equivalent interface) `CalendarProvider`:

```
list_calendars() -> [CalendarInfo]
fetch_events(calendar_ids, from, to) -> [Event]
test_connection() -> Result
```

Real implementations for Graph and CalDAV, plus a **fake provider** for tests. The rest of the app depends only on the trait.

### 6.2 Microsoft Graph
- Auth: OAuth 2.0 authorization code flow with PKCE (loopback redirect) or device code flow. **The user supplies their own Azure app registration client ID** in the account settings. No client ID is bundled.
- Scope: `Calendars.Read` (and `offline_access` for refresh tokens). Read-only.
- Use `calendarView` for the time window. Handle paging and recurring-event expansion via `calendarView`.
- Refresh tokens are stored in the OS keychain, never on disk.
- Provide `docs/graph-setup.md` that explains registering the app (redirect URI, scopes, public client).

### 6.3 CalDAV
- Auth: Basic or app-password credentials (server URL, username, password) stored in the OS keychain (password only. URL and username go in settings).
- Discover calendars via `PROPFIND`. Fetch events with `REPORT` calendar-query on a time range.
- Must parse iCalendar `VEVENT` including `RRULE` expansion, `EXDATE`, time zones, and all-day events.
- Tested against at least one real server type if possible (iCloud and Nextcloud are the likely ones). Unit tests use recorded fixtures.

### 6.4 Accounts and merging
- **Multiple accounts**, any mix of Graph and CalDAV.
- Per account, the user chooses which calendars are included.
- Events from all accounts are merged into one list, sorted by start time. De-duplicate by (title, start, end) as a best effort when the same event appears in two accounts.
- **Read-only**: the app never creates, edits or deletes calendar data.

### 6.5 Sync and cache
- Poll on a configurable interval, **default 10 minutes** (allowed range 5-15). Also sync on app start and on resume from sleep.
- Sync window: from now to now + 7 days (configurable internally).
- Cache upcoming events in SQLite. **Reminders are scheduled from the cache**, so they fire offline.
- The cat shows `thinking` while a sync is in progress. Sync failures are non-fatal. Keep the last good cache, show a subtle error indicator in the tray menu and the Settings account row, and retry with backoff.
- Do not sync while paused? **No.** Syncing continues while reminders are paused. Only reminder display is suppressed.

## 7. Reminders

### 7.1 Meeting reminders
- Delivered as a **speech bubble** from the cat, with the cat entering `alert`.
- Bubble content: event title, start time (and "in N min"), location if present. No join link.
- **Lead times:** configurable list, default **10 min, 5 min and 1 min** before start.
- **Actions:** `Dismiss` and `Snooze` (snooze durations configurable, default 5 min). Snoozing re-fires that reminder after the delay. Dismissing marks that reminder instance as handled.
- Every (event, lead time) pair fires at most once unless snoozed.
- Events that already started before the app launched should not retroactively fire reminders for lead times that are already past.
- Default filters: skip all-day events, skip events the user declined. Both are toggles in Settings.
- Multiple simultaneous reminders queue and show one at a time, with a counter badge ("+2").

### 7.2 Local tasks
- A task: `id`, `title`, `due_at` (a one-off date and time), `done` (bool), `done_at`, `created_at`.
- Create **via UI only** (the popover's add form and the right-click menu).
- Completion tracking: mark done or undone from the popover. Done tasks are listed in a collapsed section and can be deleted.
- When `due_at` arrives, show a bubble using the same Dismiss/Snooze actions plus `Mark done`.
- Overdue tasks fire once, then show as "overdue" in the popover. No re-nag.

### 7.3 Extension point for external task sources
Define a `TaskSource` trait with `fetch_tasks() -> [Task]` and an implementation for local storage. Do **not** implement external sources in v1, but structure the code so a source can be added without touching the scheduler.

### 7.4 Scheduler
- One scheduler component owns all timers. It computes the next due instant across meetings and tasks, sleeps until then (a single timer, not polling), and recomputes when the cache, tasks, or settings change.
- Must be unit-testable with an injected clock.

### 7.5 Pause and quiet hours
- "Pause reminders" toggle in the tray: suppresses bubbles while on. Reminders that fell due while paused are **replayed on unpause**. Task reminders replay. Meeting reminders replay only if the meeting has not yet started. Replays are batched into one bubble, not fired one after another.
- **Quiet hours** (a scheduled do-not-disturb window): included in v1, setting exists but is off by default.

## 8. Settings

Stored as JSON in the OS app-config directory. Secrets never go in this file.

| Section | Settings |
|---|---|
| **Calendar accounts** | Add/remove accounts (Graph or CalDAV). Test connection. Choose included calendars. Per-account sync status and last error. |
| **Reminder behavior** | Lead times, snooze durations, sound on/off, skip all-day, skip declined, sync interval (5-15 min). |
| **Appearance** | Cat size (scale), screen corner, monitor, opacity, sprite pack. |
| **System** | Launch at login, quiet hours, sleep-after-idle minutes, check for updates (opt-in). |

Settings changes apply live where practical, without a restart.

## 9. Privacy and security

- No telemetry, analytics or crash reporting. The only network traffic is to the user's configured calendar servers, Microsoft login endpoints, and an optional, opt-in update check.
- Secrets live only in the OS keychain.
- Never log tokens, passwords or event contents at default log levels.
- The Tauri allowlist/capabilities must be minimal. The buddy window gets only the IPC commands it needs.
- Content Security Policy must be set. Do not load remote scripts or assets.

## 10. Performance requirements and how to verify

| Metric | Target |
|---|---|
| Resident memory, idle, buddy window only | < 80 MB (report the measured number on each platform). Counts only memory the app uses on its own, summed over the app process and its WebView child processes: private working set on Windows, physical footprint on macOS. Shared memory is excluded. See `docs/perf.md`. |
| CPU, idle (sleeping or idle state) | ~0% (sustained < 1%) |
| Timers | One scheduler timer. No busy loops or fixed-interval polling on the UI side. |

Provide a short `docs/perf.md` documenting how to measure, and record the measured results in the PR that completes each milestone.

## 11. Proposed repository layout

```
/src-tauri/          Rust core (scheduler, providers, storage, state machine, IPC)
/src/buddy/          Buddy window frontend (canvas, bubble)
/src/settings/       Settings window frontend
/assets/sprites/     Default sprite pack (generated placeholders)
/tools/              Placeholder sprite generator script
/docs/               human.md, spec.md, sprite-pack.md, graph-setup.md, perf.md
/.github/workflows/  CI
```

## 12. Testing

- **Unit tests (required):** state machine, scheduler (injected clock), reminder de-duplication and snooze, event merging and de-duplication, RRULE and all-day handling, settings validation.
- **Provider tests:** CalDAV and Graph parsing against recorded fixtures, with no live network in CI.
- **Fake provider** used for integration tests of sync, cache and scheduling.
- **CI:** build and run tests on Windows and macOS on every PR. Linux is allowed to fail.

## 13. Milestones

Each milestone ends in a working, committed state with tests passing.

1. **M1: Skeleton.** Tauri app, transparent always-on-top buddy window, click-through, tray icon, quit. Runs on Windows and macOS. CI green.
2. **M2: Animation.** Sprite-pack format, placeholder generator, canvas renderer, state machine with all four states, idle-to-sleep. Perf baseline recorded.
3. **M3: Tasks and reminders.** SQLite, local tasks, scheduler, speech bubble, dismiss/snooze, popover with add and complete. The right-click menu.
4. **M4: Settings.** Settings window (created and destroyed on demand), persistence, live-apply, appearance and reminder options.
5. **M5: CalDAV.** Provider, keychain storage, account UI, sync and cache, meeting reminders from the cache, offline behavior.
6. **M6: Microsoft Graph.** PKCE or device-code auth, provider, `docs/graph-setup.md`, multi-account merging.
7. **M7: Polish and release.** Launch at login, quiet hours, opt-in update check, perf verification on both platforms, unsigned GitHub release workflow, README with macOS Gatekeeper instructions.

## 14. Owner decisions

1. **Lead times:** 10, 5 and 1 min.
2. **Join link:** none in the meeting bubble.
3. **Overdue tasks:** fire once, no re-nag.
4. **Paused reminders:** replay on unpause (tasks, and meetings not yet started; batched into one bubble).
5. **Quiet hours:** included in v1, off by default.
6. **Multi-monitor:** defaults to the primary monitor, bottom-right corner. Corner and monitor are changeable in Settings.
7. **Unsigned macOS builds:** v1 ships unsigned. The README documents the right-click-Open and `xattr` workaround. Notarization is not required.
8. **Sprite style:** gray tabby for the placeholder generator.
9. **Sound:** the agent generates a simple placeholder chime and bundles it.

## 15. Instructions for the implementing agent

- Work is split into phases and tasks tracked in beads, and may run in parallel. A task starts only when the tasks it depends on are merged. Milestones (section 13) remain release checkpoints: each ends in a working state with tests passing.
- Shared contracts (domain types, the `CalendarProvider`, `TaskSource` and `Clock` traits, IPC command signatures) are frozen once they are merged. Agents working in parallel build against them and must not change them. A needed change is raised with the orchestrator, who makes it.
- Each parallel task runs on its own branch or worktree and touches only the modules it owns. Merge through a PR with passing CI and a review.
- If you hit a gap the spec does not cover, pick the simplest option, and list it in the PR description.
- Keep dependencies few. Justify each new crate or npm package against the memory target.
- Never commit secrets, tokens, or personal calendar data. Use fixtures with fake data.
- If a requirement here conflicts with the memory or CPU targets, stop and raise it instead of silently relaxing the target.
