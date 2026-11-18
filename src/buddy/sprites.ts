// The bundled default sprite pack (docs/sprite-pack.md). `pack.json` and the
// state PNGs are bundled by Vite; the small PNGs are inlined as data: URLs
// (allowed by the CSP `img-src 'self' data:`). `preview.png` is excluded.

import pack from "../../assets/sprites/default/pack.json";
import type { BuddyState } from "./ipc";
import type { Playback } from "./frames";

const urls = import.meta.glob<string>(
  ["../../assets/sprites/default/*.png", "!../../assets/sprites/default/preview.png"],
  { query: "?url", import: "default", eager: true },
);

export interface StateSprite extends Playback {
  image: HTMLImageElement;
}

export interface SpritePack {
  frameWidth: number;
  frameHeight: number;
  states: Record<BuddyState, StateSprite>;
}

function loadImage(src: string): Promise<HTMLImageElement> {
  const img = new Image();
  img.src = src;
  return img.decode().then(() => img);
}

/** Loads and decodes every state strip of the default pack. */
export async function loadDefaultPack(): Promise<SpritePack> {
  const names: BuddyState[] = ["idle", "sleeping", "thinking", "alert"];
  const entries = await Promise.all(
    names.map(async (name) => {
      const s = pack.states[name];
      const url = urls[`../../assets/sprites/default/${s.file}`];
      if (url === undefined) throw new Error(`sprite file missing: ${s.file}`);
      const sprite: StateSprite = {
        image: await loadImage(url),
        frames: s.frames,
        fps: s.fps,
        // Format 2 (optional): per-frame durations replace fps.
        durations: "durations" in s ? s.durations : undefined,
        loop: s.loop,
      };
      return [name, sprite] as const;
    }),
  );
  return {
    frameWidth: pack.frameSize.width,
    frameHeight: pack.frameSize.height,
    states: Object.fromEntries(entries) as Record<BuddyState, StateSprite>,
  };
}
