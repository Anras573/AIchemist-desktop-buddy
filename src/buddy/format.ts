// Pure formatting helpers for the upcoming popover (task 2.3). No DOM, no
// imports: tested with `node --test` (tests/frontend/format.test.mjs).
// Times are shown in the local time zone of the machine.

const MINUTE_MS = 60_000;
const WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/** Local midnight of the day of `d`. */
function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

/** Whole local calendar days from `now`'s day to `d`'s day (DST-safe). */
function dayDiff(d: Date, now: Date): number {
  return Math.round((startOfDay(d).getTime() - startOfDay(now).getTime()) / 86_400_000);
}

/**
 * Time until `at`, rounded up to whole minutes: "now" (at or past), "in 5
 * min", "in 2 h 5 min", "in 3 h", "in 2 d". A reminder 10 minutes ahead
 * reads "in 10 min" even a few seconds later.
 */
export function relativeTime(at: Date, now: Date): string {
  const minutes = Math.ceil((at.getTime() - now.getTime()) / MINUTE_MS);
  if (!(minutes > 0)) return "now";
  if (minutes < 60) return `in ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  if (hours < 24) return rest === 0 ? `in ${hours} h` : `in ${hours} h ${rest} min`;
  return `in ${Math.floor(hours / 24)} d`;
}

/** "Today", "Tomorrow", "Yesterday", a weekday within the next week, else "17 Nov". */
export function dayLabel(d: Date, now: Date): string {
  const diff = dayDiff(d, now);
  if (diff === 0) return "Today";
  if (diff === 1) return "Tomorrow";
  if (diff === -1) return "Yesterday";
  if (diff > 1 && diff < 7) return WEEKDAYS[d.getDay()]!;
  return `${d.getDate()} ${MONTHS[d.getMonth()]}`;
}

/** "14:05" today, otherwise prefixed with the day: "Tomorrow 14:05", "Tue 9:00"... */
export function whenLabel(at: Date, now: Date): string {
  const time = `${pad2(at.getHours())}:${pad2(at.getMinutes())}`;
  return dayDiff(at, now) === 0 ? time : `${dayLabel(at, now)} ${time}`;
}

/** Parses a floating `YYYY-MM-DD` date as local midnight. */
export function parseLocalDate(s: string): Date {
  const [y, m, d] = s.split("-").map(Number);
  return new Date(y!, m! - 1, d!);
}

/** `Event.time` (src-tauri/src/domain/event.rs). */
export type EventTime =
  | { kind: "timed"; start: string; end: string }
  | { kind: "allDay"; start: string; end: string };

/** Label for a meeting: "Tomorrow 14:05 · in 20 h", "now" while it runs, "Today · all day". */
export function meetingLabel(time: EventTime, now: Date): string {
  if (time.kind === "allDay") return `${dayLabel(parseLocalDate(time.start), now)} · all day`;
  const start = new Date(time.start);
  const end = new Date(time.end);
  if (start <= now) return end > now ? `${whenLabel(start, now)} · now` : whenLabel(start, now);
  return `${whenLabel(start, now)} · ${relativeTime(start, now)}`;
}

/** Same rule as `Task::is_overdue`: open and due at or before `now`. */
export function isOverdue(task: { done: boolean; dueAt: string }, now: Date): boolean {
  return !task.done && new Date(task.dueAt).getTime() <= now.getTime();
}

/** Label for an open task's due time: "14:05 · in 5 min", or just the time when overdue. */
export function taskDueLabel(task: { done: boolean; dueAt: string }, now: Date): string {
  const due = new Date(task.dueAt);
  if (task.done || isOverdue(task, now)) return whenLabel(due, now);
  return `${whenLabel(due, now)} · ${relativeTime(due, now)}`;
}

/** Value for `<input type="datetime-local">`: local "YYYY-MM-DDTHH:MM". */
export function toLocalInputValue(d: Date): string {
  return (
    `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}` +
    `T${pad2(d.getHours())}:${pad2(d.getMinutes())}`
  );
}

/** Default due time for a new task: the next full hour, at least 30 min away. */
export function defaultDue(now: Date): Date {
  const d = new Date(now.getFullYear(), now.getMonth(), now.getDate(), now.getHours() + 1);
  if (d.getTime() - now.getTime() < 30 * MINUTE_MS) d.setHours(d.getHours() + 1);
  return d;
}

/**
 * Parses a `datetime-local` value as local time. Returns `null` for an
 * empty or invalid value (`new Date("")` is an invalid date).
 */
export function parseLocalInput(value: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(value);
  if (!m) return null;
  const d = new Date(+m[1]!, +m[2]! - 1, +m[3]!, +m[4]!, +m[5]!);
  return Number.isNaN(d.getTime()) ? null : d;
}
