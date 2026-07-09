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
  error: string | null;
  send: (msg: InboundMessage) => void;
}

function wsUrl(): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${window.location.host}/ws`;
}

/** The `?convo=` id from the address bar, or `null` if absent. */
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
  const [error, setError] = useState<string | null>(null);

  const wsRef = useRef<WebSocket | null>(null);
  const backoffRef = useRef(500); // ms, capped below
  const closedByUs = useRef(false);
  // The conversation this client is bound to. Seeded from the URL, updated by
  // `attached` and by conversation changes; re-sent verbatim on reconnect.
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
            // `/model` / `/load` / `/new` move under us.
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

    connect();

    return () => {
      closedByUs.current = true;
      if (reconnectTimer) window.clearTimeout(reconnectTimer);
      wsRef.current?.close();
    };
  }, []);

  return { connected, state, models, activeAlias, conversations, commands, error, send };
}
