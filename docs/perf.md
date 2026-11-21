# Performance: idle memory and CPU

Spec section 10 sets a target of **< 80 MB idle memory, buddy window only**,
and **~0 % CPU when idle (sustained < 1 %)**. This page defines how those
numbers are measured and records the results. CPU is read as a share of
one core (the stricter reading); the share of the whole machine is reported
next to it.

## Metric

Owner decision (2026-11-16): the target counts only memory the app uses on its
own: **private (unshared) memory, summed over the app process and its WebView
processes**. Shared memory (system DLLs or frameworks mapped into every
process) is excluded.

| OS | Per-process figure | Processes summed |
|---|---|---|
| Windows | **Private working set** (Task Manager's "Memory" column). We also report **Private Bytes** (commit charge, resident or not), which is the stricter of the two. | `aichemist-desktop-buddy.exe` and every `msedgewebview2.exe` whose parent chain leads to it (browser, renderer, GPU, utility, crashpad). |
| macOS | **Physical footprint** (`phys_footprint`, Activity Monitor's "Memory", `footprint`, `top`'s MEM). | The app process and the `com.apple.WebKit.WebContent` / `.Networking` / `.GPU` XPC services whose responsible process is the app. |

"Idle" means: launched, buddy window shown, no interaction, measured after a
30 s settle period.

Owner decision (2026-11-16): on Windows the target is measured as **private
working set** (matches the "resident memory" wording in spec section 10).
Private Bytes is still reported for reference but is not held to 80 MB.

## How to measure

Build a release binary first: `npm ci && npm run tauri build`
(add `-- --no-bundle` on Windows, `-- --bundles app` on macOS).

Close other running instances first. All instances share one WebView2 user
data folder (per app identifier), so a second instance would reuse the first
one's browser process and the numbers would be split between them.

### Windows

```powershell
pwsh tools/measure-memory.ps1 -ExePath src-tauri/target/release/aichemist-desktop-buddy.exe
```

The script launches the app, waits 30 s (`-SettleSeconds`), walks the process
tree, prints private working set, Private Bytes and (for reference only) the
full working set per process plus a `TOTAL` line, then stops the app. Without
`-ExePath` it measures an already running instance and leaves it running.
`-Json path` also writes the result as JSON. `-ProcessId PID` measures one
specific running instance (when several builds with different identifiers
run side by side).

CPU over a window of time (default 60 s after a 30 s settle), summed over
the app and its WebView2 processes, from CPU cycle counts
(`QueryProcessCycleTime`, converted to time at the nominal clock):

```powershell
pwsh tools/measure-cpu.ps1 -ExePath src-tauri/target/release/aichemist-desktop-buddy.exe
# sleeping state: let the buddy fall asleep 5 s after launch, then measure
$env:AICHEMIST_SLEEP_AFTER_IDLE_SECS = 5
pwsh tools/measure-cpu.ps1 -ExePath src-tauri/target/release/aichemist-desktop-buddy.exe
```

It prints CPU time per process, a `TOTAL` line with % of one core and % of
the machine, then stops the app. `-ProcessId PID` measures a running
instance and leaves it running; `-Seconds`, `-SettleSeconds` and `-Json`
work as in the memory script. Cycle counts are used because
`TotalProcessorTime` and Task Manager are tick-sampled (15.6 ms) and badly
undercount the short bursts of an animation.

### macOS

```sh
tools/measure-memory.sh --app "src-tauri/target/release/bundle/macos/AIchemist Desktop Buddy.app"
```

The script launches the bundle with `open -n`, waits 30 s (`--settle`),
attributes WebKit XPC processes to the app through the kernel's
"responsible process" (a tiny C helper compiled on the fly with `cc`, which
needs the Xcode command line tools), reads `phys_footprint` from `top`, prints
per-process numbers and a `TOTAL` line, and cross-checks with `footprint`.
Launch through `open` (or Finder): an app started directly from a terminal is
not its own responsible process, and the script warns about it.
`--pid PID` measures an already running instance.

CPU over a window of time (default 60 s after a 30 s settle), summed over the
app and the same WebKit processes:

```sh
tools/measure-cpu.sh --app "src-tauri/target/release/bundle/macos/AIchemist Desktop Buddy.app"
```

It reads each process's exact CPU time (user + system, `proc_pid_rusage`,
converted from mach time units) at the start and the end of the window, and
prints CPU time and wake-ups (package idle + interrupt) per process plus a
`TOTAL` line with % of one core, % of the machine and wake-ups per second.
`ps -o time` and `top` round to 10 ms or worse, too coarse for this.
`--pid`, `--settle` and `--seconds` work as above.

