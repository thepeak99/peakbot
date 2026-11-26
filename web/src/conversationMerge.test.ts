// Pure listing-merge / cap / quiesce / replay tests for the shared remote
// conversation cache (design "Shared remote conversation cache + delta sync —
// revised design" §2, §3, §4).
// RED before implementation: ./conversationMerge does not exist yet.
//
// Interface under test (pure, no IndexedDB, no React):
//
//   mergeListing({ live, cachedByOrigin, replacedOrigins, limit })
//     - `live`            = local rows + this refresh's successful origin rows
//     - `cachedByOrigin`  = origin → last cached snapshot
//     - `replacedOrigins` = origins whose response SUCCEEDED this refresh;
//                           their cached rows are dropped (live supersedes)
//     - a failed/offline/connecting origin is NOT in `replacedOrigins`, so its
//       cached rows survive ("absence is not removal")
//     - dedupe by id, sort updated_at desc / id asc, cap at `limit` (100)
//
//   quiesceAppStateForCache(state)  → strip running/command/stop/pending/
//                                     status liveness; keep chat + ids
//   shouldCacheState(state)         → settle gate (running/command ⇒ false)
//   makeContentCacheKey(route, id)  → JSON.stringify([route, id])

import { describe, expect, it } from "vitest";
import {
  mergeListing,
  quiesceAppStateForCache,
  shouldCacheState,
  makeContentCacheKey,
} from "./conversationMerge";
import type { ConversationSummary } from "./state";

/** A cached row: the client summary plus the cache's per-origin machine tag
 * (Stage 2 adds `machine` to `ConversationSummary`). */
type CacheRow = ConversationSummary & { machine?: string | null };

// ─── helpers ────────────────────────────────────────────────────────────

