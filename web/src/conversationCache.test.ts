// @vitest-environment jsdom
//
// IndexedDB shared conversation cache tests (design "Shared remote
// conversation cache + delta sync — revised design" §2, §4, §7).
// RED before implementation: ./conversationCache does not exist yet.
//
// Contract:
//   - ONE IndexedDB database for all conversation cache data
//   - Per-origin list snapshots: successful response replaces only that origin
//   - Failed/offline origins retain cached rows
//   - Content cache keyed by JSON.stringify([profileRoute, convoId])
//   - Same convo id under two routes stays isolated
//   - Persistent across reload
//   - Content includes origin/DeviceID metadata for revocation cleanup
//   - Settled-only disk writes
//   - Best-effort: cache unavailable/quota/corrupt => cache miss, never crash

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { indexedDB as fakeIndexedDB } from "fake-indexeddb";
import {
  createConversationCache,
  DEFAULT_CACHE_LIMITS,
  type ConversationCache,
} from "./conversationCache";
import type { ConversationSummary } from "./state";

/** A cached row: the client summary plus the cache's per-origin machine tag
 * (Stage 2 adds `machine` to `ConversationSummary`). */
type CacheRow = ConversationSummary & { machine?: string | null };

// Install fake indexedDB globally for all tests
beforeEach(() => {
  Object.defineProperty(globalThis, "indexedDB", {
    value: fakeIndexedDB,
    writable: true,
    configurable: true,
  });
});

// ─── helpers ────────────────────────────────────────────────────────────