### Sleeping state in a release build

`AICHEMIST_SLEEP_AFTER_IDLE_SECS` only works in debug builds. For a release
build, save the minimum sleep-after-idle in the settings file before launch
(`settings.json` in the app config directory: `%APPDATA%\<identifier>` on
Windows, `~/Library/Application Support/<identifier>` on macOS; other
sections fall back to their defaults):

```json
{"schemaVersion":1,"system":{"sleepAfterIdleMinutes":1}}
```

and measure with a 90 s settle, so the cat has been asleep for ~30 s when
the window starts.

### CI

`.github/workflows/perf.yml` builds a release on `windows-latest` and
`macos-latest`, measures memory and CPU (60 s window) in the `idle` state
(30 s settle) and then in the `sleeping` state (settings file above, 90 s
settle), and writes the numbers to the job summary. It runs on
`workflow_dispatch` and on PRs that touch the window, the webview config,
the Rust code, the buddy page or the scripts. CI runners have no GPU, so
their numbers differ a little from a desktop's; the macOS runners are
virtual machines with a different CPU count per run.

## Kept configuration (Windows)

`src-tauri/tauri.conf.json`, buddy window:

```json
"additionalBrowserArgs": "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required --disable-gpu --disable-software-rasterizer --in-process-gpu --enable-features=NetworkServiceInProcess,NetworkServiceInProcess2"
```

Setting `additionalBrowserArgs` replaces wry's defaults, so the first two
flags restate them (`--disable-features=msWebOOUI,...` and the autoplay
policy, which the reminder chime will need). Note that Chromium honours only
the last `--disable-features` / `--enable-features` switch, so any future
feature flag must be merged into the existing lists, not added as a second
switch.

What the kept flags do:

- `--disable-gpu --disable-software-rasterizer`: no hardware GPU process; the
  page is rasterised on the CPU. Saves ~50 MB of Private Bytes (the GPU
  process commits a lot of memory it never touches). Transparency still works
  (checked visually). Trade-off: animation is CPU-rasterised. For a small
  sprite canvas this should be negligible, but the CPU target (~0% idle,
  section 10) must be re-checked once the animation lands (M2).
- `--in-process-gpu`: the remaining (software) GPU work runs inside the
  browser process instead of a separate one.
- `--enable-features=NetworkServiceInProcess,NetworkServiceInProcess2`: the
  network service runs inside the browser process. Trade-off: a network
  service crash takes the browser process down with it. The buddy loads only
  local assets, so this is acceptable.

## Results

All numbers are MB, release build, 30 s settle, mean of 3 runs (baseline: 6).

### Windows, local desktop

2026-11-16, Windows 11 Pro 23H2 (22631), WebView2 runtime 157.0.4341.46,
3440x1440 display, discrete GPU. Release build of this branch (transparent,
frameless, always-on-top, skip-taskbar window; placeholder page).

Browser flags were tried through `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`
(WebView2 appends it to the args the app passes; checked on the browser
process command line) and the kept set was then measured from the config.

| # | Variant | Processes | Private WS | Private Bytes | Working set (incl. shared) | Verdict |
|---|---|---|---|---|---|---|
| 1 | Baseline (wry default args) | 7 | 69.1 | 152.7 | 296.4 | |
| 2 | `--in-process-gpu` | 6 | 66.9 | 141.5 | 274.4 | small gain alone |
| 3 | `--disable-gpu` | 7 | 64.5 | 102.2 | 282.6 | big Private Bytes gain |
| 4 | `--disable-gpu --disable-software-rasterizer` | 7 | 61.9 | 98.6 | 279.2 | kept |
| 5 | `--enable-low-end-device-mode` | 7 | 73.2 | 156.4 | 305.7 | rejected, no gain |
| 6 | `--enable-features=NetworkServiceInProcess,NetworkServiceInProcess2` | 6 | 70.8 | 151.6 | 283.4 | gain only combined |
| 7 | `--js-flags=--lite-mode` | 7 | 74.3 | 160.3 | 307.3 | rejected, no gain |
| 8 | `--single-process` | 3 | 55.4 | 115.9 | 201.6 | rejected: unsupported by Chromium/WebView2, worse Private Bytes than 11 |
| 9 | `--disable-background-networking --disable-component-update --disable-extensions --disable-sync --no-pings --disable-breakpad` | 7 | 70.7 | 157.3 | 299.1 | rejected, no gain (crashpad stays) |
| 10 | `MemoryUsageTargetLevel = Low` (set from Rust via `with_webview`, `ICoreWebView2_19`) | 7 | 76.6 | 159.2 | 312.7 | rejected, no gain (call succeeds, level reads back Low) |
| 11 | Minimal page (empty `<body>`, no CSS/JS), default args | 7 | 73.0 | 159.4 | 307.7 | rejected, no gain |
| 12 | Minimal page + flags of 4 | 7 | 62.5 | 99.1 | 282.1 | no gain over 4 |
| 13 | 4 + `--in-process-gpu` | 6 | 57.7 | 89.9 | 255.7 | |
| 14 | 13 + network service in process | 5 | 54.6 | 81.1 | 228.6 | |
| 15 | **Kept config (14, set in `tauri.conf.json`)** | 5 | **51.9** | **77.9** | 226.6 | **kept** |

