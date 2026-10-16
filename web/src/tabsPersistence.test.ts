// Tests for `tabsPersistence.ts` — the localStorage boundary (design §3/§5):
//
//   export const TABS_STORAGE_KEY = "peakbot.desktop.tabs";
//   export const RESTORE_ON_LAUNCH = true; // future setting hooks in here
//   export function parseStored(raw: string | null): TabsState | null;
//   export function loadTabs(storage: Storage, restore?: boolean): TabsState | null;
//   export function saveTabs(storage: Storage, s: TabsState): void; // never throws
//
// `parseStored` rules (applied once, at load):
//   - invalid JSON, `v !== 1`, or not an object → null (start fresh)
//   - drop entries whose `convo` is not a string or null, or `title` not a string
//   - trim `title` to 200 chars; keep at most 100 tabs
//   - dedupe by `convo`, keeping the first
//   - nothing left → null; `active` not an integer in range → 0
//
// `loadTabs`: `restore=false` → null (the future setting hooks in here,
// nowhere else); a throwing storage → null.
// `saveTabs`: writes `{v:1, tabs, active}`; never throws.
//
// RED before implementation: `./tabsPersistence` does not exist yet, so this
// file fails at import — the right reason (feature absent).
//
// Runs under `node`: the contract takes a `Storage` parameter, so a small
// in-memory fake stands in for localStorage (same shape as vitest.setup.ts).

import { describe, expect, it } from "vitest";
import {
  RESTORE_ON_LAUNCH,
  TABS_STORAGE_KEY,
  loadTabs,
  parseStored,
  saveTabs,
} from "./tabsPersistence";
import type { TabsState } from "./convoTabs";

// ─── fake Storage (node env: no DOM) ─────────────────────────────────────

class FakeStorage {
  private map = new Map<string, string>();
  getItem(key: string): string | null {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string): void {
    this.map.set(key, String(value));
  }
  removeItem(key: string): void {
    this.map.delete(key);
  }
  clear(): void {
    this.map.clear();
  }
  key(index: number): string | null {
    return [...this.map.keys()][index] ?? null;
  }
  get length(): number {
    return this.map.size;
  }
}

/** A storage whose `getItem` throws — stands in for a disabled/quota-broken
 * localStorage, which `loadTabs` must survive. */
class ThrowingStorage {
  getItem(): string | null {
    throw new Error("SecurityError: localStorage is not available for this document");
  }
  setItem(): void {
    throw new Error("quota");
  }
  removeItem(): void {
    throw new Error("quota");
  }
  clear(): void {
    throw new Error("quota");
  }
  key(): string | null {
    throw new Error("quota");
  }
  get length(): number {
    return 0;
  }
}

const asStorage = (s: FakeStorage | ThrowingStorage): Storage => s as unknown as Storage;

// ─── fixtures ────────────────────────────────────────────────────────────

const state = (tabs: { convo: string | null; title: string }[], active = 0): TabsState =>
  ({ tabs, active }) as TabsState;

// ─── constants ───────────────────────────────────────────────────────────

describe("constants", () => {
  it("TABS_STORAGE_KEY is 'peakbot.desktop.tabs'", () => {
    expect(TABS_STORAGE_KEY).toBe("peakbot.desktop.tabs");
  });

  it("RESTORE_ON_LAUNCH is true (the future setting hooks in here, nowhere else)", () => {
    expect(RESTORE_ON_LAUNCH).toBe(true);
  });
});

// ─── parseStored: junk → fresh (null) ────────────────────────────────────

describe("parseStored — invalid input gives null", () => {
  it("returns null for null (first launch)", () => {
    expect(parseStored(null)).toBeNull();
  });

  it.each([
    ["not JSON at all", "not json"],
    ["a truncated object", "{"],
    ["a bare array", "[1,2,3]"],
    ["a bare number", "42"],
    ["a bare string", "\"hello\""],
    ["a bare true", "true"],
    ["a bare null", "null"],
  ])("returns null for %s", (_label, raw) => {
    expect(parseStored(raw)).toBeNull();
  });

  it("returns null when the version is missing", () => {
    expect(parseStored(JSON.stringify({ tabs: [{ convo: null, title: "" }], active: 0 }))).toBeNull();
  });

  it("returns null when the version is not 1", () => {
    const body = { tabs: [{ convo: null, title: "" }], active: 0 };
    expect(parseStored(JSON.stringify({ ...body, v: 2 }))).toBeNull();
    // Strict `v !== 1`: a string version is not 1.
    expect(parseStored(JSON.stringify({ ...body, v: "1" }))).toBeNull();
  });

  it("returns null when the top level is an array (an object is required)", () => {
    expect(parseStored(JSON.stringify([{ convo: null, title: "" }]))).toBeNull();
  });
});

