// The mobile Chats home screen (design §4.1, PR1): a full-screen overlay with
// a "Chats" header (logo, connection dot, theme/notify/settings), a search
// field, profile filter chips, and grouped chat rows. All view-model logic
// (filter/group/meta) lives in chatList.ts; this component owns only the
// local search query and the selected chip.

import { useMemo, useState, type Ref } from "react";
import type { ReactNode } from "react";
import type { ConversationSummary, TabColor } from "../state";
import type { CatalogProfile } from "../types";
import type { NotifyPermission } from "../useTaskNotifications";
import { filterChats, groupChats, metaLine } from "../chatList";
import type { ChatRow, Range } from "../chatList";
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
  /** The overlay root, for the slide animation (React 19 ref-as-prop). */
  ref?: Ref<HTMLElement>;
  /** Mid-exit: still painted, but inert (no input, focus or AT). */
  leaving?: boolean;
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
  ref,
  leaving,
}: ChatListProps) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("");
  // A chip the catalog no longer carries (a remote profile dropped, a rename)
  // silently falls back to All — derived, not an effect, so the re-render
  // itself is the fix.
  const activeProfile =
    selected !== "" && catalog.some((p) => p.name === selected) ? selected : "";
  // `now` is display-only (meta line, day buckets) and re-taken each render;
  // the memo takes its own fresh clock so its deps stay data-stable.
  const now = new Date();
  const searching = query.trim() !== "";
  const sections = useMemo(
    () =>
      groupChats(
        filterChats(conversations, query, activeProfile === "" ? null : activeProfile),
        new Date(),
        searching,
      ),
    [conversations, query, activeProfile, searching],
  );

  return (
    <section
      ref={ref}
      inert={leaving}
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
        <div className="relative mt-1">
          <svg
            aria-hidden="true"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-zinc-500"
          >
            <circle cx="11" cy="11" r="7" />
            <path d="m21 21-4.35-4.35" />
          </svg>
          <input
            type="text"
            aria-label="Search chats"
            placeholder="Search chats"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="h-11 w-full rounded-lg border border-zinc-800 bg-zinc-900 pl-9 pr-9 text-sm text-zinc-100 placeholder-zinc-500 outline-none focus:border-zinc-600"
          />
          {query !== "" && (
            <button
              type="button"
              aria-label="Clear search"
              onClick={() => setQuery("")}
              className="absolute right-2 top-1/2 flex h-6 w-6 -translate-y-1/2 cursor-pointer items-center justify-center rounded-full bg-zinc-800 text-xs text-zinc-300 transition-colors hover:bg-zinc-700"
            >
              ✕
            </button>
          )}
        </div>
        <div
          className="flex gap-2 overflow-x-auto py-2.5"
          role="group"
          aria-label="Filter by profile"
        >
          <Chip
            label="All"
            profile=""
            pressed={activeProfile === ""}
            onSelect={setSelected}
          />
          {catalog.map((p) => (
            <Chip
              key={p.name}
              label={p.name}
              profile={p.name}
              color={p.color}
              remote={p.origin === "remote"}
              pressed={activeProfile === p.name}
              onSelect={setSelected}
            />
          ))}
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
      <div className="flex-1 overflow-y-auto overscroll-contain pb-24">
        {!loaded ? (
          <div
            data-testid="chat-loading"
            className="flex items-center gap-2 px-4 py-6 text-sm text-zinc-500"
          >
            <span
              aria-label="Loading chats"
              className="h-4 w-4 animate-spin rounded-full border-2 border-zinc-600 border-t-zinc-300"
            />
            Loading chats…
          </div>
        ) : sections.length === 0 ? (
          <div
            data-testid="chat-empty"
            className="px-4 py-8 text-center text-sm text-zinc-500"
          >
            {searching ? `No chats match “${query.trim()}”` : "No chats yet"}
          </div>
        ) : (
          sections.map((s) => (
          <section key={s.kind} data-testid="chat-section" data-kind={s.kind}>
            <h2 className="px-4 pb-1.5 pt-4 text-[11px] font-semibold uppercase tracking-[0.14em] text-zinc-500">
              {s.label}
            </h2>
            {s.rows.map((r) => (
              <Row
                key={r.chat.id}
                row={r}
                now={now}
                catalog={catalog}
                isUnread={unread.has(r.chat.id)}
                onOpen={onOpen}
              />
            ))}
          </section>
          ))
        )}
      </div>
      <button
        type="button"
        aria-label="New chat"
        onClick={onNew}
        className="absolute bottom-[calc(1.25rem+env(safe-area-inset-bottom))] right-4 z-10 flex h-14 w-14 cursor-pointer items-center justify-center rounded-2xl bg-emerald-600 pb-1 text-3xl font-light text-white shadow-lg transition-colors hover:bg-emerald-500"
      >
        +
      </button>
    </section>
  );
}