Run-to-run spread is about +-3 MB, so differences below ~5 MB (rows 5, 6, 7,
9, 10, 11) are noise. Tauri does not expose `MemoryUsageTargetLevel`; it needs
`webview2-com` and `windows-core` as direct dependencies, so with no measured
gain it was dropped.

Breakdown of the kept config (one run): app 4.1 / 5.7, WebView2 browser
33.3 / 43.0, renderer 12.2 / 20.4, storage service 3.1 / 7.6, crashpad
1.9 / 3.3 (private WS / Private Bytes).

After task 1.A (tray icon, single-instance plugin, window subclass, demo
page), same machine, release build, kept config, 3 runs: private WS 56.6 /
55.6 / 58.1 (mean **56.8**), Private Bytes 81.1 / 81.3 / 83.8 (mean 82.1),
5 processes. About +5 MB private WS over row 15, partly run-to-run noise.
Idle CPU over 60 s after a 30 s settle: 0 ms for the app and its WebView2
processes (below the 15.6 ms timer resolution).

### M2 perf baseline: animated cat (task 2.1), Windows, local desktop

2026-11-17, same machine (Windows 11 23H2, 24 logical CPUs, 3.7 GHz),
release build, kept config, default sprite pack, cat scale 2 (96 x 96 CSS
px canvas), 30 s settle. The renderer draws only when the frame changes:
a timeout waits for the next frame, one `requestAnimationFrame` draws it.

**Memory** (`tools/measure-memory.ps1`, 3 runs per state, 5 processes):

| State | Private WS | Private Bytes |
|---|---|---|
| `idle` (8 frames at 5 fps) | 53.8 / 50.4 / 51.0, mean **51.7** | 79.3 / 75.8 / 77.1, mean 77.4 |
| `sleeping` (4 frames at 3 fps) | 52.5 / 57.2 / 52.8, mean **54.2** | 76.1 / 81.0 / 79.0, mean 78.7 |

No measurable change from the 1.A demo page: the canvas, sprites and IPC
cost less than the run-to-run noise. **Under 80 MB.**

**CPU** over 60 s, summed over the app and its WebView2 processes, 3 runs
per state. Measured as CPU cycles (`QueryProcessCycleTime`) converted to
time at the nominal clock. `TotalProcessorTime` / Task Manager are
tick-sampled (15.6 ms) and badly undercount these short bursts: the same
runs read 15-500 ms there.

| State | CPU time per 60 s | % of one core | % of the machine (24 CPUs) |
|---|---|---|---|
| `idle`, animating at 5 fps | 1078 / 693 / 674 ms, mean **815 ms** | **1.36 %** | 0.06 % |
| `sleeping`, animating at 3 fps | 642 / 646 / 448 ms, mean **579 ms** | **0.96 %** | 0.04 % |
| static frame / 1.A demo page | 0 ms (tick-sampled) | ~0 % | ~0 % |

Almost all of it is in the WebView2 browser process (display compositor,
in-process software GPU) and the renderer, roughly 2.7-3.2 ms of CPU per
drawn frame. So the cost scales with frames drawn per second, not with the
canvas size. The app process itself uses ~0 (the idle timer sleeps on a
condition variable).

GPU experiment (one run, idle at 5 fps): removing `--disable-gpu
--disable-software-rasterizer --in-process-gpu` gives **909 ms** per 60 s
(no real gain: the per-frame cost is frame production and presentation,
not rasterising a 96 px canvas) and costs memory: 6 processes, private WS
70.5 MB, Private Bytes 144 MB. Kept config stays.

**Verdict against spec 10 ("~0%, sustained < 1%")**: as Task Manager
reports it (share of the whole machine) the animated cat is 0.04-0.06 %,
shown as 0 %. Read as a share of one core, `sleeping` is just under 1 %
and `idle` is ~1.4 %, over the target. Raised with the owner rather than
relaxed. Options, cheapest first:

