// Buddy window entry point (task 2.1): draws the cat on a canvas, follows the
// buddy state and appearance from Rust, and reports hit regions.
//
// Layout (src-tauri/src/placement.rs): 8 px padding on every side, the cat
// (48 x 48 frame at `catScale`) at the bottom, on the side of the configured
// corner, and the 240 x 120 bubble area above it. `#bubble` is filled by the
// reminder bubble (2.2); its size changes are picked up automatically.
//
// CPU (docs/perf.md): every drawn frame costs ~3 ms of CPU in WebView2 (GPU
// disabled), so the page draws only when the frame changes. Exactly one
// timeout is pending while animating: it fires when the next frame is due
// (per-frame durations can hold a pose for seconds), draws it and schedules
// the next. Static frames, finished one-shot animations and a hidden window
// schedule nothing. No requestAnimationFrame loop.

import { frameAt } from "./frames";
import { catHandlers, onCatClick, onCatContextMenu } from "./hooks";
import {
  type Appearance,
  type BuddyState,
  type HitRect,
  inTauri,
  invoke,
  listen,
  reportInteraction,
  setHitRegions,
} from "./ipc";
import { initPopover } from "./popover";
import { loadDefaultPack, type SpritePack } from "./sprites";

const main = document.querySelector<HTMLElement>("#buddy")!;
const canvas = document.querySelector<HTMLCanvasElement>("#cat")!;
const bubble = document.querySelector<HTMLElement>("#bubble")!;
const ctx = canvas.getContext("2d")!;

/** Extra hit area around the drawn boxes (CSS px). On Windows the window
 * region also clips drawing, so it must cover everything drawn. */
const HIT_MARGIN = 2;
/** At most one hover report per this many ms. */
const HOVER_THROTTLE_MS = 3000;

let pack: SpritePack | null = null;
let state: BuddyState = "idle";
let stateStart = performance.now();
let appearance: Appearance = {
  catScale: 2,
  corner: "bottomRight",
  monitor: null,
  opacityPercent: 100,
  spritePack: "default",
};

// ----- Rendering -----

let timer: number | undefined;
let drawnFrame = -1;

function stopLoop(): void {
  if (timer !== undefined) clearTimeout(timer);
  timer = undefined;
}

/** Draws the current frame if it changed and schedules the next change. */
function tick(): void {
  timer = undefined;
  if (!pack || document.hidden) return;
  const sprite = pack.states[state];
  const { index, nextInMs } = frameAt(sprite, performance.now() - stateStart);
  if (index !== drawnFrame) {
    const { frameWidth: w, frameHeight: h } = pack;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.imageSmoothingEnabled = false;
    ctx.drawImage(sprite.image, index * w, 0, w, h, 0, 0, canvas.width, canvas.height);
    drawnFrame = index;
  }
  if (nextInMs !== null) timer = window.setTimeout(tick, Math.max(1, Math.ceil(nextInMs)));
}

/** (Re)starts the loop, redrawing the current frame. */
function restart(): void {
  stopLoop();
  drawnFrame = -1;
  tick();
}

function setState(next: BuddyState): void {
  if (next === state) return;
  state = next;
  stateStart = performance.now(); // Entering a state restarts it at frame 0.
  restart();
}

// ----- Layout -----

function catCssSize(): { width: number; height: number } {
  const scale = Math.max(1, Math.floor(appearance.catScale));
  const w = pack?.frameWidth ?? 48;
  const h = pack?.frameHeight ?? 48;
  return { width: w * scale, height: h * scale };
}

/** Sizes the canvas for the scale and devicePixelRatio, positions it. */
function layout(): void {
  const { width, height } = catCssSize();
  const dpr = window.devicePixelRatio || 1;
  canvas.style.width = `${width}px`;
  canvas.style.height = `${height}px`;
  const bw = Math.round(width * dpr);
  const bh = Math.round(height * dpr);
  if (canvas.width !== bw || canvas.height !== bh) {
    canvas.width = bw; // Also clears the canvas and resets the context.
    canvas.height = bh;
  }
  main.style.setProperty("--cat-height", `${height}px`);
  main.dataset.side = appearance.corner.endsWith("Left") ? "left" : "right";
  main.style.opacity = String(Math.min(100, Math.max(20, appearance.opacityPercent)) / 100);
  restart();
  reportHitRegions();
}

function grow(r: DOMRect, m: number): HitRect {
  const x = Math.max(0, r.x - m);
  const y = Math.max(0, r.y - m);
  return { x, y, width: r.right + m - x, height: r.bottom + m - y };
}

function reportHitRegions(): void {
  const regions = [grow(canvas.getBoundingClientRect(), HIT_MARGIN)];
  for (const el of [bubble, popover.element]) {
    const r = el.getBoundingClientRect();
    if (!el.hidden && r.width > 0 && r.height > 0) regions.push(grow(r, HIT_MARGIN));
  }
  setHitRegions(regions);
}

// ----- Popover and cat menu (task 2.3) -----

const popover = initPopover({
  main,
  corner: () => appearance.corner,
  changed: reportHitRegions,
});
onCatClick(() => popover.toggle());
onCatContextMenu(() => {
  if (inTauri) invoke("show_cat_menu").catch((err: unknown) => console.error("show_cat_menu failed", err));
});

// ----- Interaction -----

let lastReport = -Infinity;

function interacted(force: boolean): void {
  const now = performance.now();
  if (!force && now - lastReport < HOVER_THROTTLE_MS) return;
  lastReport = now;
  reportInteraction();
}

canvas.addEventListener("pointerenter", () => interacted(false));
canvas.addEventListener("pointermove", () => interacted(false));
canvas.addEventListener("click", (e) => {
  interacted(true);
  catHandlers.click(e);
});
// No browser context menu anywhere in the buddy window.
document.addEventListener("contextmenu", (e) => e.preventDefault());
canvas.addEventListener("contextmenu", (e) => {
  interacted(true);
  catHandlers.contextMenu(e);
});

// ----- Wiring -----

let dpr = window.devicePixelRatio;
window.addEventListener("resize", () => {
  if (window.devicePixelRatio !== dpr) {
    dpr = window.devicePixelRatio;
    layout();
  } else {
    reportHitRegions();
  }
});

// The window starts hidden and is shown once Rust has placed it.
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    stopLoop();
  } else {
    restart();
    reportHitRegions();
  }
});

// The bubble (2.2) appearing, disappearing or resizing.
const observer = new ResizeObserver(reportHitRegions);
observer.observe(bubble);
observer.observe(popover.element);

async function start(): Promise<void> {
  // Listen first so no change between the reads and the listen is missed.
  await Promise.all([
    listen<BuddyState>("buddy-state-changed", setState),
    listen<Appearance>("appearance-changed", (a) => {
      appearance = a;
      layout();
    }),
  ]);
  const [initialState, initialAppearance, loaded] = await Promise.all([
    inTauri ? invoke<BuddyState>("get_buddy_state") : Promise.resolve(state),
    inTauri ? invoke<Appearance>("get_appearance") : Promise.resolve(appearance),
    loadDefaultPack(),
  ]);
  pack = loaded;
  appearance = initialAppearance;
  state = initialState;
  stateStart = performance.now();
  layout();
}

start().catch((err: unknown) => console.error("buddy start failed", err));
