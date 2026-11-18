// Pure frame-timing logic for sprite animations (docs/sprite-pack.md,
// "Playback semantics"). No DOM, no imports: tested with `node --test`
// (tests/frontend/frames.test.mjs), which strips the types.

export interface Playback {
  /** Frames in the strip, >= 1. */
  frames: number;
  /** Frames per second; 0 means a static frame. Ignored when `durations` is used. */
  fps: number;
  /**
   * Pack format 2: milliseconds each frame is shown, one entry per frame.
   * When present (and the right length) it replaces `fps`.
   */
  durations?: readonly number[];
  /** Repeat forever, or play once and hold the last frame. */
  loop: boolean;
}

export interface FrameAt {
  /** Frame to draw. */
  index: number;
  /**
   * Milliseconds until the next frame change, or `null` when the frame
   * never changes again (static state, or a one-shot animation that reached
   * its last frame). `null` means: stop the render loop.
   */
  nextInMs: number | null;
}

/** Per-frame durations to use, or null to fall back to `fps`. */
function frameDurations(p: Playback): readonly number[] | null {
  const d = p.durations;
  return Array.isArray(d) && d.length === Math.floor(p.frames) ? d : null;
}

/**
 * A state is static when it has a single frame, or when it has no
 * `durations` and `fps` is 0.
 */
export function isStatic(p: Playback): boolean {
  if (!(p.frames > 1)) return true;
  return frameDurations(p) === null && !(p.fps > 0);
}

/** Frame shown `elapsedMs` after entering the state. */
export function frameAt(p: Playback, elapsedMs: number): FrameAt {
  if (isStatic(p)) return { index: 0, nextInMs: null };
  const frames = Math.floor(p.frames);
  const elapsed = Number.isFinite(elapsedMs) && elapsedMs > 0 ? elapsedMs : 0;
  const given = frameDurations(p);
  // Every frame lasts at least 1 ms so time always moves forward.
  const durations = given
    ? given.map((d) => (Number.isFinite(d) && d >= 1 ? d : 1))
    : new Array<number>(frames).fill(1000 / p.fps);

  // A one-shot animation holds its last frame once the others have played.
  const total = durations.reduce((a, b) => a + b, 0);
  const toLast = total - durations[frames - 1];
  if (!p.loop && elapsed >= toLast) return { index: frames - 1, nextInMs: null };

  let t = p.loop ? elapsed % total : elapsed;
  for (let i = 0; i < frames; i++) {
    if (t < durations[i]) return { index: i, nextInMs: durations[i] - t };
    t -= durations[i];
  }
  // Only reachable through floating-point rounding at the very end of a loop.
  return { index: 0, nextInMs: durations[0] };
}