// One profile filter chip. The pressed chip carries the emerald border
// (mock: "All" and the picked profile); a remote profile shows its badge
// inline, like the row badge.
function Chip({
  label,
  profile,
  color,
  remote = false,
  pressed,
  onSelect,
}: {
  label: string;
  profile: string;
  color?: TabColor;
  remote?: boolean;
  pressed: boolean;
  onSelect(profile: string): void;
}) {
  return (
    <button
      type="button"
      data-testid="chat-filter"
      data-profile={profile}
      aria-pressed={pressed}
      onClick={() => onSelect(profile)}
      className={`flex h-9 shrink-0 cursor-pointer items-center gap-1.5 rounded-full border px-3 text-sm transition-colors ${
        pressed
          ? "border-emerald-700 bg-emerald-950/40 text-emerald-300"
          : "border-zinc-800 bg-zinc-900/60 text-zinc-300 hover:border-zinc-700"
      }`}
    >
      {color && (
        <span
          aria-hidden="true"
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: `var(--profile-${color})` }}
        />
      )}
      <span className="whitespace-nowrap">{label}</span>
      {remote && (
        <span className="shrink-0 rounded bg-zinc-800 px-1 text-[10px] text-zinc-400">
          remote
        </span>
      )}
    </button>
  );
}

// One chat row: a 4px profile-colour bar at the left edge, the title (+ a
// remote badge when the catalog says so) over the muted meta line. The whole
// row is the single 44px-minimum chat-open tap target.
function Row({
  row,
  now,
  catalog,
  isUnread,
  onOpen,
}: {
  row: ChatRow;
  now: Date;
  catalog: CatalogProfile[];
  isUnread: boolean;
  onOpen(convo: string, profile: string | null): void;
}) {
  const chat = row.chat;
  // Colour and origin come from the catalog entry, never derived here; a
  // legacy row (profile null) has neither a bar nor a badge.
  const profile = chat.profile
    ? catalog.find((p) => p.name === chat.profile)
    : undefined;
  const color = profile?.color;
  const running = chat.running === true;
  const live = chat.active && !running;
  return (
    <div
      data-testid="chat-row"
      data-convo={chat.id}
      className={live ? "bg-emerald-950/40" : ""}
    >
      <button
        type="button"
        data-testid="chat-open"
        onClick={() => onOpen(chat.id, chat.profile ?? null)}
        className="relative flex min-h-11 w-full cursor-pointer flex-col items-start px-4 py-2.5 text-left transition-colors hover:bg-zinc-900/60"
      >
        {color && (
          <span
            data-testid="chat-bar"
            aria-hidden="true"
            className="absolute inset-y-0 left-0 w-1"
            style={{ backgroundColor: `var(--profile-${color})` }}
          />
        )}
        <span className="flex w-full items-center gap-2 pl-1.5">
          <span
            className={`min-w-0 truncate text-[15px] ${
              live ? "font-semibold text-emerald-200" : "font-medium text-zinc-100"
            }`}
          >
            <Title name={chat.name} matches={row.titleMatches} />
          </span>
          {profile?.origin === "remote" && (
            <span className="shrink-0 rounded bg-zinc-800 px-1 text-[10px] text-zinc-400">
              remote
            </span>
          )}
          <span className="ml-auto flex shrink-0 items-center gap-1.5 pl-1.5">
            {running && (
              <span
                data-testid="chat-status-running"
                aria-label="running"
                className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-amber-400 border-t-transparent"
              />
            )}
            {live && (
              <span
                data-testid="chat-status-live"
                aria-label="live session"
                className="h-2 w-2 rounded-full bg-emerald-400"
              />
            )}
            {isUnread && (
              <span
                data-testid="chat-status-unread"
                aria-label="unread"
                className="h-2 w-2 rounded-full bg-sky-400"
              />
            )}
          </span>
        </span>
        <span className="mt-0.5 w-full truncate pl-1.5 text-xs text-zinc-500">
          {metaLine(chat, now)}
        </span>
      </button>
    </div>
  );
}

// The row title with the search's matched ranges wrapped in <mark> (amber
// tint, per the mock). Ranges index the raw name, so slicing the original
// keeps case and Unicode intact.
function Title({ name, matches }: { name: string; matches: readonly Range[] }) {
  if (matches.length === 0) return <>{name}</>;
  const parts: ReactNode[] = [];
  let from = 0;
  for (const [start, end] of matches) {
    if (start > from) parts.push(name.slice(from, start));
    parts.push(
      <mark key={start} className="rounded-sm bg-amber-400/30 px-0.5 text-amber-100">
        {name.slice(start, end)}
      </mark>,
    );
    from = end;
  }
  if (from < name.length) parts.push(name.slice(from));
  return <>{parts}</>;
}
