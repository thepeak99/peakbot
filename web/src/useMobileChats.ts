// Mobile "Chats home" glue (design §3.4): two screens over the one shared
// socket, the list poll, the unread fold, last-chat persistence and the
// refused-resume fallback. Returns `null` outside the Tauri mobile shell.

import { useCallback, useEffect, useRef, useState } from "react";
import { isMobile } from "./desktop";
import { useBackDismiss } from "./useBackDismiss";
import { foldActivity, NO_ACTIVITY, type Activity } from "./chatList";
import { saveLastChat } from "./mobileStorage";
import { profileFromUrl, type ConversationSummary } from "./state";
import type { AgentConnection } from "./useAgent";

export type Screen = "chats" | "conversation";

export interface MobileChatsApi {
  screen: Screen;
  /** A `conversations_list` has arrived since mount. */
  loaded: boolean;
  unread: ReadonlySet<string>;
  open(convo: string, profile: string | null): void;
  /** null = the server-default machine; a profile name = that machine's. */
  newChat(profile: string | null): void;
  showChats(): void;
}

/** How often the bound list is refreshed while the socket is up. */
const POLL_MS = 4000;

export function useMobileChats(
  agent: AgentConnection,
  onNotice: (message: string) => void,
): MobileChatsApi | null {
  // The shell never changes during the page's life, so hooks stay unconditional
  // and only the effects are gated.
  const [enabled] = useState(isMobile);
  const [screen, setScreen] = useState<Screen>("conversation");
  // Folded running/unread, derived from each fresh conversations array. State
  // (not a ref) so a changed fold re-renders; `loaded` flips on the first list
  // that arrives after mount.
  const [activity, setActivity] = useState<Activity>(NO_ACTIVITY);
  const [loaded, setLoaded] = useState(false);
  const listRef = useRef<readonly ConversationSummary[] | null>(null);
  // A refused resume falls back exactly once; the flag (not the condition)
  // guards it, since the re-attach is what clears the triggering state.
  const refusedRef = useRef(false);
  const agentRef = useRef(agent);
  useEffect(() => {
    agentRef.current = agent;
  });

  // One refresh per entry into the list, whether by "‹ Chats" or by OS Back.
  useEffect(() => {
    if (enabled && screen === "chats") agentRef.current.send({ type: "request_conversations" });
  }, [enabled, screen]);

  // A bound conversation is the one to resume next boot; a null binding saves
  // nothing so a fresh chat never overwrites the stored last chat.
  useEffect(() => {
    if (!enabled || agent.convo === null) return;
    saveLastChat(window.localStorage, { convo: agent.convo, profile: profileFromUrl() });
  }, [enabled, agent.convo]);

  // The connect edge refreshes once, then the list stays current on a timer;
  // a hidden page skips its tick (read live, never cached).
  useEffect(() => {
    if (!enabled || !agent.connected) return;
    agentRef.current.send({ type: "request_conversations" });
    const id = window.setInterval(() => {
      if (document.hidden) return;
      agentRef.current.send({ type: "request_conversations" });
    }, POLL_MS);
    return () => window.clearInterval(id);
  }, [enabled, agent.connected]);

  // Each fresh conversations array is folded against the chat on screen; the
  // mount-time array only sets the baseline and leaves `loaded` false.
  useEffect(() => {
    if (!enabled) return;
    const list = agent.conversations;
    if (listRef.current === list) return;
    const first = listRef.current === null;
    listRef.current = list;
    const viewing = screen === "conversation" ? agent.convo : null;
    setActivity((prev) => foldActivity(prev, list, viewing));
    if (!first) setLoaded(true);
  }, [enabled, agent.conversations, screen, agent.convo]);

  // A stored chat the server refuses to reopen (no state, an error, a bound
  // convo): tell the user and re-attach to a fresh chat, exactly once.
  useEffect(() => {
    if (!enabled) return;
    if (agent.state !== null || agent.error === null || agent.convo === null) return;
    if (refusedRef.current) return;
    refusedRef.current = true;
    onNotice(`Couldn't reopen your last chat — ${agent.error}`);
    agentRef.current.switchConvo(null, null);
  }, [enabled, agent.state, agent.error, agent.convo, onNotice]);

  const showChats = useCallback(() => setScreen("chats"), []);
  useBackDismiss(enabled && screen === "conversation", showChats);

  const open = useCallback((convo: string, profile: string | null) => {
    if (convo !== agentRef.current.convo) agentRef.current.switchConvo(convo, profile);
    setScreen("conversation");
  }, []);

  const newChat = useCallback((profile: string | null) => {
    agentRef.current.switchConvo(null, profile);
    setScreen("conversation");
  }, []);

  if (!enabled) return null;
  const unread = activity.unread;
  return {
    screen,
    loaded,
    unread,
    open,
    newChat,
    showChats,
  };
}
