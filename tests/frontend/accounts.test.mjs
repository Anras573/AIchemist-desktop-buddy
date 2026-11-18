// Unit tests for src/settings/accounts.ts. Run with `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  accountDetail,
  addFormErrors,
  buildAddRequest,
  buildUpdateRequest,
  calendarChoices,
  describeAccountError,
  needsSignIn,
  relativeTime,
  syncStatusText,
  toggleCalendar,
} from "../../src/settings/accounts.ts";

const form = {
  type: "caldav",
  displayName: "Home",
  serverUrl: "https://caldav.icloud.com",
  username: "jane@icloud.com",
  password: "abcd-efgh-ijkl-mnop",
  clientId: "11111111-2222-3333-4444-555555555555",
  tenant: "  ",
};

function status(overrides = {}) {
  return {
    account: {
      id: "a1",
      displayName: "Home",
      connection: { type: "caldav", serverUrl: "https://x", username: "jane" },
      includedCalendars: ["/cal/a/"],
    },
    syncState: "neverSynced",
    lastSyncedAt: null,
    lastError: null,
    hasSecret: true,
    signedInAs: null,
    ...overrides,
  };
}

const graphStatus = (overrides = {}) =>
  status({
    account: {
      id: "g1",
      displayName: "Work",
      connection: { type: "graph", clientId: "c", tenant: "common" },
      includedCalendars: [],
    },
    hasSecret: false,
    ...overrides,
  });

test("add request carries only the fields of the chosen type", () => {
  assert.deepEqual(buildAddRequest(form), {
    displayName: "Home",
    connection: { type: "caldav", serverUrl: "https://caldav.icloud.com", username: "jane@icloud.com" },
    secret: "abcd-efgh-ijkl-mnop",
  });
  assert.deepEqual(buildAddRequest({ ...form, type: "graph" }), {
    displayName: "Home",
    connection: { type: "graph", clientId: form.clientId, tenant: "common" },
    secret: null,
  });
  assert.equal(buildAddRequest({ ...form, type: "graph", tenant: " consumers " }).connection.tenant, "consumers");
  assert.equal(buildAddRequest({ ...form, password: "" }).secret, null);
});

test("update request keeps the account and changes only what is asked", () => {
  const s = status();
  assert.deepEqual(buildUpdateRequest(s), {
    accountId: "a1",
    displayName: "Home",
    connection: s.account.connection,
    includedCalendars: ["/cal/a/"],
    secret: null,
  });
  assert.equal(buildUpdateRequest(s, { secret: "new" }).secret, "new");
  assert.deepEqual(buildUpdateRequest(s, { includedCalendars: [] }).includedCalendars, []);
});

test("toggling calendars keeps order and never duplicates", () => {
  assert.deepEqual(toggleCalendar(["a", "b"], "c", true), ["a", "b", "c"]);
  assert.deepEqual(toggleCalendar(["a", "b"], "a", true), ["b", "a"]);
  assert.deepEqual(toggleCalendar(["a", "b"], "a", false), ["b"]);
  assert.deepEqual(toggleCalendar([], "x", false), []);
});

test("calendar choices mark included and vanished calendars", () => {
  const cals = [
    { id: "/cal/a/", accountId: "a1", name: "Home", color: "#ff0000" },
    { id: "/cal/b/", accountId: "a1", name: "", color: null },
  ];
  assert.deepEqual(calendarChoices(cals, ["/cal/a/", "/cal/gone/"]), [
    { id: "/cal/a/", name: "Home", color: "#ff0000", checked: true, missing: false },
    { id: "/cal/b/", name: "/cal/b/", color: null, checked: false, missing: false },
    { id: "/cal/gone/", name: "/cal/gone/", color: null, checked: true, missing: true },
  ]);
});

test("errors are labelled by kind", () => {
  assert.equal(
    describeAccountError({ kind: "network", message: "could not connect" }),
    "Network error: could not connect",
  );
  assert.equal(describeAccountError({ kind: "auth", message: "rejected" }), "Authentication failed: rejected");
  assert.equal(
    describeAccountError({ kind: "validation", errors: [{ field: "x", message: "bad" }] }),
    "Invalid input: bad",
  );
  assert.equal(describeAccountError({ kind: "weird" }), "Error");
  assert.equal(describeAccountError(undefined), "Unexpected error");
  assert.equal(describeAccountError("boom"), "boom");
});

test("add-form errors are grouped by field, the rest under the form", () => {
  const grouped = addFormErrors({
    kind: "validation",
    errors: [
      { field: "connection.serverUrl", message: "must be an https:// URL" },
      { field: "secret", message: "enter the password or app password" },
      { field: "accounts[0].id", message: "duplicate account id" },
    ],
  });
  assert.deepEqual([...grouped.entries()], [
    ["connection.serverUrl", ["must be an https:// URL"]],
    ["secret", ["enter the password or app password"]],
    ["", ["accounts[0].id: duplicate account id"]],
  ]);
  assert.deepEqual([...addFormErrors({ kind: "keychain", message: "locked" }).entries()], [
    ["", ["Keychain error: locked"]],
  ]);
});

test("row detail shows type, user and sign-in state", () => {
  assert.equal(accountDetail(status()), "CalDAV · jane");
  assert.equal(accountDetail(status({ hasSecret: false })), "CalDAV · jane · no password saved");
  assert.equal(accountDetail(graphStatus()), "Microsoft 365 / Outlook · not signed in");
  assert.equal(
    accountDetail(graphStatus({ hasSecret: true, signedInAs: "jane@example.com" })),
    "Microsoft 365 / Outlook · signed in as jane@example.com",
  );
  assert.equal(accountDetail(graphStatus({ hasSecret: true })), "Microsoft 365 / Outlook · signed in");
  assert.equal(needsSignIn(graphStatus()), true);
  assert.equal(needsSignIn(graphStatus({ hasSecret: true })), false);
  assert.equal(needsSignIn(status({ hasSecret: false })), false);
});

test("sync status text and relative times", () => {
  const now = new Date("2026-11-17T12:00:00Z");
  assert.equal(relativeTime("2026-11-17T11:59:40Z", now), "just now");
  assert.equal(relativeTime("2026-11-17T11:55:00Z", now), "5 min ago");
  assert.equal(relativeTime("2026-11-17T09:00:00Z", now), "3 h ago");
  assert.equal(relativeTime("2026-11-16T09:00:00Z", now), "1 day ago");
  assert.equal(relativeTime("2026-11-14T09:00:00Z", now), "3 days ago");
  assert.equal(relativeTime("nope", now), "");

  assert.equal(syncStatusText(status(), now), "Not synced yet");
  assert.equal(syncStatusText(graphStatus(), now), "Not synced yet: choose calendars to include");
  assert.equal(syncStatusText(status({ syncState: "syncing" }), now), "Syncing…");
  assert.equal(
    syncStatusText(status({ syncState: "ok", lastSyncedAt: "2026-11-17T11:50:00Z" }), now),
    "Synced, last synced 10 min ago",
  );
  assert.equal(syncStatusText(status({ syncState: "error" }), now), "Sync failed");
});
