// Right-rail sidebar: stats, todos, background processes. Conversations live
// in the top bar (lg+) / bottom bar (mobile) alongside model + cwd.
// Rendered in two places by App:
//   1. the static `<aside>` on lg+ screens (always visible)
//   2. inside a full-viewport drawer on smaller screens (toggled by the hamburger)

import { StatsPanel } from "./StatsPanel";
import { TodoPanel } from "./TodoPanel";
import { BgPanel } from "./BgPanel";
import type { ContextUsage, SessionStats, TodoItem, BgProcess } from "../types";

export function Sidebar({
  stats,
  context,
  todos,
  bg,
}: {
  stats: SessionStats | null;
  context: ContextUsage | null;
  todos: TodoItem[];
  bg: BgProcess[];
}) {
  return (
    <>
      {stats && context && (
        <StatsPanel stats={stats} context={context} />
      )}
      <TodoPanel items={todos} />
      <BgPanel processes={bg} />
    </>
  );
}
