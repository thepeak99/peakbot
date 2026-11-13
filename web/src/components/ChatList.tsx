// The mobile Chats home screen (design §4.1, PR1): a full-screen overlay with
// a "Chats" header (connection dot, theme/settings), a search
// field, profile filter chips, and grouped chat rows. With ≥ 2 machines the
// header gains a machine pill (design §2.5/§2.7): a Machine sheet, a
// "New chat on…" FAB picker, machine-filtered chips/rows, row tags and the
// offline empty state. All view-model logic (filter/group/meta/machines)
// lives in chatList.ts; this component owns only its local UI state.

import { useCallback, useMemo, useState, type Ref } from "react";
import type { ReactNode } from "react";
import type { ConversationSummary, TabColor, WireMachineStatus } from "../state";
import type { CatalogProfile, MachineId } from "../types";
import {
  deriveMachines,
  filterByMachine,
  filterChats,
  groupChats,
  machineName,
  machineOf,
  relativeTime,
  resolveFilter,
  type ChatRow,
  type MachineFilter,
  type Range,
} from "../chatList";
import { loadMachineFilter, saveMachineFilter } from "../mobileStorage";
import { IB, IBI, MTAG } from "./phoneUi";
import { Sheet } from "./Sheet";
import { ThemeToggle } from "./ThemeToggle";

