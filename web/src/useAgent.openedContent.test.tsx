// @vitest-environment jsdom
//
// Integration test: useAgent opened-content flow.
//
// Contract (design §4.2, §6.4):
//   - Opening a remote/cloud conversation with a persistent cache hit paints
//     a quiesced cached state (running/command fields stripped).
//   - The cached paint is never overwritten by a live frame.
//   - A slow cache read from a previous binding cannot overwrite the new live
//     state or a newly selected conversation.
//   - Only settled remote/cloud states are saved to the content cache.
//   - Local (profile=null) states are never saved to the content cache.
//
// RED before implementation: useAgent's opened-content save/replay logic is
// wired but may not be fully correct. These tests target the integration
// path, not the pure functions (those are tested in conversationMerge.test.ts).

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

// The cache under test (separate handle from the hook's shared singleton; both
// open the SAME IndexedDB database).
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
  // Don't close the cache each test; it's reused
});

// ─── helpers ────────────────────────────────────────────────────────────

function wireState(
  convoId: string,
  isRunning: boolean,
  profile: string | null,
) {
  return {
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

/** Flush the async IndexedDB chain the hook awaits on mount / feed. */
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
  const mounted = root;
  if (mounted) {
    await act(async () => {
      mounted.unmount();
    });
  }
  root = null;
  if (container) {
    container.remove();
    container = null;
  }
  FakeWebSocket.instances = [];
  conn = null;
}

// ─── cache hit paints quiesced state ────────────────────────────────────

describe("opened-content cache hit paints quiesced state", () => {
  it("a cold boot with ?convo=&profile= replays cached quiesced state", async () => {
    // Seed the cache with a remote conversation
    const cachedState = wireState("cached-convo", false, "remote");
    cachedState.is_running = true; // running on save → should be quiesced on read
    cachedState.chat = { messages: [{ role: "user", content: "hello", timestamp: "2026-01-01T00:00:00Z" }], auto_scroll: false, scroll_offset: 0 };
    await cache!.saveOpenedContent("remote", "cached-convo", "http://remote.example.com", cachedState);

    // Boot with ?convo=cached-convo&profile=remote
    window.history.replaceState(null, "", "/?convo=cached-convo&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "cached-convo" });

    // Flush cache replay
    await flush();

    // State should be the cached quiesced version (is_running=false)
    expect(conn).not.toBeNull();
    expect(conn!.state).not.toBeNull();
    // Cached state has running quiesced to false
    expect(conn!.state!.is_running).toBe(false);

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("a late cache read does not overwrite live state", async () => {
    const cachedState = wireState("convo", false, "remote");
    await cache!.saveOpenedContent("remote", "convo", "http://remote.example.com", cachedState);

    window.history.replaceState(null, "", "/?convo=convo&profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "convo" });

    // Live state arrives BEFORE cache
    const liveState = wireState("convo", true, "remote");
    await feed({ type: "state", state: liveState });

    // Give cache read a chance
    await flush();

    // State should be the LIVE version (is_running=true), not the cached
    expect(conn!.state!.is_running).toBe(true);

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── settled-only saves ────────────────────────────────────────────────

describe("settled-only content cache saves", () => {
  it("running state is NOT saved to content cache", async () => {
    window.history.replaceState(null, "", "/?profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "run-convo" });

    // Send a running state
    const runningState = wireState("run-convo", true, "remote");
    await feed({ type: "state", state: runningState });
    await flush();

    // Should NOT be cached because it's running
    const hit = await cache!.getOpenedContent("remote", "run-convo");
    expect(hit).toBeNull();

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("settled state IS saved to content cache", async () => {
    window.history.replaceState(null, "", "/?profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "set-convo" });

    // Send a settled state
    const settledState = wireState("set-convo", false, "remote");
    await feed({ type: "state", state: settledState });
    await flush();

    // Should be cached
    const hit = await cache!.getOpenedContent("remote", "set-convo");
    expect(hit).not.toBeNull();
    expect(hit!.origin).toBe("http://remote.example.com");

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── profile=null never saved ──────────────────────────────────────────

describe("profile=null content cache exclusion", () => {
  it("a local (profile=null) conversation is never saved to content cache", async () => {
    window.history.replaceState(null, "", "/");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "local-convo" });

    // Send a settled local state
    const localState = wireState("local-convo", false, null);
    await feed({ type: "state", state: localState });
    await flush();

    // Should NOT be cached because profile is null
    const hit = await cache!.getOpenedContent(null, "local-convo");
    expect(hit).toBeNull();

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── slow cache read cannot overwrite new convo ────────────────────────

describe("slow cache read race protection", () => {
  it("switching convos discards a late cache read from the previous convo", async () => {
    // Seed first convo
    const state1 = wireState("convo-1", false, "remote");
    await cache!.saveOpenedContent("remote", "convo-1", "http://remote.example.com", state1);

    window.history.replaceState(null, "", "/?profile=remote");
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: "convo-1" });

    // Switch to convo-2 before cache read completes
    await act(async () => {
      conn!.switchConvo("convo-2", "remote");
      await Promise.resolve();
    });
    await feed({ type: "attached", convo: "convo-2" });

    // Give cache read for convo-1 a chance to complete (it will be stale)
    await flush();

    // State should be for convo-2 (null or attached), not the cached convo-1
    expect(conn!.convo).toBe("convo-2");

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── same path for desktop and mobile shells ───────────────────────────

describe("shell-agnostic opened-content path", () => {
  it.each(["desktop", "mobile", "browser"])(
    "opened-content flow works on %s shell",
    async (shell) => {
      delete (globalThis as { peakbotShell?: unknown }).peakbotShell;
      if (shell !== "browser") {
        (globalThis as { peakbotShell?: unknown }).peakbotShell = shell;
      }

      const cachedState = wireState("shell-convo", false, "remote");
      cachedState.chat = {
        messages: [{ role: "user", content: "shell", timestamp: "2026-01-01T00:00:00Z" }],
        auto_scroll: false,
        scroll_offset: 0,
      };
      await cache!.saveOpenedContent("remote", "shell-convo", "http://remote.example.com", cachedState);

      window.history.replaceState(null, "", "/?convo=shell-convo&profile=remote");
      vi.stubGlobal("WebSocket", FakeWebSocket);
      vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

      await mountAgent();
      await feed({ type: "attached", convo: "shell-convo" });

      await flush();

      expect(conn).not.toBeNull();
      expect(conn!.state).not.toBeNull();
      // Cached content was loaded regardless of shell marker
      expect(conn!.state!.is_running).toBe(false);

      await resetAgent();
      vi.unstubAllGlobals();
    },
  );
});