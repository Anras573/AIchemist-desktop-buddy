// Pure helpers for the "Calendar accounts" section of the settings window:
// types mirroring the Rust contract (src-tauri/src/domain/ipc.rs and
// settings.rs, camelCase JSON) and request/label builders with no DOM
// access, so they run under `node --test`. No runtime imports on purpose.

export type AccountConnection =
  | { type: "graph"; clientId: string; tenant: string }
  | { type: "caldav"; serverUrl: string; username: string };

export type AccountType = AccountConnection["type"];

export interface AccountConfig {
  id: string;
  displayName: string;
  connection: AccountConnection;
  includedCalendars: string[];
}

export type SyncState = "neverSynced" | "syncing" | "ok" | "error";

export interface IpcError {
  kind: string;
  message?: string;
  errors?: { field: string; message: string }[];
}

export interface AccountStatus {
  account: AccountConfig;
  syncState: SyncState;
  /** RFC 3339 UTC. */
  lastSyncedAt: string | null;
  lastError: IpcError | null;
  hasSecret: boolean;
  signedInAs?: string | null;
}

export interface CalendarInfo {
  id: string;
  accountId: string;
  name: string;
  color: string | null;
}

export interface AddAccountRequest {
  displayName: string;
  connection: AccountConnection;
  secret: string | null;
}

export interface UpdateAccountRequest {
  accountId: string;
  displayName: string;
  connection: AccountConnection;
  includedCalendars: string[];
  secret: string | null;
}

/** Values of the add-account form. */
export interface AddFormValues {
  type: AccountType;
  displayName: string;
  serverUrl: string;
  username: string;
  password: string;
  clientId: string;
  tenant: string;
}

export const DEFAULT_TENANT = "common";

export const ACCOUNT_TYPES: readonly { value: AccountType; label: string }[] = [
  { value: "graph", label: "Microsoft 365 / Outlook" },
  { value: "caldav", label: "CalDAV (iCloud, Nextcloud…)" },
];

export function accountTypeLabel(connection: AccountConnection): string {
  return connection.type === "graph" ? "Microsoft 365 / Outlook" : "CalDAV";
}

/** The `add_account` request for the form. Text is sent as typed; the core trims and validates. */
export function buildAddRequest(v: AddFormValues): AddAccountRequest {
  if (v.type === "graph") {
    return {
      displayName: v.displayName,
      connection: { type: "graph", clientId: v.clientId, tenant: v.tenant.trim() || DEFAULT_TENANT },
      secret: null,
    };
  }
  return {
    displayName: v.displayName,
    connection: { type: "caldav", serverUrl: v.serverUrl, username: v.username },
    secret: v.password === "" ? null : v.password,
  };
}

/** An `update_account` request that keeps everything but the given changes. */
export function buildUpdateRequest(
  status: AccountStatus,
  changes: { includedCalendars?: string[]; secret?: string | null; displayName?: string } = {},
): UpdateAccountRequest {
  const a = status.account;
  return {
    accountId: a.id,
    displayName: changes.displayName ?? a.displayName,
    connection: a.connection,
    includedCalendars: changes.includedCalendars ?? a.includedCalendars,
    secret: changes.secret ?? null,
  };
}

/** `included` with `id` switched on or off, keeping order and no duplicates. */
export function toggleCalendar(included: readonly string[], id: string, on: boolean): string[] {
  const rest = included.filter((c) => c !== id);
  return on ? [...rest, id] : rest;
}

export interface CalendarChoice {
  id: string;
  name: string;
  color: string | null;
  checked: boolean;
  /** Included in settings but no longer offered by the server. */
  missing: boolean;
}

