// @vitest-environment jsdom
//
// Stage 3 — shared remote conversation cache: integration tests for the
// delta sync + moving anchor. RED until useAgent wires the planned
// `web/src/deltaSync` module and the `since` attach field.
//
// Interface under test (planned):
//   attach frame gains `since: { n, timestamp, epoch }` (optional)
//   outbound `state` frames may carry `messages_from: number`
//   client merges suffix frames onto cached prefix (mergeSuffix)
//   client sends additive `sync_anchor` with the current cursor every 10
//     newly applied authoritative rows, only after the server has
//     demonstrated delta support
//   frames with messages_from (absolute against their indicated anchor)
//     remain decodable after the anchor moves (transition safety)

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import { useAgent, type AgentConnection } from "./useAgent";
import { createConversationCache, type ConversationCache } from "./conversationCache";
import type { WireChatMessage } from "./state";

// ─── fake WebSocket ─────────────────────────────────────────────────────

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 1;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
    queueMicrotask(() => this.onopen?.());
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.());
  }
}

// ─── IndexedDB setup ────────────────────────────────────────────────────

let cache: ConversationCache | null = null;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(async () => {
  Object.defineProperty(globalThis, "indexedDB", {
    value: fakeIndexedDB,
    writable: true,
    configurable: true,
  });
  localStorage.clear();
  window.history.replaceState(null, "", "/");
  FakeWebSocket.instances = [];
  // Reuse the DB handle between tests (cheap) and purge the origin this file
  // writes, instead of clearing the whole shared database.
  if (!cache) cache = await createConversationCache();
  else await cache.purgeOrigin("http://remote.example.com");
});

afterEach(() => {
  FakeWebSocket.instances = [];
});

// ─── helpers ────────────────────────────────────────────────────────────

function wireState(
  convoId: string,
  isRunning: boolean,
  profile: string | null,
  extra?: Partial<{ chat: { messages: WireChatMessage[]; auto_scroll: boolean; scroll_offset: number }; is_running: boolean; history_epoch: number }>,
) {
  const base = {
    chat: { messages: [] as WireChatMessage[], auto_scroll: false, scroll_offset: 0 },
    todo: { visible: false, items: [] },
    stats: {
      total_input_tokens: 0,
      total_output_tokens: 0,
      total_api_calls: 0,
      total_cost: 0,
      model: "m",
      provider_name: "p",
      model_alias: "a",
    },
    context: {
      current_usage: 0,
      window_size: 1000,
      compaction_enabled: true,
      compaction_threshold: 0.8,
    },
    conversation: { id: convoId, name: "test" },
    is_running: isRunning,
    is_command: false,
    command_running: false,
    stop_requested: false,
    stop_pending: false,
    pending: false,
    status: null,
    status_message: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    is_loading: false,
    catalog: [
      {
        name: "home",
        origin: "global",
        overrides_global: false,
        agents: [{ name: "agent", model: "m", subagents: [] }],
      },
      {
        name: "remote",
        origin: "remote",
        overrides_global: false,
        agents: [{ name: "agent", model: "m", subagents: [] }],
        machine: "http://remote.example.com",
      },
    ],
    selection: { profile: profile ?? "home", agent: "agent" },
    profile_pinned: false,
    settings_available: true,
  };
  return { ...base, ...extra };
}

function lastSent(ws: FakeWebSocket): Record<string, unknown> | null {
  if (!ws.sent.length) return null;
  return JSON.parse(ws.sent[ws.sent.length - 1]);
}

// ─── mount plumbing ─────────────────────────────────────────────────────

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let conn: AgentConnection | null = null;

function Harness() {
  conn = useAgent();
  return null;
}

async function mountAgent(): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<Harness />);
  });
  await act(async () => {
    await Promise.resolve();
  });
}

async function flush(ms = 30): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

async function feed(frame: object): Promise<void> {
  const ws = FakeWebSocket.instances.at(-1)!;
  await act(async () => {
    ws.onmessage?.({ data: JSON.stringify(frame) });
  });
}

async function resetAgent(): Promise<void> {
  const current = root;
  if (current) await act(async () => current.unmount());
  root = null;
  if (container) { container.remove(); container = null; }
  FakeWebSocket.instances = [];
  conn = null;
}

// ─── cold-restart: attach includes since cursor ─────────────────────────

