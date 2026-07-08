import type { TodoItem, TodoStatus } from "../mock";

const STATUS_META: Record<TodoStatus, { glyph: string; cls: string }> = {
  completed: { glyph: "✔", cls: "text-emerald-400" },
  inProgress: { glyph: "◐", cls: "text-sky-400" },
  pending: { glyph: "○", cls: "text-zinc-500" },
  cancelled: { glyph: "✕", cls: "text-zinc-600 line-through" },
};

// The TODO side panel. Mirrors TodoState (src/ui/app_state.rs) and the
// TUI's todo_panel: glyph + strike-through per status, a done/total count.
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
        {items.map((item) => {
          const meta = STATUS_META[item.status];
          return (
            <li key={item.id} className="flex items-start gap-2">
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
                {item.text}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
