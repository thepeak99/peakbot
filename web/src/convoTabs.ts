// Pure, immutable model of the desktop conversation tab bar. No DOM, no React.
//
// Invariants every op keeps:
//   I1 tabs.length >= 1 — closing the last tab leaves one fresh tab
//   I2 0 <= active < tabs.length
//   I3 non-null `convo` values are unique (several `null` tabs are allowed)
//   I4 ops share structure: tabs that don't change keep their object identity,
//      so `activeTab(prev) === activeTab(next)` means "same active tab"
//   I5 tab ids are unique and never change for the life of a tab; `freshTab`
//      is the only place they are minted (they key the per-tab composers)
// An invalid index returns the state unchanged (same reference).

import type { ConversationSummary } from "./state";

export type TabId = string;

export interface ConvoTab {
  readonly id: TabId;
  /** `null` = a new conversation the server has not bound an id to yet. */
  readonly convo: string | null;
  /** Last known title; "" = untitled. */
  readonly title: string;
}

export interface TabsState {
  readonly tabs: readonly ConvoTab[];
  readonly active: number;
}

export interface TabStatus {
  readonly running: boolean;
  readonly unread: boolean;
}

/** Ephemeral, never persisted. Holds inactive tabs only. */
export type StatusMap = Readonly<Record<string, TabStatus>>;

export const NEW_TAB_LABEL = "New conversation";

let nextTabId = 0;

/** The only id mint site (I5). */
export function freshTab(convo: string | null = null, title = ""): ConvoTab {
  nextTabId += 1;
  return { id: `t${nextTabId}`, convo, title };
}

const validIndex = (s: TabsState, i: number): boolean =>
  Number.isInteger(i) && i >= 0 && i < s.tabs.length;

const indexOfConvo = (s: TabsState, convo: string): number =>
  s.tabs.findIndex((t) => t.convo === convo);

export function activeTab(s: TabsState): ConvoTab {
  return s.tabs[s.active];
}

/** Build the boot state from storage and the URL (the URL wins). */
export function initialTabs(
  stored: TabsState | null,
  urlConvo: string | null,
): TabsState {
  if (!stored) {
    return { tabs: [freshTab(urlConvo)], active: 0 };
  }
  if (urlConvo === null) return stored;
  return openConvo(stored, urlConvo);
}

export function newTab(s: TabsState): TabsState {
  return { tabs: [...s.tabs, freshTab()], active: s.tabs.length };
}

export function activate(s: TabsState, index: number): TabsState {
  if (!validIndex(s, index) || index === s.active) return s;
  return { tabs: s.tabs, active: index };
}

export function closeTab(s: TabsState, index: number): TabsState {
  if (!validIndex(s, index)) return s;
  if (s.tabs.length === 1) return { tabs: [freshTab()], active: 0 };
  const tabs = s.tabs.filter((_, i) => i !== index);
  let active = s.active;
  if (index < active) active -= 1;
  // Closing the active tab: the right neighbour slides into `index`; with no
  // right neighbour fall back to the left one.
  else if (index === active) active = Math.min(index, tabs.length - 1);
  return { tabs, active };
}

export function cycle(s: TabsState, delta: 1 | -1): TabsState {
  const n = s.tabs.length;
  return activate(s, (s.active + delta + n) % n);
}

/** n in 1..8 → tab n (if it exists); 9 → the last tab; anything else: no-op. */
export function jumpTo(s: TabsState, n: number): TabsState {
  if (!Number.isInteger(n) || n < 1 || n > 9) return s;
  if (n === 9) return activate(s, s.tabs.length - 1);
  return activate(s, n - 1);
}

export function openConvo(s: TabsState, convo: string, title = ""): TabsState {
  const i = indexOfConvo(s, convo);
  if (i >= 0) return activate(s, i);
  return { tabs: [...s.tabs, freshTab(convo, title)], active: s.tabs.length };
}

/** The server says the active tab's socket is bound to `convo`; follow it. */
export function bound(s: TabsState, convo: string): TabsState {
  const current = activeTab(s);
  if (current.convo === convo) return s;
  const other = indexOfConvo(s, convo);
  if (other < 0) {
    return withActive(s, { ...current, convo, title: "" });
  }
  // Another tab already shows this conversation: it takes the active slot
  // (keeps I3), so it keeps its own id, title and composer.
  const merged = s.tabs[other];
  const tabs = s.tabs
    .map((t, i) => (i === s.active ? merged : t))
    .filter((_, i) => i !== other);
  return { tabs, active: other < s.active ? s.active - 1 : s.active };
}

function withActive(s: TabsState, tab: ConvoTab): TabsState {
  return {
    tabs: s.tabs.map((t, i) => (i === s.active ? tab : t)),
    active: s.active,
  };
}

export function retitle(s: TabsState, convo: string, title: string): TabsState {
  const i = indexOfConvo(s, convo);
  if (i < 0 || s.tabs[i].title === title) return s;
  return {
    tabs: s.tabs.map((t, j) => (j === i ? { ...t, title } : t)),
    active: s.active,
  };
}

/** Fold a polled conversations list into the background status of inactive
 * tabs. Unread = a turn finished (running → idle) while the tab was inactive. */
export function applyList(
  prev: StatusMap,
  list: readonly ConversationSummary[],
  s: TabsState,
): StatusMap {
  const activeConvo = activeTab(s).convo;
  const byId = new Map(list.map((c) => [c.id, c]));
  const next: Record<string, TabStatus> = {};
  let changed = false;
  for (const t of s.tabs) {
    const id = t.convo;
    if (id === null || id === activeConvo || id in next) continue;
    const before = prev[id];
    const item = byId.get(id);
    if (!item) {
      if (before) next[id] = before; // not listed: leave as it was
      continue;
    }
    const running = item.running ?? false;
    const status: TabStatus = {
      running,
      unread: (before?.unread ?? false) || ((before?.running ?? false) && !running),
    };
    if (before && before.running === status.running && before.unread === status.unread) {
      next[id] = before;
    } else {
      next[id] = status;
      changed = true;
    }
  }
  if (!changed && Object.keys(next).length === Object.keys(prev).length) return prev;
  return next;
}

/** Activating a tab clears its status (and its unread dot). */
export function enter(prev: StatusMap, convo: string | null): StatusMap {
  if (convo === null || !(convo in prev)) return prev;
  const next = { ...prev };
  delete next[convo];
  return next;
}

/** Leaving a tab seeds its status, so a turn that is running as you leave
 * still produces a dot when it finishes. */
export function leave(
  prev: StatusMap,
  convo: string | null,
  running: boolean,
): StatusMap {
  if (convo === null) return prev;
  return { ...prev, [convo]: { running, unread: false } };
}

export interface TabView {
  key: string;
  label: string;
  tooltip: string;
  untitled: boolean;
  active: boolean;
  running: boolean;
  unread: boolean;
}

export function tabViews(
  s: TabsState,
  status: StatusMap,
  activeRunning: boolean,
): TabView[] {
  return s.tabs.map((t, i) => {
    const active = i === s.active;
    const st = t.convo !== null ? status[t.convo] : undefined;
    const label = t.title || NEW_TAB_LABEL;
    return {
      key: t.id,
      label,
      tooltip: label,
      untitled: t.title === "",
      active,
      running: active ? activeRunning : (st?.running ?? false),
      unread: active ? false : (st?.unread ?? false),
    };
  });
}
