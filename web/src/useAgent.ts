// WebSocket client hook. Connects to `GET /ws`, parses protocol frames
// (state.ts `OutboundMessage`), and exposes the latest `AppState`, the
// model list, connection status, and a `send` for inbound messages.
//
// Sticky sessions (issue #118): the conversation id rides in the URL
// (`?convo=…`). On connect the client sends `attach {convo}`; the server
// binds to (or resumes / mints) that session and replies `attached {convo}`.
// A reconnect re-sends the same id, so a dropped socket rejoins the *same*
// live session instead of starting a fresh transcript. The URL is kept in
// sync with the session's current conversation so refresh/bookmark/share all
// land on the same session.

import { useCallback, useEffect, useRef, useState } from "react";
import type {
  AppState,
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
  SlashCommand,
} from "./state";

export interface AgentConnection {
  connected: boolean;
  state: AppState | null;
  models: ModelInfo[];
  activeAlias: string;
  conversations: ConversationSummary[];
  commands: SlashCommand[];
  /** Latest `dir_listing` reply for the cwd picker (request/response over the
   * socket, not part of AppState — browsing is ephemeral). `null` until the
   * first `list_dir` is answered. */
  dirListing: DirListing | null;
  /** Most-recently-used directories for the cwd picker's "Recent" section
   * (newest-first, deduped, existing dirs only, cwd excluded, max 8). Empty
   * until the first `request_recent_dirs` is answered. */
  recentDirs: string[];
  error: string | null;
  send: (msg: InboundMessage) => void;
  /** Switch this client to another conversation by re-attaching: bind the new
   * id and reconnect the socket. Shares the live session if one is active,
   * else resumes it from disk — the same handshake as first load, so no
   * `/load` (which would clear background processes). */
  switchConvo: (id: string) => void;
}

function wsUrl(): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}/ws`;
}

/** The `?convo=` id from the address bar, or `null` if absent. The URL is the
 * *only* binding: a new window with no `?convo=` starts a fresh conversation;
 * a refresh, reopened same-URL tab, or shared link rejoins the same session. */
function convoFromUrl(): string | null {
  return new URLSearchParams(window.location.search).get("convo");
}

/** Write `convo` into the address bar without a navigation (idempotent). */
function setConvoInUrl(convo: string): void {
  const url = new URL(window.location.href);
  if (url.searchParams.get("convo") === convo) return;
  url.searchParams.set("convo", convo);
  window.history.replaceState(null, "", url);
}

export function useAgent(): AgentConnection {
  const [connected, setConnected] = useState(false);
  const [state, setState] = useState<AppState | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [activeAlias, setActiveAlias] = useState("");
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const [dirListing, setDirListing] = useState<DirListing | null>(null);
  const [recentDirs, setRecentDirs] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  const wsRef = useRef<WebSocket | null>(null);
  const connectRef = useRef<() => void>(() => {});
  const backoffRef = useRef(500); // ms, capped below
  const closedByUs = useRef(false);
  // The conversation this client is bound to. Seeded from the URL (the only
  // binding), updated by `attached`/state, re-sent verbatim on reconnect.
  const convoRef = useRef<string | null>(convoFromUrl());

  const send = useCallback((msg: InboundMessage) => {
    const ws = wsRef.current;
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify(msg));
    }
  }, []);

  // Slash-command list is a global constant — fetch it once, not per WS
  // connection. On failure the palette is simply empty (a typed command
  // still dispatches server-side).
  useEffect(() => {
    let cancelled = false;
    fetch("/commands")
      .then((r) => (r.ok ? r.json() : []))
      .then((cmds: SlashCommand[]) => {
        if (!cancelled) setCommands(cmds);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    closedByUs.current = false;
    let reconnectTimer: number | undefined;

    const connect = () => {
      const ws = new WebSocket(wsUrl());
      wsRef.current = ws;

      ws.onopen = () => {
        setConnected(true);
        setError(null);
        backoffRef.current = 500;
        // First frame binds this socket to a session (or reconnects to it).
        ws.send(JSON.stringify({ type: "attach", convo: convoRef.current }));
      };

      ws.onmessage = (ev) => {
        let msg;
        try {
          msg = JSON.parse(ev.data as string);
        } catch {
          return;
        }
        switch (msg.type) {
          case "ready":
            break;
          case "attached":
            convoRef.current = msg.convo;
            setConvoInUrl(msg.convo);
            break;
          case "models_available":
            setModels(msg.models);
            setActiveAlias(msg.active);
            break;
          case "state":
            setState(msg.state);
            // Keep the URL on the session's current conversation, which
            // `/model` / `/new` move under us.
            {
              const id = msg.state?.conversation?.id;
              if (id && id !== convoRef.current) {
                convoRef.current = id;
                setConvoInUrl(id);
              }
            }
            break;
          case "conversations_list":
            setConversations(msg.items);
            break;
          case "dir_listing":
            setDirListing({
              path: msg.path,
              parent: msg.parent,
              entries: msg.entries,
              error: msg.error,
            });
            break;
          case "recent_dirs":
            setRecentDirs(msg.dirs);
            break;
          case "error":
            setError(msg.message);
            break;
        }
      };

      ws.onclose = () => {
        setConnected(false);
        if (closedByUs.current) return;
        // Reconnect with capped exponential backoff.
        const delay = backoffRef.current;
        backoffRef.current = Math.min(delay * 2, 8000);
        reconnectTimer = window.setTimeout(connect, delay);
      };

      ws.onerror = () => {
        ws.close();
      };
    };

    connectRef.current = connect;
    connect();

    return () => {
      closedByUs.current = true;
      if (reconnectTimer) window.clearTimeout(reconnectTimer);
      wsRef.current?.close();
    };
  }, []);

  // Re-attach to another conversation: bind the id, then reconnect. Closing
  // the old socket detaches from (but does not kill) its session; the fresh
  // connect's `attach` frame joins the target — sharing it if live, resuming
  // it from disk if not.
  const switchConvo = useCallback((id: string) => {
    if (id === convoRef.current) return;
    convoRef.current = id;
    setConvoInUrl(id);
    backoffRef.current = 500;
    const ws = wsRef.current;
    if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
      // onclose auto-reconnects (closedByUs is false) and re-sends `attach`
      // with the new convoRef.
      ws.close();
    } else {
      connectRef.current();
    }
  }, []);

  return {
    connected,
    state,
    models,
    activeAlias,
    conversations,
    commands,
    dirListing,
    recentDirs,
    error,
    send,
    switchConvo,
  };
}
