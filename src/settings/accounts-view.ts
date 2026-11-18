// "Calendar accounts" section of the settings window (spec 8): the account
// rows, the add-account form and per-account actions. Logic that does not
// touch the DOM lives in ./accounts.ts (unit tested).
//
// The list is rendered from `list_accounts` plus per-row UI state, and
// re-rendered after every command and on `accounts-changed`. No dialogs:
// removal is confirmed inline (window.confirm/alert are never used).

import {
  ACCOUNT_TYPES,
  type AccountStatus,
  type AccountType,
  type AddFormValues,
  type CalendarInfo,
  accountDetail,
  addFormErrors,
  buildAddRequest,
  buildUpdateRequest,
  calendarChoices,
  describeAccountError,
  needsSignIn,
  syncStatusText,
  toggleCalendar,
} from "./accounts";

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
interface Internals {
  invoke: Invoke;
  transformCallback(callback: (response: never) => void, once?: boolean): number;
}
const ipc = (window as unknown as { __TAURI_INTERNALS__?: Internals }).__TAURI_INTERNALS__;

function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!ipc) return Promise.reject({ kind: "internal", message: "not running in the app" });
  return ipc.invoke(cmd, args) as Promise<T>;
}

/** Listens for the page's lifetime (the capability grants only `listen`). */
function listen(event: string, handler: () => void): void {
  if (!ipc) return;
  const callback = ipc.transformCallback(() => handler());
  void ipc
    .invoke("plugin:event|listen", { event, target: { kind: "Any" }, handler: callback })
    .catch((err) => console.error(`listen ${event} failed`, err));
}

type Panel = "calendars" | "password" | "remove";

interface RowState {
  busy?: string;
  message?: { text: string; ok: boolean };
  panel?: Panel;
  calendars?: CalendarInfo[];
  calendarsError?: string;
  passwordDraft?: string;
}

let accounts: AccountStatus[] = [];
const rows = new Map<string, RowState>();
let loadError = "";

function rowState(id: string): RowState {
  let s = rows.get(id);
  if (!s) {
    s = {};
    rows.set(id, s);
  }
  return s;
}

// ----- tiny DOM helpers -----

function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<{ className: string; text: string; type: string; role: string }> = {},
  ...children: (Node | null)[]
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (props.className) node.className = props.className;
  if (props.text !== undefined) node.textContent = props.text;
  if (props.type) node.setAttribute("type", props.type);
  if (props.role) node.setAttribute("role", props.role);
  for (const c of children) if (c) node.append(c);
  return node;
}

function button(text: string, onClick: () => void, disabled = false): HTMLButtonElement {
  const b = h("button", { type: "button", text });
  b.disabled = disabled;
  b.addEventListener("click", onClick);
  return b;
}

const $ = <T extends HTMLElement>(id: string): T => {
  const found = document.getElementById(id);
  if (!found) throw new Error(`missing element ${id}`);
  return found as T;
};

// ----- loading and rendering -----

async function reload(): Promise<void> {
  try {
    accounts = await invoke<AccountStatus[]>("list_accounts");
    loadError = "";
    for (const id of rows.keys()) {
      if (!accounts.some((a) => a.account.id === id)) rows.delete(id);
    }
  } catch (err) {
    loadError = `Cannot load accounts: ${describeAccountError(err)}`;
  }
  render();
}

function render(): void {
  $("accounts-error").textContent = loadError;
  $("no-accounts").hidden = accounts.length > 0 || loadError !== "";
  $("account-list").replaceChildren(...accounts.map(renderRow));
}

function renderRow(status: AccountStatus): HTMLLIElement {
  const a = status.account;
  const s = rowState(a.id);
  const busy = s.busy !== undefined;

  const badge = h("span", {
    className: `badge state-${status.syncState}`,
    text: { neverSynced: "Not synced", syncing: "Syncing", ok: "OK", error: "Problem" }[
      status.syncState
    ],
  });
  const li = h(
    "li",
    { className: "account" },
    h("div", { className: "account-head" }, h("strong", { text: a.displayName }), badge),
    h("p", { className: "hint", text: accountDetail(status) }),
    h("p", { className: "hint", text: syncStatusText(status, new Date()) }),
    status.lastError && status.syncState === "error"
      ? h("p", { className: "error", text: describeAccountError(status.lastError) })
      : null,
    needsSignIn(status) && !busy
      ? h("p", { className: "error", text: "Select Sign in to connect this account." })
      : null,
  );

  const actions = h("div", { className: "actions" });
  if (a.connection.type === "graph") {
    actions.append(button(status.hasSecret ? "Sign in again" : "Sign in", () => signIn(a.id), busy));
  }
  actions.append(
    button("Test connection", () => testConnection(a.id), busy),
    button("Calendars…", () => togglePanel(a.id, "calendars"), busy),
  );
  if (a.connection.type === "caldav") {
    actions.append(button("Change password…", () => togglePanel(a.id, "password"), busy));
  }
  actions.append(button("Remove…", () => togglePanel(a.id, "remove"), busy));
  li.append(actions);

  const message = s.busy ?? s.message?.text ?? "";
  li.append(
    h("p", {
      className: `message ${s.busy ? "" : s.message?.ok ? "ok" : "failed"}`,
      text: message,
      role: "status",
    }),
  );

  if (s.panel === "calendars") li.append(renderCalendars(status, s));
  if (s.panel === "password") li.append(renderPassword(status, s));
  if (s.panel === "remove") li.append(renderRemove(status));
  return li;
}

