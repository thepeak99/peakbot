import type { TodoItem, TodoNode, TodoStatus } from "../types";

const STATUS_META: Record<TodoStatus, { glyph: string; cls: string }> = {
  completed: { glyph: "✔", cls: "text-emerald-400" },
  inProgress: { glyph: "◐", cls: "text-sky-400" },
  pending: { glyph: "○", cls: "text-zinc-500" },
  cancelled: { glyph: "✕", cls: "text-zinc-600 line-through" },
};

// One todo row's contents: status glyph, optional lane pill, text. Shared by
// parent rows and the nested sub-agent rows, so a child reads like any other
// todo — just indented. The caller owns the surrounding <li>.
function TodoRow({ item }: { item: TodoItem }) {
  const meta = STATUS_META[item.status];
  return (
    <div className="flex items-start gap-2">
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
    </div>
  );
}

// The TODO side panel. Mirrors the TUI's todo_panel: glyph + strike-through
// per status, a done/total count. Items are derived from the watched lane's
// transcript (see todosFromMessages), so every lane surfaces its own todos. In
// the global view each item carries a `lane` label, shown as a pill so you can
// tell which agent owns it, and a delegation's todos hang one level under the
// orchestrator item it was handed off from (todoTree); scoped views are flat
// and unlabeled. The count covers every rendered row — parents and children.
export function TodoPanel({ nodes }: { nodes: TodoNode[] }) {
  const rows = nodes.flatMap((n) => [n.item, ...n.children]);
  const done = rows.filter((i) => i.status === "completed").length;
  return (
    <section>
      <div className="mb-2 flex items-baseline justify-between">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
          Todo
        </h3>
        <span className="font-mono text-[11px] tabular-nums text-zinc-600">
          {done}/{rows.length}
        </span>
      </div>
      <ul className="space-y-1 text-xs">
        {nodes.map((node, i) => (
          <li key={`${node.item.lane ?? "_"}:${node.item.id}:${i}`}>
            <TodoRow item={node.item} />
            {node.children.length > 0 && (
              <ul className="mt-1 ml-3 space-y-1 border-l border-zinc-800 pl-3">
                {node.children.map((child, j) => (
                  <li key={`${child.lane ?? "_"}:${child.id}:${j}`}>
                    <TodoRow item={child} />
                  </li>
                ))}
              </ul>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
