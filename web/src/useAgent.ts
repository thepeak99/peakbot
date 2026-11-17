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
//
// Liveness: the server beats a `heartbeat` frame every 20 s. Once a socket
// has produced one, a watchdog (liveness.ts) restarts it after > 45 s of
// silence; resume signals (tab visible, bfcache restore, network back,
// mobile `peakbot-resume`) restart it at once when it is not provably
// healthy. A server that never beats never arms the watchdog.

import { useCallback, useEffect, useRef, useState } from "react";
import { WATCHDOG_TICK_MS, heard, shouldReconnect } from "./liveness";
import { createLastSeen } from "./lastSeen";
import { profileFromUrl, setConvoInUrl } from "./state";
import type {
  AppState,
  ConversationSummary,
  DirListing,
  InboundMessage,
  ModelInfo,
  SlashCommand,
  WireMachineStatus,
} from "./state";

export interface AgentConnection {
  connected: boolean;
  /** Always describes `convo` (or is null). Live once this socket's first
   *  `state` frame lands; before that, right after a binding move, it is the
   *  conversation's last-seen snapshot with in-flight-turn fields quiesced
   *  (lastSeen.ts), or null if none is held. A reconnect of the same
   *  conversation keeps the live snapshot. */
  state: AppState | null;
  models: ModelInfo[];
  activeAlias: string;
  conversations: ConversationSummary[];
  conversationsError: string | null;
  /** Machine statuses of the last `conversations_list` frame (remote
   * servers only); `[]` when the frame omitted them (old/no-remote server).
   * A fresh frame replaces the previous snapshot. */
  machines: WireMachineStatus[];
  commands: SlashCommand[];
  /** Latest `dir_listing` reply for the cwd picker (request/response over the
   * socket, not part of AppState — browsing is ephemeral). `null` until the
   * first `list_dir` is answered. */
  dirListing: DirListing | null;
  /** Most-recently-used directories for the cwd picker's "Recent" section
   * (newest-first, deduped, existing dirs only, cwd excluded, max 8). Empty
   * until the first `request_recent_dirs` is answered. */
  recentDirs: string[];
  /** Set after receiving the `no_local_profile` frame: the local config has
   * no profiles to attach to. The SPA shows the cloud picker instead of
   * reconnecting. */
  noLocal: boolean;
  error: string | null;
  send: (msg: InboundMessage) => void;
  /** The conversation this socket is bound to: seeded from the URL, then
   * following the server's `attached` / `state` (which `/new`, `/model` and
   * `/load` move under us). `null` until a fresh session is bound. */
  convo: string | null;
  /** Switch this client to another conversation by re-attaching: bind the new
   * id and reconnect the socket. Shares the live session if one is active,
   * else resumes it from disk — the same handshake as first load, so no
   * `/load` (which would clear background processes). `null` starts a new
   * conversation (the server mints the session and replies `attached`).
   * `profile` (re-)binds the socket to a catalogue profile — remote profiles
   * route through the relay; `null` means local. `agent` rides the ONE
   * attach of the fresh socket, so the new conversation starts on that pair.
   * `firstFrame` is held and sent exactly once on the attach that succeeds —
   * the busy-session seam (a session verb can't ride the live socket); a
   * later switchConvo overwrites it, so it never reaches a different
   * conversation. A tagged wire frame (callers pass fresh literals, so the
   * `type` tag is the contract, not the `InboundMessage` union). */
  switchConvo: (
    id: string | null,
    profile?: string | null,
    agent?: string,
    firstFrame?: { type: string; [key: string]: unknown },
  ) => void;
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

export function useAgent(): AgentConnection {
  const [connected, setConnected] = useState(false);
  const [state, setState] = useState<AppState | null>(null);
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [activeAlias, setActiveAlias] = useState("");
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [conversationsError, setConversationsError] = useState<string | null>(
    null,
  );
  const [machines, setMachines] = useState<WireMachineStatus[]>([]);
  const [commands, setCommands] = useState<SlashCommand[]>([]);
  const [dirListing, setDirListing] = useState<DirListing | null>(null);
  const [recentDirs, setRecentDirs] = useState<string[]>([]);
  const [noLocal, setNoLocal] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // One per hook instance (lazy): the last-seen cache behind the invariant
  // "`state` always describes `convo`" (design §3).
  const [lastSeen] = useState(createLastSeen);

  const wsRef = useRef<WebSocket | null>(null);
  const restartRef = useRef<() => void>(() => {});
  const backoffRef = useRef(500); // ms, capped below
  // Per-socket liveness clock (liveness.ts): null until this socket's first
  // `heartbeat`, so a server that never beats never trips the watchdog.
  const lastHeardRef = useRef<number | null>(null);
  // The conversation this client is bound to. Seeded from the URL (the only
  // binding), updated by `attached`/state, re-sent verbatim on reconnect.
  const convoRef = useRef<string | null>(convoFromUrl());
  // The profile this socket attaches under (the routing key, design §6.4).
  // Seeded from the URL, re-sent verbatim on EVERY reconnect — a retry is
  // also remote (G7). `null` = local; the key is omitted from the frame.
  const profileRef = useRef<string | null>(profileFromUrl());
  // Rides exactly one attach (a switchConvo with an agent), then clears, so
  // reconnects re-send only (convo, profile).
  const attachAgentRef = useRef<string | null>(null);
  // A session verb held for the fresh conversation's first attach. Sent once
  // on the `attached` that succeeds; survives a reconnect before then.
  const pendingFrameRef = useRef<{ type: string; [key: string]: unknown } | null>(null);
  const [convo, setConvo] = useState<string | null>(convoRef.current);
  // Pending reconnect timer; shared so `switchConvo` can cancel it.
  const reconnectTimerRef = useRef<number | undefined>(undefined);

  // Single place that moves the binding: ref (for reconnects), state (for
  // consumers) and the URL (refresh / bookmark / share). `profile` is only
  // written when given — server echoes move the convo, never the profile.
  const bindConvo = useCallback((id: string | null, profile?: string | null) => {
    convoRef.current = id;
    if (profile !== undefined) profileRef.current = profile;
    setConvo(id);
    setConvoInUrl(id, profileRef.current);
  }, []);

  // Move the binding to a conversation this socket has no live frame for:
  // swap `state` to its last-seen snapshot (or null) so it can never
  // describe the previous conversation (design §4.1).
  const rebind = useCallback((id: string | null, profile?: string | null) => {
    bindConvo(id, profile);
    setState(lastSeen.recall(profileRef.current, id));
    // The previous socket's error / model alias are not this convo's.
    setError(null);
    setActiveAlias("");
  }, [bindConvo, lastSeen]);

  // History entries keep the URL they were created with, so a back-traversal
  // can show a conversation we no longer are bound to — re-sync to the binding.
  useEffect(() => {
    const onPopstate = () => setConvoInUrl(convoRef.current, profileRef.current);
    window.addEventListener("popstate", onPopstate);
    return () => window.removeEventListener("popstate", onPopstate);
  }, []);

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
    const connect = () => {
      const ws = new WebSocket(wsUrl());
      wsRef.current = ws;
      // A replacement socket starts disarmed: its own first heartbeat arms it.
      lastHeardRef.current = null;

      ws.onopen = () => {
        setConnected(true);
        setError(null);
        backoffRef.current = 500;
        // First frame binds this socket to a session (or reconnects to it).
        // `profile` is the routing key — omitted when unknown, never dropped
        // once known (a reconnect after an error is also remote, G7).
        const attach: { type: "attach"; convo: string | null; profile?: string; agent?: string } = {
          type: "attach",
          convo: convoRef.current,
        };
        if (profileRef.current !== null) attach.profile = profileRef.current;
        const agent = attachAgentRef.current;
        if (agent !== null) {
          attach.agent = agent;
          attachAgentRef.current = null;
        }
        ws.send(JSON.stringify(attach));
      };

      ws.onmessage = (ev) => {
        let msg;
        try {
          msg = JSON.parse(ev.data as string);
        } catch {
          return;
        }
        // Any frame proves the pipe is live; only a `heartbeat` arms the clock.
        lastHeardRef.current = heard(lastHeardRef.current, msg.type, Date.now());
        switch (msg.type) {
          case "ready":
            break;
          case "attached":
            // A reply with a different id (fresh-fallback, a null attach's
            // mint) means nothing we hold describes it: a rebind.
            if (msg.convo === convoRef.current) bindConvo(msg.convo);
            else rebind(msg.convo);
            {
              // The held frame rides the attach that succeeds; clearing it
              // first makes a second `attached` a no-op.
              const held = pendingFrameRef.current;
              if (held !== null) {
                pendingFrameRef.current = null;
                ws.send(JSON.stringify(held));
              }
            }
            break;
          case "models_available":
            setModels(msg.models);
            setActiveAlias(msg.active);
            break;
          case "state":
            setState(msg.state);
            // Cache the live snapshot for a later switch-back (design §4.2).
            lastSeen.remember(profileRef.current, msg.state);
            // Keep the URL on the session's current conversation, which
            // `/model` / `/new` move under us.
            {
              const id = msg.state?.conversation?.id;
              if (id && id !== convoRef.current) bindConvo(id);
            }
            break;
          case "conversations_list":
            setConversations(msg.items);
            setConversationsError(msg.error ?? null);
            setMachines(msg.machines ?? []);
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
          case "heartbeat":
            break;
          case "no_local_profile":
            // Zero-local config: nothing to attach to. Detach handlers so the
            // subsequent server-side close doesn't trigger a reconnect loop.
            setNoLocal(true);
            drop();
            break;
        }
      };

      ws.onclose = () => {
        // Only unasked closes reach here: every intentional close goes
        // through drop(), which detaches the handlers before closing.
        setConnected(false);
        scheduleReconnect();
      };

      ws.onerror = () => {
        ws.close();
      };
    };

    const scheduleReconnect = () => {
      // Reconnect with capped exponential backoff.
      const delay = backoffRef.current;
      backoffRef.current = Math.min(delay * 2, 8000);
      reconnectTimerRef.current = window.setTimeout(connect, delay);
    };

    const drop = () => {
      window.clearTimeout(reconnectTimerRef.current);
      const ws = wsRef.current;
      if (ws) {
        // Detach before close: the queued close event must not reach a
        // replaced socket's handlers and schedule a duplicate dial.
        ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
        ws.close();
      }
      setConnected(false);
    };

    const restart = () => {
      backoffRef.current = 500;
      drop();
      connect();
    };

    restartRef.current = restart;

    const kick = () => {
      // A "we are back" signal: any non-OPEN socket (or a stale OPEN one)
      // is suspect — a suspended browser's socket is usually dead in place.
      if (
        shouldReconnect("kick", wsRef.current?.readyState ?? null, lastHeardRef.current, Date.now())
      ) {
        restart();
      }
    };

    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") kick();
    };
    const onPageshow = (ev: PageTransitionEvent) => {
      // A bfcache restore: the socket survived neither the navigation nor the cache.
      if (ev.persisted) kick();
    };
    const onOnline = () => kick();
    const onResume = () => kick();
    const onOffline = () => {
      drop();
      scheduleReconnect();
    };

