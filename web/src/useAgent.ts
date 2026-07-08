// WebSocket client hook. Connects to `GET /ws`, parses protocol frames
// (state.ts `OutboundMessage`), and exposes the latest `AppState`, the
// model list, connection status, and a `send` for inbound messages.
//
// Under Option C (webui.md §10) each connection is a fresh agent session,
// so a reconnect yields a *new* transcript — restore prior history with
// `/load <id>`, not by reattaching.

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
          case "models_available":
            setModels(msg.models);
            setActiveAlias(msg.active);
            break;
          case "state":
            setState(msg.state);
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
