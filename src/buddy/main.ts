// Buddy window entry point.
//
// Click-through spike (task 0.5): the page reports the bounding boxes of the
// cat and the speech bubble to Rust, which makes the rest of the window
// click-through. Left-click on the cat toggles the bubble; right-click and
// hover change the cat's colour, to show they reach the page.

interface HitRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

// Minimal IPC bridge. Avoids pulling in @tauri-apps/api for one call; the real
// frontend can switch to `invoke` from that package.
declare global {
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown>;
    };
  }
}

const cat = document.querySelector<HTMLDivElement>("#cat");
const bubble = document.querySelector<HTMLDivElement>("#bubble");

function hitRegions(): HitRect[] {
  return [cat, bubble]
    .filter((el): el is HTMLDivElement => el !== null && !el.hidden)
    .map((el) => {
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, width: r.width, height: r.height };
    });
}

function reportHitRegions(): void {
  const ipc = window.__TAURI_INTERNALS__;
  if (!ipc) return; // Plain browser (vite dev without Tauri).
  ipc.invoke("set_hit_regions", { regions: hitRegions() }).catch((err: unknown) => {
    console.error("set_hit_regions failed", err);
  });
}

if (cat && bubble) {
  cat.addEventListener("mouseenter", () => cat.classList.add("hover"));
  cat.addEventListener("mouseleave", () => cat.classList.remove("hover"));
  cat.addEventListener("click", () => {
    bubble.hidden = !bubble.hidden;
    reportHitRegions();
  });
  cat.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    cat.classList.toggle("menu");
  });
}

window.addEventListener("resize", reportHitRegions);
// The window starts hidden and is shown once Rust has placed it; report
// again when it becomes visible in case layout was not final before.
document.addEventListener("visibilitychange", reportHitRegions);
reportHitRegions();

export {};