function row(
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

const LIMIT = 100;

// ─── merge: success replaces, failure retains ──────────────────────────

describe("origin snapshots", () => {
  it("a successful origin response replaces only that origin's cached rows", () => {
    const merged = mergeListing({
      live: [row("a-new", "A new", "2026-01-06T12:00:00Z", "http://a")],
      cachedByOrigin: {
        "http://a": [row("a-old", "A old", "2026-01-01T12:00:00Z", "http://a")],
      },
      replacedOrigins: ["http://a"],
      limit: LIMIT,
    });
    expect(merged.map((r: CacheRow) => r.id)).toEqual(["a-new"]);
  });

  it("a failed/offline origin keeps its cached rows", () => {
    const merged = mergeListing({
      live: [],
      cachedByOrigin: {
        "http://a": [row("a-old", "A old", "2026-01-01T12:00:00Z", "http://a")],
      },
      replacedOrigins: [], // A did not answer
      limit: LIMIT,
    });
    expect(merged.map((r: CacheRow) => r.id)).toEqual(["a-old"]);
  });

  it("one origin's success never touches another origin's cache", () => {
    const merged = mergeListing({
      live: [row("a-new", "A new", "2026-01-06T12:00:00Z", "http://a")],
      cachedByOrigin: {
        "http://a": [row("a-old", "A old", "2026-01-01T12:00:00Z", "http://a")],
        "http://b": [row("b-old", "B old", "2026-01-02T12:00:00Z", "http://b")],
      },
      replacedOrigins: ["http://a"],
      limit: LIMIT,
    });
    expect(merged.map((r: CacheRow) => r.id).sort()).toEqual(["a-new", "b-old"]);
  });
});

// ─── merge: sort + tiebreak + dedupe ───────────────────────────────────

describe("merge sort + tiebreak + dedupe", () => {
  it("sorts by updated_at descending", () => {
    const merged = mergeListing({
      live: [
        row("new", "New", "2026-01-06T12:00:00Z", "http://a"),
        row("old", "Old", "2026-01-03T12:00:00Z", "http://b"),
      ],
      cachedByOrigin: {},
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged.map((r: CacheRow) => r.id)).toEqual(["new", "old"]);
  });

  it("uses id ascending as tiebreak for identical updated_at", () => {
    const merged = mergeListing({
      live: [
        row("bbb", "B", "2026-01-05T12:00:00Z", "http://a"),
        row("aaa", "A", "2026-01-05T12:00:00Z", "http://a"),
      ],
      cachedByOrigin: {},
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged.map((r: CacheRow) => r.id)).toEqual(["aaa", "bbb"]);
  });

  it("deduplicates by id, live wins over cache", () => {
    const merged = mergeListing({
      live: [row("same", "Live", "2026-01-06T12:00:00Z", null)],
      cachedByOrigin: {
        "http://a": [row("same", "Cached", "2026-01-03T12:00:00Z", "http://a")],
      },
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged.length).toBe(1);
    expect(merged[0].name).toBe("Live");
    expect(merged[0].machine ?? null).toBeNull();
  });
});

// ─── merge: the GLOBAL 100 cap ─────────────────────────────────────────

describe("global 100 cap", () => {
  it("caps the merged set at 100 total, not 100 per origin", () => {
    const cachedByOrigin: Record<string, CacheRow[]> = { "http://a": [], "http://b": [] };
    for (let i = 0; i < 60; i++) {
      cachedByOrigin["http://a"].push(row(`a-${i}`, `A${i}`, `2026-01-0${(i % 9) + 1}T12:00:00Z`, "http://a"));
      cachedByOrigin["http://b"].push(row(`b-${i}`, `B${i}`, `2026-01-0${(i % 9) + 1}T12:00:00Z`, "http://b"));
    }
    const merged = mergeListing({
      live: [],
      cachedByOrigin,
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged.length).toBe(100);
  });

  it("keeps the newest rows when capping", () => {
    const cached: CacheRow[] = [];
    for (let i = 0; i < 120; i++) {
      // old rows first, newest last; two-digit-safe ISO stamps
      const day = String(1 + (i % 28)).padStart(2, "0");
      cached.push(row(`r-${i}`, `R${i}`, `2026-01-${day}T12:00:${String(i % 60).padStart(2, "0")}Z`, "http://a"));
    }
    const merged = mergeListing({
      live: [],
      cachedByOrigin: { "http://a": cached },
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged.length).toBe(100);
    // The newest row is present.
    expect(merged.map((r: CacheRow) => r.id)).toContain("r-119");
  });

  it("honours a custom limit", () => {
    const merged = mergeListing({
      live: [
        row("a", "A", "2026-01-05T12:00:00Z", "http://a"),
        row("b", "B", "2026-01-04T12:00:00Z", "http://a"),
        row("c", "C", "2026-01-03T12:00:00Z", "http://a"),
      ],
      cachedByOrigin: {},
      replacedOrigins: [],
      limit: 2,
    });
    expect(merged.map((r: CacheRow) => r.id)).toEqual(["a", "b"]);
  });
});

// ─── machine tag passthrough ───────────────────────────────────────────

describe("machine tag", () => {
  it("local rows carry a null machine", () => {
    const merged = mergeListing({
      live: [row("local", "Local", "2026-01-06T12:00:00Z", null)],
      cachedByOrigin: {},
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged[0].machine ?? null).toBeNull();
  });

  it("direct-remote rows keep their origin url", () => {
    const merged = mergeListing({
      live: [],
      cachedByOrigin: {
        "http://10.0.0.1:7823": [row("r", "R", "2026-01-06T12:00:00Z", "http://10.0.0.1:7823")],
      },
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged[0].machine).toBe("http://10.0.0.1:7823");
  });

  it("cloud rows keep their DeviceID (not a renameable slug)", () => {
    const merged = mergeListing({
      live: [],
      cachedByOrigin: {
        "cloud://device-abc123": [row("r", "R", "2026-01-06T12:00:00Z", "cloud://device-abc123")],
      },
      replacedOrigins: [],
      limit: LIMIT,
    });
    expect(merged[0].machine).toBe("cloud://device-abc123");
  });
});

// ─── content cache key ─────────────────────────────────────────────────

describe("content cache key", () => {
  it("is JSON.stringify([profileRoute, convoId])", () => {
    expect(makeContentCacheKey("route-a", "convo-1")).toBe('["route-a","convo-1"]');
  });

  it("isolates the same convo id under two routes", () => {
    expect(makeContentCacheKey("route-a", "convo-1")).not.toBe(
      makeContentCacheKey("route-b", "convo-1"),
    );
  });

  it("cannot be spoofed by separators in either part", () => {
    // A naive `${route}:${id}` join would collide these two.
    const a = makeContentCacheKey('a","b', "c");
    const b = makeContentCacheKey("a", 'b","c');
    expect(a).not.toBe(b);
  });
});

// ─── quiesce for cache ─────────────────────────────────────────────────

describe("quiesce for cache", () => {
  it("strips running / command / stop / pending / status liveness", () => {
    const state = {
      is_running: true,
      is_command: true,
      stop_requested: true,
      pending: true,
      status: "working…",
      chat: [],
    };
    const quiesced = quiesceAppStateForCache(state);
    expect(quiesced.is_running).toBe(false);
    expect(quiesced.is_command).toBeFalsy();
    expect(quiesced.stop_requested).toBeFalsy();
    expect(quiesced.pending).toBeFalsy();
    expect(quiesced.status ?? null).toBeNull();
  });

  it("preserves chat history and conversation identity", () => {
    const state = {
      is_running: false,
      is_command: false,
      chat: [{ role: "user" as const, content: "hello" }],
      conversation: { id: "conv-1", name: "test" },
    };
    const quiesced = quiesceAppStateForCache(state);
    expect(quiesced.chat).toEqual(state.chat);
    expect(((quiesced.conversation as { id?: string; name?: string })?.id)).toBe("conv-1");
  });
});

// ─── settled-only writes ───────────────────────────────────────────────

describe("settled-only caching", () => {
  it("does not cache a running state", () => {
    expect(shouldCacheState({ is_running: true })).toBe(false);
  });

  it("does not cache a command state", () => {
    expect(shouldCacheState({ is_running: false, is_command: true })).toBe(false);
  });

  it("caches a settled idle state", () => {
    expect(shouldCacheState({ is_running: false, is_command: false })).toBe(true);
  });
});