1. **Accept** the machine-wide reading (0.06 %; on a 4-core laptop about
   0.35 %).
2. **Lower frame rates in the pack** (art only, no code): CPU scales with
   frames per second, e.g. idle at 3 fps is ~1 % of a core, 2 fps ~0.6 %.
3. **Holds in the animation** (sprite-pack format change): per-frame
   durations, so idle can hold a frame for seconds and blink briefly. Most
   of the time no frame is drawn, which brings idle close to 0.
4. **Animate idle only for a while** (e.g. 30 s after entering `idle` or
   after an interaction), then hold frame 0 until the next event.
   Behaviour change, small code change.
5. GPU on: measured, no gain, +19 MB private WS. Rejected.
6. Native drawing (layered window): removes the webview cost entirely;
   large cost (see options below).

Owner decision (2026-11-17): option 3. Results in the next section.

### Frame durations (task 2.1b), Windows, local desktop

2026-11-17, same machine and method as the M2 baseline (release build, kept
config, cat scale 2, 30 s settle, 60 s window, `tools/measure-cpu.ps1`;
sleeping forced with `AICHEMIST_SLEEP_AFTER_IDLE_SECS=5`). Sprite-pack
format 2: per-frame durations (docs/sprite-pack.md). The renderer keeps one
timeout pending, set to the next frame change, and draws only then.

| State | Frame changes per second | CPU time per 60 s | % of one core | % of the machine (24 CPUs) |
|---|---|---|---|---|
| `idle`, 2.1 baseline (8 frames at 5 fps) | 5 | 1078 / 693 / 674 ms, mean 815 ms | 1.36 % | 0.06 % |
| `idle`, 2.1b (sit 4 s, blink, sit 3.5 s, swish, sit 5 s, double blink) | 0.8 | 256 / 261 / 403 ms, mean **306 ms** | **0.51 %** | 0.02 % |
| `sleeping`, 2.1 baseline (4 frames at 3 fps) | 3 | 642 / 646 / 448 ms, mean 579 ms | 0.96 % | 0.04 % |
| `sleeping`, 2.1b (breath held ~2.6 s, a "z" every 1.3 s) | 0.67 | 274 / 624 / 201 / 208 / 187 / 185 ms, mean **280 ms** | **0.47 %** | 0.02 % |

Sleeping was run 6 times because run 2 (624 ms) was far off the others;
other builds were running on the machine at the time, and runs 4-6
(185-208 ms) confirm it as noise. Without it the mean is 211 ms (0.35 %).
Nearly all of the time is in the WebView2 browser process (~75 %) and the
renderer (~23 %); the app process uses ~2 ms per minute.

**Memory** (`tools/measure-memory.ps1`, 3 runs per state, 5 processes),
private WS / Private Bytes, MB:

| State | Private WS | Private Bytes |
|---|---|---|
| `idle` | 50.7 / 52.8 / 55.8, mean **53.1** | 73.1 / 77.6 / 81.0, mean 77.2 |
| `sleeping` | 59.1 / 53.2 / 55.4, mean **55.9** | 82.6 / 78.6 / 79.0, mean 80.1 |

Unchanged from the 2.1 baseline within run-to-run noise.

**Verdict:** sustained CPU is under 1 % of one core in both calm states
(about a third of the 2.1 numbers), and ~0.02 % of the machine. CPU now
scales with the pack's frame changes per second; packs that want to stay
under the target should keep calm states below about 1.5 changes per
second (docs/sprite-pack.md, authoring tip).

### Settings window (task 2.4), Windows, local desktop

Same machine, release build, kept config, 30 s settle per measurement,
settings window opened through `tray::open_settings` (from a temporary test
hook) and closed through UI Automation (`WindowPattern.Close`, same as the
close button). Private WS / Private Bytes, MB. Runs 1-2: before task 2.1
(placeholder buddy page); run 3: on top of task 2.1 (canvas renderer).

| Run | Before (idle) | Settings open | After close | After 4 more open/close cycles |
|---|---|---|---|---|
| 1 | 52.6 / 77.6 (5 proc.) | 78.9 / 114.7 (6) | 58.7 / 84.5 (5) | |
| 2 | 49.7 / 74.9 (5) | 72.1 / 106.9 (6) | 54.2 / 80.7 (5) | 55.0 / 81.5 (5) |
| 3 (with 2.1) | 50.9 / 74.6 (5) | 69.2 / 104.5 (6) | 53.0 / 79.3 (5) | 53.5 / 79.8 (5) |
| 4 (with 3.5 accounts section) | 54.2 / 78.2 (5) | 74.0 / 107.5 (6) | 56.3 / 81.4 (5) | 57.1 / 83.2 (5) |

