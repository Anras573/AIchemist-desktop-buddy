// Pure helpers for the settings form: types mirroring the Rust contract
// (src-tauri/src/domain/settings.rs, camelCase JSON) and parsing/mapping
// functions with no DOM access.

export type ScreenCorner = "topLeft" | "topRight" | "bottomLeft" | "bottomRight";

export interface ReminderSettings {
  leadTimesMinutes: number[];
  snoozeDurationsMinutes: number[];
  soundEnabled: boolean;
  skipAllDay: boolean;
  skipDeclined: boolean;
  syncIntervalMinutes: number;
}

export interface AppearanceSettings {
  catScale: number;
  corner: ScreenCorner;
  monitor: string | null;
  opacityPercent: number;
  spritePack: string;
}

export interface QuietHours {
  enabled: boolean;
  /** "HH:MM:SS" from Rust; "HH:MM" is accepted on input. */
  start: string;
  end: string;
}

export interface SystemSettings {
  launchAtLogin: boolean;
  quietHours: QuietHours;
  sleepAfterIdleMinutes: number;
  checkForUpdates: boolean;
}

export interface Settings {
  schemaVersion: number;
  accounts: unknown[];
  reminders: ReminderSettings;
  appearance: AppearanceSettings;
  system: SystemSettings;
}

export interface UpdateSettingsRequest {
  reminders: ReminderSettings;
  appearance: AppearanceSettings;
  system: SystemSettings;
}

export interface FieldError {
  field: string;
  message: string;
}

export interface MonitorInfo {
  name: string;
  primary: boolean;
  width: number;
  height: number;
}

/** Result of parsing one form value: the value, or a message for the field. */
export type Parsed<T> = { ok: true; value: T } | { ok: false; message: string };

/**
 * Parses a list of whole minutes such as "10, 5, 1" (commas and/or spaces).
 * Only the syntax is checked here; ranges, counts and duplicates are
 * validated by the core, which reports them per entry.
 */
export function parseMinutesList(text: string): Parsed<number[]> {
  const parts = text.split(/[\s,]+/).filter((p) => p !== "");
  if (parts.length === 0) return { ok: false, message: "enter at least one value" };
  const values: number[] = [];
  for (const part of parts) {
    if (!/^\d{1,4}$/.test(part)) {
      return { ok: false, message: `"${part}" is not a whole number of minutes` };
    }
    values.push(Number(part));
  }
  return { ok: true, value: values };
}

export function formatMinutesList(values: readonly number[]): string {
  return values.join(", ");
}

/** Parses a whole number from a number input. */
export function parseWholeNumber(text: string): Parsed<number> {
  const t = text.trim();
  if (!/^\d{1,6}$/.test(t)) return { ok: false, message: "enter a whole number" };
  return { ok: true, value: Number(t) };
}

/** "22:00:00" or "22:00" -> "22:00" for `<input type="time">`. */
export function timeForInput(time: string): string {
  return time.slice(0, 5);
}

/** "HH:MM" (or "HH:MM:SS") from a time input; empty is invalid. */
export function parseTime(text: string): Parsed<string> {
  if (!/^([01]\d|2[0-3]):[0-5]\d(:[0-5]\d)?$/.test(text)) {
    return { ok: false, message: "enter a time" };
  }
  return { ok: true, value: text.length === 5 ? `${text}:00` : text };
}

/**
 * Maps a core error path to the form field it belongs to and a message:
 * `reminders.leadTimesMinutes[1]` -> field `reminders.leadTimesMinutes`,
 * message "entry 2: ...". Paths with no matching field fall back to "".
 */
export function errorTarget(
  error: FieldError,
  knownFields: ReadonlySet<string>,
): { field: string; message: string } {
  const m = /^(.*)\[(\d+)\]$/.exec(error.field);
  const base = m ? m[1] : error.field;
  const message = m ? `entry ${Number(m[2]) + 1}: ${error.message}` : error.message;
  return { field: knownFields.has(base) ? base : "", message };
}

/** Groups field errors by form field (see {@link errorTarget}). */
export function groupErrors(
  errors: readonly FieldError[],
  knownFields: ReadonlySet<string>,
): Map<string, string[]> {
  const grouped = new Map<string, string[]>();
  for (const e of errors) {
    const { field, message } = errorTarget(e, knownFields);
    const list = grouped.get(field) ?? [];
    list.push(message);
    grouped.set(field, list);
  }
  return grouped;
}

/** Whether a rejected `invoke` carries the core's validation error. */
export function isValidationError(
  err: unknown,
): err is { kind: "validation"; errors: FieldError[] } {
  return (
    typeof err === "object" &&
    err !== null &&
    (err as { kind?: unknown }).kind === "validation" &&
    Array.isArray((err as { errors?: unknown }).errors)
  );
}

/** A short message for any other rejection (never shows raw objects). */
export function describeError(err: unknown): string {
  if (typeof err === "object" && err !== null && "message" in err) {
    return String((err as { message: unknown }).message);
  }
  return typeof err === "string" ? err : "unexpected error";
}

export const CORNERS: readonly { value: ScreenCorner; label: string }[] = [
  { value: "topLeft", label: "Top left" },
  { value: "topRight", label: "Top right" },
  { value: "bottomLeft", label: "Bottom left" },
  { value: "bottomRight", label: "Bottom right" },
];

/** Sprite packs offered in the selector. Only the bundled one for now. */
export const SPRITE_PACKS: readonly string[] = ["default"];

/** Options for the monitor selector: "" is the primary monitor (`null`). */
export function monitorOptions(
  monitors: readonly MonitorInfo[],
  selected: string | null,
): { value: string; label: string }[] {
  const options = [{ value: "", label: "Primary monitor" }];
  for (const m of monitors) {
    const primary = m.primary ? ", primary" : "";
    options.push({ value: m.name, label: `${m.name} (${m.width} x ${m.height}${primary})` });
  }
  if (selected !== null && !monitors.some((m) => m.name === selected)) {
    options.push({ value: selected, label: `${selected} (not connected)` });
  }
  return options;
}
