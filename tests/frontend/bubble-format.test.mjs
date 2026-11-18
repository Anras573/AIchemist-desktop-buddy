// Unit tests for src/buddy/bubble-format.ts. Run with `npm test`.
// Dates are built from local components, so the tests pass in any time zone.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  badgeText,
  clock,
  detailLine,
  durationLabel,
  minutesUntil,
  msToNextMinute,
  relative,
  replayLabel,
  snoozeChoices,
  when,
} from "../../src/buddy/bubble-format.ts";

const now = new Date(2026, 10, 17, 14, 0, 0);
const at = (h, m, s = 0, day = 17) => new Date(2026, 10, day, h, m, s);

const meeting = (start, extra = {}) => ({
  key: {
    kind: "meeting",
    event: { accountId: "a", calendarId: "c", eventId: "e" },
    leadMinutes: 5,
  },
  title: "Standup",
  at: start.toISOString(),
  allDay: false,
  location: null,
  ...extra,
});

const task = (due) => ({
  key: { kind: "task", taskId: "t1" },
  title: "Water plants",
  at: due.toISOString(),
  allDay: false,
  location: null,
});

test("clock and when use local 24-hour time", () => {
  assert.equal(clock(at(9, 5)), "09:05");
  assert.equal(when(at(16, 30), now), "16:30");
  assert.equal(when(at(9, 0, 0, 18), now), "18 Nov 09:00");
});

test("minutesUntil rounds the future up and the past toward zero", () => {
  assert.equal(minutesUntil(at(14, 5), now), 5);
  assert.equal(minutesUntil(at(14, 4, 10), now), 5);
  assert.equal(minutesUntil(now, now), 0);
  assert.equal(minutesUntil(at(13, 57), now), -3);
  assert.equal(minutesUntil(at(13, 59, 30), now), 0);
});

test("relative spans", () => {
  assert.equal(relative(5, "started"), "in 5 min");
  assert.equal(relative(80, "started"), "in 1 h 20 min");
  assert.equal(relative(120, "started"), "in 2 h");
  assert.equal(relative(0, "started"), "now");
  assert.equal(relative(-3, "started"), "started 3 min ago");
});

test("meeting detail line", () => {
  assert.equal(detailLine(meeting(at(14, 10)), now), "14:10 · in 10 min");
  assert.equal(detailLine(meeting(at(14, 0)), now), "14:00 · now");
  assert.equal(detailLine(meeting(at(13, 58)), now), "13:58 · started 2 min ago");
  assert.equal(detailLine(meeting(at(0, 0), { allDay: true }), now), "All day");
});

test("task detail line: due, due now, overdue", () => {
  assert.equal(detailLine(task(at(14, 30)), now), "Due 14:30 · in 30 min");
  assert.equal(detailLine(task(at(14, 0)), now), "Due now");
  assert.equal(detailLine(task(at(13, 0)), now), "Overdue · due 13:00");
  assert.equal(detailLine(task(at(9, 0, 0, 16)), now), "Overdue · due 16 Nov 09:00");
});

test("badge and replay label", () => {
  assert.equal(badgeText(0), null);
  assert.equal(badgeText(2), "+2");
  assert.equal(replayLabel(false), null);
  assert.equal(replayLabel(true), "While you were away");
});

test("snooze choices: first is the default, the rest deduplicated", () => {
  assert.deepEqual(snoozeChoices([5]), { primary: 5, others: [] });
  assert.deepEqual(snoozeChoices([10, 5, 30, 5, 10]), { primary: 10, others: [5, 30] });
  assert.deepEqual(snoozeChoices([]), { primary: 5, others: [] });
  assert.equal(durationLabel(5), "5 min");
  assert.equal(durationLabel(90), "1 h 30 min");
});

test("msToNextMinute", () => {
  assert.equal(msToNextMinute(at(14, 0, 0)), 60_000);
  assert.equal(msToNextMinute(new Date(2026, 10, 17, 14, 0, 45, 500)), 14_500);
});
