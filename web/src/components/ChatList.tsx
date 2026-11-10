// The mobile Chats home screen (design §4.1, PR1): a full-screen overlay with
// a "Chats" header (logo, connection dot, theme/notify/settings), a search
// field, profile filter chips, and grouped chat rows. All view-model logic
// (filter/group/meta) lives in chatList.ts; this component owns only the
// local search query and the selected chip.

import { useState } from "react";
import type { ConversationSummary } from "../state";
import type { CatalogProfile } from "../types";
import type { NotifyPermission } from "../useTaskNotifications";
import { NotifyToggle } from "./NotifyToggle";
import { ThemeToggle } from "./ThemeToggle";

export interface ChatListProps {
  conversations: ConversationSummary[];
  catalog: CatalogProfile[];
  loaded: boolean;
  unread: ReadonlySet<string>;
  connected: boolean;
  error: string | null;
  onOpen(convo: string, profile: string | null): void;
  onNew(): void;
  /** The gear renders only when given (same rule as the TopBar). */
  onOpenSettings?: () => void;
  notifyEnabled: boolean;
  notifyPermission: NotifyPermission;
  onToggleNotify(): void;
}

export function ChatList({
  conversations,
  catalog,
  loaded,
  unread,
  connected,
  error,
  onOpen,
  onNew,
  onOpenSettings,
  notifyEnabled,
  notifyPermission,
  onToggleNotify,
}: ChatListProps) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("");
  void query;
  void selected;
  void setQuery;
  void setSelected;
  void conversations;
  void catalog;
  void loaded;
  void unread;
  void onOpen;
  void onNew;

  return (
    <section
      aria-label="Chats"
      data-testid="chat-list"
      className="fixed inset-0 z-40 flex flex-col overflow-hidden bg-zinc-950 text-zinc-100"
    >
      {/* The header is sticky by construction: only the list below scrolls.
          Plain zinc utilities, like the TopBar/ChatHeader — the theme layer
          remaps them for the light theme. */}
      <header className="shrink-0 border-b border-zinc-800 bg-zinc-950/95 px-4 pt-[env(safe-area-inset-top)]">
        <div className="flex items-center gap-2.5 py-2.5">
          <img src="/logo_shifu.png" alt="" className="h-8 w-8 shrink-0 rounded-full" />
          <h1 className="text-2xl font-bold tracking-tight">Chats</h1>
          <div className="ml-auto flex shrink-0 items-center gap-3">
            <ThemeToggle />
            <NotifyToggle
              enabled={notifyEnabled}
              permission={notifyPermission}
              onToggle={onToggleNotify}
            />
            {onOpenSettings && (
              <button
                type="button"
                onClick={onOpenSettings}
                title="Settings"
                aria-label="Settings"
                className="flex h-6 w-6 cursor-pointer items-center justify-center rounded text-zinc-500 transition-colors hover:text-zinc-300"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  className="h-4 w-4"
                >
                  <circle cx="12" cy="12" r="3" />
                  <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                </svg>
              </button>
            )}
            <span
              data-testid="chats-conn"
              aria-label={connected ? "connected" : "offline"}
              className="flex items-center gap-1.5 text-xs"
            >
              <span
                className={`h-1.5 w-1.5 rounded-full ${
                  connected ? "bg-emerald-400" : "bg-zinc-600"
                }`}
              />
              <span className="hidden sm:inline">
                {connected ? "connected" : "offline"}
              </span>
            </span>
          </div>
        </div>
        {error && (
          <div
            data-testid="chats-error"
            role="alert"
            className="mb-2 rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-xs text-red-300"
          >
            {error}
          </div>
        )}
      </header>
    </section>
  );
}