/** Checkbox list: the server's calendars, then included ones it no longer lists. */
export function calendarChoices(
  calendars: readonly CalendarInfo[],
  included: readonly string[],
): CalendarChoice[] {
  const choices: CalendarChoice[] = calendars.map((c) => ({
    id: c.id,
    name: c.name || c.id,
    color: c.color,
    checked: included.includes(c.id),
    missing: false,
  }));
  for (const id of included) {
    if (!calendars.some((c) => c.id === id)) {
      choices.push({ id, name: id, color: null, checked: true, missing: true });
    }
  }
  return choices;
}

const KIND_LABELS: Record<string, string> = {
  auth: "Authentication failed",
  network: "Network error",
  protocol: "Server error",
  notFound: "Not found",
  storage: "Storage error",
  keychain: "Keychain error",
  invalidInput: "Invalid input",
  validation: "Invalid input",
  internal: "Internal error",
};

/** "Network error: could not connect" for any rejected command. */
export function describeAccountError(err: unknown): string {
  if (typeof err === "object" && err !== null) {
    const e = err as IpcError;
    const label = typeof e.kind === "string" ? (KIND_LABELS[e.kind] ?? "Error") : "Error";
    if (Array.isArray(e.errors) && e.errors.length > 0) {
      return `${label}: ${e.errors.map((f) => f.message).join("; ")}`;
    }
    if (typeof e.message === "string" && e.message !== "") return `${label}: ${e.message}`;
    return label;
  }
  return typeof err === "string" && err !== "" ? err : "Unexpected error";
}

/** Add-form field paths that have an error slot. */
export const ADD_FORM_FIELDS: readonly string[] = [
  "displayName",
  "connection.serverUrl",
  "connection.username",
  "connection.clientId",
  "connection.tenant",
  "secret",
];

/**
 * Groups a rejected `add_account` by add-form field. Anything else (another
 * error kind, an unknown path) goes under "" with {@link describeAccountError}.
 */
export function addFormErrors(err: unknown): Map<string, string[]> {
  const grouped = new Map<string, string[]>();
  const push = (field: string, message: string) => {
    grouped.set(field, [...(grouped.get(field) ?? []), message]);
  };
  const e = err as IpcError | null;
  if (e && e.kind === "validation" && Array.isArray(e.errors)) {
    for (const f of e.errors) {
      if (ADD_FORM_FIELDS.includes(f.field)) push(f.field, f.message);
      else push("", `${f.field}: ${f.message}`);
    }
  } else {
    push("", describeAccountError(err));
  }
  return grouped;
}

/** Whether a Graph account still needs its first (or a new) sign-in. */
export function needsSignIn(status: AccountStatus): boolean {
  return status.account.connection.type === "graph" && !status.hasSecret;
}

/** Second line of an account row: who it connects as. */
export function accountDetail(status: AccountStatus): string {
  const c = status.account.connection;
  if (c.type === "caldav") {
    const pw = status.hasSecret ? "" : " · no password saved";
    return `CalDAV · ${c.username}${pw}`;
  }
  if (!status.hasSecret) return "Microsoft 365 / Outlook · not signed in";
  return status.signedInAs
    ? `Microsoft 365 / Outlook · signed in as ${status.signedInAs}`
    : "Microsoft 365 / Outlook · signed in";
}

/** "5 min ago", "2 h ago", "3 days ago", or "just now". */
export function relativeTime(iso: string, now: Date): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const minutes = Math.floor((now.getTime() - then) / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  return days === 1 ? "1 day ago" : `${days} days ago`;
}

/** Status line of an account row. */
export function syncStatusText(status: AccountStatus, now: Date): string {
  const last = status.lastSyncedAt ? `last synced ${relativeTime(status.lastSyncedAt, now)}` : "";
  switch (status.syncState) {
    case "syncing":
      return "Syncing…";
    case "ok":
      return last ? `Synced, ${last}` : "Synced";
    case "error":
      return last ? `Sync failed (${last})` : "Sync failed";
    default:
      if (status.account.includedCalendars.length === 0) {
        return "Not synced yet: choose calendars to include";
      }
      return "Not synced yet";
  }
}
