// Generates the default placeholder sprite pack (a gray tabby cat) into
// assets/sprites/default/: pack.json, one PNG strip per state, and preview.png.
//
//   npm run gen:sprites
//
// Pure Node, no npm dependencies. Deterministic: no randomness, no timestamps,
// so re-running produces byte-identical files. See docs/sprite-pack.md.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { encodeIndexedPng } from "./lib/png.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUT_DIR = join(ROOT, "assets", "sprites", "default");

const W = 48;
const H = 48;
const PREVIEW_SCALE = 6;

// Palette (index -> RGBA). Index 0 is transparent.
const C = {
  clear: 0,
  outline: 1,
  fur: 2,
  furLight: 3,
  stripe: 4,
  pink: 5,
  eye: 6,
  pupil: 7,
  white: 8,
  yellow: 9,
  zblue: 10,
  bg: 11, // preview only
  bgGrid: 12, // preview only
};
const PALETTE = [
  [0, 0, 0, 0],
  [0x26, 0x26, 0x2e, 255],
  [0x8c, 0x8f, 0x99, 255],
  [0xc8, 0xcb, 0xd2, 255],
  [0x4c, 0x4e, 0x58, 255],
  [0xe8, 0x91, 0x9f, 255],
  [0x9c, 0xcc, 0x52, 255],
  [0x15, 0x15, 0x1a, 255],
  [0xff, 0xff, 0xff, 255],
  [0xff, 0xc9, 0x3c, 255],
  [0xa8, 0xd4, 0xff, 255],
  [0xee, 0xe8, 0xdc, 255],
  [0xd8, 0xd0, 0xc0, 255],
];

// ---------------------------------------------------------------------------
// Drawing primitives. A mask is a Uint8Array of W*H (1 = covered).

class Frame {
  constructor() {
    this.px = new Uint8Array(W * H);
  }
  set(x, y, c) {
    if (x >= 0 && y >= 0 && x < W && y < H) this.px[y * W + x] = c;
  }
  /** Fill a mask with a colour. */
  fill(mask, c) {
    for (let i = 0; i < mask.length; i++) if (mask[i]) this.px[i] = c;
  }
  /** Fill a mask and draw a 1px outline ring (4-neighbour) around it. */
  fillOutlined(mask, c) {
    const ring = dilate(mask);
    for (let i = 0; i < mask.length; i++) if (ring[i] && !mask[i]) this.px[i] = C.outline;
    this.fill(mask, c);
  }
  /** Set individual pixels: list of [x, y]. */
  dots(points, c, dy = 0) {
    for (const [x, y] of points) this.set(x, y + dy, c);
  }
}

function newMask() {
  return new Uint8Array(W * H);
}

function maskFrom(test) {
  const m = newMask();
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) if (test(x, y)) m[y * W + x] = 1;
  return m;
}

function union(...masks) {
  const m = newMask();
  for (const mk of masks) for (let i = 0; i < m.length; i++) m[i] |= mk[i];
  return m;
}

function dilate(mask) {
  return maskFrom(
    (x, y) =>
      mask[y * W + x] ||
      (x > 0 && mask[y * W + x - 1]) ||
      (x < W - 1 && mask[y * W + x + 1]) ||
      (y > 0 && mask[(y - 1) * W + x]) ||
      (y < H - 1 && mask[(y + 1) * W + x]),
  );
}

function ellipse(cx, cy, rx, ry) {
  return maskFrom((x, y) => ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 <= 1);
}

function triangle([ax, ay], [bx, by], [cx, cy]) {
  const side = (px, py, x1, y1, x2, y2) => (px - x2) * (y1 - y2) - (x1 - x2) * (py - y2);
  return maskFrom((x, y) => {
    const d1 = side(x, y, ax, ay, bx, by);
    const d2 = side(x, y, bx, by, cx, cy);
    const d3 = side(x, y, cx, cy, ax, ay);
    const neg = d1 < 0 || d2 < 0 || d3 < 0;
    const pos = d1 > 0 || d2 > 0 || d3 > 0;
    return !(neg && pos);
  });
}

