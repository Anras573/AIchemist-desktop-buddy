// The reminder speech bubble (task 2.2). Renders the `BubbleView` from
// `reminder-shown` into `#bubble`, hides it on `reminder-cleared`, and sends
// Dismiss / Snooze / Mark done. Shown and hidden with the `hidden`
// attribute; main.ts's ResizeObserver keeps the hit regions in step.
//
// The bubble fits the 240 x 120 area: long titles are cut with an ellipsis
// and a replay batch of several items scrolls. While a bubble is shown the
// "in N min" text refreshes once a minute (one timeout, stopped when hidden).

import {
  badgeText,
  detailLine,
  durationLabel,
  isTask,
  msToNextMinute,
  replayLabel,
  snoozeChoices,
} from "./bubble-format";
import { type BubbleItem, type BubbleView, inTauri, invoke, listen } from "./ipc";

let root: HTMLElement;
let current: BubbleView | null = null;
let refreshTimer: number | undefined;
let busy = false;

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function button(label: string, onClick: () => void, className = "bubble-btn"): HTMLButtonElement {
  const b = el("button", className, label);
  b.type = "button";
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    if (!busy) onClick();
  });
  return b;
}

// ----- Actions -----

async function act(call: () => Promise<unknown>): Promise<void> {
  busy = true;
  root.classList.add("busy");
  try {
    await call();
  } catch (err: unknown) {
    // Usually a stale bubble id: resync with what Rust shows.
    console.error("bubble action failed", err);
    await restore();
  } finally {
    busy = false;
    root.classList.remove("busy");
  }
}

function dismiss(view: BubbleView): void {
  void act(() => invoke("dismiss_bubble", { request: { bubbleId: view.id } }));
}

function snooze(view: BubbleView, minutes: number): void {
  void act(() => invoke("snooze_bubble", { request: { bubbleId: view.id, minutes } }));
}

function markDone(item: BubbleItem): void {
  if (item.key.kind !== "task") return;
  const taskId = item.key.taskId;
  void act(() => invoke("set_task_done", { request: { taskId, done: true } }));
}

// ----- Rendering -----

function renderItem(item: BubbleItem, compact: boolean, now: Date): HTMLLIElement {
  const li = el("li", "bubble-item");
  const head = el("div", "bubble-item-head");
  const title = el("span", "bubble-title", item.title);
  title.title = item.title;
  head.append(title);
  if (isTask(item)) head.append(button("Mark done", () => markDone(item), "bubble-btn small"));
  li.append(head, el("div", "bubble-detail", detailLine(item, now)));
  if (item.location && !compact) {
    const where = el("div", "bubble-where", item.location);
    where.title = item.location;
    li.append(where);
  }
  return li;
}

function render(view: BubbleView): void {
  const now = new Date();
  const header = el("div", "bubble-header");
  const label = replayLabel(view.replay);
  if (label) header.append(el("span", "bubble-replay", label));
  const badge = badgeText(view.queued);
  if (badge) {
    const b = el("span", "bubble-badge", badge);
    b.title = `${view.queued} more waiting`;
    header.append(b);
  }

  const list = el("ul", "bubble-items");
  const compact = view.items.length > 1;
  for (const item of view.items) list.append(renderItem(item, compact, now));

  const { primary, others } = snoozeChoices(view.snoozeOptionsMinutes);
  const actions = el("div", "bubble-actions");
  const choices = el("div", "bubble-actions bubble-choices");
  choices.hidden = true;
  actions.append(button("Dismiss", () => dismiss(view)));
  actions.append(button(`Snooze ${durationLabel(primary)}`, () => snooze(view, primary)));
  if (others.length > 0) {
    const more = button("▾", () => {
      choices.hidden = !choices.hidden;
      more.setAttribute("aria-expanded", String(!choices.hidden));
    }, "bubble-btn narrow");
    more.title = "Other snooze durations";
    more.setAttribute("aria-expanded", "false");
    actions.append(more);
    for (const m of others) choices.append(button(durationLabel(m), () => snooze(view, m)));
  }

  const parts: HTMLElement[] = [];
  if (header.childElementCount > 0) parts.push(header);
  parts.push(list, actions, choices);
  root.replaceChildren(...parts);
}

/** Re-renders only the time texts (keeps an open snooze chooser). */
function refreshTimes(): void {
  if (!current) return;
  const now = new Date();
  const details = root.querySelectorAll<HTMLElement>(".bubble-detail");
  current.items.forEach((item, i) => {
    const d = details[i];
    if (d) d.textContent = detailLine(item, now);
  });
}

function scheduleRefresh(): void {
  if (refreshTimer !== undefined) clearTimeout(refreshTimer);
  refreshTimer = undefined;
  if (!current) return;
  refreshTimer = window.setTimeout(() => {
    refreshTimer = undefined;
    refreshTimes();
    scheduleRefresh();
  }, msToNextMinute(new Date()) + 50);
}

function show(view: BubbleView): void {
  current = view;
  render(view);
  root.hidden = false;
  scheduleRefresh();
}

function hide(): void {
  current = null;
  root.hidden = true;
  root.replaceChildren();
  scheduleRefresh();
}

async function restore(): Promise<void> {
  if (!inTauri) return;
  const view = await invoke<BubbleView | null>("get_current_bubble");
  if (view) show(view);
  else hide();
}

/** Wires the bubble to `#bubble`: listens first, then restores what Rust shows. */
export async function initBubble(container: HTMLElement): Promise<void> {
  root = container;
  await Promise.all([
    listen<BubbleView>("reminder-shown", show),
    listen<null>("reminder-cleared", hide),
  ]);
  await restore();
}
