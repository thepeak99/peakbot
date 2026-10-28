// Desktop conversation tabs (issue #373): glue between the pure tab store
// (`convoTabs.ts`), the one shared WebSocket (`useAgent`) and the outside
// world (localStorage, the window's keyboard, the conversations poll).
//
// The tab bar is a thin view over the socket's single binding. The user picks
// the active tab (-> `switchConvo`); the server tells us which conversation
// that tab really is on (`agent.convo`) and the store follows, never calling
// `switchConvo` itself.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activate,
  activeTab,
  applyList,
  bound,
  closeTab,
  cycle,
  enter,
  initialTabs,
  jumpTo,
  leave,
  newTab,
  openConvo,
  retitle,
  tabViews,
  type StatusMap,
  type TabView,
  type TabsState,
} from "./convoTabs";
import { loadTabs, saveTabs } from "./tabsPersistence";
import { tabShortcut } from "./tabShortcuts";
import { setConvoInUrl, type AgentConnection } from "./useAgent";

/** How often background tabs' running state is refreshed (only with >1 tab). */
const POLL_MS = 4000;

function safeStorage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** Build the tab state before the first render and point the URL at the
 * active tab, so `useAgent`'s first attach joins the right conversation. The
 * URL wins over storage: the desktop bridge's re-navigation carries no
 * `?convo=`, while an in-page reload keeps it. */
export function bootDesktopTabs(): TabsState {
  const storage = safeStorage();
  const urlConvo = new URLSearchParams(window.location.search).get("convo");
  const state = initialTabs(storage ? loadTabs(storage) : null, urlConvo);
  setConvoInUrl(activeTab(state).convo);
  return state;
}

export interface ConvoTabsApi {
  /** Read-only mirror of the store; App keys one composer slot per tab id. */
  state: TabsState;
  views: TabView[];
  count: number;
  activate(i: number): void;
  close(i: number): void;
  newTab(): void;
  open(convo: string): void;
}

/** `initial === null` (a browser) disables everything and returns `null`.
 * `activeTitle` is the active conversation's name, or `null` while it has no
 * transcript (nothing worth titling a tab with yet). */
export function useConvoTabs(
  agent: AgentConnection,
  initial: TabsState | null,
  activeTitle: string | null,
): ConvoTabsApi | null {
  const enabled = initial !== null;
  const [tabs, setTabs] = useState<TabsState | null>(initial);
  const [status, setStatus] = useState<StatusMap>({});
  const tabsRef = useRef<TabsState | null>(initial);

  // The active tab's running state comes live from the socket. Until the new
  // conversation's first `state` frame lands, `agent.state` still describes
  // the previous one — don't attribute its spinner to the new tab.
  const stateId = agent.state?.conversation?.id;
  const stateIsLive = stateId === undefined || stateId === agent.convo;
  const activeRunning = stateIsLive && (agent.state?.is_running ?? false);

  // Latest values for handlers/timers that must not re-subscribe every render.
  const agentRef = useRef(agent);
  const runningRef = useRef(activeRunning);
  useEffect(() => {
    agentRef.current = agent;
    runningRef.current = activeRunning;
  });

  /** Store-only update (server-driven): never touches the socket. */
  const apply = useCallback((next: TabsState) => {
    if (next === tabsRef.current) return;
    tabsRef.current = next;
    setTabs(next);
  }, []);

  /** User op: update the store and, if the active tab changed, re-attach. */
  const dispatch = useCallback(
    (op: (s: TabsState) => TabsState) => {
      const prev = tabsRef.current;
      if (!prev) return;
      const next = op(prev);
      if (next === prev) return;
      apply(next);
      const from = activeTab(prev);
      const to = activeTab(next);
      if (from === to) return;
      const wasRunning = runningRef.current;
      setStatus((st) => {
        const stillOpen = next.tabs.includes(from);
        const left = stillOpen ? leave(st, from.convo, wasRunning) : st;
        return enter(left, to.convo);
      });
      agentRef.current.switchConvo(to.convo);
    },
    [apply],
  );

  // Follow the server's binding.
  const { convo } = agent;
  useEffect(() => {
    if (convo === null || !tabsRef.current) return;
    apply(bound(tabsRef.current, convo));
  }, [convo, apply]);

  // Title the active tab from its conversation's name.
  useEffect(() => {
    if (convo === null || activeTitle === null || !tabsRef.current) return;
    apply(retitle(tabsRef.current, convo, activeTitle));
  }, [convo, activeTitle, apply]);

  // Fold each conversations list into the background status + inactive titles.
  const { conversations } = agent;
  useEffect(() => {
    const s = tabsRef.current;
    if (!s) return;
    setStatus((st) => applyList(st, conversations, s));
    const activeConvo = activeTab(s).convo;
    let next = s;
    for (const c of conversations) {
      if (c.message_count > 0 && c.id !== activeConvo) {
        next = retitle(next, c.id, c.name);
      }
    }
    apply(next);
  }, [conversations, apply]);

  useEffect(() => {
    const storage = safeStorage();
    if (tabs && storage) saveTabs(storage, tabs);
  }, [tabs]);

  // Poll for background turns, only when there is a background to watch.
  const multi = (tabs?.tabs.length ?? 0) > 1;
  const { connected } = agent;
  useEffect(() => {
    if (!enabled || !connected || !multi) return;
    const id = window.setInterval(() => {
      agentRef.current.send({ type: "request_conversations" });
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [enabled, connected, multi]);

  // Keyboard shortcuts (capture phase, so focused inputs can't swallow them).
  useEffect(() => {
    if (!enabled) return;
    const onKeyDown = (e: KeyboardEvent) => {
      const cmd = tabShortcut(e);
      if (!cmd) return;
      e.preventDefault();
      // Holding Ctrl+W must not chew through every tab.
      if (e.repeat && (cmd.kind === "new" || cmd.kind === "close")) return;
      switch (cmd.kind) {
        case "new":
          dispatch(newTab);
          break;
        case "close":
          dispatch((s) => closeTab(s, s.active));
          break;
        case "cycle":
          dispatch((s) => cycle(s, cmd.delta));
          break;
        case "jump":
          dispatch((s) => jumpTo(s, cmd.n));
          break;
      }
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [enabled, dispatch]);

  const views = useMemo(
    () => (tabs ? tabViews(tabs, status, activeRunning) : []),
    [tabs, status, activeRunning],
  );

  const api = useMemo<ConvoTabsApi | null>(
    () =>
      tabs
        ? {
            state: tabs,
            views,
            count: tabs.tabs.length,
            activate: (i) => dispatch((s) => activate(s, i)),
            close: (i) => dispatch((s) => closeTab(s, i)),
            newTab: () => dispatch(newTab),
            open: (c) => dispatch((s) => openConvo(s, c)),
          }
        : null,
    [tabs, views, dispatch],
  );
  return api;
}
