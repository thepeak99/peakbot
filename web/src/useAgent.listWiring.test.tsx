// @vitest-environment jsdom
//
// Integration test: useAgent list wiring with shared conversation cache.
//
// Contract (design §2-4):
//   - Cached origin list rows load and merge into the conversations list
//     before any live frame arrives.
//   - A successful origin response replaces only that origin's rows.
//   - Failed/offline origin rows (not in machine statuses) remain from cache.
//   - The merged list is capped globally at 100.
//   - Rows are sorted by updated_at descending, ties broken by id ascending.
//
// RED before implementation: the merge logic in useAgent may not correctly
// load, merge, or cap cached rows on the real wiring path.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import { useAgent, type AgentConnection } from "./useAgent";
import { createConversationCache, type ConversationCache } from "./conversationCache";

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

// One cache handle reused across tests; the per-origin purges in beforeEach
// reset it (a fresh deleteDatabase would block on the still-open shared
// connection).
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
  // Reuse the DB handle between tests (cheap) and purge the origins this file
  // writes, instead of clearing the whole shared database.
  if (!cache) cache = await createConversationCache();
  else {
    for (const origin of ["http://remote-a", "http://remote-b", "http://remote-fail"]) {
      await cache.purgeOrigin(origin);
    }
  }
});

afterEach(() => {
  FakeWebSocket.instances = [];
});

// ─── helpers ────────────────────────────────────────────────────────────