/** Mirror an x coordinate around the vertical centre line of the frame. */
const mx = (x) => W - 1 - x;

/** Sample a quadratic Bezier curve. Returns [{x, y, t}]. */
function bezier([x0, y0], [x1, y1], [x2, y2], steps = 48) {
  const pts = [];
  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    const u = 1 - t;
    pts.push({ x: u * u * x0 + 2 * u * t * x1 + t * t * x2, y: u * u * y0 + 2 * u * t * y1 + t * t * y2, t });
  }
  return pts;
}

/**
 * A thick tail along a curve, plus a mask of its dark rings.
 * Ring placement uses the curve parameter of the nearest sample point.
 */
function tail(points, radius) {
  const body = newMask();
  const rings = newMask();
  for (let y = 0; y < H; y++) {
    for (let x = 0; x < W; x++) {
      let best = Infinity;
      let bestT = 0;
      for (const p of points) {
        const d = (p.x - x) ** 2 + (p.y - y) ** 2;
        if (d < best) {
          best = d;
          bestT = p.t;
        }
      }
      if (best <= radius * radius) {
        body[y * W + x] = 1;
        if (bestT > 0.88 || (bestT > 0.3 && Math.floor(bestT * 9) % 2 === 1)) rings[y * W + x] = 1;
      }
    }
  }
  return { body, rings };
}

/** Bitmap glyph -> mask at (x, y). Rows are strings, '#' = set. */
function glyph(rows, x0, y0) {
  const m = newMask();
  rows.forEach((row, y) => {
    [...row].forEach((ch, x) => {
      const px = x0 + x;
      const py = y0 + y;
      if (ch === "#" && px >= 0 && py >= 0 && px < W && py < H) m[py * W + px] = 1;
    });
  });
  return m;
}

const GLYPH = {
  zSmall: ["####", "..#.", ".#..", "####"],
  zMid: ["#####", "...#.", "..#..", ".#...", "#####"],
  zBig: ["######", "....#.", "...#..", "..#...", ".#....", "######"],
  bang: ["##", "##", "##", "##", "##", "##", "..", "##", "##"],
  dot: ["##", "##"],
};

// ---------------------------------------------------------------------------
// The cat.

/**
 * Sitting gray tabby, facing the viewer.
 * @param {Frame} f
 * @param {{dy?: number, tip?: [number, number], eyes?: 'open'|'blink'|'up'|'wide', ears?: 'normal'|'tall'}} o
 */