Run 4 (task 3.5, 2026-11-18): the settings page now also loads the accounts
section and calls `list_accounts`; the keychain store, the provider factories
and the account service are created at startup. Opened through a temporary
trigger-file hook calling `tray::open_settings`, closed with `WM_CLOSE` (the
same path as the close button). Same pattern as runs 1-3: the renderer goes
away on close and the remainder stays flat across cycles.

The open window adds one WebView2 renderer (~17 MB private WS) and ~4-7 MB
in the shared browser process. Closing destroys the window and its renderer;
what stays (~+5 MB, mostly the browser process keeping heap it grew) does not
grow with further open/close cycles. The idle state with no settings window
stays well under 80 MB.

### Upcoming popover (task 2.3), Windows, local desktop

Same machine, release build, kept config, private working set summed over the
app and its WebView2 tree. Idle with the popover closed (fresh launch,
`tools/measure-memory.ps1`, 30 s settle): **49.8 MB** (Private Bytes 74.8 MB,
5 processes). Popover open/close was driven through the WebView2 DevTools
port (`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=...`),
which costs a few MB itself: closed 53.2 MB, popover open (window grown to
304 x 472, 20 s settle) 55.6 MB, closed again 53.6 MB. The popover adds no
process; it is DOM in the buddy page, and its lists are dropped on close.

### Reminder bubble (task 2.2), Windows, local desktop

2026-11-18, same machine, release build, kept config, default settings
(sound on). `tools/measure-memory.ps1 -ProcessId PID`. Private WS / Private
Bytes, MB.

| State | Processes | Private WS | Private Bytes |
|---|---|---|---|
| Bubble showing (task reminder, cat in `alert`, chime played; measured ~10 s after it fired) | 6 | **59.5** | 90.5 |
| Idle after a restart (scheduler and ledger writer running, no bubble; 30 s settle) | 5 | **53.3** | 78.4 |

Playing the chime starts WebView2's audio service utility process (~3.5 MB
private WS), which is why the bubble row has 6 processes. The bubble DOM
itself costs less than the run-to-run noise. Idle stays at the M2 baseline
(52-54 MB): the scheduler thread and the ledger writer thread sleep on
condition variables. **Under 80 MB in both states.**

### Calendar sync wiring (task 3.6), Windows, local desktop

2026-11-18, same machine, release build, temporary identifier `...dev36`,
one CalDAV account pointing at an unreachable host (`https://*.invalid/`),
so the sync thread fails and retries with backoff throughout. Fresh launch
per run, 30 s settle.

| Metric | Runs | Mean |
|---|---|---|
| Private WS (`tools/measure-memory.ps1`, 5 processes), MB | 51.4 / 51.5 / 51.4 | **51.4** |
| Private Bytes, MB | 76.8 / 77.5 / 77.4 | 77.2 |
| CPU per 60 s (`tools/measure-cpu.ps1`), ms | 187.8 / 156.6 / 157.4 | **167 (0.28 % of one core, 0.01 % of the machine)** |

The app process itself used ~24 ms per minute (the sync thread sleeps on a
condition variable, waking at most once a minute or at the next retry).
Unchanged from the 2.2 idle numbers within noise. **Under 80 MB, CPU under
1 % of one core.**

### CI runners

| Date | Runner | Build | Config | Processes | Result |
|---|---|---|---|---|---|
| 2026-11-16 | windows-latest (WebView2 156.0.4314.58, no GPU) | release | default args + `MemoryUsageTargetLevel` Low (row 10) | 7 | private WS 56.4, Private Bytes 95.9 |
| 2026-11-16 | macos-latest (macOS 26.7.2) | release `.app` | transparent window, `macOSPrivateApi` | 3 | **footprint 38.4** (app 19.0, WebContent 14.0, Networking 5.4) |
| 2026-11-16 | windows-latest (WebView2 156.0.4314.58, no GPU) | release | kept config | 5 | **private WS 54.0, Private Bytes 77.0** (app 3.8 / 4.9, browser 34.1 / 42.1, renderer 11.1 / 20.1, storage 2.8 / 7.5, crashpad 2.2 / 2.4) |
| 2026-11-16 | macos-latest (macOS 26.7.2) | release `.app` | kept config (no change on macOS) | 3 | **footprint 37.5** (app 19.0, WebContent 13.0, Networking 5.5) |

macOS uses WKWebView, which does not take browser args; no separate GPU
process was attributed to the app at idle.

## M7 results (task 4.2)

