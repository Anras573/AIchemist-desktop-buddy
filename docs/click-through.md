# Click-through for the buddy window

Spike for task 0.5. Requirement (spec section 4): the buddy window is
transparent, frameless, always-on-top and has no taskbar/dock entry. Only the
cat and the speech bubble take the mouse (their bounding boxes are enough).
Everywhere else, clicks, hovers and scrolls go to whatever is underneath. The
cat must still get hover, left-click and right-click. Idle CPU must stay at
about 0%, with no fixed-interval polling (spec section 10).

Code: `src-tauri/src/click_through.rs`. Demo page: `src/buddy/` (an orange
"cat" box and a white "bubble" box; left-click the cat to show or hide the
bubble, right-click turns the cat blue, hover makes it lighter).

## How hit regions flow

1. The buddy page owns the layout. Whenever it changes (load, resize, bubble
   shown or hidden), it sends the bounding boxes of the visible hit targets
   to the `set_hit_regions` IPC command, in CSS pixels relative to the
   webview. The command is granted in `capabilities/buddy.json` as
   `allow-set-hit-regions` (`build.rs` declares it in the app manifest, so no
   app command is allowed unless granted).
2. Rust validates the list (at most 8 rectangles, finite, non-negative,
   bounded) and stores it.
3. The platform layer applies it. Rust code can also call
   `click_through::set_regions(&window, regions)` directly.

Until the page reports its regions, the whole window is click-through (on
Windows it is also invisible, because the region clips drawing too).

## Chosen approach per OS

| OS | Mechanism | Runtime cost |
|---|---|---|
| Windows | `SetWindowRgn` shapes the window to the union of the hit rectangles | None. The OS hit-tests against the region. No hooks, timers or messages. |
| macOS | `NSWindow.ignoresMouseEvents`, toggled by global + local `NSEvent` mouse-move monitors | One point-in-rect test per mouse-move event. Nothing runs while the mouse is still. |
| Linux | Not implemented (best effort, spec section 2). The window takes all clicks. | None |

### Windows: `SetWindowRgn`

The window region is the only part of a window that exists for hit-testing
and drawing. Outside it, `WindowFromPoint` and real mouse input go to the
window below, across processes. The WebView2 child windows are clipped by the
top-level region too, so it works with WebView2's windowed hosting.

- Rectangles are converted to physical pixels using the window's scale factor
  and rounded outwards. The region is rebuilt on `ScaleFactorChanged` and
  `Resized`.
- The region also clips drawing. Nothing outside the hit boxes is visible.
  That is fine for bounding boxes, but **the page must report boxes that
  cover everything it draws**, including animation overshoot. Anything outside
  is clipped.
- Per-pixel hit-testing is possible later by building the region from the
  sprite's alpha mask (`ExtCreateRegion`), but bounding boxes meet the spec.

### macOS: `ignoresMouseEvents` + event monitors

- While the window ignores the mouse, mouse moves go to other apps. A
  **global** monitor (`addGlobalMonitorForEventsMatchingMask`) sees them.
  Global monitors for mouse events need no Accessibility permission; only key
  events do.
- While the window accepts the mouse, moves go to us. A tracking area on the
  content view (`MouseMoved | MouseEnteredAndExited | ActiveAlways |
  InVisibleRect`) makes AppKit generate them even while the app is inactive,
  which is almost always. A **local** monitor sees them and passes them on
  unchanged, so the webview still gets its hover events.
- On each event: cursor (`NSEvent.mouseLocation`) minus the content rect,
  flipped to top-left origin, tested against the regions in points (one point
  is one CSS pixel at default zoom). `setIgnoresMouseEvents` is called only
  when the answer changes.
- When the regions change, the state is checked again right away, so a bubble
  appearing under a still cursor takes clicks without waiting for a move.
- The monitors and the tracking area live as long as the app (stored in a
  main-thread `thread_local`).

## Options evaluated

### Windows