function drawSitting(f, { dy = 0, tip = [43, 28], eyes = "open", ears = "normal" } = {}) {
  // Tail (behind the body), curling up on the right.
  const t = tail(bezier([31, 43 + dy], [45, 46 + dy], [tip[0], tip[1] + dy]), 1.7);
  f.fillOutlined(t.body, C.fur);
  f.fill(t.rings, C.stripe);

  // Body: pear shape (haunches + chest).
  const body = union(ellipse(23.5, 38.5 + dy, 10.5, 8), ellipse(23.5, 31 + dy, 7.5, 6.5));
  f.fillOutlined(body, C.fur);
  // Flank stripes: a few pixels in from each edge on alternating rows.
  for (const row of [30, 33, 36, 39, 42]) {
    const y = row + dy;
    if (y < 0 || y >= H) continue;
    let left = -1;
    for (let x = 0; x < W / 2; x++) if (body[y * W + x]) { left = x; break; }
    if (left < 0) continue;
    const len = row >= 36 ? 4 : 3;
    for (let k = 0; k < len; k++) {
      f.set(left + k, y, C.stripe);
      f.set(mx(left + k), y, C.stripe);
    }
    // A pixel on the next row makes the stripe slant down toward the chest.
    f.set(left + len, y + 1, C.stripe);
    f.set(mx(left + len), y + 1, C.stripe);
  }
  // Light chest and belly.
  f.fill(ellipse(23.5, 37 + dy, 4, 6.5), C.furLight);

  // Front paws.
  f.fillOutlined(ellipse(20, 45 + dy, 2.6, 1.6), C.furLight);
  f.fillOutlined(ellipse(27, 45 + dy, 2.6, 1.6), C.furLight);

  // Ears (drawn before the head so the head overlaps their base).
  const tipY = ears === "tall" ? 3 : 5;
  const earL = triangle([13, 17 + dy], [15, tipY + dy], [22, 12 + dy]);
  const earR = triangle([mx(13), 17 + dy], [mx(15), tipY + dy], [mx(22), 12 + dy]);
  f.fillOutlined(earL, C.fur);
  f.fillOutlined(earR, C.fur);
  f.fill(triangle([15, 15 + dy], [16, tipY + 4 + dy], [20, 13 + dy]), C.pink);
  f.fill(triangle([mx(15), 15 + dy], [mx(16), tipY + 4 + dy], [mx(20), 13 + dy]), C.pink);

  // Head.
  f.fillOutlined(ellipse(23.5, 19 + dy, 10.5, 8), C.fur);
  // Muzzle.
  f.fill(ellipse(23.5, 23.5 + dy, 4.2, 2.6), C.furLight);
  // Tabby "M" on the forehead.
  f.dots([[20, 12], [20, 13], [23, 11], [23, 12], [23, 13], [24, 11], [24, 12], [24, 13], [27, 12], [27, 13]], C.stripe, dy);
  // Cheek stripes.
  const cheeks = [[13, 19], [14, 19], [15, 19], [14, 21], [15, 21]];
  f.dots(cheeks, C.stripe, dy);
  f.dots(cheeks.map(([x, y]) => [mx(x), y]), C.stripe, dy);

  // Eyes (3x3, left eye at x 19..21, right eye mirrored at 26..28).
  drawEyes(f, eyes, dy);

  // Nose and mouth.
  f.dots([[23, 22], [24, 22]], C.pink, dy);
  f.dots([[23, 23], [24, 23], [22, 24], [25, 24]], C.outline, dy);
}

function drawEyes(f, mode, dy) {
  for (const side of [0, 1]) {
    const X = (x) => (side ? mx(x) : x);
    if (mode === "blink") {
      f.dots([[X(19), 20], [X(20), 20], [X(21), 20]], C.outline, dy);
      continue;
    }
    const top = mode === "wide" ? 17 : 18;
    for (let y = top; y <= 20; y++) for (const x of [19, 20, 21]) f.set(X(x), y + dy, C.eye);
    if (mode === "up") {
      f.dots([[X(20), 18], [X(21), 18]], C.pupil, dy);
      f.dots([[X(20), 19]], C.pupil, dy);
    } else if (mode === "wide") {
      f.dots([[X(20), 18], [X(20), 19]], C.pupil, dy);
      f.dots([[X(19), 17]], C.white, dy);
    } else {
      f.dots([[X(20), 18], [X(20), 19], [X(20), 20]], C.pupil, dy);
    }
  }
}