function wireRow(
  id: string,
  name: string,
  updatedAt: string,
  machine: string | null = null,
  profile?: string,
): CacheRow {
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

async function resetDb(name: string): Promise<void> {
  const req = indexedDB.deleteDatabase(name);
  return new Promise<void>((resolve, reject) => {
    req.onsuccess = () => resolve();
    req.onerror = () => reject(req.error);
  });
}

// ─── per-origin list snapshots ──────────────────────────────────────────

describe("per-origin list snapshots", () => {
  let cache: ConversationCache;
  beforeEach(async () => {
    await resetDb("peakbot-conversation-cache");
    cache = await createConversationCache();
  });
  afterEach(async () => cache.close());

  it("saves and retrieves list data for an origin", async () => {
    const rows = [wireRow("a", "A", "2026-01-05T12:00:00Z", "http://remote-a")];
    await cache.saveOriginList("http://remote-a", rows);
    const retrieved = await cache.getOriginList("http://remote-a");
    expect(retrieved).toEqual(rows);
  });

  it("replaces only the specified origin on save", async () => {
    const rowsA = [wireRow("a", "A", "2026-01-05T12:00:00Z", "http://remote-a")];
    const rowsB = [wireRow("b", "B", "2026-01-04T12:00:00Z", "http://remote-b")];
    await cache.saveOriginList("http://remote-a", rowsA);
    await cache.saveOriginList("http://remote-b", rowsB);
    await cache.saveOriginList("http://remote-a", [
      wireRow("a2", "A2", "2026-01-06T12:00:00Z", "http://remote-a"),
    ]);
    expect(await cache.getOriginList("http://remote-a")).toEqual([
      wireRow("a2", "A2", "2026-01-06T12:00:00Z", "http://remote-a"),
    ]);
    expect(await cache.getOriginList("http://remote-b")).toEqual(rowsB);
  });

  it("returns null for an origin with no cached list", async () => {
    expect(await cache.getOriginList("http://unknown")).toBeNull();
  });

  it("clears only the specified origin", async () => {
    await cache.saveOriginList("http://remote-a", [wireRow("a", "A", "2026-01-05T12:00:00Z", "http://remote-a")]);
    await cache.saveOriginList("http://remote-b", [wireRow("b", "B", "2026-01-04T12:00:00Z", "http://remote-b")]);
    await cache.clearOrigin("http://remote-a");
    expect(await cache.getOriginList("http://remote-a")).toBeNull();
    expect((await cache.getOriginList("http://remote-b"))?.length).toBe(1);
  });

  it("purgeOrigin clears that origin's list and content but leaves others", async () => {
    await cache.saveOriginList("cloud://a", [wireRow("a", "A", "2026-01-05T12:00:00Z", "cloud://a")]);
    await cache.saveOriginList("cloud://c", [wireRow("c", "C", "2026-01-05T12:00:00Z", "cloud://c")]);
    await cache.saveOpenedContent("r", "convo-c", "cloud://c", { chat: [] });

    await cache.purgeOrigin("cloud://c");

    expect(await cache.getOriginList("cloud://a")).not.toBeNull();
    expect(await cache.getOriginList("cloud://c")).toBeNull();
    expect(await cache.getOpenedContent("r", "convo-c")).toBeNull();
  });
});

// ─── content cache ─────────────────────────────────────────────────────

describe("content cache", () => {
  let cache: ConversationCache;
  beforeEach(async () => {
    await resetDb("peakbot-conversation-cache");
    cache = await createConversationCache();
  });
  afterEach(async () => cache.close());

  it("saves and retrieves opened conversation content", async () => {
    const state = { chat: [{ role: "user", content: "hello" }], is_running: false };
    await cache.saveOpenedContent("route", "convo-1", "http://remote-a", state);
    const retrieved = await cache.getOpenedContent("route", "convo-1");
    expect(retrieved).not.toBeNull();
    expect(retrieved?.state.chat).toEqual(state.chat);
    expect(retrieved?.origin).toBe("http://remote-a");
  });

  it("keeps same convo id under two routes isolated", async () => {
    const stateA = { chat: [{ role: "user", content: "A" }], is_running: false };
    const stateB = { chat: [{ role: "user", content: "B" }], is_running: false };
    await cache.saveOpenedContent("route-a", "same-convo", "http://remote-a", stateA);
    await cache.saveOpenedContent("route-b", "same-convo", "http://remote-b", stateB);
    const a = await cache.getOpenedContent("route-a", "same-convo");
    const b = await cache.getOpenedContent("route-b", "same-convo");
    expect((a?.state.chat as { role: string; content: string }[])[0].content).toBe("A");
    expect((b?.state.chat as { role: string; content: string }[])[0].content).toBe("B");
  });

  it("returns null for uncached conversation", async () => {
    expect(await cache.getOpenedContent("route", "missing")).toBeNull();
  });

  it("clears all content for an origin", async () => {
    await cache.saveOpenedContent("route-a", "convo-1", "http://remote-a", { chat: [] });
    await cache.saveOpenedContent("route-b", "convo-2", "http://remote-a", { chat: [] });
    await cache.saveOpenedContent("route-c", "convo-3", "http://remote-b", { chat: [] });
    await cache.clearOriginContent("http://remote-a");
    expect(await cache.getOpenedContent("route-a", "convo-1")).toBeNull();
    expect(await cache.getOpenedContent("route-b", "convo-2")).toBeNull();
    expect((await cache.getOpenedContent("route-c", "convo-3"))?.state.chat).toEqual([]);
  });

  it("persists content across createConversationCache calls (reload)", async () => {
    await cache.saveOpenedContent("route", "convo-1", "http://remote-a", { chat: ["persist"] });
    await cache.close();
    const cache2 = await createConversationCache();
    const retrieved = await cache2.getOpenedContent("route", "convo-1");
    expect(retrieved?.state.chat).toEqual(["persist"]);
    await cache2.close();
  });

  it("persists list data across reload", async () => {
    const rows = [wireRow("a", "A", "2026-01-05T12:00:00Z", "http://remote-a")];
    await cache.saveOriginList("http://remote-a", rows);
    await cache.close();
    const cache2 = await createConversationCache();
    expect(await cache2.getOriginList("http://remote-a")).toEqual(rows);
    await cache2.close();
  });
});

// ─── content bounds (injected tiny limits) ─────────────────────────────

describe("content bounds", () => {
  it("ships the starting limits: 20 entries / 24 MiB total / skip >8 MiB", async () => {
    expect(DEFAULT_CACHE_LIMITS.maxEntries).toBe(20);
    expect(DEFAULT_CACHE_LIMITS.maxTotalBytes).toBe(24 * 1024 * 1024);
    expect(DEFAULT_CACHE_LIMITS.maxEntryBytes).toBe(8 * 1024 * 1024);
  });

  it("keeps at most maxEntries opened conversations (oldest evicted)", async () => {
    await resetDb("peakbot-conversation-cache");
    const cache = await createConversationCache({ maxEntries: 2 });
    await cache.saveOpenedContent("r", "oldest", "http://a", { chat: [] }, { now: 1 });
    await cache.saveOpenedContent("r", "middle", "http://a", { chat: [] }, { now: 2 });
    await cache.saveOpenedContent("r", "newest", "http://a", { chat: [] }, { now: 3 });

    expect(await cache.getOpenedContent("r", "newest")).not.toBeNull();
    expect(await cache.getOpenedContent("r", "middle")).not.toBeNull();
    expect(await cache.getOpenedContent("r", "oldest")).toBeNull();
    await cache.close();
  });

  it("skips an individual entry larger than maxEntryBytes", async () => {
    await resetDb("peakbot-conversation-cache");
    const cache = await createConversationCache({ maxEntryBytes: 200 });
    const huge = { chat: ["x".repeat(1000)] };
    await cache.saveOpenedContent("r", "too-big", "http://a", huge);
    await cache.saveOpenedContent("r", "small", "http://a", { chat: [] });

    expect(await cache.getOpenedContent("r", "too-big")).toBeNull();
    expect(await cache.getOpenedContent("r", "small")).not.toBeNull();
    await cache.close();
  });

  it("honours a total byte budget by evicting older entries", async () => {
    await resetDb("peakbot-conversation-cache");
    const cache = await createConversationCache({ maxTotalBytes: 400 });
    // ~250 bytes each ⇒ only one fits the 400-byte budget.
    const payload = { chat: ["y".repeat(230)] };
    await cache.saveOpenedContent("r", "first", "http://a", payload, { now: 1 });
    await cache.saveOpenedContent("r", "second", "http://a", payload, { now: 2 });

    expect(await cache.getOpenedContent("r", "second")).not.toBeNull();
    expect(await cache.getOpenedContent("r", "first")).toBeNull();
    await cache.close();
  });
});

// ─── best-effort: storage unavailable / quota / corrupt ────────────────

describe("best-effort storage", () => {
  it("resolves to a miss-only cache when IndexedDB is unavailable", async () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, "indexedDB");
    Object.defineProperty(globalThis, "indexedDB", {
      value: undefined,
      writable: true,
      configurable: true,
    });
    try {
      const cache = await createConversationCache();
      await cache.saveOriginList("http://a", [wireRow("a", "A", "2026-01-05T12:00:00Z", "http://a")]);
      expect(await cache.getOriginList("http://a")).toBeNull();
      expect(await cache.getOpenedContent("r", "c1")).toBeNull();
      await cache.close();
    } finally {
      if (original) Object.defineProperty(globalThis, "indexedDB", original);
    }
  });

  it("a quota failure on write never throws and keeps earlier data readable", async () => {
    await resetDb("peakbot-conversation-cache");
    const cache = await createConversationCache();
    await cache.saveOpenedContent("r", "before", "http://a", { chat: ["kept"] });

    const { IDBDatabase } = await import("fake-indexeddb");
    const proto = IDBDatabase.prototype as unknown as {
      transaction: (...args: unknown[]) => unknown;
    };
    const original = proto.transaction;
    proto.transaction = () => {
      throw new DOMException("quota", "QuotaExceededError");
    };
    try {
      await expect(
        cache.saveOpenedContent("r", "after", "http://a", { chat: ["quota"] }),
      ).resolves.toBeUndefined();
    } finally {
      proto.transaction = original;
    }

    // The old entry is still readable; no crash.
    expect((await cache.getOpenedContent("r", "before"))?.state.chat).toEqual(["kept"]);
    await cache.close();
  });
});