function renderCalendars(status: AccountStatus, s: RowState): HTMLElement {
  const panel = h("div", { className: "panel" }, h("p", { className: "label", text: "Include calendars" }));
  if (s.calendarsError) {
    panel.append(h("p", { className: "error", text: s.calendarsError }));
    return panel;
  }
  if (!s.calendars) {
    panel.append(h("p", { className: "hint", text: "Loading calendars…" }));
    return panel;
  }
  const choices = calendarChoices(s.calendars, status.account.includedCalendars);
  if (choices.length === 0) {
    panel.append(h("p", { className: "hint", text: "This account has no calendars with events." }));
  }
  for (const c of choices) {
    const box = h("input", { type: "checkbox" });
    box.checked = c.checked;
    box.disabled = s.busy !== undefined;
    box.addEventListener("change", () => setIncluded(status, c.id, box.checked));
    const swatch = h("span", { className: "swatch" });
    if (c.color && /^#[0-9a-fA-F]{6}$/.test(c.color)) swatch.style.background = c.color;
    const label = h("label", { className: "check" }, box, swatch, document.createTextNode(c.name));
    if (c.missing) label.append(h("span", { className: "hint", text: "(no longer on the server)" }));
    panel.append(label);
  }
  return panel;
}

function renderPassword(status: AccountStatus, s: RowState): HTMLElement {
  const id = `pw-${status.account.id}`;
  const input = h("input", { type: "password" });
  input.id = id;
  input.autocomplete = "new-password";
  input.value = s.passwordDraft ?? "";
  input.addEventListener("input", () => {
    s.passwordDraft = input.value;
  });
  const label = h("label", { text: "New password or app password" });
  label.htmlFor = id;
  return h(
    "div",
    { className: "panel" },
    label,
    input,
    h("div", { className: "actions" },
      button("Save password", () => savePassword(status), s.busy !== undefined),
      button("Cancel", () => togglePanel(status.account.id, "password")),
    ),
  );
}

function renderRemove(status: AccountStatus): HTMLElement {
  const what =
    status.account.connection.type === "graph" ? "its Microsoft sign-in" : "its saved password";
  return h(
    "div",
    { className: "panel confirm" },
    h("p", {
      text: `Remove “${status.account.displayName}”? This also deletes ${what} from the keychain and its cached events.`,
    }),
    h("div", { className: "actions" },
      button("Remove", () => removeAccount(status.account.id), rowState(status.account.id).busy !== undefined),
      button("Cancel", () => togglePanel(status.account.id, "remove")),
    ),
  );
}

// ----- actions -----

async function withBusy(id: string, text: string, work: () => Promise<string | null>): Promise<void> {
  const s = rowState(id);
  s.busy = text;
  s.message = undefined;
  render();
  try {
    const done = await work();
    s.message = done ? { text: done, ok: true } : undefined;
  } catch (err) {
    s.message = { text: describeAccountError(err), ok: false };
  } finally {
    s.busy = undefined;
  }
  await reload();
}

function togglePanel(id: string, panel: Panel): void {
  const s = rowState(id);
  s.panel = s.panel === panel ? undefined : panel;
  if (s.panel === "calendars") void loadCalendars(id);
  if (s.panel !== "password") s.passwordDraft = undefined;
  render();
}

async function loadCalendars(id: string): Promise<void> {
  const s = rowState(id);
  s.calendars = undefined;
  s.calendarsError = undefined;
  render();
  try {
    s.calendars = await invoke<CalendarInfo[]>("list_account_calendars", { request: { accountId: id } });
  } catch (err) {
    s.calendarsError = `Cannot list calendars: ${describeAccountError(err)}`;
  }
  render();
}

function setIncluded(status: AccountStatus, calendarId: string, on: boolean): void {
  const includedCalendars = toggleCalendar(status.account.includedCalendars, calendarId, on);
  void withBusy(status.account.id, "Saving…", async () => {
    await invoke("update_account", { request: buildUpdateRequest(status, { includedCalendars }) });
    return "Calendars saved";
  });
}

function testConnection(id: string): void {
  void withBusy(id, "Testing the connection…", async () => {
    await invoke("test_connection", { request: { accountId: id } });
    return "Connection OK";
  });
}

function signIn(id: string): void {
  void withBusy(id, "Waiting for the sign-in in your browser… (closing Settings cancels it)", async () => {
    await invoke("sign_in_account", { request: { accountId: id } });
    return "Signed in";
  });
}

function savePassword(status: AccountStatus): void {
  const s = rowState(status.account.id);
  const secret = s.passwordDraft ?? "";
  if (secret === "") {
    s.message = { text: "Enter the new password", ok: false };
    render();
    return;
  }
  void withBusy(status.account.id, "Saving…", async () => {
    await invoke("update_account", { request: buildUpdateRequest(status, { secret }) });
    s.panel = undefined;
    s.passwordDraft = undefined;
    return "Password saved";
  });
}

function removeAccount(id: string): void {
  void withBusy(id, "Removing…", async () => {
    await invoke("remove_account", { request: { accountId: id } });
    rows.delete(id);
    return null;
  });
}

// ----- add form -----

const addFieldIds: Record<string, string> = {
  displayName: "add.displayName",
  "connection.serverUrl": "add.serverUrl",
  "connection.username": "add.username",
  "connection.clientId": "add.clientId",
  "connection.tenant": "add.tenant",
  secret: "add.password",
};

function addType(): AccountType {
  const checked = document.querySelector<HTMLInputElement>('input[name="add.type"]:checked');
  return (checked?.value as AccountType) ?? "graph";
}

function showAddType(): void {
  const type = addType();
  $("add-caldav").hidden = type !== "caldav";
  $("add-graph").hidden = type !== "graph";
  $<HTMLButtonElement>("add-submit").textContent =
    type === "graph" ? "Add and sign in" : "Add account";
}

function showAddErrors(errors: Map<string, string[]>): void {
  for (const slot of document.querySelectorAll<HTMLElement>("[data-add-error]")) {
    const field = slot.dataset.addError ?? "";
    slot.textContent = (errors.get(field) ?? []).join("; ");
    const control = addFieldIds[field] ? document.getElementById(addFieldIds[field]) : null;
    if (control) {
      if (errors.has(field)) control.setAttribute("aria-invalid", "true");
      else control.removeAttribute("aria-invalid");
    }
  }
}

function readAddForm(): AddFormValues {
  const v = (id: string) => $<HTMLInputElement>(id).value;
  return {
    type: addType(),
    displayName: v("add.displayName"),
    serverUrl: v("add.serverUrl"),
    username: v("add.username"),
    password: v("add.password"),
    clientId: v("add.clientId"),
    tenant: v("add.tenant"),
  };
}

function resetAddForm(): void {
  for (const id of ["add.displayName", "add.serverUrl", "add.username", "add.password", "add.clientId"]) {
    $<HTMLInputElement>(id).value = "";
  }
  $<HTMLInputElement>("add.tenant").value = "common";
  showAddErrors(new Map());
}

function setAddOpen(open: boolean): void {
  $("add-panel").hidden = !open;
  $("add-open").hidden = open;
  if (open) {
    showAddType();
    $<HTMLInputElement>("add.displayName").focus();
  } else {
    resetAddForm();
  }
}

let adding = false;

async function submitAdd(): Promise<void> {
  if (adding) return;
  adding = true;
  const submit = $<HTMLButtonElement>("add-submit");
  submit.disabled = true;
  const values = readAddForm();
  try {
    const status = await invoke<AccountStatus>("add_account", { request: buildAddRequest(values) });
    setAddOpen(false);
    await reload();
    if (values.type === "graph") signIn(status.account.id);
    else togglePanel(status.account.id, "calendars");
  } catch (err) {
    showAddErrors(addFormErrors(err));
  } finally {
    adding = false;
    submit.disabled = false;
  }
}

/** Wires the section and loads the accounts. */
export function initAccounts(): void {
  const typeChoices = $("add-types");
  typeChoices.replaceChildren(
    ...ACCOUNT_TYPES.map((t, i) => {
      const radio = h("input", { type: "radio" });
      radio.name = "add.type";
      radio.value = t.value;
      radio.checked = i === 0;
      radio.addEventListener("change", showAddType);
      return h("label", { className: "check" }, radio, document.createTextNode(t.label));
    }),
  );
  $("add-open").addEventListener("click", () => setAddOpen(true));
  $("add-cancel").addEventListener("click", () => setAddOpen(false));
  const form = $<HTMLFormElement>("accounts");
  form.addEventListener("submit", (e) => {
    e.preventDefault();
    void submitAdd();
  });
  listen("accounts-changed", () => void reload());
  void reload();
}
