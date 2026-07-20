import type { TodoItem, TodoStatus } from "../types";

const STATUS_META: Record<TodoStatus, { glyph: string; cls: string }> = {
  completed: { glyph: "✔", cls: "text-emerald-400" },
  inProgress: { glyph: "◐", cls: "text-sky-400" },
  pending: { glyph: "○", cls: "text-zinc-500" },
  cancelled: { glyph: "✕", cls: "text-zinc-600 line-through" },
};

// The TODO side panel. Mirrors the TUI's todo_panel: glyph + strike-through
// per status, a done/total count. Items are derived from the watched lane's
// transcript (see todosFromMessages), so every lane surfaces its own todos. In
// the global view each item carries a `lane` label, shown as a pill so you can
// tell which agent owns it; scoped views omit the label.
export function TodoPanel({ items }: { items: TodoItem[] }) {
  const done = items.filter((i) => i.status === "completed").length;
  return (
    <section>
      <div className="mb-2 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Todo
        </h3>
        <span className="font-mono text-[11px] tabular-nums text-zinc-600">
          {done}/{items.length}
        </span>
      </div>
      <ul className="space-y-1 text-xs">
        {items.map((item, i) => {
          const meta = STATUS_META[item.status];
          return (
            <li
              key={`${item.lane ?? "_"}:${item.id}:${i}`}
              className="flex items-start gap-2"
            >
              <span className={`mt-px ${meta.cls}`}>{meta.glyph}</span>
              <span
                className={
                  item.status === "cancelled"
                    ? "text-zinc-600 line-through"
                    : item.status === "completed"
                      ? "text-zinc-500"
                      : "text-zinc-300"
                }
              >
                {item.lane && (
                  <span className="mr-1.5 rounded bg-zinc-800/80 px-1.5 py-0.5 text-[10px] text-zinc-400">
                    {item.lane}
                  </span>
                )}
                {item.text}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