Feature-complete app (main at `63cc5f3`, task 3.6), release builds, kept
config. Memory in MB (Windows: private working set / Private Bytes; macOS:
`phys_footprint`), CPU over a 60 s window as % of one core (share of the
whole machine in brackets). Targets (spec 10): memory < 80 MB idle, buddy
window only; CPU sustained < 1 % idle or sleeping.

### Windows, local desktop

2026-11-21, Windows 11 Pro 23H2 (22631), AMD Ryzen 9 5900X (24 logical
CPUs, 3.7 GHz nominal), WebView2 157.0.4341.60, 3440x1440. Temporary
identifier `...dev42` (own WebView2 profile and data). Realistic
configuration: default settings plus one CalDAV account pointing at an
unreachable host (`https://perf-unreachable.invalid/`, password in
Credential Manager), so the sync thread fails and backs off (30 s, 60 s,
120 s, ... capped at the 10 min interval; checked in the app log), and
four tasks in the database (three open, one done). Fresh launch per run.
Sleeping was reached in the release build through the settings file
(`sleepAfterIdleMinutes: 1`, 90 s settle, see "Sleeping state in a release
build"); a screenshot during a run confirmed the sleeping cat. Account,
credential and data were removed afterwards.

| State | Runs | Memory (mean) | CPU per 60 s (mean) | Verdict |
|---|---|---|---|---|
| `idle` | memory 52.0 / 53.5 / 59.3; CPU 432 / 233 / 403 / 184 ms | **54.9** / 78.7, 5 processes | 313 ms, **0.52 %** (0.022 %) | pass |
| `sleeping` | memory 55.6 / 52.7 / 58.5; CPU 81 / 89 / 100 ms | **55.6** / 81.5, 5 processes | 90 ms, **0.15 %** (0.006 %) | pass |
| Bubble showing (task reminder fired ~15 s earlier, chime played) | 1 (baseline of the same launch: 57.3 / 81.4) | **61.3** / 91.3, 6 processes | not measured | pass (not the idle target) |
| Popover open (cat clicked, 20 s settle) | 1 (baseline 55.7 / 82.5) | **61.0** / 87.0, 5 processes; closed again: 58.5 / 84.3 | not measured | pass (not the idle target) |
| Settings open (cat menu, 30 s settle), 3 cycles | 79.7 / 81.9 / 80.4 | **80.7** / 114.6, 6 processes | not measured | not the idle target (buddy window only) |
| Settings closed again (`WM_CLOSE`, 30 s settle), 3 cycles | 63.0 / 61.3 / 61.3 | **61.9** / 87.8, 5 processes | not measured | renderer released; +3-6 MB stays, flat |

Notes:

- A first `idle` memory run 3 read 66.0 MB with 6 processes: the tree walk
  had picked up an unrelated process whose recorded parent PID had been
  reused by the app. The scripts now require a child to start after its
  parent (and stop only the app and `msedgewebview2` processes); the run
  was repeated (59.3 MB). Without the stray process it was 54.4 MB.
- `idle` CPU: the app process itself used 6-8 ms per minute (sync
  retries included); the rest is the WebView2 browser (~80 %) and
  renderer drawing the idle animation (0.8 frame changes per second).
  `sleeping`: the app process used 0.3-1.9 ms per minute.
- Settings: as in task 2.4/3.5, the window's renderer (~17 MB) goes away
  on close and the browser process keeps a few MB it grew; the remainder
  does not grow over further cycles (63.0, 61.3, 61.3).
- Bubble: the chime starts WebView2's audio service process (6th
  process), as in task 2.2.
- Popover and Settings were driven with synthetic mouse and keyboard input
  (click on the cat; right-click, cat menu "Settings"), so no DevTools port
  was open this time.

### CI runners

`.github/workflows/perf.yml` (this PR) on the branch of this PR, which is
main plus measurement-only changes. Default settings (no accounts, no
tasks), 3 workflow runs.