    // Wall-clock staleness, not a tick count: a suspended tab wakes with a
    // large Date.now() gap and the first tick after it sees a dead socket.
    const tick = () => {
      if (
        document.visibilityState === "visible" &&
        shouldReconnect("tick", wsRef.current?.readyState ?? null, lastHeardRef.current, Date.now())
      ) {
        restart();
      }
    };
    const watchdog = window.setInterval(tick, WATCHDOG_TICK_MS);

    document.addEventListener("visibilitychange", onVisibilityChange);
    window.addEventListener("pageshow", onPageshow);
    window.addEventListener("online", onOnline);
    window.addEventListener("peakbot-resume", onResume);
    window.addEventListener("offline", onOffline);

    connect();

    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      window.removeEventListener("pageshow", onPageshow);
      window.removeEventListener("online", onOnline);
      window.removeEventListener("peakbot-resume", onResume);
      window.removeEventListener("offline", onOffline);
      window.clearInterval(watchdog);
      window.clearTimeout(reconnectTimerRef.current);
      drop();
    };
  }, [bindConvo, rebind]);

  // Re-attach to another conversation: bind the id, then restart at once.
  // Closing the old socket detaches from (but does not kill) its session; the
  // fresh connect's `attach` frame joins the target — sharing it if live,
  // resuming it from disk if not. restart() cancels the pending backoff
  // reconnect and detaches the old socket's handlers first, so a switch can
  // never leave two sockets (or a stale `onclose` scheduling a third).
  // `profile`/`agent` are the chip's reattach (design §6.4): same convo with
  // a new profile still re-attaches, and the agent rides that one attach.
  const switchConvo = useCallback(
    (
      id: string | null,
      profile?: string | null,
      agent?: string,
      firstFrame?: { type: string; [key: string]: unknown },
    ) => {
      const profileChanged = profile !== undefined && profile !== profileRef.current;
      if (id === convoRef.current && !profileChanged && agent === undefined) return;
      rebind(id, profile);
      attachAgentRef.current = agent ?? null;
      // Every switch overwrites the held frame, so a verb can never ride
      // into a conversation the user did not ask for.
      pendingFrameRef.current = firstFrame ?? null;
      restartRef.current();
    },
    [rebind],
  );

  return {
    connected,
    state,
    models,
    activeAlias,
    conversations,
    conversationsError,
    machines,
    commands,
    dirListing,
    recentDirs,
    noLocal,
    error,
    send,
    convo,
    switchConvo,
  };
}