// ─── parseStored: entry filtering, caps, dedupe ──────────────────────────

describe("parseStored — entry validation", () => {
  it("drops entries whose convo is not a string or null", () => {
    const raw = {
      v: 1,
      tabs: [
        { convo: 42, title: "number convo" },
        { convo: { id: "a" }, title: "object convo" },
        { convo: "a", title: "good" },
        { convo: null, title: "" },
      ],
      active: 0,
    };
    const s = parseStored(JSON.stringify(raw));
    expect(s?.tabs).toEqual([{ convo: "a", title: "good" }, { convo: null, title: "" }]);
  });

  it("drops entries whose title is not a string (including a missing title)", () => {
    const raw = {
      v: 1,
      tabs: [
        { convo: "a", title: 7 },
        { convo: "b" }, // title missing → undefined → not a string
        { convo: "c", title: "ok" },
      ],
      active: 0,
    };
    const s = parseStored(JSON.stringify(raw));
    expect(s?.tabs).toEqual([{ convo: "c", title: "ok" }]);
  });

  it("returns null when every entry is dropped", () => {
    const raw = { v: 1, tabs: [{ convo: 1, title: "x" }, { convo: "a", title: 2 }], active: 0 };
    expect(parseStored(JSON.stringify(raw))).toBeNull();
  });

  it("returns null for an empty or missing tabs array", () => {
    expect(parseStored(JSON.stringify({ v: 1, tabs: [], active: 0 }))).toBeNull();
    expect(parseStored(JSON.stringify({ v: 1, active: 0 }))).toBeNull();
  });

  it("keeps several null-convo tabs (dedupe applies to convo values only)", () => {
    const raw = {
      v: 1,
      tabs: [
        { convo: null, title: "" },
        { convo: null, title: "" },
        { convo: "a", title: "a" },
      ],
      active: 1,
    };
    const s = parseStored(JSON.stringify(raw));
    expect(s?.tabs).toHaveLength(3);
  });

  it("dedupes by convo, keeping the first occurrence", () => {
    const raw = {
      v: 1,
      tabs: [
        { convo: "a", title: "first" },
        { convo: "b", title: "b" },
        { convo: "a", title: "second" },
      ],
      active: 0,
    };
    const s = parseStored(JSON.stringify(raw));
    expect(s?.tabs).toEqual([
      { convo: "a", title: "first" },
      { convo: "b", title: "b" },
    ]);
  });

  it("trims a title to exactly 200 chars", () => {
    const long = "t".repeat(250);
    const s = parseStored(
      JSON.stringify({ v: 1, tabs: [{ convo: "a", title: long }], active: 0 }),
    );
    expect(s?.tabs[0].title).toHaveLength(200);
    expect(s?.tabs[0].title).toBe("t".repeat(200));
  });

  it("keeps a 200-char title untouched", () => {
    const exact = "t".repeat(200);
    const s = parseStored(
      JSON.stringify({ v: 1, tabs: [{ convo: "a", title: exact }], active: 0 }),
    );
    expect(s?.tabs[0].title).toBe(exact);
  });

  it("keeps at most 100 tabs (the first 100)", () => {
    const tabs = Array.from({ length: 105 }, (_, i) => ({
      convo: `c${i}`,
      title: `t${i}`,
    }));
    const s = parseStored(JSON.stringify({ v: 1, tabs, active: 0 }));
    expect(s?.tabs).toHaveLength(100);
    expect(s?.tabs[0]).toEqual({ convo: "c0", title: "t0" });
    expect(s?.tabs[99]).toEqual({ convo: "c99", title: "t99" });
  });

  it("keeps exactly 100 tabs when there are exactly 100", () => {
    const tabs = Array.from({ length: 100 }, (_, i) => ({
      convo: `c${i}`,
      title: `t${i}`,
    }));
    const s = parseStored(JSON.stringify({ v: 1, tabs, active: 99 }));
    expect(s?.tabs).toHaveLength(100);
    expect(s?.active).toBe(99);
  });
});