| Option | Verdict |
|---|---|
| **`SetWindowRgn`** (window shaping) | **Chosen.** OS-native hit-testing, no runtime cost, works across processes, updates are one GDI call. |
| `WM_NCHITTEST` returning `HTTRANSPARENT` (subclass) | Rejected. `HTTRANSPARENT` only forwards to windows **of the same thread**, so clicks never reach other apps. Also, the webview area is covered by WebView2's `Chrome_RenderWidgetHostHWND` child window, owned by the WebView2 process, so the top-level window's `WM_NCHITTEST` is not consulted there. |
| `WS_EX_TRANSPARENT` + `WS_EX_LAYERED` toggling (what Tauri's `set_ignore_cursor_events` does) | Rejected. It is all or nothing for the whole window. To switch it back off over the cat you need the cursor position from elsewhere: either polling `GetCursorPos` on a timer (breaks section 10) or a global low-level mouse hook (`WH_MOUSE_LL`). The hook puts every mouse event in the system through our process, adds input lag if we stall, and looks suspicious to security tools. |
| Per-pixel alpha hit-testing (`UpdateLayeredWindow`) | Not applicable. WebView2 does not render into a layered-window bitmap. |
| Window sized to the cat, plus a separate bubble window | Works, but a second webview costs memory (section 10), and a native bubble means a second UI stack. Kept as the fallback. |

### macOS

| Option | Verdict |
|---|---|
| **`ignoresMouseEvents` + global/local `NSEvent` monitors** | **Chosen.** Event-driven, no polling, no extra permissions. |
| Default pass-through of fully transparent pixels in a non-opaque window | Not relied on. The window server's alpha hit test is designed for windows that draw into their own backing store. WKWebView renders through remote Core Animation layers, and Tauri/wry users report that transparent areas still swallow clicks. Even if it works, the result would depend on WebKit internals. The checklist below asks for a quick check, for the record. |
| `ignoresMouseEvents` + polling `NSEvent.mouseLocation` on a timer | Rejected (section 10). |
| Overriding `hitTest:` to return `nil` outside the regions | Rejected. By then the window server has already sent the click to our window, so returning `nil` only drops it. It does not reach the app below. |
| Window shaping | AppKit has no window-region API. The only equivalent is resizing the window or using separate windows (the fallback). |

### Fallback (either OS)

If an approach fails on real hardware: make the buddy window exactly the
cat's size and position. Show the bubble either by growing the window while
it is visible (clicks over the transparent margin are blocked only while the
bubble shows) or in a second small window created only when needed. Both stay
event-free, so section 10 still holds. The cost is more window management
code, and memory if the bubble gets its own webview.

## CPU impact

Measured on Windows 11 (24 logical CPUs), debug build, buddy window showing
the demo page, mouse in use elsewhere during the measurement, 60 s:

| Process | CPU time in 60 s |
|---|---|
| `aichemist-desktop-buddy.exe` (all click-through code runs here) | **0 ms** |
| App + its 5 WebView2 processes | 109 ms (0.18% of one core, 0.008% of the machine) |

The WebView2 time is the webview's own baseline (renderer/GPU housekeeping),
not click-through. `SetWindowRgn` adds no work at rest or while the mouse
moves.

Measure again with:

```powershell
$p = Get-Process aichemist-desktop-buddy
$t0 = $p.TotalProcessorTime; Start-Sleep 60; $p.Refresh()
($p.TotalProcessorTime - $t0).TotalMilliseconds
```

macOS: not measured (no Mac available). At rest nothing runs. While the mouse
moves, one main-thread callback runs per move event (about 60-120 per second
during motion). Each does one `mouseLocation`, one `frame` read and up to 8
rectangle tests. Expected to be negligible. Measure it as part of the
checklist below.

## What was verified

Windows (locally, Windows 11, scale factor 1.0):

- [x] Transparent window: only the cat and bubble boxes are visible
  (screenshot).
- [x] OS hit-testing (`WindowFromPoint`): points in the empty part of the
  window resolve to the window underneath (another app's window). Points on
  the cat resolve to the buddy's WebView2 window.
- [x] Region updates at runtime: a left-click on the cat shows the bubble, the
  page calls `set_hit_regions`, and the window region grows from one
  rectangle to two (`GetWindowRgnBox` and `WindowFromPoint` on the bubble).
  A second click shrinks it again.
- [x] Left-click and right-click reach the page (bubble toggles, cat turns
  blue). Driven with posted mouse messages to the WebView2 window, because a
  full-screen app had the foreground during testing.
- [ ] Hover with the real cursor, and a real click on the cat. These need the
  real cursor and were not driven automatically. Owner: run the app, hover
  the cat (it lightens), click it (bubble toggles), right-click it (turns
  blue), and click the empty area (the window below gets the click).
- [ ] Scale factors above 100% and a secondary monitor. The code handles
  them (physical conversion, re-apply on `ScaleFactorChanged`) but they were
  not tested.

macOS: compiles and passes clippy in CI (`macos-latest`). Locally,
`cargo clippy --target aarch64-apple-darwin` type-checks too, with `DOCS_RS=1`
so `objc2-exception-helper` skips its C build. **Nothing has been run on a
Mac.**

## Manual test checklist for macOS

Build and run (`npm ci && npm run tauri dev`, or a `tauri build` bundle). The
window opens in the middle of the main screen. Put a Finder window or a text
editor underneath it.

1. [ ] The window is transparent: only the orange "cat" box is visible, with
   no title bar and no shadow.
2. [ ] Click on the empty area of the buddy window (well away from the cat).
   The app underneath gets the click (for example, it focuses or selects
   text).
3. [ ] Scroll over the empty area. The app underneath scrolls.
4. [ ] Hover over the empty area of a text editor underneath: its I-beam
   cursor shows, and hover effects in the app below work.
5. [ ] Move onto the cat. It lightens (hover) **while another app is
   active**, without clicking first.
6. [ ] Left-click the cat. The white bubble appears above it.
7. [ ] Click the bubble area: the click does **not** go through. Click the
   empty area beside the bubble: it goes through.
8. [ ] Click the cat again. The bubble disappears, and clicks where it was
   go through again. (The demo cannot show the bubble under a still cursor.
   The real reminder flow, where Rust shows the bubble, covers that case.)
9. [ ] Right-click the cat. It turns blue, and no system context menu appears.
10. [ ] Move quickly from the cat out of the window, then click right away.
    The click goes to the app below (the `mouseExited` path).
11. [ ] Check that no Accessibility or Input Monitoring prompt appears.
12. [ ] Idle CPU: leave the mouse still for 60 s. In Activity Monitor, CPU
    for the app process stays at 0.0%. Then move the mouse in circles
    elsewhere on screen for 30 s and note the CPU (expected well under 1%).
13. [ ] (For the record) Before step 2, comment out
    `click_through::init` in `lib.rs` and check whether clicks on fully
    transparent pixels pass through on their own. If they do, note it in
    this doc. The monitor approach stays, because it does not depend on
    WebKit internals.
14. [ ] On a Retina display and on a second monitor with a different scale,
    the hit area matches the drawn boxes exactly.

## Known limitations and gaps

- **Bounding boxes only.** Per-pixel hit-testing is not implemented
  (allowed by the spec).
- **Windows: the region clips drawing.** The page must report boxes that
  cover everything it draws.
- **macOS: the crossing move.** The mouse-move that carries the cursor into
  the cat goes to the app below (it is the event that triggers the switch).
  Later events go to the buddy. In practice it is invisible.
- **macOS: dock icon.** Hidden since task 1.A with
  `ActivationPolicy::Accessory` (`skipTaskbar` is Windows/Linux only). See
  "Buddy window shell" below.
- **`macOSPrivateApi`** is enabled (Cargo feature `macos-private-api` and
  `app.macOSPrivateApi`), which Tauri requires for transparent windows on
  macOS. It rules out the Mac App Store, which is not a target.
- **Linux:** no click-through.
- The buddy page calls IPC through `window.__TAURI_INTERNALS__`
  (`src/buddy/ipc.ts`: `invoke` plus `listen` via `plugin:event|listen`)
  instead of adding `@tauri-apps/api`. It needs only those two calls, and
  never unlistens, so the capability grants only `core:event:allow-listen`.

## Buddy window shell (task 1.A)

Code: `src-tauri/src/buddy_window.rs` (window behaviour and re-placement),
`src-tauri/src/placement.rs` (pure placement math, unit-tested),
`src-tauri/src/tray.rs` (tray / menu bar icon), `src-tauri/src/pause.rs`
(pause flag).

### Window

- Config: `visible: false` (shown only after it is placed, so it never
  flashes in the middle of the screen), `focus: false`,
  `visibleOnAllWorkspaces: true` (macOS only). Size 256 x 232 CSS px, which
  is what placement computes for the default cat scale (a unit test checks
  that they match).
- **Size** (`placement::window_size_css`): width `max(240, 48 * scale) + 16`,
  height `120 + 48 * scale + 16`. That is the 48 x 48 sprite frame at the
  cat scale, a 240 x 120 bubble area above it, and 8 px of transparent
  padding on every side. Scale 1: 256 x 184, scale 2: 256 x 232, scale 8:
  400 x 520. The page is responsible for drawing the cat at the bottom of
  the window and the bubble above it, on the side of the configured corner.
- **Position**: the configured corner of the configured monitor's work area
  (excludes taskbar, menu bar and Dock), 8 CSS px from the edges. `monitor:
  None` or a monitor that is not connected means the primary monitor.
  Windows works in physical pixels with the monitor's scale factor; macOS
  works in points (see the module docs of `placement.rs` for why).
- **Re-placement** is event-driven, never polled: `ScaleFactorChanged`,
  `WM_DISPLAYCHANGE` and `WM_SETTINGCHANGE(SPI_SETWORKAREA)` on Windows
  (taskbar moved, resized or set to auto-hide; monitors added or removed),
  `NSApplicationDidChangeScreenParametersNotification` on macOS, and
  `buddy_window::set_appearance`. The work runs from the event loop, not
  inside the window procedure.
- **Windows: `WS_EX_TOOLWINDOW`.** Hides the buddy from Alt+Tab and Task
  View, and Windows shows tool windows on every virtual desktop. Tao rewrites
  the extended style whenever it changes window flags (for example on
  show), so a window subclass adds the bit back in `WM_STYLECHANGING`.
  Checked: the style is still set after `show()`.
- **Focus.** On Windows, `focus: false` makes tao show the window with
  `SW_SHOWNOACTIVATE`. On macOS the app is an accessory app and is not
  activated by showing a window.
- **macOS.** `ActivationPolicy::Accessory`: no Dock icon, no app menu, not
  in Cmd+Tab. `visibleOnAllWorkspaces` sets `CanJoinAllSpaces`. The window
  does not appear over other apps' full-screen Spaces (that would need
  `FullScreenAuxiliary`; not done, open question for the owner).

### Tray / menu bar

Menu: Open Settings (opens or focuses the settings window, task 2.4), Upcoming (emits `open-popover`
with `"upcoming"` to the buddy window), Pause reminders (check item; toggles
the in-memory flag and emits `paused-changed`), separator, Quit. The icon is
the app icon (`default_window_icon`), marked as a template image on macOS so
the menu bar draws the cat-head silhouette in its own colours. Left-click
also opens the menu. `tray::set_error` adds a disabled status line at the top
of the menu and a second tooltip line (for task 3.5).

### Single instance

`tauri-plugin-single-instance`: a second launch exits with code 0 before it
creates a window. Windows: named mutex. macOS: a Unix socket in `/tmp`.
Since task 2.4 the running instance then shows a small native notice,
"AIchemist Buddy is already running.", with a hint where to find it
(`src-tauri/src/already_running.rs`): `MessageBoxW` on its own thread on
Windows, `NSAlert` on the main thread on macOS, a log line on Linux. Only
one notice at a time; launches while it shows are ignored.

### Verified on Windows (2026-11-16, Windows 11 23H2, 3440 x 1440 at 100%)

Release build:

- [x] Window at 3176,1152, 256 x 232: bottom-right of the primary monitor
  (work area 3440 x 1392), 8 px above the taskbar and from the right edge.
- [x] Not in the taskbar (UI Automation list of taskbar buttons), extended
  style has `WS_EX_TOOLWINDOW` and `WS_EX_TOPMOST`.
- [x] Showing it does not take focus (foreground window unchanged, 5 of 5
  launches).
- [x] Click-through: `WindowFromPoint` in the empty part of the window
  returns the app underneath; on the cat it returns the buddy.
- [x] Tray icon in the notification area (overflow), tooltip
  "AIchemist Buddy", menu with the four items. Pause shows its check mark
  after one click and clears it after a second. Quit exits the app and its
  WebView2 processes.
- [x] A second launch exits (code 0) and the first keeps running.
- [x] Re-placement: after moving the window away, `WM_SETTINGCHANGE` with
  `SPI_SETWORKAREA` and `WM_DISPLAYCHANGE` each put it back in the corner;
  an unrelated `WM_SETTINGCHANGE` does not.
- [ ] Not tested: scale factors above 100%, a second monitor, a real
  taskbar move, virtual desktops (Win+Ctrl+Left/Right should keep the cat
  visible on every desktop). Owner: please check these by hand.

### Manual test checklist for macOS (task 1.A)

Run as above (`npm run tauri dev`, or the `.app` bundle).

1. [ ] No Dock icon, and the app is not in Cmd+Tab.
2. [ ] The window appears in the bottom-right of the main screen, above the
   Dock (if the Dock is at the bottom), 8 points from the edges, and never
   flashes in the middle of the screen at start.
3. [ ] Launching does not take focus from the frontmost app (keep typing in
   a text editor while it starts).
4. [ ] Switch Spaces (Ctrl+Left/Right): the cat stays visible on every
   Space. Note whether it shows over a full-screen app (expected: no).
5. [ ] Menu bar icon: the cat-head silhouette, drawn in the menu bar's
   colour (template), correct in light and dark mode. Click opens the menu:
   Open Settings, Upcoming, Pause reminders, Quit.
6. [ ] Pause reminders shows a check mark after one click and clears it
   after a second.
7. [ ] Quit exits the app.
8. [ ] Launch the app again while it runs (`open -n` the bundle, or run the
   binary twice): the second copy exits and no second cat appears. (Since
   task 2.4: see item 8 of the task 2.4 checklist for the notice.)
9. [ ] Move the Dock to the left/right or change its size, and turn Dock
   auto-hide on and off: note whether the cat moves to stay clear of it
   (depends on whether macOS posts the screen-parameters notification for
   Dock changes).
10. [ ] With a second display: unplug it while the buddy is configured for
    it (or rearrange displays in System Settings); the cat moves to the main
    screen without a restart.
11. [ ] On a Retina display, the window is 256 x 232 points and the cat box
    sits in its bottom-right corner (no clipping).
12. [ ] Idle CPU still 0.0% in Activity Monitor after these steps.

## Buddy renderer (task 2.1)

Code: `src/buddy/` (canvas renderer, `frames.ts` pure frame timing,
`ipc.ts`, `sprites.ts`, `hooks.ts`), `src-tauri/src/buddy_state.rs` (state
runtime and idle-to-sleep timer).

- The cat is a `<canvas>` sized `48 * catScale` CSS px, with a backing store
  of that size times `devicePixelRatio` and `imageSmoothingEnabled = false`,
  so the pixel art stays crisp. It sits 8 px from the bottom, on the side of
  the configured corner (right for `*Right`, left for `*Left`). `#bubble`
  (filled by 2.2) sits above it, max 240 x 120.
- Hit regions: the canvas box plus 2 px, and the bubble box plus 2 px when
  it is shown. Re-reported on appearance change, window resize / DPR change,
  when the window becomes visible, and (through a `ResizeObserver`) whenever
  the bubble appears, disappears or changes size. The canvas never draws
  outside its box, so the region always covers what is drawn.
- Hover (`pointerenter` / `pointermove`, at most once per 3 s), click and
  right-click on the cat call `report_interaction`. The browser context menu
  is suppressed everywhere in the window.

### Verified on Windows (2026-11-17, Windows 11 23H2, 3440 x 1440 at 100%)

Release build:

- [x] The idle cat animates (frames change between screenshots 230 ms
  apart) at 96 x 96 in the bottom-right corner of the 256 x 232 window.
- [x] Window region is 150,126 - 250,226 (the 96 px cat plus the 2 px
  margin). `WindowFromPoint` on the cat returns the buddy; in the empty part
  of the window it returns the window underneath.
- [x] With a short idle timeout (`AICHEMIST_SLEEP_AFTER_IDLE_SECS`, debug
  builds only; a temporary local build enabled it in release for the check)
  the cat switches to the sleeping animation; moving the cursor over the cat
  wakes it (idle animation again).

### Manual test checklist for macOS (task 2.1)

1. [ ] The cat animates (idle) at the bottom-right of the window, crisp on a
   Retina display (no blurry edges).
2. [ ] Hovering the cat and clicking it work; clicks next to the cat (inside
   the window but outside the cat) reach the app underneath.
3. [ ] Right-click on the cat shows no browser context menu.
4. [ ] Debug build with `AICHEMIST_SLEEP_AFTER_IDLE_SECS=15 npm run tauri
   dev`: after 15 s the cat sleeps; hovering wakes it.
5. [ ] Activity Monitor: CPU of the app and its WebKit processes while the
   cat animates (idle) and while it sleeps. Record the numbers in
   `docs/perf.md`.

## Settings window (task 2.4)

Code: `src-tauri/src/settings_service.rs` (load, merge, validate, save,
listeners, `get_settings` / `update_settings`), `src-tauri/src/settings_window.rs`
(window lifecycle, `list_monitors`), `src-tauri/src/logger.rs` (log file),
`src/settings/` (page).

- **Lifecycle.** `tray::open_settings` (tray menu; later the cat menu and the
  `open_settings` command) creates the `settings` window on demand: decorated, resizable,
  560 x 680, centered, title "AIchemist Buddy Settings". If it exists it is
  unminimized and focused. Closing it uses Tauri's default close, which
  destroys the window and its webview; nothing hides it. It is built with the
  buddy's `additionalBrowserArgs`, because WebView2 refuses a second
  environment with different options on the same user data folder.
- **Capabilities.** `capabilities/settings.json` grants the settings window
  `get_settings`, `update_settings` and `list_monitors`, nothing else. The
  buddy capability is unchanged.
- **Apply on change.** Every `change` event sends the whole form. The core
  validates; on error the page shows the messages next to the fields and
  nothing is saved. On success the file is written atomically, the buddy is
  re-placed (appearance) and listeners registered with
  `settings_service::on_settings_changed` are called with `(old, new)`.
- **Log file.** `app_log_dir()/aichemist-buddy.log`, rotated to
  `aichemist-buddy.old.log` at 1 MB. Info and up for this crate, warn and up
  for dependencies. Windows: `%LOCALAPPDATA%\io.github.anras573.aichemistbuddy\logs`,
  macOS: `~/Library/Logs/io.github.anras573.aichemistbuddy`.

### Verified on Windows (2026-11-17, release build, UI Automation)

- [x] Opening twice gives one window only.
- [x] A second launch shows "AIchemist Buddy is already running." (from the
  first instance) and exits with code 0; launches while the notice shows
  open no second notice.
- [x] Cat size slider 2 -> 4: the buddy resizes live (256 x 232 -> 256 x 328)
  and stays in the corner.
- [x] Corner "Top left": the buddy moves live to 8,8.
- [x] Lead times "10, 500, 10": the field shows "entry 2: must be between 0
  and 120; entry 3: duplicate value" and the file keeps `[10, 5, 1]`.
- [x] Settings persist across a restart (`settings.json` in
  `%APPDATA%\<identifier>`; the buddy starts at the saved size and corner).
- [x] An invalid file (`catScale: 99`) loads with the default appearance and
  logs a warning with the field path only.
- [x] Closing the window destroys it: the window and its renderer process
  are gone; memory in `docs/perf.md`.
- Window checks above were driven through the same `tray::open_settings`
  call as the tray item (a temporary test hook in an earlier revision of
  this PR).

### Manual test checklist for macOS (task 2.4)

1. [ ] Menu bar icon > Open Settings: the window opens centered, in front,
   with focus (the app is an accessory app; note if it opens behind the
   frontmost app).
2. [ ] Open Settings again while it is open: the same window comes to the
   front; no second window.
3. [ ] Change the cat size: the cat resizes live. Change the corner: it moves
   live. Pick a second display in Monitor (if any): it moves there.
4. [ ] Enter `10, 500` as lead times and leave the field: an error shows
   under it and `~/Library/Application Support/io.github.anras573.aichemistbuddy/settings.json`
   is unchanged.
5. [ ] Quit and relaunch: the changed settings are kept.
6. [ ] Close the window (red button or Cmd+W): in Activity Monitor the extra
   WebContent process goes away and the app's memory returns to about the
   idle number (`tools/measure-memory.sh --pid PID` before, while open,
   after).
7. [ ] Light and dark mode both render the form legibly.
8. [ ] Launch the app a second time while it runs: an alert "AIchemist Buddy
   is already running. Find it in the menu bar." appears in front; click OK.
   Launch twice quickly: only one alert. No second cat appears.

## Upcoming popover and cat menu (task 2.3)

Code: `src/buddy/popover.ts` (popover), `src/buddy/format.ts` (pure
formatting), `src-tauri/src/popover_window.rs` (`set_popover_open`),
`src-tauri/src/placement.rs` (`popover_window_size_css`, `place_grown`),
`src-tauri/src/cat_menu.rs` (native menu, `show_cat_menu`, `open_settings`),
`src-tauri/src/upcoming.rs` (`list_upcoming` and the task commands),
`src-tauri/src/app_storage.rs` (database handle), `src-tauri/src/snooze.rs`
(`snooze_all`, stub until 2.2).

- **Window.** The popover is 288 x 360 CSS px and does not fit the 240 x 120
  bubble area. While it is open the page calls `set_popover_open` and Rust
  grows the window away from its corner, so the cat stays where it is:
  bottom corners grow up (popover above the cat, 304 x 472 at scale 2), top
  corners grow down (bubble area above the cat, popover below it, 304 x 592).
  Closing restores the normal size. Display changes while it is open keep the
  grown size (`buddy_window::reposition` reads the flag).
- **Hit regions.** The popover box plus 2 px is reported as a third region
  while it is shown (the window region clips drawing on Windows).
- **Open / close.** Left-click on the cat toggles it; the tray "Upcoming"
  and the cat menu emit `open-popover`. Escape, the close button, a second
  click on the cat or 60 s without pointer/keyboard activity in the popover
  close it. Opening never focuses the window; only the "Add task" view
  focuses the title field.
- **Cat menu.** Native `Menu::popup` at the cursor, run from the event loop
  (not inside the WebView2 IPC callback). Item ids are prefixed `cat-` so the
  tray handler, which sees every menu event, ignores them.

### Verified on Windows (2026-11-18, Windows 11 23H2, 3440 x 1440 at 100%)

Release build. The desktop was in active use by its owner during the check,
so no real mouse input was injected; page events were dispatched through the
WebView2 DevTools port and the result checked with screenshots,
`GetWindowRect` and `WindowFromPoint`.

- [x] Click on the cat: the window grows from 256 x 232 to 304 x 472 with its
  bottom-right corner fixed; the cat stays in place; the popover shows
  "No meetings in the next 7 days" and the tasks.
- [x] Add task: a blank title shows "must not be empty" (backend
  validation) inline; an empty due time shows "Pick a date and time."; a
  past due time shows the task with the red "overdue" badge; open tasks are
  sorted by due time with "in N min".
- [x] Mark done moves the task to the collapsed "Done (1)" section; its
  delete button removes it and the section hides when empty.
- [x] `WindowFromPoint` inside the popover returns the buddy; left of the
  popover and next to the cat it returns the window underneath
  (click-through intact). After closing, the popover area is click-through
  again and the window is back to 256 x 232.
- [x] Escape, a second click on the cat and the 60 s idle timeout close it.
- [ ] Native cat menu and the tray "Upcoming" item: not exercised by hand
  (would have popped a menu under the owner's cursor). Code path: tray
  emits `open-popover` (unchanged since 1.A); cat menu items call the same
  functions as the tray and the commands.

### Manual test checklist (macOS, and the native menu on Windows) (task 2.3)

1. [ ] Click the cat: the popover opens above it and the cat does not move
   or flicker noticeably while the window grows. Click the cat again: it
   closes and the window shrinks back.
2. [ ] Set the corner to Top left / Top right in Settings: the popover opens
   below the cat and the cat stays in place.
3. [ ] Add a task with a title and a due time in the past: it shows as
   "overdue". Submit an empty title: "must not be empty" shows under the
   form. The native date/time picker opens and is usable (not clipped).
4. [ ] Tick a task: it moves to "Done"; expand Done and delete it.
5. [ ] Clicks next to the popover and next to the cat reach the app
   underneath; clicks inside the popover do not.
6. [ ] Right-click the cat: a native menu (Upcoming, Add task, Snooze all,
   Settings, Quit) appears at the cursor. Upcoming opens the popover; Add
   task opens it with the title field focused; Snooze all does nothing
   visible (stub until 2.2, logged); Settings opens the settings window;
   Quit exits.
7. [ ] Menu bar / tray "Upcoming" opens the popover without activating the
   app (the frontmost app keeps focus); Escape does not apply until you
   click into the popover. It closes after 60 s untouched.
8. [ ] Memory with the popover open and after closing it
   (`tools/measure-memory.sh --pid PID`); record in `docs/perf.md`.
