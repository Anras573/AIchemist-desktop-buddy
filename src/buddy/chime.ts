// The reminder chime (task 2.2). Rust decides when to play it (a new bubble,
// sound enabled) and emits the app-internal `play-chime` event to the buddy
// window (src-tauri/src/reminders/mod.rs, CHIME_EVENT). An event rather than
// a script evaluated from Rust (task 4.4): the page exposes no global
// function and needs no permission beyond `core:event:allow-listen`.
// One Audio element, created on first use and reused. The WAV is bundled by
// Vite as a file (too big to inline), so the CSP's default-src 'self'
// covers it. WebView2 runs with --autoplay-policy=no-user-gesture-required.

import chimeUrl from "../../assets/sounds/chime.wav";
import { listen } from "./ipc";

/** `CHIME_EVENT` in src-tauri/src/reminders/mod.rs. */
const CHIME_EVENT = "play-chime";

let audio: HTMLAudioElement | null = null;

export function playChime(): void {
  audio ??= new Audio(chimeUrl);
  audio.currentTime = 0;
  audio.play().catch((err: unknown) => console.error("chime failed", err));
}

listen<null>(CHIME_EVENT, playChime).catch((err: unknown) =>
  console.error(`listen ${CHIME_EVENT} failed`, err),
);
