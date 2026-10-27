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
// `saveTabs`: writes `{v:1, tabs:[{convo,title}], active}`; never throws.
//
// Tab ids (desktop-tab-composers, I5): ids are RUNTIME-only React keys. The
// stored format is unchanged — `saveTabs` writes exactly `{convo, title}` per
// tab (no `id` key) and `parseStored` mints fresh unique ids via `freshTab`,
// ignoring any `id` found in storage.
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

/** A parsed tab: `{convo, title}` plus the runtime id minted at parse time. */
const parsed = (convo: string | null, title: string) => ({
  id: expect.any(String),
  convo,
  title,
});
/** The `{convo, title}` projection of parsed tabs (drops the minted id). */
const stripIds = (s: TabsState | null) =>
  s?.tabs.map((t) => ({ convo: t.convo, title: t.title }));

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
  ({ tabs: tabs.map((t, i) => ({ id: `rt-${i}`, ...t })), active }) as TabsState;

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
    expect(s?.tabs).toEqual([parsed("a", "good"), parsed(null, "")]);
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
    expect(s?.tabs).toEqual([parsed("c", "ok")]);
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
    expect(s?.tabs).toEqual([parsed("a", "first"), parsed("b", "b")]);
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
    expect(s?.tabs[0]).toEqual(parsed("c0", "t0"));
    expect(s?.tabs[99]).toEqual(parsed("c99", "t99"));
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
        parsed("6f1c0000-1111-4111-8111-111111111111", "Fix login bug"),
        parsed(null, ""),
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
      tabs: [parsed("a", "A")],
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
  it("writes {v:1, tabs:[{convo,title}], active} under TABS_STORAGE_KEY", () => {
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

  it("round-trips convo, title and active through parseStored (ids are re-minted)", () => {
    const storage = new FakeStorage();
    const s = state([{ convo: "a", title: "A" }, { convo: null, title: "" }], 1);
    saveTabs(asStorage(storage), s);
    const back = parseStored(storage.getItem(TABS_STORAGE_KEY));
    expect(back).not.toBeNull();
    expect(stripIds(back)).toEqual(stripIds(s));
    expect(back!.active).toBe(1);
  });

  it("never throws when the storage throws (quota / disabled)", () => {
    expect(() =>
      saveTabs(asStorage(new ThrowingStorage()), state([{ convo: null, title: "" }])),
    ).not.toThrow();
  });
});

// ─── tab ids (I5): runtime-only, never persisted ─────────────────────────

describe("saveTabs — ids are not persisted", () => {
  it("writes exactly {convo, title} per tab: no `id` key anywhere", () => {
    const storage = new FakeStorage();
    saveTabs(asStorage(storage), state([{ convo: "a", title: "A" }, { convo: null, title: "" }], 0));
    const written = JSON.parse(storage.getItem(TABS_STORAGE_KEY)!) as {
      tabs: Record<string, unknown>[];
    };
    for (const t of written.tabs) {
      expect(Object.keys(t).sort()).toEqual(["convo", "title"]);
    }
    expect(storage.getItem(TABS_STORAGE_KEY)).not.toContain("rt-0");
  });

  it("keeps the top-level shape exactly {v, tabs, active}", () => {
    const storage = new FakeStorage();
    saveTabs(asStorage(storage), state([{ convo: "a", title: "A" }], 0));
    const written = JSON.parse(storage.getItem(TABS_STORAGE_KEY)!) as Record<string, unknown>;
    expect(Object.keys(written).sort()).toEqual(["active", "tabs", "v"]);
    expect(written.v).toBe(1);
  });
});

describe("parseStored — ids are minted fresh", () => {
  const raw = (tabs: unknown[]) => JSON.stringify({ v: 1, tabs, active: 0 });

  it("gives every parsed tab a non-empty string id", () => {
    const s = parseStored(raw([{ convo: "a", title: "A" }, { convo: null, title: "" }]));
    for (const t of s!.tabs) {
      expect(typeof t.id).toBe("string");
      expect(t.id.length).toBeGreaterThan(0);
    }
  });

  it("gives every parsed tab a distinct id, including several null-convo tabs", () => {
    const s = parseStored(
      raw([
        { convo: null, title: "" },
        { convo: null, title: "" },
        { convo: "a", title: "A" },
        { convo: "b", title: "B" },
      ]),
    );
    const ids = s!.tabs.map((t) => t.id);
    expect(new Set(ids).size).toBe(4);
  });

  it("ignores an `id` present in storage (even a colliding one)", () => {
    const s = parseStored(
      raw([
        { id: "dup", convo: "a", title: "A" },
        { id: "dup", convo: "b", title: "B" },
      ]),
    );
    const ids = s!.tabs.map((t) => t.id);
    expect(ids).not.toContain("dup");
    expect(new Set(ids).size).toBe(2);
  });

  it("does not let a non-string stored id break the entry", () => {
    const s = parseStored(raw([{ id: 42, convo: "a", title: "A" }]));
    expect(stripIds(s)).toEqual([{ convo: "a", title: "A" }]);
    expect(typeof s!.tabs[0].id).toBe("string");
  });

  it("mints different ids on every parse of the same payload", () => {
    const body = raw([{ convo: "a", title: "A" }]);
    expect(parseStored(body)!.tabs[0].id).not.toBe(parseStored(body)!.tabs[0].id);
  });

  it("dropped entries do not leave holes: surviving tabs still all have ids", () => {
    const s = parseStored(
      raw([{ convo: 1, title: "bad" }, { convo: "ok", title: "ok" }, { convo: "ok", title: "dup" }]),
    );
    expect(s!.tabs).toHaveLength(1);
    expect(typeof s!.tabs[0].id).toBe("string");
  });
});

describe("saveTabs → loadTabs round trip", () => {
  it("keeps convo, title and active for a multi-tab state", () => {
    const storage = new FakeStorage();
    const s = state(
      [
        { convo: "a", title: "Alpha" },
        { convo: null, title: "" },
        { convo: "c", title: "Gamma ünïcødé ✓" },
      ],
      2,
    );
    saveTabs(asStorage(storage), s);
    const back = loadTabs(asStorage(storage));
    expect(stripIds(back)).toEqual(stripIds(s));
    expect(back!.active).toBe(2);
  });
});
