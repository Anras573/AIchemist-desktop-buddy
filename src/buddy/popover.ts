// The upcoming popover (spec 4.4, task 2.3): next meetings, open tasks with
// "mark done", a collapsed "Done" section with delete buttons, and an
// "Add task" form.
//
// Window: the popover does not fit the 240 x 120 bubble area, so opening it
// asks Rust to grow the buddy window (`set_popover_open`) away from its
// corner; the cat stays in place. For top corners the popover sits below
// the cat (`data-popover="below"`), otherwise above it. Closing restores the
// normal size. The page reports the popover's box as a hit region (main.ts),
// because on Windows the window region also clips drawing.
//
// Opens on a left-click on the cat, from the tray "Upcoming" and from the cat
// menu (`open-popover`). Closes on Escape, on clicking the cat again, or
// after IDLE_CLOSE_MS without interaction. Opening never focuses the window;
// only the add-task view focuses its title field, which does not activate
// the window either.
//
// CPU: no timers while closed. While open: one idle-close timeout and one
// timeout per minute to refresh the "in N min" labels.

import {
  defaultDue,
  isOverdue,
  meetingLabel,
  parseLocalInput,
  taskDueLabel,
  toLocalInputValue,
} from "./format";
import {
  type CalendarEvent,
  type IpcError,
  type PopoverView,
  type ScreenCorner,
  type Task,
  type UpcomingResponse,
  inTauri,
  invoke,
  listen,
} from "./ipc";

/** Close after this long without pointer or keyboard activity in the popover. */
const IDLE_CLOSE_MS = 60_000;