| Platform | State | Run 1 | Run 2 | Run 3 | Mean | Verdict |
|---|---|---|---|---|---|---|
| windows-latest (WebView2 157.0.4341.46, 4 vCPUs) | `idle` memory | 51.7 / 75.7 | 52.9 / 76.8 | 52.8 / 78.0 | **52.5** / 76.8 | pass |
| | `idle` CPU | 0.47 % | 0.47 % | 0.41 % | **0.45 %** (0.11 %) | pass |
| | `sleeping` memory | 47.0 / 71.3 | 50.2 / 76.0 | 47.3 / 72.2 | **48.2** / 73.2 | pass |
| | `sleeping` CPU | 0.24 % | 0.23 % | 0.65 % | **0.37 %** (0.09 %) | pass |
| macos-latest (macOS 26.7.2, virtual Apple silicon: run 1 M1 with 3 CPUs, run 2 M2 Pro with 5, run 3 M2 Pro with 5) | `idle` footprint | 49.6 | 48.3 | 47.5 | **48.5** | pass |
| | `idle` CPU | 0.48 % (0.16 %) | 0.82 % (0.16 %) | 0.72 % (0.14 %) | **0.67 %** (0.16 %) | pass |
| | `sleeping` footprint | 50.8 | 48.7 | 49.5 | **49.7** | pass |
| | `sleeping` CPU | 0.84 % (0.28 %) | 1.001 % (0.20 %) | 0.91 % (0.18 %) | **0.92 %** (0.22 %) | **pass with no margin** (one run at 1.001 %) |

macOS processes: the app, `WebKit.WebContent`, `WebKit.GPU` and
`WebKit.Networking` (4; the GPU process is now attributed to the app,
unlike at M1). Per-process CPU (run 2, `idle`): app 126 ms, GPU 194 ms,
WebContent 176 ms, Networking 0.4 ms per minute. Unlike on Windows, the
app process itself does real work per frame on macOS (WKWebView hands its
layer tree to the UI process), and the app process sees 20-40 wake-ups per
second while the cat animates.

Windows CI `sleeping` run 3 (0.65 %) was mostly the WebView2 browser
process and is an outlier against runs 1-2 and the local desktop
(0.13-0.17 %); every Windows reading is under 1 %.

**macOS `sleeping` CPU is at the limit.** Its mean (0.92 % of one core,
0.2 % of the machine) is under the 1 % target, but with no margin: run 2
read 1.001 %. On macOS a drawn frame costs ~9-14 ms of CPU across the app,
WebContent and GPU processes (Windows: ~3 ms), and the sleeping state costs
more per frame than idle on all three runs (0.67 vs 0.81 frame changes per
second, yet more CPU). These are virtual machines without a real GPU, with
3-5 vCPUs that differ from run to run, so a real Mac may well read lower,
but that is unverified. Raised with the owner, target unchanged. Options,
cheapest first:

1. **Measure on a real Mac** (`tools/measure-cpu.sh`, both states) before
   deciding; the CI VMs may overstate it.
2. **Pack art only:** fewer frame changes in `sleeping` (it changes every
   1.3 s; e.g. holding the breath poses ~2.5 s halves the changes). CPU
   scales with frame changes per second on both platforms (2.1b).
3. **Accept** the current numbers (mean under 1 %, 0.2 % of the machine).
4. Code, macOS only, unmeasured: draw by swapping a CSS background
   position on a `div` instead of drawing on a canvas, which may let
   WebKit skip repainting the canvas layer. Needs a Mac to measure.

### Timers and background threads (spec 10: "one scheduler timer, no busy loops or fixed-interval polling on the UI side")

Audit of `src-tauri/src` and `src/` on main at `63cc5f3`. No busy loops and
no `setInterval` or `requestAnimationFrame` loop anywhere.

Rust threads and timers:

| Thread / timer | Exists | Wakes | Periodic wake while idle? | Why |
|---|---|---|---|---|
| `scheduler` (reminder scheduler driver, `scheduler/driver.rs`) | always | at the next reminder or quiet-hours boundary, on any change (condition variable), and at most 60 s after the last wake | yes: once a minute when nothing is due sooner | The single timer for all reminders. The condition variable times out on the monotonic clock, which stops while the computer sleeps on macOS and ignores wall-clock changes; the 60 s cap bounds how late a reminder can fire after resume or a clock change. A wake that finds nothing due costs microseconds. |
| `calendar-sync` (`sync/worker.rs`) | always | at the next regular sync (5-15 min, default 10), the next retry (backoff 30 s, doubling, capped at the interval), on `sync_now` / settings changes, and at most 60 s after the last wake | yes: once a minute | Same clock argument, and it detects resume from sleep by comparing wall-clock and monotonic time over each wait (tao reports no resume on desktop). |
| `buddy-idle-timer` (`buddy_state.rs`) | always | at last interaction + sleep-after-idle (default 10 min); no deadline while asleep or in a non-idle state | no | One deadline, moved by interactions. |
| `ledger-writer` (`reminders/ledger_writer.rs`) | always | only when the scheduler submits a ledger snapshot | no | Keeps SQLite writes off the scheduler lock. |
| `update-check` (`system/update_check.rs`) | only while the opt-in update check is on (default off: no thread) | 60 s after it is turned on, then every 24 h (or later on `Retry-After`) | daily | The feature itself. |
| Graph sign-in loopback listener (`graph/loopback.rs`) | only during an interactive Microsoft sign-in (5 min timeout) | polls a non-blocking `accept` every 100 ms | not while idle | Lets the wait be cancelled. Only runs while the user is signing in in the browser. |
| Graph throttling retry (`graph/provider.rs`) | during a sync that gets 429/503 | sleeps `Retry-After` (at most 10 s), once | no | Server-requested backoff. |
| Already-running notice (`already_running.rs`, Windows) | on a second launch | one-shot message box thread | no | |
| Tauri async runtime (tokio) workers | always | one-shot jobs (window creation, storage calls off the main thread, startup pruning) | no (parked) | |
| Click-through (`click_through.rs`) | always | Windows: window region, no events or timers; macOS: tracking-area events | no | |

