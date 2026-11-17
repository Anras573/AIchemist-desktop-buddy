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