function wireRow(
  id: string,
  name: string,
  updatedAt: string,
  machine: string | null = null,
  profile?: string,
) {
  return {
    id,
    name,
    updated_at: updatedAt,
    message_count: 3,
    model: "sonnet",
    active: false,
    running: false,
    profile,
    machine,
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

/** Flush the async IndexedDB chain the hook awaits (cache list load / save). */
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

// ─── cache preload: list shows cached rows ──────────────────────────────

describe("list wiring with cached rows", () => {
  it("shows cached rows before any live frame arrives", async () => {
    // Seed a cached list for a remote origin
    const cachedRows = [
      wireRow("cached-1", "Cached 1", "2026-01-05T12:00:00Z", "http://remote-a"),
      wireRow("cached-2", "Cached 2", "2026-01-04T12:00:00Z", "http://remote-a"),
    ];
    await cache!.saveOriginList("http://remote-a", cachedRows);

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });

    // Wait for the cache load in the mount effect
    await flush();

    // conn.conversations should contain the cached rows
    expect(conn!.conversations).toHaveLength(2);
    expect(conn!.conversations.map((c) => c.id)).toEqual(["cached-1", "cached-2"]);

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("a successful conversations_list replaces cached rows", async () => {
    const cachedRows = [
      wireRow("cached-1", "Cached 1", "2026-01-05T12:00:00Z", "http://remote-a"),
    ];
    await cache!.saveOriginList("http://remote-a", cachedRows);

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });

    await flush();
    expect(conn!.conversations).toHaveLength(1);

    // Now send a live list with newer rows
    const liveRows = [
      wireRow("live-1", "Live 1", "2026-01-06T12:00:00Z", "http://remote-a"),
    ];
    await feed({
      type: "conversations_list",
      items: liveRows,
      machines: [{ id: "http://remote-a", online: true, stable_id: "http://remote-a" }],
    });

    // Should now show live rows only (cached replaced)
    expect(conn!.conversations).toHaveLength(1);
    expect(conn!.conversations[0].id).toBe("live-1");

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("a failed/offline origin retains its cached rows", async () => {
    const cachedRows = [
      wireRow("cached-fail", "Cached Fail", "2026-01-05T12:00:00Z", "http://remote-fail"),
    ];
    await cache!.saveOriginList("http://remote-fail", cachedRows);

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });

    // Show cached rows
    await flush();
    expect(conn!.conversations).toHaveLength(1);

    // Send live list: origin is OFFLINE in machine statuses
    const liveRows = [
      wireRow("live-1", "Live 1", "2026-01-06T12:00:00Z", null), // local row only
    ];
    await feed({
      type: "conversations_list",
      items: liveRows,
      machines: [{ id: "http://remote-fail", online: false, stable_id: "http://remote-fail" }],
    });

    // Should show both: live local + cached from failed origin
    await flush();
    expect(conn!.conversations).toHaveLength(2);
    expect(conn!.conversations.map((c) => c.id)).toContain("live-1");
    expect(conn!.conversations.map((c) => c.id)).toContain("cached-fail");

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── global 100 cap ────────────────────────────────────────────────────

describe("merged list global cap", () => {
  it("caps the combined list at 100", async () => {

    // Seed 60 cached rows for one origin
    const cachedRows = [];
    for (let i = 0; i < 60; i++) {
      cachedRows.push(
        wireRow(`cached-${i}`, `Cached ${i}`, `2026-01-0${(i % 9) + 1}T12:00:00Z`, "http://remote-a"),
      );
    }
    await cache!.saveOriginList("http://remote-a", cachedRows);

    // Seed 60 cached rows for another origin
    const cachedRows2 = [];
    for (let i = 0; i < 60; i++) {
      cachedRows2.push(
        wireRow(`cached-b-${i}`, `Cached B ${i}`, `2026-01-0${(i % 9) + 1}T12:00:00Z`, "http://remote-b"),
      );
    }
    await cache!.saveOriginList("http://remote-b", cachedRows2);

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });

    // Wait for cache load
    await flush();

    // 120 cached rows, but cap is 100
    expect(conn!.conversations.length).toBeLessThanOrEqual(100);

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("live + cached rows from a failed origin merge and cap globally at 100", async () => {
    // Origin A is offline: 80 cached rows retained. Origin B answers live: 30
    // rows. Merged = 110 → the global cap trims to 100.
    const cachedRows = [];
    for (let i = 0; i < 80; i++) {
      const day = String(1 + (i % 28)).padStart(2, "0");
      cachedRows.push(
        wireRow(`cached-${i}`, `Cached ${i}`, `2026-01-${day}T12:00:00Z`, "http://remote-a"),
      );
    }
    await cache!.saveOriginList("http://remote-a", cachedRows);

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });
    await flush();

    // 30 live rows from origin B (online); origin A failed.
    const liveRows = [];
    for (let i = 0; i < 30; i++) {
      const day = String(1 + (i % 28)).padStart(2, "0");
      liveRows.push(
        wireRow(`live-${i}`, `Live ${i}`, `2026-01-${day}T12:00:00Z`, "http://remote-b"),
      );
    }
    await feed({
      type: "conversations_list",
      items: liveRows,
      machines: [
        { id: "http://remote-b", online: true, stable_id: "http://remote-b" },
        { id: "http://remote-a", online: false, stable_id: "http://remote-a" },
      ],
    });
    await flush();

    // 30 live + 80 cached = 110 rows, capped to a global 100.
    expect(conn!.conversations.length).toBe(100);

    await resetAgent();
    vi.unstubAllGlobals();
  });

  it("a live-only frame larger than 100 is capped globally (BUG: live path skips the cap)", async () => {
    // The server caps each SOURCE at 100 but not the aggregate, so a local +
    // remote frame can carry >100 rows. The client's global cap must trim it.
    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });

    const liveRows = [];
    for (let i = 0; i < 150; i++) {
      const day = String(1 + (i % 28)).padStart(2, "0");
      liveRows.push(wireRow(`live-${i}`, `Live ${i}`, `2026-01-${day}T12:00:00Z`, null));
    }
    await feed({ type: "conversations_list", items: liveRows, machines: [] });
    await flush();

    expect(
      conn!.conversations.length,
      "the global 100 cap must apply to a live-only frame too",
    ).toBeLessThanOrEqual(100);

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── sort + tiebreak ───────────────────────────────────────────────────

describe("merged list sort + tiebreak", () => {
  it("after merging cached rows the list is sorted updated_at desc, ties by id asc", async () => {
    // Failed origin A holds a cached row newer than the live rows; the merge
    // must re-sort (the server's own order no longer covers the cached rows).
    await cache!.saveOriginList("http://remote-a", [
      wireRow("z", "Z", "2026-01-09T12:00:00Z", "http://remote-a"),
    ]);

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });
    await flush();

    // Live rows: two with equal timestamps (tie → id asc) older than the cached.
    const liveRows = [
      wireRow("b", "B", "2026-01-05T12:00:00Z", "http://remote-b"),
      wireRow("a", "A", "2026-01-05T12:00:00Z", "http://remote-b"),
    ];
    await feed({
      type: "conversations_list",
      items: liveRows,
      machines: [
        { id: "http://remote-b", online: true, stable_id: "http://remote-b" },
        { id: "http://remote-a", online: false, stable_id: "http://remote-a" },
      ],
    });
    await flush();

    expect(conn!.conversations.map((c) => c.id)).toEqual(["z", "a", "b"]);

    await resetAgent();
    vi.unstubAllGlobals();
  });
});

// ─── local rows are never cached ───────────────────────────────────────

describe("local rows excluded from cache", () => {
  it("local rows (machine=null) are not saved to origin cache", async () => {

    vi.stubGlobal("WebSocket", FakeWebSocket);
    vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));

    await mountAgent();
    await feed({ type: "attached", convo: null });

    // Send local rows
    const localRows = [
      wireRow("local-1", "Local 1", "2026-01-06T12:00:00Z", null),
    ];
    await feed({
      type: "conversations_list",
      items: localRows,
      machines: [],
    });

    await flush();

    // Check that nothing was cached (only origin rows are cached)
    const origins = await cache!.listOrigins();
    expect(origins.length).toBe(0);

    await resetAgent();
    vi.unstubAllGlobals();
  });
});