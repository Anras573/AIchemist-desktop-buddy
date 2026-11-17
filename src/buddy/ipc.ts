// Minimal IPC bridge over `window.__TAURI_INTERNALS__` (what @tauri-apps/api
// wraps). The buddy page needs only `invoke` and `listen`; this keeps the
// page dependency-free. In a plain browser (vite dev without Tauri) calls
// are no-ops.

export interface HitRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** `BuddyState` (src-tauri/src/domain/animation.rs). */
export type BuddyState = "idle" | "sleeping" | "thinking" | "alert";

/** `ScreenCorner` (src-tauri/src/domain/settings.rs). */
export type ScreenCorner = "topLeft" | "topRight" | "bottomLeft" | "bottomRight";

/** `AppearanceSettings` (src-tauri/src/domain/settings.rs). */
export interface Appearance {
  catScale: number;
  corner: ScreenCorner;
  monitor: string | null;
  opacityPercent: number;
  spritePack: string;
}

interface TauriEvent<T> {
  event: string;
  id: number;
  payload: T;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown>;
      transformCallback(callback: (response: never) => void, once?: boolean): number;
    };
  }
}

const tauri = window.__TAURI_INTERNALS__;

/** Whether the page runs inside Tauri. */
export const inTauri = tauri !== undefined;

export function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!tauri) return Promise.reject(new Error("not running in Tauri"));
  return tauri.invoke(cmd, args) as Promise<T>;
}

/**
 * Listens to an app event for the lifetime of the page (never unlistened,
 * so the capability only needs `core:event:allow-listen`).
 */
export function listen<T>(event: string, handler: (payload: T) => void): Promise<void> {
  if (!tauri) return Promise.resolve();
  const callback = tauri.transformCallback((e: TauriEvent<T>) => handler(e.payload));
  return tauri
    .invoke("plugin:event|listen", { event, target: { kind: "Any" }, handler: callback })
    .then(() => undefined);
}

function logError(cmd: string): (err: unknown) => void {
  return (err) => console.error(`${cmd} failed`, err);
}

export function setHitRegions(regions: HitRect[]): void {
  if (inTauri) invoke("set_hit_regions", { regions }).catch(logError("set_hit_regions"));
}

export function reportInteraction(): void {
  if (inTauri) invoke("report_interaction").catch(logError("report_interaction"));
}
