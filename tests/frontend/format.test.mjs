// Unit tests for src/buddy/format.ts. Dates are built with the local-time
// Date constructor, so the tests pass in any time zone.
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  dayLabel,
  defaultDue,
  isOverdue,
  meetingLabel,
  parseLocalInput,
  relativeTime,
  taskDueLabel,
  toLocalInputValue,
  whenLabel,
} from "../../src/buddy/format.ts";

// Tuesday 17 Nov 2026, 09:00 local.
const now = new Date(2026, 10, 17, 9, 0, 0);
const at = (h, m = 0, day = 17) => new Date(2026, 10, day, h, m, 0);
const plusMs = (ms) => new Date(now.getTime() + ms);

test("relativeTime rounds up to whole minutes", () => {
  assert.equal(relativeTime(at(9, 10), now), "in 10 min");
  assert.equal(relativeTime(plusMs(9 * 60_000 + 30_000), now), "in 10 min");
  assert.equal(relativeTime(plusMs(1000), now), "in 1 min");
  assert.equal(relativeTime(at(9, 59), now), "in 59 min");
});

test("relativeTime: now, hours and days", () => {
  assert.equal(relativeTime(now, now), "now");
  assert.equal(relativeTime(at(8, 0), now), "now");
  assert.equal(relativeTime(at(10, 0), now), "in 1 h");
  assert.equal(relativeTime(at(11, 5), now), "in 2 h 5 min");
  assert.equal(relativeTime(at(9, 0, 19), now), "in 2 d");
});

test("day and when labels", () => {
  assert.equal(dayLabel(at(23, 59), now), "Today");
  assert.equal(dayLabel(at(0, 0, 18), now), "Tomorrow");
  assert.equal(dayLabel(at(12, 0, 16), now), "Yesterday");
  assert.equal(dayLabel(at(12, 0, 20), now), "Fri");
  assert.equal(dayLabel(at(12, 0, 24), now), "24 Nov");
  assert.equal(whenLabel(at(14, 5), now), "14:05");
  assert.equal(whenLabel(at(9, 0, 18), now), "Tomorrow 09:00");
});

test("meeting labels", () => {
  const timed = (s, e) => ({ kind: "timed", start: s.toISOString(), end: e.toISOString() });
  assert.equal(meetingLabel(timed(at(9, 20), at(9, 50)), now), "09:20 · in 20 min");
  assert.equal(meetingLabel(timed(at(8, 50), at(9, 20)), now), "08:50 · now");
  assert.equal(
    meetingLabel({ kind: "allDay", start: "2026-11-18", end: "2026-11-19" }, now),
    "Tomorrow · all day",
  );
});

test("overdue matches Task::is_overdue", () => {
  assert.equal(isOverdue({ done: false, dueAt: now.toISOString() }, now), true);
  assert.equal(isOverdue({ done: false, dueAt: at(8, 0).toISOString() }, now), true);
  assert.equal(isOverdue({ done: true, dueAt: at(8, 0).toISOString() }, now), false);
  assert.equal(isOverdue({ done: false, dueAt: at(9, 1).toISOString() }, now), false);
});

test("task due labels", () => {
  assert.equal(taskDueLabel({ done: false, dueAt: at(9, 5).toISOString() }, now), "09:05 · in 5 min");
  assert.equal(taskDueLabel({ done: false, dueAt: at(8, 0).toISOString() }, now), "08:00");
});

test("datetime-local round trip and default due", () => {
  assert.equal(toLocalInputValue(at(7, 5, 3)), "2026-11-03T07:05");
  assert.equal(parseLocalInput("2026-11-03T07:05").getTime(), at(7, 5, 3).getTime());
  assert.equal(parseLocalInput(""), null);
  assert.equal(parseLocalInput("garbage"), null);
  assert.equal(defaultDue(now).getTime(), at(10, 0).getTime());
  assert.equal(defaultDue(at(9, 45)).getTime(), at(11, 0).getTime());
});
