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
- **macOS: dock icon.** Hiding the dock icon needs
  `ActivationPolicy::Accessory` (`skipTaskbar` is Windows/Linux only). That
  is left to the task that builds the real buddy window.
- **`macOSPrivateApi`** is enabled (Cargo feature `macos-private-api` and
  `app.macOSPrivateApi`), which Tauri requires for transparent windows on
  macOS. It rules out the Mac App Store, which is not a target.
- **Linux:** no click-through.
- The page calls IPC through `window.__TAURI_INTERNALS__.invoke` to avoid
  adding `@tauri-apps/api` for one call. The real frontend should use the
  package.