Buddy page (WebView):

| Timer | Exists | Interval | Why |
|---|---|---|---|
| Renderer timeout (`buddy/main.ts`) | while the current state still has a frame change ahead and the window is visible | until the next frame change (per-frame durations, 120 ms to 5 s); exactly one pending | Draws only when the frame changes. Static frames, finished one-shot animations and a hidden window schedule nothing. |
| Bubble time refresh (`buddy/bubble.ts`) | only while a bubble shows | next minute boundary | Keeps "in 5 min" texts right. |
| Popover minute refresh (`buddy/popover.ts`) | only while the popover is open | next minute boundary | Same, for the lists. |
| Popover idle close (`buddy/popover.ts`) | only while the popover is open | 60 s after the last interaction inside it | Closes a forgotten popover. |
| Hover throttle (`buddy/main.ts`) | not a timer | drops pointer reports less than 3 s apart | Limits IPC on mouse movement. |

The settings page has no timers.

**Verdict:** met. All reminders go through the one scheduler timer, and
the other threads each wait on a single deadline for their own job instead
of polling. The only periodic wakes while idle are the two 60 s safety
caps (scheduler and sync), justified above; the measured app-process CPU
with both running and the sync failing every few minutes is 6-8 ms per
minute on Windows. Nothing polls on the UI side: the page's only
recurring work is the frame timer of the cat's own animation.

## Verdict (2026-11-16)

- **macOS: 38 MB footprint, well under 80 MB** with no tweaks.
- **Windows: under 80 MB with the kept flags**: 52 MB private working set,
  78 MB Private Bytes. Without them Private Bytes is ~153 MB (private working
  set ~69 MB).
- The Windows private working set margin is ~26-28 MB (Private Bytes, for
  reference only, has ~2 MB). This is a bare placeholder page. The sprite canvas, bubble and IPC will add to it, so the
  number must be re-measured at M2 (perf baseline) and at every milestone.
- M2 baseline (2026-11-17, animated canvas): 52-54 MB private working set,
  unchanged. CPU while animating: see the M2 section above (0.06 % of the
  machine, ~1-1.4 % of one core; open question for the owner).
- Frame durations (2026-11-17, task 2.1b, owner decision): idle and
  sleeping hold poses for seconds. CPU **0.51 % / 0.47 % of one core**,
  under the 1 % target; memory unchanged (53-56 MB private WS).
- Reminders (2026-11-18, task 2.2): 53 MB private working set idle, 59.5 MB
  with a bubble showing after the chime (audio service process added).
- Calendar sync (2026-11-18, task 3.6): 51.4 MB private working set idle
  with a failing account retrying; CPU 0.28 % of one core.
- M7 (2026-11-21, task 4.2, feature-complete app): memory passes on both
  platforms (Windows desktop 55-56 MB idle/sleeping, CI 48-53 MB; macOS CI
  48-50 MB). CPU passes on Windows (desktop 0.52 % idle, 0.15 % sleeping;
  CI 0.45 % / 0.37 %) and for macOS idle (0.67 %). **macOS sleeping is
  0.92 % of one core on average with one CI run at 1.001 %: no margin,
  raised with the owner** (see M7 results for options). Timer audit: met.

Options if Windows private working set goes over 80 MB later, cheapest first:

1. Keep the page lean: no frameworks, small decoded sprite sheets, one canvas
   (already the plan, spec section 3).
2. Release memory when hidden or sleeping: `TrySuspend` / hide the webview
   while the cat sleeps. Costs wake-up latency and complexity; only helps while
   hidden.
3. Draw the buddy natively (no webview in the buddy window, e.g. a layered
   window with a bitmap). Removes ~70 MB on Windows but means a second
   rendering path and much more platform code. Large cost.