export interface ChatListProps {
  conversations: ConversationSummary[];
  catalog: CatalogProfile[];
  loaded: boolean;
  unread: ReadonlySet<string>;
  connected: boolean;
  error: string | null;
  onOpen(convo: string, profile: string | null): void;
  /** null = the server-default machine (single-machine path); a profile
   * name = a new chat on that machine's first catalog profile. */
  onNew(profile: string | null): void;
  /** The gear renders only when given (same rule as the TopBar). */
  onOpenSettings?: () => void;
  /** The last `conversations_list`'s machine statuses (`agent.machines`). */
  machineStatus: WireMachineStatus[];
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
  machineStatus,
  ref,
  leaving,
}: ChatListProps) {
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState("");
  // The machine filter: stored once, derived each render. A stale id (the
  // machine left the catalog) or < 2 machines resolves to All; storage is
  // not rewritten, so a re-added remote is restored.
  const [stored, setStored] = useState<MachineFilter>(() => loadMachineFilter(window.localStorage));
  const [machineOpen, setMachineOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const closeMachine = useCallback(() => setMachineOpen(false), []);
  const closePicker = useCallback(() => setPickerOpen(false), []);

  const machines = useMemo(() => deriveMachines(catalog, machineStatus), [catalog, machineStatus]);
  const machineUi = machines.length >= 2;
  const filter = resolveFilter(stored, machines);
  const selectedMachine =
    filter.kind === "one" ? (machines.find((m) => m.id === filter.id) ?? null) : null;
  // With one machine selected a tag would repeat the only possible value.
  const showTags = machineUi && filter.kind === "all";

  // An offline machine shows no rows at all (Z2: no cache) — only the
  // offline empty state, whatever a stale list still carries.
  const offline = selectedMachine !== null && !selectedMachine.online;
  const rows = useMemo(
    () => (offline ? [] : filterByMachine(conversations, catalog, filter)),
    [offline, conversations, catalog, filter],
  );

  // A chip the visible set no longer carries (a foreign machine's profile, a
  // dropped rename) silently falls back to All — derived, not an effect, so
  // the re-render itself is the fix.
  const chips = selectedMachine
    ? catalog.filter((p) => selectedMachine.profiles.includes(p.name))
    : catalog;
  const activeProfile =
    selected !== "" && chips.some((p) => p.name === selected) ? selected : "";
  // `now` is display-only (meta line, day buckets) and re-taken each render;
  // the memo takes its own fresh clock so its deps stay data-stable.
  const now = new Date();
  const searching = query.trim() !== "";
  const sections = useMemo(
    () =>
      groupChats(
        filterChats(rows, query, activeProfile === "" ? null : activeProfile),
        new Date(),
        searching,
      ),
    [rows, query, activeProfile, searching],
  );

  const counts = useMemo(() => {
    const c = new Map<MachineId, number>();
    for (const r of conversations) {
      const id = machineOf(catalog, r.profile ?? null);
      c.set(id, (c.get(id) ?? 0) + 1);
    }
    return c;
  }, [conversations, catalog]);

  const chooseMachine = (f: MachineFilter): void => {
    setStored(f);
    saveMachineFilter(window.localStorage, f);
    setMachineOpen(false);
  };
  // The FAB: with a machine selected it starts on that machine's first
  // catalog profile; on All with machines it opens the picker.
  const fabProfile = selectedMachine ? selectedMachine.profiles[0] : null;
  const fabDisabled = offline;

  const headerActions = (
    <>
      <span
        data-testid="chats-conn"
        aria-label={connected ? "connected" : "offline"}
        className={`mx-1.5 h-2 w-2 shrink-0 rounded-full ${
          connected ? "bg-emerald-400 light:bg-[#16a34a]" : "bg-zinc-500"
        }`}
      />
      <ThemeToggle phone />
      {onOpenSettings && (
        <button
          type="button"
          onClick={onOpenSettings}
          title="Settings"
          aria-label="Settings"
          className={IB}
        >
          <span className={IBI}>
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="h-5 w-5"
          >
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
          </span>
        </button>
      )}
    </>
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
      <header className="shrink-0 px-4 pt-[env(safe-area-inset-top)]">
        {machineUi ? (
          // 3-column grid: title left, machine pill centred, actions right.
          <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-2 pb-2.5 pt-3.5">
            <h1 className="min-w-0 truncate text-[26px] font-bold tracking-[-0.02em]">Chats</h1>
            <MachinePill
              label={filter.kind === "all" ? "All machines" : (selectedMachine?.name ?? "")}
              online={filter.kind === "all" ? true : (selectedMachine?.online ?? false)}
              onOpen={() => setMachineOpen(true)}
            />
            <div className="flex items-center justify-self-end gap-0.5">{headerActions}</div>
          </div>
        ) : (
          <div className="flex items-center gap-2 pb-2.5 pt-3.5">
            <h1 className="text-[26px] font-bold tracking-[-0.02em]">Chats</h1>
            <div className="ml-auto flex shrink-0 items-center gap-0.5">{headerActions}</div>
          </div>
        )}
        <div className="relative">
          <svg
            aria-hidden="true"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="pointer-events-none absolute left-3 top-1/2 h-[18px] w-[18px] -translate-y-1/2 text-zinc-500"
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
            className="h-11 w-full rounded-[14px] border border-zinc-800 bg-zinc-900 pl-[38px] pr-9 text-[15px] text-zinc-100 placeholder-zinc-500 outline-none focus:border-zinc-600"
          />
          {query !== "" && (
            <button
              type="button"
              aria-label="Clear search"
              onClick={() => setQuery("")}
              className="absolute right-2 top-1/2 flex h-[22px] w-[22px] -translate-y-1/2 cursor-pointer items-center justify-center rounded-[11px] bg-zinc-800 text-xs text-zinc-300 transition-colors hover:bg-zinc-700"
            >
              ✕
            </button>
          )}
        </div>
        <div
          className="flex gap-2 overflow-x-auto pb-1.5 pt-2.5"
          role="group"
          aria-label="Filter by profile"
        >
          <Chip
            label="All"
            profile=""
            pressed={activeProfile === ""}
            onSelect={setSelected}
          />
          {chips.map((p) => (
            <Chip
              key={p.name}
              label={p.name}
              profile={p.name}
              color={p.color}
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
      <div className="flex-1 overflow-y-auto overscroll-contain pb-[calc(env(safe-area-inset-bottom)+88px)] pt-1.5">
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
            className="px-6 py-8 text-center text-sm text-zinc-500"
          >
            {searching
              ? `No chats match “${query.trim()}”`
              : selectedMachine
                ? offline
                  ? `${selectedMachine.name} is offline`
                  : `No chats on ${selectedMachine.name} yet`
                : "No chats yet"}
          </div>
        ) : (
          sections.map((s) => (
          <section key={s.kind} data-testid="chat-section" data-kind={s.kind}>
            <h2 className="px-[22px] pb-2 pt-4 text-[11px] font-semibold uppercase tracking-[0.08em] text-zinc-500">
              {s.label}
            </h2>
            {s.rows.map((r) => (
              <Row
                key={r.chat.id}
                row={r}
                now={now}
                catalog={catalog}
                machineTag={showTags ? machineName(machineOf(catalog, r.chat.profile ?? null)) : undefined}
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
        disabled={fabDisabled}
        onClick={() => (machineUi && filter.kind === "all" ? setPickerOpen(true) : onNew(fabProfile))}
        className="absolute bottom-[calc(1.75rem+env(safe-area-inset-bottom))] right-5 z-10 flex h-14 w-14 cursor-pointer items-center justify-center rounded-[19px] bg-emerald-600 text-white shadow-[0_8px_20px_rgba(5,150,105,0.4)] transition-colors hover:bg-emerald-500 disabled:cursor-not-allowed disabled:opacity-50"
      >
        <svg aria-hidden="true" width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <path d="M12 5v14M5 12h14" />
        </svg>
      </button>

      {machineUi && machineOpen && (
        <Sheet title="Machine" onClose={closeMachine}>
          <div className="flex flex-col gap-0.5 px-4 pb-2 pt-0.5">
            <MachineOption
              machine="all"
              label="All machines"
              sub="local + remote"
              online={true}
              count={conversations.length}
              pressed={filter.kind === "all"}
              onPick={() => chooseMachine({ kind: "all" })}
            />
            {machines.map((m) => (
              <MachineOption
                key={m.id ?? "local"}
                machine={m.id ?? "local"}
                label={m.name}
                sub={!m.online ? "offline" : m.id === null ? "local" : "remote"}
                online={m.online}
                count={counts.get(m.id) ?? 0}
                pressed={filter.kind === "one" && filter.id === m.id}
                onPick={() => chooseMachine({ kind: "one", id: m.id })}
              />
            ))}
          </div>
        </Sheet>
      )}

      {machineUi && pickerOpen && (
        <Sheet title="New chat on…" onClose={closePicker}>
          <div className="flex flex-col gap-0.5 px-4 pb-2 pt-0.5">
            {machines.map((m) => (
              <button
                key={m.id ?? "local"}
                type="button"
                data-testid="newchat-option"
                data-machine={m.id ?? "local"}
                disabled={!m.online}
                // One handler for both: the picker's unmount releases its back
                // marker before the chat layer pushes (design §2.6), so one
                // Back leaves the new chat for the list.
                onClick={() => {
                  setPickerOpen(false);
                  onNew(m.profiles[0]);
                }}
                className="flex min-h-14 w-full cursor-pointer items-center gap-3 rounded-[14px] px-2.5 py-2 text-left transition-colors hover:bg-zinc-800/70 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent"
              >
                <Dot online={m.online} />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-[15px] font-semibold text-zinc-100">
                    {m.name}
                  </span>
                  <span className="block text-xs text-zinc-500">
                    {!m.online ? "offline" : m.id === null ? "local" : "remote"}
                  </span>
                </span>
              </button>
            ))}
          </div>
        </Sheet>
      )}
    </section>
  );
}

// One profile filter chip: dot + label. The pressed chip carries the emerald
// border (mock: "All" and the picked profile).
function Chip({
  label,
  profile,
  color,
  pressed,
  onSelect,
}: {
  label: string;
  profile: string;
  color?: TabColor;
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
      className={`flex h-10 shrink-0 cursor-pointer items-center gap-1.5 rounded-full border px-3.5 text-[13.5px] transition-colors ${
        pressed
          ? "border-emerald-600 bg-emerald-600/13 font-semibold text-emerald-400"
          : "border-zinc-800 bg-zinc-900 text-zinc-400 hover:border-zinc-700"
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
    </button>
  );
}

// One chat row: a 3px profile-colour bar at the left edge, the title over a
// `profile · [machine tag] · time` second line. The whole row is the single
// 60px-minimum chat-open tap target.
function Row({
  row,
  now,
  catalog,
  machineTag,
  isUnread,
  onOpen,
}: {
  row: ChatRow;
  now: Date;
  catalog: CatalogProfile[];
  /** The machine's display name in All mode; absent = no tag. */
  machineTag?: string;
  isUnread: boolean;
  onOpen(convo: string, profile: string | null): void;
}) {
  const chat = row.chat;
  // Colour comes from the catalog entry, never derived here; a legacy row
  // (profile null) has no bar.
  const profile = chat.profile
    ? catalog.find((p) => p.name === chat.profile)
    : undefined;
  const color = profile?.color;
  const running = chat.running === true;
  const live = chat.active && !running;
  const rel = relativeTime(chat.updated_at, now);
  return (
    <div data-testid="chat-row" data-convo={chat.id}>
      <button
        type="button"
        data-testid="chat-open"
        onClick={() => onOpen(chat.id, chat.profile ?? null)}
        className="relative flex min-h-[60px] w-full cursor-pointer flex-col items-start gap-1 py-[13px] pl-[22px] pr-[18px] text-left transition-colors hover:bg-zinc-900/60"
      >
        {color && (
          <span
            data-testid="chat-bar"
            aria-hidden="true"
            className="absolute bottom-[10px] left-0 top-[10px] w-[3px] rounded-[2px]"
            style={{ backgroundColor: `var(--profile-${color})` }}
          />
        )}
        <span className="flex w-full items-center gap-2">
          <span className="min-w-0 truncate text-[15px] font-semibold tracking-[-0.01em] text-zinc-100">
            <Title name={chat.name} matches={row.titleMatches} />
          </span>
          <span className="ml-auto flex shrink-0 items-center gap-2">
            {running && (
              <span
                data-testid="chat-status-running"
                aria-label="running"
                className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-amber-500 border-t-transparent"
              />
            )}
            {live && (
              // sr-only: the emerald tint is the visible state; this is for screen readers.
              <span data-testid="chat-status-live" aria-label="live session" className="sr-only">
                live session
              </span>
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
        <span className="flex w-full min-w-0 items-center text-xs text-zinc-500">
          {chat.profile && <span className="truncate">{chat.profile}</span>}
          {machineTag !== undefined && (
            <span data-testid="chat-machine-tag" className={`ml-1.5 ${MTAG}`}>
              {machineTag}
            </span>
          )}
          {rel && <span className="shrink-0">&nbsp;·&nbsp;{rel}</span>}
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

// The centred pill in the Chats header: status dot + label + chevron.
function MachinePill({
  label,
  online,
  onOpen,
}: {
  label: string;
  online: boolean;
  onOpen(): void;
}) {
  return (
    <button
      type="button"
      data-testid="machine-pill"
      aria-label="Choose machine"
      onClick={onOpen}
      className="flex h-[34px] max-w-[45vw] cursor-pointer items-center gap-[7px] rounded-full border border-zinc-800 bg-zinc-900 px-3 text-[13px] font-medium text-zinc-300 transition-colors hover:border-zinc-700"
    >
      <Dot online={online} testid="machine-pill-dot" />
      <span className="truncate">{label}</span>
      <svg aria-hidden="true" width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-zinc-500">
        <path d="M6 9l6 6 6-6" />
      </svg>
    </button>
  );
}

function Dot({ online, testid }: { online: boolean; testid?: string }) {
  return (
    <span
      data-testid={testid}
      data-online={online}
      aria-hidden="true"
      // The light theme paints the emerald #16a34a (same as the chats-conn dot).
      className={`h-2 w-2 shrink-0 rounded-full ${online ? "bg-emerald-400 light:bg-[#16a34a]" : "bg-zinc-600"}`}
    />
  );
}

// One row of the Machine sheet: dot, name, sub, count and a ✓ when picked.
// Offline machines are untappable (the All option passes online).
function MachineOption({
  machine,
  label,
  sub,
  online,
  count,
  pressed,
  onPick,
}: {
  machine: string;
  label: string;
  sub: string;
  online: boolean;
  count: number;
  pressed: boolean;
  onPick(): void;
}) {
  return (
    <button
      type="button"
      data-testid="machine-option"
      data-machine={machine}
      aria-pressed={pressed}
      disabled={!online}
      onClick={onPick}
      className="flex min-h-14 w-full cursor-pointer items-center gap-3 rounded-[14px] px-2.5 py-2 text-left transition-colors hover:bg-zinc-800/70 disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:bg-transparent"
    >
      <Dot online={online} />
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[15px] font-semibold text-zinc-100">{label}</span>
        <span data-testid="machine-option-sub" className="block text-xs text-zinc-500">
          {sub}
        </span>
      </span>
      <span data-testid="machine-option-count" className="font-mono text-[12.5px] font-medium text-zinc-500">
        {count}
      </span>
      <span aria-hidden="true" className={`w-4 text-emerald-400 ${pressed ? "" : "invisible"}`}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round">
          <path d="M5 12.5l4.5 4.5L19 7.5" />
        </svg>
      </span>
    </button>
  );
}