export interface PopoverHost {
  /** The `#buddy` container (gets `data-popover`). */
  main: HTMLElement;
  /** Current screen corner, to pick the side of the cat. */
  corner: () => ScreenCorner;
  /** The popover was shown or hidden: re-report hit regions. */
  changed: () => void;
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function errorText(err: unknown): string {
  const e = err as Partial<IpcError> | undefined;
  if (e && "message" in e && typeof e.message === "string") return e.message;
  return "Something went wrong.";
}

export function initPopover(host: PopoverHost) {
  const root = document.querySelector<HTMLElement>("#popover")!;
  const meetingsList = root.querySelector<HTMLUListElement>("#meetings")!;
  const tasksList = root.querySelector<HTMLUListElement>("#tasks")!;
  const doneSection = root.querySelector<HTMLDetailsElement>("#done")!;
  const doneCount = root.querySelector<HTMLElement>("#done-count")!;
  const doneList = root.querySelector<HTMLUListElement>("#done-tasks")!;
  const status = root.querySelector<HTMLElement>("#popover-status")!;
  const form = root.querySelector<HTMLFormElement>("#add-task")!;
  const titleInput = form.querySelector<HTMLInputElement>("#task-title")!;
  const dueInput = form.querySelector<HTMLInputElement>("#task-due")!;
  const titleError = form.querySelector<HTMLElement>("#task-title-error")!;
  const dueError = form.querySelector<HTMLElement>("#task-due-error")!;
  const formError = form.querySelector<HTMLElement>("#task-form-error")!;
  const closeButton = root.querySelector<HTMLButtonElement>("#popover-close")!;

  let isOpen = false;
  /** Bumped on every open/close, so stale async work is dropped. */
  let generation = 0;
  let data: UpcomingResponse | null = null;
  let idleTimer: number | undefined;
  let minuteTimer: number | undefined;

  // ----- Rendering -----

  function renderMeeting(m: CalendarEvent, now: Date): HTMLLIElement {
    const li = el("li", "item");
    li.append(el("div", "title", m.title.trim() || "(no title)"));
    const meta = el("div", "meta", meetingLabel(m.time, now));
    li.append(meta);
    if (m.location) li.append(el("div", "meta location", m.location));
    return li;
  }

  function renderTask(t: Task, now: Date): HTMLLIElement {
    const li = el("li", "item task");
    const label = el("label", "check");
    const box = el("input");
    box.type = "checkbox";
    box.checked = t.done;
    box.addEventListener("change", () => setDone(t.id, box.checked, box));
    label.append(box, el("span", "title", t.title));
    li.append(label);
    if (t.done) {
      const del = el("button", "delete", "×");
      del.type = "button";
      del.title = "Delete";
      del.setAttribute("aria-label", `Delete ${t.title}`);
      del.addEventListener("click", () => deleteTask(t.id, del));
      li.append(del);
    } else {
      const meta = el("div", "meta", taskDueLabel(t, now));
      if (isOverdue(t, now)) {
        li.classList.add("overdue");
        meta.append(" ", el("span", "badge", "overdue"));
      }
      li.append(meta);
    }
    return li;
  }

  function render(): void {
    const now = new Date();
    if (!data) {
      meetingsList.replaceChildren();
      tasksList.replaceChildren();
      doneList.replaceChildren();
      return;
    }
    meetingsList.replaceChildren(
      ...(data.meetings.length
        ? data.meetings.map((m) => renderMeeting(m, now))
        : [el("li", "empty", "No meetings in the next 7 days")]),
    );
    tasksList.replaceChildren(
      ...(data.openTasks.length
        ? data.openTasks.map((t) => renderTask(t, now))
        : [el("li", "empty", "No open tasks")]),
    );
    doneCount.textContent = String(data.doneTasks.length);
    doneSection.hidden = data.doneTasks.length === 0;
    doneList.replaceChildren(...data.doneTasks.map((t) => renderTask(t, now)));
    status.textContent = data.paused ? "Reminders paused" : "";
  }

  function scheduleMinuteRefresh(): void {
    clearTimeout(minuteTimer);
    const ms = 60_000 - (Date.now() % 60_000) + 50;
    minuteTimer = window.setTimeout(() => {
      render();
      scheduleMinuteRefresh();
    }, ms);
  }

  async function refresh(): Promise<void> {
    if (!inTauri) return;
    const gen = generation;
    try {
      const next = await invoke<UpcomingResponse>("list_upcoming");
      if (gen !== generation || !isOpen) return;
      data = next;
      render();
    } catch (err) {
      if (gen === generation) status.textContent = errorText(err);
    }
  }

  // ----- Actions -----

  async function setDone(taskId: string, done: boolean, box: HTMLInputElement): Promise<void> {
    box.disabled = true;
    try {
      await invoke("set_task_done", { request: { taskId, done } });
      // `upcoming-changed` triggers the refetch.
    } catch (err) {
      box.checked = !done;
      box.disabled = false;
      status.textContent = errorText(err);
    }
  }

  async function deleteTask(taskId: string, button: HTMLButtonElement): Promise<void> {
    button.disabled = true;
    try {
      await invoke("delete_task", { request: { taskId } });
    } catch (err) {
      button.disabled = false;
      status.textContent = errorText(err);
    }
  }

  function clearFormErrors(): void {
    titleError.textContent = dueError.textContent = formError.textContent = "";
    titleInput.removeAttribute("aria-invalid");
    dueInput.removeAttribute("aria-invalid");
  }

  function resetForm(): void {
    clearFormErrors();
    titleInput.value = "";
    dueInput.value = toLocalInputValue(defaultDue(new Date()));
  }

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    clearFormErrors();
    const due = parseLocalInput(dueInput.value);
    if (!due) {
      dueError.textContent = "Pick a date and time.";
      dueInput.setAttribute("aria-invalid", "true");
      return;
    }
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]")!;
    submit.disabled = true;
    try {
      await invoke("create_task", {
        request: { title: titleInput.value, dueAt: due.toISOString() },
      });
      resetForm();
    } catch (err) {
      const e = err as IpcError;
      if (e && e.kind === "validation" && "errors" in e) {
        for (const f of e.errors) {
          const target = f.field === "title" ? titleError : f.field === "dueAt" ? dueError : formError;
          target.textContent = f.message;
          if (f.field === "title") titleInput.setAttribute("aria-invalid", "true");
        }
      } else {
        formError.textContent = errorText(err);
      }
    } finally {
      submit.disabled = false;
    }
  });

  // ----- Open / close -----

  function touch(): void {
    if (!isOpen) return;
    clearTimeout(idleTimer);
    idleTimer = window.setTimeout(close, IDLE_CLOSE_MS);
  }

  async function open(view: PopoverView): Promise<void> {
    if (!isOpen) {
      isOpen = true;
      const gen = ++generation;
      // Top corners: the popover goes below the cat. Set before growing the
      // window: in the normal-size window both layouts put the cat at the
      // same spot, so nothing moves.
      const below = host.corner().startsWith("top");
      host.main.dataset.popover = below ? "below" : "above";
      resetForm();
      if (inTauri) {
        try {
          await invoke("set_popover_open", { request: { open: true } });
        } catch (err) {
          console.error("set_popover_open failed", err);
        }
      }
      if (gen !== generation) return; // Closed meanwhile.
      root.hidden = false;
      host.changed();
      scheduleMinuteRefresh();
      void refresh();
    }
    touch();
    if (view === "addTask") {
      titleInput.focus({ preventScroll: true });
      form.scrollIntoView({ block: "nearest" });
    }
  }

  function close(): void {
    if (!isOpen) return;
    isOpen = false;
    ++generation;
    clearTimeout(idleTimer);
    clearTimeout(minuteTimer);
    root.hidden = true;
    data = null;
    render(); // Drops the list DOM while closed.
    host.changed();
    const done = () => {
      // The window is back to normal size; drop the layout flag.
      if (!isOpen) delete host.main.dataset.popover;
    };
    if (inTauri) {
      invoke("set_popover_open", { request: { open: false } })
        .catch((err: unknown) => console.error("set_popover_open failed", err))
        .finally(done);
    } else {
      done();
    }
  }

  function toggle(): void {
    if (isOpen) close();
    else void open("upcoming");
  }

  closeButton.addEventListener("click", close);
  for (const type of ["pointerdown", "pointermove", "keydown", "wheel", "input"]) {
    root.addEventListener(type, touch, { passive: true });
  }
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && isOpen) close();
  });

  void listen<PopoverView>("open-popover", (view) => void open(view));
  void listen<null>("upcoming-changed", () => {
    if (isOpen) void refresh();
  });

  return {
    element: root,
    get isOpen() {
      return isOpen;
    },
    open,
    close,
    toggle,
  };
}
