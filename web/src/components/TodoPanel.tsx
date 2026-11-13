import type { TodoItem, TodoNode, TodoStatus } from "../types";
import { todoCounts } from "../adapt";
import { MTAG, PANEL_HEAD, PANEL_SUB, PANEL_TITLE, ROW } from "./phoneUi";

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

// Phone row (mock .trow): a status circle + the text. Children are the same
// row, indented, instead of a bordered nested list.
const CIRCLE: Record<TodoStatus, string> = {
  completed: "border-emerald-600 bg-emerald-600 text-white",
  inProgress: "border-amber-500",
  pending: "border-zinc-700",
  cancelled: "border-zinc-700",
};
const TEXT: Record<TodoStatus, string> = {
  completed: "text-zinc-500 line-through decoration-zinc-700",
  cancelled: "text-zinc-600 line-through",
  inProgress: "text-zinc-100",
  pending: "text-zinc-100",
};

function PhoneTodoRow({ item, child }: { item: TodoItem; child?: boolean }) {
  return (
    <li className={`${ROW} ${child ? "pl-[34px]" : ""}`}>
      <span
        className={`grid h-[22px] w-[22px] shrink-0 place-items-center rounded-full border-[1.8px] ${CIRCLE[item.status]}`}
      >
        {item.status === "completed" && (
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="3"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden
            className="h-3.5 w-3.5"
          >
            <path d="M5 12.5l4.5 4.5L19 7.5" />
          </svg>
        )}
      </span>
      <span className={`min-w-0 flex-1 text-[15px] leading-[1.3] ${TEXT[item.status]}`}>
        {item.lane && <span className={`mr-1.5 ${MTAG}`}>{item.lane}</span>}
        {item.text}
      </span>
    </li>
  );
}

function PhoneTodo({ nodes }: { nodes: TodoNode[] }) {
  const { open, total } = todoCounts(nodes);
  const done = total - open;
  return (
    <section>
      <div className={PANEL_HEAD}>
        <h3 className={PANEL_TITLE}>Todo</h3>
        <span className={PANEL_SUB}>
          {done} of {total} done
        </span>
      </div>
      <div
        role="progressbar"
        aria-label="Todo progress"
        aria-valuemin={0}
        aria-valuemax={total}
        aria-valuenow={done}
        className="mx-0.5 mb-2.5 h-[5px] overflow-hidden rounded-[3px] bg-zinc-800"
      >
        <div
          className="h-full rounded-[3px] bg-emerald-600 transition-[width]"
          style={{ width: `${total ? (done / total) * 100 : 0}%` }}
        />
      </div>
      <ul>
        {nodes.flatMap((node, i) => [
          <PhoneTodoRow key={`${node.item.lane ?? "_"}:${node.item.id}:${i}`} item={node.item} />,
          ...node.children.map((child, j) => (
            <PhoneTodoRow key={`${child.lane ?? "_"}:${child.id}:${i}.${j}`} item={child} child />
          )),
        ])}
      </ul>
    </section>
  );
}

// The TODO side panel. Mirrors the TUI's todo_panel: glyph + strike-through
// per status, a done/total count. Items are derived from the watched lane's
// transcript (see todosFromMessages), so every lane surfaces its own todos. In
// the global view each item carries a `lane` label, shown as a pill so you can
// tell which agent owns it, and a delegation's todos hang one level under the
// orchestrator item it was handed off from (todoTree); scoped views are flat
// and unlabeled. The count covers every rendered row — parents and children.
export function TodoPanel({ nodes, phone = false }: { nodes: TodoNode[]; phone?: boolean }) {
  if (phone) return <PhoneTodo nodes={nodes} />;
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
