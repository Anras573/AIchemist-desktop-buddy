# AIchemist Desktop Buddy

A small, always-running desktop companion: a pixel-art gray tabby cat that sits in a corner of
your screen and reminds you of upcoming meetings and of your own tasks. It reads your Microsoft
365 / Outlook and CalDAV (iCloud, Nextcloud, ...) calendars, shows a speech bubble before each
meeting, and keeps a simple local list of one-off task reminders. It is built with
[Tauri 2](https://tauri.app/) (Rust core) and plain TypeScript, and aims to stay under 80 MB of
memory and near 0% CPU while idle ([measurements](docs/perf.md)).

**What it does**

- Meeting reminders as a speech bubble from the cat, by default 10, 5 and 1 minute before start,
  with **Dismiss** and **Snooze**. Several reminders queue up with a "+N" badge.
- Local task reminders: add a task with a due time, get a bubble when it is due, mark it done.
- Several calendar accounts at once (any mix of Microsoft 365 and CalDAV), with a choice of
  calendars per account. Events are merged into one list.
- Pause reminders from the tray, plus optional quiet hours. Reminders that fall due while paused
  are replayed (batched into one bubble) when you unpause.
- Settings for lead times, snooze durations, sound, cat size, corner, monitor, opacity and sprite
  pack. Changes apply immediately.

**What it does not do**

- It never changes your calendars: access is read-only.
- No AI features (the "thinking" cat is just an animation), no recurring tasks, no task import
  from To Do or other apps, and the cat does not walk around your screen.
- Builds are not code-signed or notarized (see [Install](#install)).

**Privacy.** All data stays on your computer: settings in a JSON file, tasks and the calendar
cache in a local SQLite database, passwords and tokens only in the OS keychain (Windows
Credential Manager, macOS Keychain). There is no telemetry, analytics or crash reporting. The
app only connects to the calendar servers you configure, to Microsoft's login and Graph
endpoints if you add a Microsoft 365 account, and, if you opt in, to GitHub to check for a new release. Logs never
contain passwords, tokens or event contents.

> **Status: pre-release.** All v1 features are in place: task reminders, meeting reminders
> from CalDAV and Microsoft 365 calendars (synced in the background, with "Sync now" in
> Settings), launch at login and the opt-in update check. CalDAV sync has been tested against
> a real iCloud account. It has not yet been tested against Nextcloud or Microsoft 365
> accounts, so please report problems.

## Install

Download the installer for your system from the
[latest release](https://github.com/Anras573/AIchemist-desktop-buddy/releases/latest). Each
release lists SHA-256 checksums in `SHA256SUMS.txt`; checking them is a good idea because the
installers are unsigned.

The builds are **not code-signed** (Windows) and **not notarized** (macOS; the app is only ad-hoc signed, without a developer certificate). This is a deliberate
choice for a free hobby project, not a sign that something is wrong, but your OS will warn you
the first time. Here is how to get past the warning.

### Windows 10 and 11

Download `AIchemist-Desktop-Buddy_<version>_x64-setup.exe` (or the `.msi`) and run it.

If Microsoft Defender SmartScreen shows **"Windows protected your PC"**, select **More info**,
check that the app name is right, then select **Run anyway**. Your browser may also ask you to
confirm keeping the download ("Keep" / "Keep anyway").

The installer needs the Microsoft Edge WebView2 runtime, which Windows 10 and 11 already have;
if it is missing, the installer downloads it.

### macOS (Apple silicon and Intel)

Download `AIchemist-Desktop-Buddy_<version>_universal.dmg`, open it and drag **AIchemist Desktop
Buddy** to **Applications**. The first time you open it, macOS says it cannot verify the
developer. Use one of these:

- **System Settings** (macOS 13 and newer, and the only way on macOS 15 Sequoia): try to open the
  app once and select **Done**. Then open **System Settings > Privacy & Security**, scroll to
  **Security**, and select **Open Anyway** next to the message about AIchemist Desktop Buddy.
  Confirm with your password and select **Open Anyway** again.
- **Right-click > Open** (macOS 14 Sonoma and older): in Finder, open **Applications**,
  Control-click (or right-click) **AIchemist Desktop Buddy** and choose **Open**, then **Open**
  in the dialog.
- **Terminal**: remove the quarantine flag that the browser added to the download. This also
  fixes an "is damaged and can't be opened" message:

  ```sh
  xattr -dr com.apple.quarantine "/Applications/AIchemist Desktop Buddy.app"
  ```

You only need to do this once per installed version. The cat lives in the menu bar (no Dock
icon). macOS may ask for permission to access the keychain the first time the app stores or
reads an account password; choose **Always Allow**.

### Linux (best-effort)

Releases include a `.deb` and an `.AppImage` when the Linux build succeeds. They are not tested,
and Linux has **no keychain backend** in v1, so calendar accounts cannot store their password or
token there. Task reminders work. Bug reports are welcome; fixes that do not affect Windows and
macOS are too.

### Uninstall

If you turned on **Launch at login**, turn it off in Settings first (it adds a `Run` registry
value on Windows and `~/Library/LaunchAgents/io.github.anras573.aichemistbuddy.plist` on macOS).
Then use **Settings > Apps** on Windows, or move the app to the Trash on macOS. Your settings,
tasks and logs stay behind; delete them by hand if you like:

| | Settings and database | Logs |
|---|---|---|
| Windows | `%APPDATA%\io.github.anras573.aichemistbuddy\` | `%LOCALAPPDATA%\io.github.anras573.aichemistbuddy\logs\` |
| macOS | `~/Library/Application Support/io.github.anras573.aichemistbuddy/` | `~/Library/Logs/io.github.anras573.aichemistbuddy/` |

Account passwords and tokens are in Windows Credential Manager or the macOS Keychain under the
service name `io.github.anras573.aichemistbuddy`. Removing an account in Settings deletes its
entry; to clean up after uninstalling, remove those entries there.

## Using the buddy

The cat starts in the bottom-right corner of your primary monitor. Only the cat and its bubble
take clicks; the rest of its window lets clicks through to whatever is below.

- **Click the cat** to open **Upcoming**: the next meetings and your open tasks. Add a task
  there (title and due time), mark tasks done or undone, and delete done tasks.
- **Right-click the cat** for **Upcoming**, **Add task**, **Snooze all**, **Settings** and
  **Quit**.
- **Tray / menu bar icon:** **Open Settings**, **Upcoming**, **Pause reminders** (a toggle),
  **Quit**. If a calendar account fails to sync, a status line appears at the top of this menu.
  With the update check on, an **Update available: vX.Y.Z** item appears when a newer release
  exists; it opens the release page (the app never downloads or installs anything itself).
- **Reminder bubble:** **Dismiss** handles that reminder; **Snooze** brings it back after the
  snooze time (5 minutes by default). Task bubbles also have **Mark done**. An overdue task fires
  once and then shows as overdue in Upcoming; it does not nag.
- The cat falls asleep after a while without reminders or interaction (10 minutes by default),
  shows a "thinking" animation while it syncs calendars, and sits up when a reminder fires.

### Settings

Open Settings from the tray or the cat's menu. Every change is saved and applied right away.

- **Reminder behavior:** lead times before meetings, snooze durations, sound on or off, skip
  all-day events, skip declined events, calendar sync interval (5 to 15 minutes).
- **Appearance:** cat size, screen corner, monitor, opacity, sprite pack.
- **System:** launch at login, quiet hours, minutes before the cat sleeps, and check for
  updates (off by default; when on, the app asks GitHub for the latest release once a day,
  sending no personal data).
- **Calendar accounts:** add and remove accounts, test the connection, choose which calendars
  to include, sign in again or change a password, and see each account's sync status and last
  error.

### Pause and quiet hours

**Pause reminders** in the tray menu hides reminder bubbles until you switch it off. Calendars
keep syncing while paused. When you unpause, task reminders that fell due are replayed, as are
meeting reminders for meetings that have not started yet, all in one bubble.

**Quiet hours** (Settings > System, off by default) do the same every day between two times,
for example 22:00 to 07:00.

### Adding a CalDAV account (iCloud, Nextcloud, others)

In **Settings > Calendar accounts**, select **Add account...**, choose **CalDAV**, and enter:

| Server | Server URL | User name | Password |
|---|---|---|---|
| iCloud | `https://caldav.icloud.com` | your Apple ID email | an **app-specific password** |
| Nextcloud | `https://your-server/remote.php/dav` | your Nextcloud user name | your password, or better an app password |
| Other | the server's CalDAV URL (must be `https://`) | | |

- **iCloud** does not accept your Apple ID password. Create an app-specific password at
  [account.apple.com](https://account.apple.com) under **Sign-In and Security > App-Specific
  Passwords** (two-factor authentication must be on) and paste it.
- **Nextcloud:** create an app password under **Personal settings > Security > Devices &
  sessions**, so you can revoke it without changing your login password.

Then **Test connection**, pick the calendars to include, and save. The password goes to the OS
keychain; only the server URL and user name are stored in the settings file.

### Adding a Microsoft 365 / Outlook account

Microsoft accounts need a one-time app registration of your own (the app ships without a
Microsoft client ID). Follow [docs/graph-setup.md](docs/graph-setup.md), about five minutes,
then add a **Microsoft 365** account in Settings with your **Application (client) ID** and
**tenant**, and select **Sign in**. Sign-in happens in your browser; the app only gets read
access to your calendars, and the refresh token is kept in the OS keychain.

## Development

### Prerequisites

- [Rust](https://rustup.rs/) stable, 1.90 or newer.
- [Node.js](https://nodejs.org/) 20.11 or newer (CI uses Node 24) and npm.
- The Tauri platform dependencies ([guide](https://tauri.app/start/prerequisites/)):
  - **Windows:** Microsoft C++ Build Tools (Visual Studio 2022 "Desktop development with C++")
    and WebView2 (preinstalled on Windows 10/11).
  - **macOS:** Xcode Command Line Tools (`xcode-select --install`).
  - **Linux (best-effort):** `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`,
    `librsvg2-dev`, `libxdo-dev`, `libssl-dev`, `build-essential`.

### Run, test, build

```sh
npm ci                        # install the exact dependency versions
npm run tauri dev             # Vite dev server on port 1420 + the app, with reload

npm run build                 # typecheck and build the frontend into dist/
npm test                      # frontend unit tests (node --test)
npm run check:sprites         # validate the bundled sprite pack and chime

cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                    # needs dist/, so run `npm run build` first

npm run tauri build           # release build and installers (unsigned)
```

CI ([`ci.yml`](.github/workflows/ci.yml)) runs all of these on Windows and macOS for every PR;
the Linux job may fail. [`perf.yml`](.github/workflows/perf.yml) measures idle memory.

### Sprite packs and sounds

The cat is a sprite pack: a folder with `pack.json` and one PNG sheet per state (`idle`,
`sleeping`, `thinking`, `alert`). The bundled placeholder pack and chime are generated by
`npm run gen:sprites` and `npm run gen:chime`. To make your own pack, see
[docs/sprite-pack.md](docs/sprite-pack.md).

### Performance

Idle memory must stay under 80 MB and idle CPU near 0%. [docs/perf.md](docs/perf.md) explains
how to measure on Windows and macOS (`tools/measure-memory.ps1`, `tools/measure-memory.sh`,
`tools/measure-cpu.ps1`) and records the results so far.

### Releases

Maintainers: see [docs/releasing.md](docs/releasing.md). In short, run
`node tools/bump-version.mjs X.Y.Z`, merge, push a `vX.Y.Z` tag, then review and publish the
draft release that [`release.yml`](.github/workflows/release.yml) creates.

### Project layout

```
src-tauri/                Rust core
  src/domain/             shared contracts: types, CalendarProvider, TaskSource, Clock, IPC
  src/caldav/, src/ical/  CalDAV provider and iCalendar parsing (RRULE, time zones)
  src/graph/              Microsoft Graph provider and OAuth (PKCE, loopback redirect)
  src/sync/               sync engine, event merging and de-duplication
  src/scheduler/          the single reminder timer (injected clock)
  src/reminders/          reminder queue, bubble, snooze, pause and quiet hours
  src/storage/            SQLite schema and migrations, settings file
  src/secrets/            OS keychain access
  src/*.rs                windows, tray, menus, click-through, settings service, logger
src/buddy/                buddy window: canvas renderer, speech bubble, Upcoming popover
src/settings/             settings window (created on demand, destroyed on close)
assets/sprites/default/   bundled sprite pack
assets/sounds/            bundled chime
tests/frontend/           frontend unit tests
tools/                    sprite/chime generators, sprite check, perf scripts, version bump
docs/                     specification and docs (below)
.github/workflows/        CI, perf and release
```

### Docs

| Document | About |
|---|---|
| [spec.md](docs/spec.md) | The specification: goals, behavior, milestones, owner decisions |
| [contracts.md](docs/contracts.md) | Frozen shared contracts and how to change them |
| [storage.md](docs/storage.md) | SQLite schema, migrations and the settings file |
| [graph-setup.md](docs/graph-setup.md) | Registering a Microsoft Entra app for Microsoft 365 calendars |
| [sprite-pack.md](docs/sprite-pack.md) | Sprite pack format and the placeholder generator |
| [click-through.md](docs/click-through.md) | How the transparent window lets clicks through, per OS |
| [perf.md](docs/perf.md) | Measuring idle memory and CPU, and the results |
| [releasing.md](docs/releasing.md) | Versioning and cutting a release |

### Contributing

Issues and PRs are welcome. Keep dependencies few (each one has to justify itself against the
memory target), never commit secrets or real calendar data (tests use fake fixtures), and make
sure CI is green on Windows and macOS.

## License

[MIT](LICENSE)
