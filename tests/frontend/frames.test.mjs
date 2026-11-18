// Unit tests for src/buddy/frames.ts. Run with `npm test` (Node's built-in
// test runner; Node >= 22.18 strips the TypeScript types natively).
import assert from "node:assert/strict";
import { test } from "node:test";

import { frameAt, isStatic } from "../../src/buddy/frames.ts";

const idle = { frames: 8, fps: 5, loop: true };

test("static states never advance", () => {
  for (const p of [
    { frames: 4, fps: 0, loop: true },
    { frames: 1, fps: 8, loop: true },
    { frames: 1, fps: 0, loop: false },
  ]) {
    assert.equal(isStatic(p), true);
    assert.deepEqual(frameAt(p, 0), { index: 0, nextInMs: null });
    assert.deepEqual(frameAt(p, 12345), { index: 0, nextInMs: null });
  }
  assert.equal(isStatic(idle), false);
});

test("advances by elapsed time at the state's fps", () => {
  assert.deepEqual(frameAt(idle, 0), { index: 0, nextInMs: 200 });
  assert.deepEqual(frameAt(idle, 199), { index: 0, nextInMs: 1 });
  assert.deepEqual(frameAt(idle, 200), { index: 1, nextInMs: 200 });
  assert.deepEqual(frameAt(idle, 1450), { index: 7, nextInMs: 150 });
});

test("looping wraps to frame 0", () => {
  assert.equal(frameAt(idle, 1600).index, 0);
  assert.equal(frameAt(idle, 1600 * 3 + 400).index, 2);
});

test("one-shot holds the last frame and stops", () => {
  const once = { frames: 4, fps: 4, loop: false };
  assert.deepEqual(frameAt(once, 0), { index: 0, nextInMs: 250 });
  assert.deepEqual(frameAt(once, 500), { index: 2, nextInMs: 250 });
  assert.deepEqual(frameAt(once, 750), { index: 3, nextInMs: null });
  assert.deepEqual(frameAt(once, 60_000), { index: 3, nextInMs: null });
});

test("bad elapsed values count as zero", () => {
  assert.equal(frameAt(idle, -50).index, 0);
  assert.equal(frameAt(idle, Number.NaN).index, 0);
});

// ----- Format 2: per-frame durations -----

// Hold, blink, hold, quick swish.
const holds = { frames: 5, fps: 0, durations: [4000, 150, 3000, 100, 100], loop: true };

test("durations make a state animated even with fps 0", () => {
  assert.equal(isStatic(holds), false);
  assert.equal(isStatic({ frames: 1, fps: 0, durations: [500], loop: true }), true);
});

test("varying durations pick the right frame and wait until the next change", () => {
  assert.deepEqual(frameAt(holds, 0), { index: 0, nextInMs: 4000 });
  assert.deepEqual(frameAt(holds, 3999), { index: 0, nextInMs: 1 });
  assert.deepEqual(frameAt(holds, 4000), { index: 1, nextInMs: 150 });
  assert.deepEqual(frameAt(holds, 4100), { index: 1, nextInMs: 50 });
  assert.deepEqual(frameAt(holds, 4150), { index: 2, nextInMs: 3000 });
  assert.deepEqual(frameAt(holds, 7150), { index: 3, nextInMs: 100 });
  assert.deepEqual(frameAt(holds, 7300), { index: 4, nextInMs: 50 });
});

test("durations loop wraps to frame 0 after the sum of all durations", () => {
  const total = 4000 + 150 + 3000 + 100 + 100;
  assert.deepEqual(frameAt(holds, total), { index: 0, nextInMs: 4000 });
  assert.deepEqual(frameAt(holds, total * 5 + 4050), { index: 1, nextInMs: 100 });
  assert.deepEqual(frameAt(holds, total - 1), { index: 4, nextInMs: 1 });
});

test("durations one-shot holds the last frame and stops", () => {
  const once = { frames: 3, fps: 0, durations: [100, 2000, 300], loop: false };
  assert.deepEqual(frameAt(once, 50), { index: 0, nextInMs: 50 });
  assert.deepEqual(frameAt(once, 100), { index: 1, nextInMs: 2000 });
  assert.deepEqual(frameAt(once, 2099), { index: 1, nextInMs: 1 });
  // The last frame's own duration does not matter: it is held.
  assert.deepEqual(frameAt(once, 2100), { index: 2, nextInMs: null });
  assert.deepEqual(frameAt(once, 1e9), { index: 2, nextInMs: null });
});

test("durations override fps", () => {
  const p = { frames: 2, fps: 12, durations: [1000, 500], loop: true };
  assert.deepEqual(frameAt(p, 900), { index: 0, nextInMs: 100 });
  assert.deepEqual(frameAt(p, 1200), { index: 1, nextInMs: 300 });
});

test("a pack can mix fps states and durations states", () => {
  const pack = {
    idle: holds,
    thinking: { frames: 4, fps: 4, loop: true },
    alert: { frames: 3, fps: 8, durations: [100, 100, 1500], loop: true },
  };
  assert.deepEqual(frameAt(pack.idle, 4000), { index: 1, nextInMs: 150 });
  assert.deepEqual(frameAt(pack.thinking, 600), { index: 2, nextInMs: 150 });
  assert.deepEqual(frameAt(pack.alert, 250), { index: 2, nextInMs: 1450 });
  assert.deepEqual(frameAt(pack.alert, 1700), { index: 0, nextInMs: 100 });
});

test("format 1 states (no durations) keep their fps timing", () => {
  const v1 = { frames: 4, fps: 3, loop: true };
  assert.equal(frameAt(v1, 0).index, 0);
  assert.equal(frameAt(v1, 334).index, 1);
  assert.equal(frameAt(v1, 1334).index, 0);
  assert.ok(Math.abs(frameAt(v1, 0).nextInMs - 1000 / 3) < 1e-9);
});

test("durations of the wrong length fall back to fps", () => {
  const bad = { frames: 3, fps: 5, durations: [1000, 1000], loop: true };
  assert.deepEqual(frameAt(bad, 250), { index: 1, nextInMs: 150 });
  assert.equal(isStatic({ frames: 3, fps: 0, durations: [1000], loop: true }), true);
});