/** Curled-up sleeping tabby, head on the left, tail wrapped in front. */
function drawSleeping(f, { breath = 0 } = {}) {
  // Body loaf; breathing raises the back by one pixel.
  const ry = 8 + breath;
  const body = ellipse(27, 46 - ry, 15, ry);
  f.fillOutlined(body, C.fur);
  // Back stripes: short vertical bands from the top edge.
  for (const x of [22, 26, 30, 34, 38]) {
    let top = -1;
    for (let y = 0; y < H; y++) if (body[y * W + x]) { top = y; break; }
    if (top < 0) continue;
    for (let k = 0; k < 3; k++) f.set(x, top + k, C.stripe);
    f.set(x + 1, top + 3, C.stripe);
  }

  // Ears.
  f.fillOutlined(triangle([7, 35], [7, 26], [13, 31]), C.fur);
  f.fillOutlined(triangle([14, 31], [19, 26], [20, 34]), C.fur);
  f.fill(triangle([8, 33], [8, 29], [11, 31]), C.pink);
  f.fill(triangle([16, 31], [18, 29], [18, 33]), C.pink);

  // Head resting on the front of the loaf.
  f.fillOutlined(ellipse(13.5, 37.5, 8, 6.5), C.fur);
  f.fill(ellipse(13.5, 41, 3.5, 2), C.furLight);
  f.dots([[12, 31], [13, 31], [13, 32], [14, 31], [14, 32], [15, 31]], C.stripe);
  f.dots([[6, 37], [7, 37], [6, 39], [21, 37], [20, 37], [21, 39]], C.stripe);
  // Closed eyes (gentle downward curves).
  f.dots([[9, 37], [10, 38], [11, 38], [12, 37]], C.outline);
  f.dots([[15, 37], [16, 38], [17, 38], [18, 37]], C.outline);
  f.dots([[13, 40], [14, 40]], C.pink);

  // Tail wrapped around the front.
  const t = tail(bezier([41, 41], [40, 47], [22, 45.5]), 1.6);
  f.fillOutlined(t.body, C.fur);
  f.fill(t.rings, C.stripe);
}

// ---------------------------------------------------------------------------
// States.

// CPU (docs/perf.md): every frame change costs ~3 ms of CPU in WebView2, so
// idle and sleeping mostly hold a pose (format 2 per-frame durations, in ms)
// and change only briefly: a blink, a tail swish, a breath, the next "z".

/** Resting tail tip; the swish moves through the others and back. */
const TAIL_REST = [43, 29];
const SWISH = [[44, 28], [45, 26], [44, 26], [42, 28]];

/** idle: [eyes, tail tip, ms]. Sit, blink, sit, swish, sit, double blink. */
const IDLE = [
  ["open", TAIL_REST, 4000],
  ["blink", TAIL_REST, 150],
  ["open", TAIL_REST, 3500],
  ...SWISH.map((tip) => ["open", tip, 150]),
  ["open", TAIL_REST, 5000],
  ["blink", TAIL_REST, 120],
  ["open", TAIL_REST, 160],
  ["blink", TAIL_REST, 120],
];

const Z = [
  [GLYPH.zSmall, 25, 20],
  [GLYPH.zMid, 31, 12],
  [GLYPH.zBig, 38, 3],
];

/** sleeping: [breath, visible z indices, ms]. Breathing out (0) and in (1),
 * each held ~5.2 s; the "z"s rise one by one every 2.6 s, then a pause.
 * Slow on purpose: each drawn frame costs CPU, and macOS draws are 3-4x
 * dearer than Windows (docs/perf.md, M7). */
const SLEEPING = [
  [0, [0], 2600],
  [0, [0, 1], 2600],
  [1, [0, 1, 2], 2600],
  [1, [1, 2], 2600],
  [0, [2], 2600],
  [0, [], 2600],
  [1, [], 5200],
];

const STATES = {
  idle: {
    durations: IDLE.map(([, , ms]) => ms),
    loop: true,
    frames: IDLE.map(([eyes, tip]) => (f) => drawSitting(f, { tip, eyes })),
  },
  sleeping: {
    durations: SLEEPING.map(([, , ms]) => ms),
    loop: true,
    frames: SLEEPING.map(([breath, zs]) => (f) => {
      drawSleeping(f, { breath });
      for (const k of zs) {
        const [g, x, y] = Z[k];
        f.fillOutlined(glyph(g, x, y), C.zblue);
      }
    }),
  },
  thinking: {
    fps: 4,
    loop: true,
    frames: [0, 1, 2, 3].map((i) => (f) => {
      drawSitting(f, { tip: i % 2 ? [44, 27] : [43, 28], eyes: "up" });
      for (let k = 0; k < i; k++) f.fillOutlined(glyph(GLYPH.dot, 35 + k * 4, 2), C.white);
    }),
  },
  // Short-lived states stay lively. thinking (during a sync) keeps plain fps;
  // alert hops quickly, then holds so a long-unanswered alert stays cheap.
  alert: {
    durations: [100, 100, 120, 100, 1400],
    loop: true,
    frames: [0, -1, -2, -1, 0].map((dy) => (f) => {
      drawSitting(f, { dy, tip: [44, 23 + dy], eyes: "wide", ears: "tall" });
      f.fillOutlined(glyph(GLYPH.bang, 40, 3), C.yellow);
    }),
  },
};