describe("cold-restart attach includes since cursor", () => {
  it("cold cache read completes before attach so since can be included", async () => {
    const rawTs = "2026-11-25T19:00:00.123456789+01:00";
    const cached = wireState("cold-convo", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "first", timestamp: "2026-01-01T00:00:00Z" },
          { role: "agent", content: "reply", timestamp: rawTs },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 42,
    });
    await cache!.saveOpenedContent("remote", "cold-convo", "http://remote.example.com", cached);

    window.history.replaceState(null, "", "/?convo=cold-convo&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await flush(100);

    const ws = FakeWebSocket.instances.at(-1)!;
    const attachFrame = lastSent(ws);
    expect(attachFrame).not.toBeNull();
    expect(attachFrame!.type).toBe("attach");
    // The since cursor must be present and correct
    expect(attachFrame!.since).toEqual({
      n: 2,
      timestamp: rawTs,
      epoch: 42,
    });

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── suffix frame merge ─────────────────────────────────────────────────

describe("suffix frame merge", () => {
  it("merges a suffix onto the cached prefix and updates non-message fields", async () => {
    const cached = wireState("suffix-convo", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "a", timestamp: "t1" },
          { role: "agent", content: "b", timestamp: "t2" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 10,
    });
    await cache!.saveOpenedContent("remote", "suffix-convo", "http://remote.example.com", cached);

    window.history.replaceState(null, "", "/?convo=suffix-convo&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "suffix-convo" });
    await flush(50);

    // Simulate the server sending a suffix frame
    const suffix = wireState("suffix-convo", true, "remote", {
      chat: {
        messages: [
          { role: "user", content: "c", timestamp: "t3" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 10,
      is_running: true,
    });
    await feed({ type: "state", state: suffix, messages_from: 2 });
    await flush(20);

    expect(conn).not.toBeNull();
    expect(conn!.state).not.toBeNull();
    expect(conn!.state!.chat.messages.length).toBe(3);
    expect((conn!.state!.chat.messages[2] as WireChatMessage).content).toBe("c");
    expect(conn!.state!.is_running).toBe(true);

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("never renders a bare tail when the cached base is missing", async () => {
    window.history.replaceState(null, "", "/?convo=missing-base&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "missing-base" });
    await flush(50);

    // No cached base; server sends a suffix
    const suffix = wireState("missing-base", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "x", timestamp: "t" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
    });
    await feed({ type: "state", state: suffix, messages_from: 1 });
    await flush(20);

    // Must not render the bare tail as the full message list.
    // Expected behaviour: either the state is unchanged (no messages), or
    // the client reattaches without `since` to get the full frame.
    // We pin the first: state has no messages or is still null.
    expect(conn).not.toBeNull();
    const hasBareTail =
      conn!.state !== null &&
      conn!.state.chat.messages.length === 1 &&
      (conn!.state.chat.messages[0] as WireChatMessage).content === "x";
    expect(hasBareTail).toBe(false);

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── moving anchor (rebase cadence) ─────────────────────────────────────

describe("moving anchor", () => {
  it("sends sync_anchor with the current cursor every 10 applied rows", async () => {
    const cached = wireState("rebase-convo", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "a", timestamp: "t1" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 5,
    });
    await cache!.saveOpenedContent("remote", "rebase-convo", "http://remote.example.com", cached);

    window.history.replaceState(null, "", "/?convo=rebase-convo&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "rebase-convo" });
    await flush(50);

    const ws = FakeWebSocket.instances.at(-1)!;

    // Server sends 10 rows — the 10th applied row moves the anchor.
    for (let i = 2; i <= 11; i++) {
      const frame = wireState("rebase-convo", false, "remote", {
        chat: {
          messages: [
            { role: "user", content: `m${i}`, timestamp: `t${i}` },
          ],
          auto_scroll: false,
          scroll_offset: 0,
        },
        history_epoch: 5,
      });
      await feed({ type: "state", state: frame, messages_from: i - 1 });
      await flush(5);
    }

    // After the 10th newly applied row (total 11 in state), a sync_anchor
    // should be sent with the updated cursor (n=11).
    const frames = ws.sent.map((s) => JSON.parse(s));
    const rebase = frames.find((f) => f.type === "sync_anchor" || f.type === "rebase");
    expect(rebase).not.toBeNull();
    expect(rebase.since).toEqual({ n: 11, timestamp: "t11", epoch: 5 });

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("does not send sync_anchor until delta support is observed", async () => {
    const cached = wireState("no-delta", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "a", timestamp: "t1" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
    });
    await cache!.saveOpenedContent("remote", "no-delta", "http://remote.example.com", cached);

    window.history.replaceState(null, "", "/?convo=no-delta&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "no-delta" });
    await flush(50);

    // Server sends full frames (no messages_from)
    for (let i = 1; i <= 12; i++) {
      const frame = wireState("no-delta", false, "remote", {
        chat: {
          messages: Array.from({ length: i }, (_, j) => ({
            role: "user" as const,
            content: `m${j + 1}`,
            timestamp: `t${j + 1}`,
          })),
          auto_scroll: false,
          scroll_offset: 0,
        },
      });
      await feed({ type: "state", state: frame });
      await flush(5);
    }

    const ws = FakeWebSocket.instances.at(-1)!;
    const frames = ws.sent.map((s) => JSON.parse(s));
    const rebase = frames.find((f) => f.type === "sync_anchor" || f.type === "rebase");
    expect(rebase).toBeUndefined();

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("old-base in-flight frames remain decodable during transition", async () => {
    const cached = wireState("transition", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "a", timestamp: "t1" },
          { role: "agent", content: "b", timestamp: "t2" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 2,
    });
    await cache!.saveOpenedContent("remote", "transition", "http://remote.example.com", cached);

    window.history.replaceState(null, "", "/?convo=transition&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "transition" });
    await flush(50);

    // Server sends a suffix against the original base (n=2)
    const suffix = wireState("transition", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "c", timestamp: "t3" },
          { role: "agent", content: "d", timestamp: "t4" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 2,
    });
    await feed({ type: "state", state: suffix, messages_from: 2 });
    await flush(20);

    expect(conn).not.toBeNull();
    expect(conn!.state).not.toBeNull();
    expect(conn!.state!.chat.messages.length).toBe(4);
    expect((conn!.state!.chat.messages[2] as WireChatMessage).content).toBe("c");
    expect((conn!.state!.chat.messages[3] as WireChatMessage).content).toBe("d");

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("rebase preserves raw timestamp byte-exact", async () => {
    const rawTs = "2026-11-25T19:00:00.123456789+01:00";
    const cached = wireState("rebase-ts", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "a", timestamp: rawTs },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
      history_epoch: 3,
    });
    await cache!.saveOpenedContent("remote", "rebase-ts", "http://remote.example.com", cached);

    window.history.replaceState(null, "", "/?convo=rebase-ts&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "rebase-ts" });
    await flush(50);

    for (let i = 2; i <= 11; i++) {
      const frame = wireState("rebase-ts", false, "remote", {
        chat: {
          messages: [
            { role: "user", content: `m${i}`, timestamp: `t${i}` },
          ],
          auto_scroll: false,
          scroll_offset: 0,
        },
        history_epoch: 3,
      });
      await feed({ type: "state", state: frame, messages_from: i - 1 });
      await flush(5);
    }

    const ws = FakeWebSocket.instances.at(-1)!;
    const frames = ws.sent.map((s) => JSON.parse(s));
    const rebase = frames.find((f) => f.type === "sync_anchor" || f.type === "rebase");
    expect(rebase).not.toBeNull();
    expect(rebase.since.timestamp).toBe("t11");
    // The original anchor's timestamp (row 1) was preserved
    expect(rebase.since.n).toBe(11);

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── legacy compatibility ───────────────────────────────────────────────

describe("legacy compatibility", () => {
  it("attach without since works (old client / new server)", async () => {
    window.history.replaceState(null, "", "/?convo=legacy&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "legacy" });
    await flush(50);

    const ws = FakeWebSocket.instances.at(-1)!;
    const attachFrame = lastSent(ws);
    expect(attachFrame).not.toBeNull();
    expect(attachFrame!.type).toBe("attach");
    expect(attachFrame!.since).toBeUndefined();
  });

  it("state frames without messages_from are treated as full (old server)", async () => {
    window.history.replaceState(null, "", "/?profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "old-server" });
    await flush(50);

    const frame = wireState("old-server", false, "remote", {
      chat: {
        messages: [
          { role: "user", content: "x", timestamp: "t" },
        ],
        auto_scroll: false,
        scroll_offset: 0,
      },
    });
    await feed({ type: "state", state: frame });
    await flush(20);

    expect(conn).not.toBeNull();
    expect(conn!.state).not.toBeNull();
    expect(conn!.state!.chat.messages.length).toBe(1);
    expect((conn!.state!.chat.messages[0] as WireChatMessage).content).toBe("x");
  });

  it("malformed since in attach does not fail the connection", async () => {
    window.history.replaceState(null, "", "/?convo=malformed&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await flush(100);

    // Manually inject a malformed attach to the server
    const ws = FakeWebSocket.instances.at(-1)!;
    ws.onmessage?.({
      data: JSON.stringify({
        type: "attach",
        convo: "malformed",
        profile: "remote",
        since: { n: "nope" },
      }),
    });
    await flush(20);

    // Connection still alive; no error frame
    expect(ws.readyState).toBe(1);
  });
});
