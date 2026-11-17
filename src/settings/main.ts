// Settings window entry point (spec 8). Plain TypeScript, no framework: the
// form is one static page with ~16 fields, so a library would add bundle
// size and parse time for no real gain. Logic lives in ./form.ts.
//
// Apply-on-change: every `change` event (on blur for text fields, on release
// for sliders, immediately for checkboxes and selects) sends the whole form
// with `update_settings`. The core validates; field errors are shown next to
// their fields and nothing is saved until the form is valid.

import {
  CORNERS,
  SPRITE_PACKS,
  type MonitorInfo,
  type Parsed,
  type Settings,
  type UpdateSettingsRequest,
  describeError,
  formatMinutesList,
  groupErrors,
  isValidationError,
  monitorOptions,
  parseMinutesList,
  parseTime,
  parseWholeNumber,
  timeForInput,
} from "./form";

// Minimal IPC bridge, as in the buddy page: avoids @tauri-apps/api.
type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
const ipc = (window as unknown as { __TAURI_INTERNALS__?: { invoke: Invoke } })
  .__TAURI_INTERNALS__;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!ipc) return Promise.reject({ kind: "internal", message: "not running in the app" });
  return ipc.invoke(cmd, args) as Promise<T>;
}

const form = document.querySelector<HTMLFormElement>("#settings")!;
const status = document.querySelector<HTMLElement>("#status")!;

