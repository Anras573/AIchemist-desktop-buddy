# Performance: idle memory

Spec section 10 sets a target of **< 80 MB idle memory, buddy window only**.
This page defines how that number is measured and records the results.

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

### CI

`.github/workflows/perf.yml` builds a release on `windows-latest` and
`macos-latest`, runs the scripts and writes the numbers to the job summary. It
runs on `workflow_dispatch` and on PRs that touch the window, the webview
config, the Rust code, the buddy page or the scripts. CI runners have no GPU,
so their Windows numbers differ a little from a desktop's.

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

The open window adds one WebView2 renderer (~17 MB private WS) and ~4-7 MB
in the shared browser process. Closing destroys the window and its renderer;
what stays (~+5 MB, mostly the browser process keeping heap it grew) does not
grow with further open/close cycles. The idle state with no settings window
stays well under 80 MB.

### CI runners

| Date | Runner | Build | Config | Processes | Result |
|---|---|---|---|---|---|
| 2026-11-16 | windows-latest (WebView2 156.0.4314.58, no GPU) | release | default args + `MemoryUsageTargetLevel` Low (row 10) | 7 | private WS 56.4, Private Bytes 95.9 |
| 2026-11-16 | macos-latest (macOS 26.7.2) | release `.app` | transparent window, `macOSPrivateApi` | 3 | **footprint 38.4** (app 19.0, WebContent 14.0, Networking 5.4) |
| 2026-11-16 | windows-latest (WebView2 156.0.4314.58, no GPU) | release | kept config | 5 | **private WS 54.0, Private Bytes 77.0** (app 3.8 / 4.9, browser 34.1 / 42.1, renderer 11.1 / 20.1, storage 2.8 / 7.5, crashpad 2.2 / 2.4) |
| 2026-11-16 | macos-latest (macOS 26.7.2) | release `.app` | kept config (no change on macOS) | 3 | **footprint 37.5** (app 19.0, WebContent 13.0, Networking 5.5) |

macOS uses WKWebView, which does not take browser args; no separate GPU
process was attributed to the app at idle.

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

Options if Windows private working set goes over 80 MB later, cheapest first:

1. Keep the page lean: no frameworks, small decoded sprite sheets, one canvas
   (already the plan, spec section 3).
2. Release memory when hidden or sleeping: `TrySuspend` / hide the webview
   while the cat sleeps. Costs wake-up latency and complexity; only helps while
   hidden.
3. Draw the buddy natively (no webview in the buddy window, e.g. a layered
   window with a bitmap). Removes ~70 MB on Windows but means a second
   rendering path and much more platform code. Large cost.
