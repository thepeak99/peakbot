// The right-rail sidebar: conversations picker, stats, todos, background
// processes. Rendered in two places by App:
//   1. the static `<aside>` on lg+ screens (always visible)
//   2. inside a slide-in drawer on smaller screens (toggled by the hamburger)
//
// The two instances are independent — each has its own dropdown-open state for
// the conversations picker, which is fine because only one is ever on-screen
// at a time. Killing the dropdown state on viewport resize would just add
// churn for no gain.

import { ConversationsPicker } from "./ConversationsPicker";
import { StatsPanel } from "./StatsPanel";
import { TodoPanel } from "./TodoPanel";
import { BgPanel } from "./BgPanel";
import type { ContextUsage, SessionStats, TodoItem, BgProcess } from "../types";
import type { ConversationSummary } from "../state";

export function Sidebar({
  conversations,
  hasTranscript,
  stats,
  context,
  todos,
  bg,
  onOpenConversations,
  onLoadConversation,
  onKillSession,
  onConvoOpened,
}: {
  conversations: ConversationSummary[];
  hasTranscript: boolean;
  stats: SessionStats | null;
  context: ContextUsage | null;
  todos: TodoItem[];
  bg: BgProcess[];
  onOpenConversations: () => void;
  onLoadConversation: (id: string) => void;
  onKillSession: (id: string) => void;
  // Fired after a conversation is loaded — the drawer uses this to auto-close
  // itself, since the user has finished navigating. Static aside ignores it.
  onConvoOpened?: () => void;
}) {
  return (
    <>
      <ConversationsPicker
        conversations={conversations}
        hasTranscript={hasTranscript}
        onOpen={onOpenConversations}
        onLoad={(id) => {
          onLoadConversation(id);
          onConvoOpened?.();
        }}
        onKill={onKillSession}
      />
      {stats && context && (
        <StatsPanel stats={stats} context={context} />
      )}
      <TodoPanel items={todos} />
      <BgPanel processes={bg} />
    </>
  );
}