// ---------------------------------------------------------------------------
// Output.

function renderStrip(state) {
  const n = state.frames.length;
  const strip = new Uint8Array(W * n * H);
  const frames = state.frames.map((draw) => {
    const f = new Frame();
    draw(f);
    return f.px;
  });
  frames.forEach((px, i) => {
    for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) strip[y * W * n + i * W + x] = px[y * W + x];
  });
  return { strip, frames };
}

/** Frame durations in ms (format 2), or each frame's 1 / fps. */
function durationsOf(state) {
  return state.durations ?? state.frames.map(() => 1000 / state.fps);
}

/**
 * All frames, one row per state, scaled up, on a checkerboard so transparency
 * is visible. A yellow bar under each frame shows how long it is held: full
 * cell width = 5 s or more.
 */
function renderPreview(rendered) {
  const maxFrames = Math.max(...rendered.map((r) => r.frames.length));
  const gap = 4;
  const bar = 6;
  const cell = W * PREVIEW_SCALE;
  const rowH = cell + gap + bar;
  const pw = maxFrames * cell + (maxFrames + 1) * gap;
  const ph = rendered.length * rowH + (rendered.length + 1) * gap;
  const px = new Uint8Array(pw * ph).fill(C.outline);
  rendered.forEach((r, row) => {
    r.frames.forEach((frame, col) => {
      const ox = gap + col * (cell + gap);
      const oy = gap + row * (rowH + gap);
      for (let y = 0; y < cell; y++) {
        for (let x = 0; x < cell; x++) {
          const v = frame[Math.floor(y / PREVIEW_SCALE) * W + Math.floor(x / PREVIEW_SCALE)];
          const checker = (Math.floor(x / 24) + Math.floor(y / 24)) % 2 ? C.bgGrid : C.bg;
          px[(oy + y) * pw + ox + x] = v || checker;
        }
      }
      const len = Math.max(2, Math.round((Math.min(r.durations[col], 5000) / 5000) * cell));
      for (let y = 0; y < bar; y++) for (let x = 0; x < len; x++) px[(oy + cell + gap + y) * pw + ox + x] = C.yellow;
    });
  });
  return encodeIndexedPng(pw, ph, px, PALETTE);
}

function main() {
  mkdirSync(OUT_DIR, { recursive: true });
  const pack = {
    $schema: "../pack.schema.json",
    format: 2,
    name: "Gray Tabby (placeholder)",
    author: "AIchemist Desktop Buddy contributors (generated by tools/gen-sprites.mjs)",
    license: "MIT",
    frameSize: { width: W, height: H },
    states: {},
  };
  const rendered = [];
  for (const [name, state] of Object.entries(STATES)) {
    const { strip, frames } = renderStrip(state);
    const file = `${name}.png`;
    // The sprite strips do not need the preview-only palette entries.
    writeFileSync(join(OUT_DIR, file), encodeIndexedPng(W * frames.length, H, strip, PALETTE.slice(0, C.bg)));
    // fps is required by the schema; 0 for states timed by durations.
    if (state.durations && state.durations.length !== frames.length) throw new Error(`${name}: durations length != frames`);
    pack.states[name] = {
      file,
      frames: frames.length,
      fps: state.fps ?? 0,
      ...(state.durations && { durations: state.durations }),
      loop: state.loop,
    };
    rendered.push({ name, frames, durations: durationsOf(state) });
  }
  writeFileSync(join(OUT_DIR, "pack.json"), JSON.stringify(pack, null, 2) + "\n");
  writeFileSync(join(OUT_DIR, "preview.png"), renderPreview(rendered));
  console.log(`Wrote default sprite pack to ${OUT_DIR}`);
}

main();