// ─── parseStored: active index ───────────────────────────────────────────

describe("parseStored — active index", () => {
  const tabs3 = [
    { convo: "a", title: "a" },
    { convo: "b", title: "b" },
    { convo: "c", title: "c" },
  ];
  const raw = (active: unknown) =>
    JSON.stringify({ v: 1, tabs: tabs3, ...(active === undefined ? {} : { active }) });

  it("keeps a valid in-range integer active", () => {
    expect(parseStored(raw(1))?.active).toBe(1);
    expect(parseStored(raw(0))?.active).toBe(0);
  });

  it("falls back to 0 when active is missing", () => {
    expect(parseStored(raw(undefined))?.active).toBe(0);
  });

  it.each([
    ["a float", 1.5],
    ["a string", "0"],
    ["a negative", -1],
    ["out of range high", 3],
    ["out of range huge", 99],
    ["a boolean", true],
    ["an object", { i: 0 }],
    ["null", null],
  ])("falls back to 0 when active is %s", (_label, active) => {
    expect(parseStored(raw(active))?.active).toBe(0);
  });
});

// ─── parseStored: the design example ─────────────────────────────────────

describe("parseStored — the design schema", () => {
  it("parses the exact schema from design §3", () => {
    const s = parseStored(
      JSON.stringify({
        v: 1,
        tabs: [
          { convo: "6f1c0000-1111-4111-8111-111111111111", title: "Fix login bug" },
          { convo: null, title: "" },
        ],
        active: 0,
      }),
    );
    expect(s).toEqual({
      tabs: [
        { convo: "6f1c0000-1111-4111-8111-111111111111", title: "Fix login bug" },
        { convo: null, title: "" },
      ],
      active: 0,
    });
  });
});

// ─── loadTabs ────────────────────────────────────────────────────────────

describe("loadTabs", () => {
  it("returns null when storage is empty (first launch)", () => {
    expect(loadTabs(asStorage(new FakeStorage()))).toBeNull();
  });

  it("parses a valid stored state", () => {
    const storage = new FakeStorage();
    storage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({ v: 1, tabs: [{ convo: "a", title: "A" }], active: 0 }),
    );
    expect(loadTabs(asStorage(storage))).toEqual({
      tabs: [{ convo: "a", title: "A" }],
      active: 0,
    });
  });

  it("returns null when restore is false, even with valid stored data", () => {
    const storage = new FakeStorage();
    storage.setItem(
      "peakbot.desktop.tabs",
      JSON.stringify({ v: 1, tabs: [{ convo: "a", title: "A" }], active: 0 }),
    );
    expect(loadTabs(asStorage(storage), false)).toBeNull();
  });

  it("returns null (never throws) when the storage itself throws", () => {
    expect(loadTabs(asStorage(new ThrowingStorage()))).toBeNull();
  });
});

// ─── saveTabs ────────────────────────────────────────────────────────────

describe("saveTabs", () => {
  it("writes {v:1, tabs, active} under TABS_STORAGE_KEY", () => {
    const storage = new FakeStorage();
    saveTabs(asStorage(storage), state([{ convo: "a", title: "A" }, { convo: null, title: "" }], 1));

    const raw = storage.getItem(TABS_STORAGE_KEY);
    expect(raw).not.toBeNull();
    const parsed = JSON.parse(raw!);
    expect(parsed).toEqual({
      v: 1,
      tabs: [
        { convo: "a", title: "A" },
        { convo: null, title: "" },
      ],
      active: 1,
    });
  });

  it("round-trips through parseStored", () => {
    const storage = new FakeStorage();
    const s = state([{ convo: "a", title: "A" }, { convo: null, title: "" }], 1);
    saveTabs(asStorage(storage), s);
    expect(parseStored(storage.getItem(TABS_STORAGE_KEY))).toEqual(s);
  });

  it("never throws when the storage throws (quota / disabled)", () => {
    expect(() =>
      saveTabs(asStorage(new ThrowingStorage()), state([{ convo: null, title: "" }])),
    ).not.toThrow();
  });
});