function el<T extends HTMLElement>(id: string): T {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element ${id}`);
  return found as T;
}

const input = (id: string) => el<HTMLInputElement>(id);
const select = (id: string) => el<HTMLSelectElement>(id);

/** Field paths that have an error slot in the page. */
const knownFields = new Set(
  Array.from(document.querySelectorAll<HTMLElement>("[data-error-for]"))
    .map((p) => p.dataset.errorFor ?? "")
    .filter((f) => f !== ""),
);

function fillSelect(sel: HTMLSelectElement, options: { value: string; label: string }[]): void {
  sel.replaceChildren(
    ...options.map((o) => {
      const opt = document.createElement("option");
      opt.value = o.value;
      opt.textContent = o.label;
      return opt;
    }),
  );
}

function showOutputs(): void {
  el("catScaleValue").textContent = `${input("appearance.catScale").value}x`;
  el("opacityValue").textContent = `${input("appearance.opacityPercent").value}%`;
}

function fill(s: Settings, monitors: MonitorInfo[]): void {
  const r = s.reminders;
  input("reminders.leadTimesMinutes").value = formatMinutesList(r.leadTimesMinutes);
  input("reminders.snoozeDurationsMinutes").value = formatMinutesList(r.snoozeDurationsMinutes);
  input("reminders.soundEnabled").checked = r.soundEnabled;
  input("reminders.skipAllDay").checked = r.skipAllDay;
  input("reminders.skipDeclined").checked = r.skipDeclined;
  input("reminders.syncIntervalMinutes").value = String(r.syncIntervalMinutes);

  const a = s.appearance;
  input("appearance.catScale").value = String(a.catScale);
  fillSelect(select("appearance.corner"), [...CORNERS]);
  select("appearance.corner").value = a.corner;
  fillSelect(select("appearance.monitor"), monitorOptions(monitors, a.monitor));
  select("appearance.monitor").value = a.monitor ?? "";
  input("appearance.opacityPercent").value = String(a.opacityPercent);
  const packs = SPRITE_PACKS.includes(a.spritePack) ? SPRITE_PACKS : [...SPRITE_PACKS, a.spritePack];
  fillSelect(
    select("appearance.spritePack"),
    packs.map((p) => ({ value: p, label: p === "default" ? "Default (gray tabby)" : p })),
  );
  select("appearance.spritePack").value = a.spritePack;

  const y = s.system;
  input("system.launchAtLogin").checked = y.launchAtLogin;
  input("system.quietHours.enabled").checked = y.quietHours.enabled;
  input("system.quietHours.start").value = timeForInput(y.quietHours.start);
  input("system.quietHours.end").value = timeForInput(y.quietHours.end);
  input("system.sleepAfterIdleMinutes").value = String(y.sleepAfterIdleMinutes);
  input("system.checkForUpdates").checked = y.checkForUpdates;
  showOutputs();
}

/** Reads the form. Syntax errors are collected per field path. */
function collect(errors: Map<string, string[]>): UpdateSettingsRequest | null {
  function take<T>(field: string, parsed: Parsed<T>, fallback: T): T {
    if (parsed.ok) return parsed.value;
    errors.set(field, [parsed.message]);
    return fallback;
  }
  const num = (id: string) => take(id, parseWholeNumber(input(id).value), 0);
  const list = (id: string) => take(id, parseMinutesList(input(id).value), []);
  const time = (id: string) => take(id, parseTime(input(id).value), "");

  const request: UpdateSettingsRequest = {
    reminders: {
      leadTimesMinutes: list("reminders.leadTimesMinutes"),
      snoozeDurationsMinutes: list("reminders.snoozeDurationsMinutes"),
      soundEnabled: input("reminders.soundEnabled").checked,
      skipAllDay: input("reminders.skipAllDay").checked,
      skipDeclined: input("reminders.skipDeclined").checked,
      syncIntervalMinutes: num("reminders.syncIntervalMinutes"),
    },
    appearance: {
      catScale: num("appearance.catScale"),
      corner: select("appearance.corner").value as UpdateSettingsRequest["appearance"]["corner"],
      monitor: select("appearance.monitor").value || null,
      opacityPercent: num("appearance.opacityPercent"),
      spritePack: select("appearance.spritePack").value,
    },
    system: {
      launchAtLogin: input("system.launchAtLogin").checked,
      quietHours: {
        enabled: input("system.quietHours.enabled").checked,
        start: time("system.quietHours.start"),
        end: time("system.quietHours.end"),
      },
      sleepAfterIdleMinutes: num("system.sleepAfterIdleMinutes"),
      checkForUpdates: input("system.checkForUpdates").checked,
    },
  };
  return errors.size === 0 ? request : null;
}

function showErrors(errors: Map<string, string[]>): void {
  for (const slot of document.querySelectorAll<HTMLElement>("[data-error-for]")) {
    const field = slot.dataset.errorFor ?? "";
    slot.textContent = (errors.get(field) ?? []).join("; ");
    const control = field ? document.getElementById(field) : null;
    if (control) {
      if (errors.has(field)) control.setAttribute("aria-invalid", "true");
      else control.removeAttribute("aria-invalid");
    }
  }
}

function setStatus(text: string, kind: "" | "ok" | "failed" = ""): void {
  status.textContent = text;
  status.className = kind;
}

/** Changes are applied one at a time, in order. */
let queue: Promise<void> = Promise.resolve();

async function apply(): Promise<void> {
  const errors = new Map<string, string[]>();
  const request = collect(errors);
  if (!request) {
    showErrors(errors);
    setStatus("Not saved: fix the marked fields", "failed");
    return;
  }
  setStatus("Saving…");
  try {
    await invoke<Settings>("update_settings", { request });
    showErrors(new Map());
    setStatus("Saved", "ok");
  } catch (err) {
    if (isValidationError(err)) {
      showErrors(groupErrors(err.errors, knownFields));
      setStatus("Not saved: fix the marked fields", "failed");
    } else {
      showErrors(new Map([["", [describeError(err)]]]));
      setStatus("Not saved", "failed");
    }
  }
}

async function load(): Promise<void> {
  try {
    const [settings, monitors] = await Promise.all([
      invoke<Settings>("get_settings"),
      invoke<MonitorInfo[]>("list_monitors"),
    ]);
    fill(settings, monitors);
    setStatus("");
    form.addEventListener("change", () => {
      queue = queue.then(apply);
    });
  } catch (err) {
    setStatus(`Cannot load settings: ${describeError(err)}`, "failed");
  }
}

form.addEventListener("submit", (e) => e.preventDefault());
form.addEventListener("input", showOutputs);
void load();
