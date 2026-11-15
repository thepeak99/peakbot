// Live "working" row for focus mode (plan §6.4): one static bar (NOT inside
// the virtualized list) showing the newest hidden step; each new step
// replaces the content, never stacks. Pure display — no state, no timers
// (the dots are CSS-only).
//
// With `status` (phone, below 1024px) it becomes one compact line instead of
// the box: an optional label plus tappable task / todo counts and the cwd,
// joined by "·" separators. Without `status` it is today's desktop row,
// unchanged. The line returns null when it has no items.

import type { JSX } from "react";
import { Fragment } from "react";
import type { WireChatMessage, WireRole } from "../state";
import { firstLine } from "../transcriptRows";
import { basename } from "../paths";

// Display glyphs only — mirrors ROLE_META in Message.tsx, which is keyed by
// the adapted view roles, so importing it would cross the wire/view seam.
const GLYPH: Record<WireRole, string> = {
  user: "▷",
  agent: "✦",
  toolcall: "🔧",
  toolresult: "↳",
  system: "•",
  summary: "≡",
};

export type ActivityLine =
  /** Focus mode: the newest hidden step; null → "Thinking". */
  | { kind: "step"; step: WireChatMessage | null }
  /** Phone, turn live, not focusing. */
  | { kind: "working" }
  /** Phone, counts only. */
  | { kind: "idle" };

export interface PhoneStatus {
  /** Running bg tasks; 0 when the Tasks tab is hidden. */
  tasks: number;
  todosDone: number;
  /** 0 when the Todo tab is hidden. */
  todosTotal: number;
  /** `browsableCwd`: null ⇒ no 📁 item (hidden tab / Local edition / no cwd). */
  cwd: string | null;
  onOpen(tab: "tasks" | "todo" | "directory"): void;
}

export function ActivityRow({ line, status }: { line: ActivityLine; status?: PhoneStatus }) {
  const step = line.kind === "step" ? line.step : null;
  const label = step ? firstLine(step.content) || step.tool_name || step.role : "Thinking";
  const subRole = step?.source?.kind === "sub_agent" ? step.source.role : undefined;

  if (status) {
    const items: JSX.Element[] = [];

    // Lead (working indicator or step label)
    if (line.kind !== "idle") {
      items.push(
        <Fragment key="lead">
          {line.kind === "working" ? (
            <span
              aria-hidden
              className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-amber-500 shadow-[0_0_0_4px_rgba(245,158,11,0.2)]"
            />
          ) : (
            <span aria-hidden>{step ? GLYPH[step.role] : "✦"}</span>
          )}
          {subRole && <span className="shrink-0">🧩 {subRole} ·</span>}
          <span
            data-testid="activity-label"
            className="min-w-0 truncate text-[13px] text-amber-500"
            title={line.kind === "step" ? label : undefined}
          >
            {line.kind === "step" ? label : "working…"}
          </span>
        </Fragment>,
      );
    }

    // Tasks
    if (status.tasks > 0) {
      const taskText = line.kind !== "idle"
        ? `⏳ ${status.tasks}`
        : `⏳ ${status.tasks} ${status.tasks === 1 ? "task" : "tasks"}`;
      const taskAria = `${status.tasks} running ${status.tasks === 1 ? "task" : "tasks"}`;
      items.push(
        <CountButton
          key="tasks"
          testId="activity-tasks"
          name={taskAria}
          text={taskText}
          title={taskAria}
          onClick={() => status.onOpen("tasks")}
        />,
      );
    }

    // Todos
    if (status.todosTotal > 0) {
      const todoText = line.kind !== "idle"
        ? `☑ ${status.todosDone}/${status.todosTotal}`
        : `☑ ${status.todosDone}/${status.todosTotal} todos`;
      const todoAria = `${status.todosDone} of ${status.todosTotal} todos done`;
      items.push(
        <CountButton
          key="todos"
          testId="activity-todos"
          name={todoAria}
          text={todoText}
          title={todoAria}
          onClick={() => status.onOpen("todo")}
        />,
      );
    }

    // Directory
    if (status.cwd !== null) {
      items.push(
        <button
          type="button"
          data-testid="activity-dir"
          aria-label={`Working directory ${status.cwd}`}
          title={status.cwd}
          onClick={() => status.onOpen("directory")}
          className="flex h-8 shrink-0 cursor-pointer items-center gap-1 rounded-lg px-1.5 text-[11.5px] text-zinc-400 active:bg-zinc-800"
        >
          📁 {basename(status.cwd)}
        </button>,
      );
    }

    if (items.length === 0) return null;

    return (
      <div
        data-testid="activity-row"
        data-variant="line"
        role="status"
        aria-live="polite"
        className="flex min-h-8 flex-nowrap items-center gap-1.5 overflow-hidden whitespace-nowrap px-3.5 text-[11px] leading-none text-zinc-500"
      >
        {items.flatMap((item, i) =>
          i === 0 ? [item] : [
            <span key={`sep-${i}`} data-testid="activity-sep" aria-hidden className="shrink-0 text-[11px] text-zinc-700">
              ·
            </span>,
            item,
          ]
        )}
      </div>
    );
  }

  return (
    <div className="lg:mr-12 px-4 pb-2 sm:px-6 md:px-8">
      <div
        data-testid="activity-row"
        data-variant="box"
        className="mx-auto flex max-w-5xl items-center gap-2 rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-1.5 text-xs text-zinc-400"
        role="status"
        aria-live="polite"
      >
        <span aria-hidden>{step ? GLYPH[step.role] : "✦"}</span>
        {subRole && <span className="shrink-0">🧩 {subRole} ·</span>}
        <span data-testid="activity-label" className="truncate" title={label}>
          {label}
        </span>
        <span aria-hidden className="flex shrink-0 items-center">
          {[0, 200, 400].map((ms) => (
            <span key={ms} className="animate-pulse" style={{ animationDelay: `${ms}ms` }}>
              ·
            </span>
          ))}
        </span>
      </div>
    </div>
  );
}

// h-8 keeps the tap target at the 32px floor inside the one-row line.
function CountButton({
  testId,
  name,
  text,
  title,
  onClick,
}: {
  testId: string;
  name: string;
  text: string;
  title: string;
  onClick(): void;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      aria-label={name}
      title={title}
      onClick={onClick}
      className="flex h-8 shrink-0 cursor-pointer items-center gap-1 rounded-lg px-1.5 text-[11.5px] text-zinc-400 active:bg-zinc-800"
    >
      {text}
    </button>
  );
}
