// The reminder chime (task 2.2). Rust decides when to play it (a new bubble,
// sound enabled) and calls `window.__aichemistPlayChime()` through
// `WebviewWindow::eval` (src-tauri/src/reminders/mod.rs, CHIME_SCRIPT).
// One Audio element, created on first use and reused. The WAV is bundled by
// Vite as a file (too big to inline), so the CSP's default-src 'self'
// covers it. WebView2 runs with --autoplay-policy=no-user-gesture-required.

import chimeUrl from "../../assets/sounds/chime.wav";

declare global {
  interface Window {
    __aichemistPlayChime?: () => void;
  }
}

let audio: HTMLAudioElement | null = null;

export function playChime(): void {
  audio ??= new Audio(chimeUrl);
  audio.currentTime = 0;
  audio.play().catch((err: unknown) => console.error("chime failed", err));
}

window.__aichemistPlayChime = playChime;
