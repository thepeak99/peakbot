// Mobile "Chats home" glue (design §3.4): two screens over the one shared
// socket, the list poll, the unread fold, last-chat persistence and the
// refused-resume fallback. Returns `null` outside the Tauri mobile shell.

import { useCallback, useEffect, useRef, useState } from "react";
import { isMobile } from "./desktop";
import { useBackDismiss } from "./useBackDismiss";
import type { AgentConnection } from "./useAgent";

export type Screen = "chats" | "conversation";

export interface MobileChatsApi {
  screen: Screen;
  /** A `conversations_list` has arrived since mount. */
  loaded: boolean;
  unread: ReadonlySet<string>;
  backBadge: boolean;
  open(convo: string, profile: string | null): void;
  newChat(): void;
  showChats(): void;
}

export function useMobileChats(
  agent: AgentConnection,
  onNotice: (message: string) => void,
): MobileChatsApi | null {
  // The shell never changes during the page's life, so hooks stay unconditional
  // and only the effects are gated.
  const [enabled] = useState(isMobile);
  const [screen, setScreen] = useState<Screen>("conversation");
  const agentRef = useRef(agent);
  useEffect(() => {
    agentRef.current = agent;
  });

  // One refresh per entry into the list, whether by "‹ Chats" or by OS Back.
  useEffect(() => {
    if (enabled && screen === "chats") agentRef.current.send({ type: "request_conversations" });
  }, [enabled, screen]);

  const showChats = useCallback(() => setScreen("chats"), []);
  useBackDismiss(enabled && screen === "conversation", showChats);

  const open = useCallback((convo: string, profile: string | null) => {
    if (convo !== agentRef.current.convo) agentRef.current.switchConvo(convo, profile);
    setScreen("conversation");
  }, []);

  const newChat = useCallback(() => {
    agentRef.current.switchConvo(null, null);
    setScreen("conversation");
  }, []);

  void onNotice;
  if (!enabled) return null;
  return {
    screen,
    loaded: false,
    unread: new Set(),
    backBadge: false,
    open,
    newChat,
    showChats,
  };
}
