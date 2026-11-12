// Live "working" row for focus mode (plan §6.4): one static bar (NOT inside
// the virtualized list) showing the newest hidden step; each new step
// replaces the content, never stacks. Pure display — no state, no timers
// (the dots are CSS-only).
//
// With `counts` (phone, below 1024px) it becomes one compact line instead of
// the box: an optional label plus tappable task / todo counts that open their
// panel. Without `counts` it is today's desktop row, unchanged.

import type { WireChatMessage, WireRole } from "../state";

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

// First non-empty, trimmed line of the server-preformatted content — the
// thought line for tool calls, so the row reads like the step itself.
function firstLine(content: string | undefined): string {
  if (!content) return "";
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (trimmed !== "") return trimmed;
  }
  return "";
}

export type ActivityLine =
  /** Focus mode: the newest hidden step; null → "Thinking". */
  | { kind: "step"; step: WireChatMessage | null }
  /** Phone, turn live, not focusing. */
  | { kind: "working" }
  /** Phone, counts only. */
  | { kind: "idle" };

export interface StatusCounts {
  /** 0 when the Tasks tab is hidden. */
  tasks: number;
  todosDone: number;
  /** 0 when the Todo tab is hidden. */
  todosTotal: number;
  onOpen(tab: "tasks" | "todo"): void;
}

export function ActivityRow({ line, counts }: { line: ActivityLine; counts?: StatusCounts }) {
  const step = line.kind === "step" ? line.step : null;
  const label = step ? firstLine(step.content) || step.tool_name || step.role : "Thinking";
  const subRole = step?.source?.kind === "sub_agent" ? step.source.role : undefined;

  if (counts) {
    return (
      <div
        data-testid="activity-row"
        data-variant="line"
        role="status"
        aria-live="polite"
        className="flex min-h-8 flex-nowrap items-center gap-1.5 overflow-hidden whitespace-nowrap px-3.5 text-[11px] leading-none text-zinc-500"
      >
        {line.kind !== "idle" && (
          <>
            {line.kind === "step" && <span aria-hidden>{step ? GLYPH[step.role] : "✦"}</span>}
            {line.kind === "working" && (
              <span aria-hidden className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-amber-400" />
            )}
            {subRole && <span className="shrink-0">🧩 {subRole} ·</span>}
            <span
              data-testid="activity-label"
              className={`min-w-0 truncate ${line.kind === "working" ? "text-amber-400" : ""}`}
              title={line.kind === "step" ? label : undefined}
            >
              {line.kind === "step" ? label : "working…"}
            </span>
          </>
        )}
        {counts.tasks > 0 && (
          <CountButton
            testId="activity-tasks"
            name="Open Tasks panel"
            text={`⏳ ${counts.tasks} ${counts.tasks === 1 ? "task" : "tasks"}`}
            onClick={() => counts.onOpen("tasks")}
          />
        )}
        {counts.todosTotal > 0 && (
          <CountButton
            testId="activity-todos"
            name="Open Todo panel"
            text={`☑ ${counts.todosDone}/${counts.todosTotal} todos`}
            onClick={() => counts.onOpen("todo")}
          />
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
  onClick,
}: {
  testId: string;
  name: string;
  text: string;
  onClick(): void;
}) {
  return (
    <button
      type="button"
      data-testid={testId}
      aria-label={name}
      onClick={onClick}
      className="h-8 shrink-0 cursor-pointer rounded-lg px-1.5 text-[11px] text-zinc-500 active:bg-zinc-800"
    >
      {text}
    </button>
  );
}
