// Pure frame-timing logic for sprite animations (docs/sprite-pack.md,
// "Playback semantics"). No DOM, no imports: tested with `node --test`
// (tests/frontend/frames.test.mjs), which strips the types.

export interface Playback {
  /** Frames in the strip, >= 1. */
  frames: number;
  /** Frames per second; 0 means a static frame. */
  fps: number;
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

/** A state is static when `fps` is 0 or it has a single frame. */
export function isStatic(p: Playback): boolean {
  return !(p.fps > 0) || !(p.frames > 1);
}

/** Frame shown `elapsedMs` after entering the state. */
export function frameAt(p: Playback, elapsedMs: number): FrameAt {
  if (isStatic(p)) return { index: 0, nextInMs: null };
  const frames = Math.floor(p.frames);
  const frameMs = 1000 / p.fps;
  const elapsed = Number.isFinite(elapsedMs) && elapsedMs > 0 ? elapsedMs : 0;
  const step = Math.floor(elapsed / frameMs);
  if (!p.loop && step >= frames - 1) return { index: frames - 1, nextInMs: null };
  return {
    index: step % frames,
    nextInMs: (step + 1) * frameMs - elapsed,
  };
}
