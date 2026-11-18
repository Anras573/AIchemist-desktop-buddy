// Pure helpers for the reminder bubble (task 2.2): text for each item, the
// snooze choices and the "+N" badge. No DOM access, so
// `npm test` runs them under Node (tests/frontend/bubble-format.test.mjs).
// Times are shown in local time, 24-hour.

import type { BubbleItem } from "./ipc";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n);
}

/** "14:05", local time. */
export function clock(d: Date): string {
  return `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

function sameLocalDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/** "14:05" today, "17 Nov 14:05" on another day. */
export function when(at: Date, now: Date): string {
  return sameLocalDay(at, now) ? clock(at) : `${at.getDate()} ${MONTHS[at.getMonth()]} ${clock(at)}`;
}

/**
 * Whole minutes from `now` until `at`, rounded up, so a meeting 4 min 10 s
 * away says "in 5 min" (it fired as the 5 min reminder). Negative when past.
 */
export function minutesUntil(at: Date, now: Date): number {
  const ms = at.getTime() - now.getTime();
  if (ms >= 0) return Math.ceil(ms / 60_000);
  return -Math.floor(-ms / 60_000) || 0; // `|| 0` turns -0 into 0.
}

/** "in 5 min", "in 1 h 20 min", "now", "started 3 min ago". */
export function relative(minutes: number, pastWord: string): string {
  if (minutes === 0) return "now";
  const abs = Math.abs(minutes);
  const h = Math.floor(abs / 60);
  const m = abs % 60;
  const span = h === 0 ? `${m} min` : m === 0 ? `${h} h` : `${h} h ${m} min`;
  return minutes > 0 ? `in ${span}` : `${pastWord} ${span} ago`;
}

export function isTask(item: BubbleItem): boolean {
  return item.key.kind === "task";
}

/**
 * The line under the title. Meetings: "14:30 · in 5 min" (or "All day").
 * Tasks: "Due 14:30 · in 5 min", "Due now" or "Overdue · due 14:30".
 */
export function detailLine(item: BubbleItem, now: Date): string {
  const at = new Date(item.at);
  const mins = minutesUntil(at, now);
  if (isTask(item)) {
    if (mins > 0) return `Due ${when(at, now)} · ${relative(mins, "")}`;
    if (mins === 0) return "Due now";
    return `Overdue · due ${when(at, now)}`;
  }
  if (item.allDay) return "All day";
  return `${when(at, now)} · ${relative(mins, "started")}`;
}

/** Text of the "+N" badge, or null when nothing is queued. */
export function badgeText(queued: number): string | null {
  return queued > 0 ? `+${queued}` : null;
}

/** The label above a replayed batch, or null. */
export function replayLabel(replay: boolean): string | null {
  return replay ? "While you were away" : null;
}

/** The default snooze (the first option) and the alternatives. */
export function snoozeChoices(options: readonly number[]): { primary: number; others: number[] } {
  const valid = options.filter((m) => Number.isInteger(m) && m > 0);
  const primary = valid[0] ?? 5;
  return { primary, others: [...new Set(valid.slice(1))].filter((m) => m !== primary) };
}

/** "5 min", "1 h", "1 h 30 min". */
export function durationLabel(minutes: number): string {
  return relative(minutes, "").replace(/^in /, "");
}

/** Milliseconds until the next whole local minute (to refresh "in N min"). */
export function msToNextMinute(now: Date): number {
  return 60_000 - (now.getSeconds() * 1000 + now.getMilliseconds());
}
