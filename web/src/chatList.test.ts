// Tests for `chatList.ts` — the pure list view-model (design §3.1/3.2/3.3/3.5).
//
// No DOM, no storage, no `Date.now()`: every time table passes an explicit
// `now`, so the suite is timezone-independent. Fixture convention per the
// design: `L(y,mo,d,h,mi,s)` is a local-date ISO string, `NOW` is a fixed
// local instant, `row(id, over)` is a `ConversationSummary` with sane
// defaults.
//
// RED before implementation: the machine helpers (`deriveMachines`,
// `machineOf`, `machineName`, `resolveFilter`, `filterByMachine`,
// `ALL_MACHINES`, `LOCAL_MACHINE_NAME`) are not exported yet, so the P1 tests
// fail with "is not a function" — the right reason (feature absent). The
// pre-existing groupChats/filterChats/... tests above must stay green.

import { beforeEach, describe, expect, it } from "vitest";
import {
  ALL_MACHINES,
  LOCAL_MACHINE_NAME,
  NO_ACTIVITY,
  deriveMachines,
  filterByMachine,
  filterChats,
  foldActivity,
  groupChats,
  machineName,
  machineOf,
  relativeTime,
  resolveFilter,
  type Activity,
  type ChatRow,
  type ChatSection,
  type Machine,
  type MachineFilter,
  type Range,
} from "./chatList";
import type { ConversationSummary, WireMachineStatus } from "./state";
import type { CatalogProfile } from "./types";
import type { Device } from "./cloud/types";

// ─── fixtures (design §3 convention) ──────────────────────────────────────

const L = (y: number, mo: number, d: number, h: number, mi: number, s = 0): string =>
  new Date(y, mo - 1, d, h, mi, s).toISOString();
// Local Tue 10 Nov 2026 19:00 — the reference instant for every table below.
const NOW = new Date(2026, 10, 10, 19, 0, 0);

const row = (id: string, over: Partial<ConversationSummary> = {}): ConversationSummary =>
  ({
    id,
    name: id,
    updated_at: L(2026, 11, 10, 12, 0),
    message_count: 3,
    model: "sonnet",
    active: false,
    running: false,
    profile: null,
    ...over,
  }) as ConversationSummary;

/** Wrap a summary in the row shape `groupChats` consumes (no search matches). */
const cr = (chat: ConversationSummary): ChatRow => ({ chat, titleMatches: [] as Range[] });

const sectionsOf = (secs: ChatSection[]): { kind: string; label: string; ids: string[] }[] =>
  secs.map((s) => ({ kind: s.kind, label: s.label, ids: s.rows.map((r) => r.chat.id) }));

// ─── §3.1 sections ─────────────────────────────────────────────────────────

describe("groupChats — sections", () => {
  it("sorts rows into running, live, today, yesterday, earlier with fixed labels", () => {
    const rows = [
      cr(row("a", { running: true, active: true, updated_at: L(2026, 11, 10, 18, 59) })),
      cr(row("b", { running: true, active: true, updated_at: L(2026, 11, 10, 18, 0) })),
      cr(row("c", { active: true, updated_at: L(2026, 11, 10, 18, 48) })),
      cr(row("d", { active: true, updated_at: L(2026, 11, 9, 23, 0) })),
      cr(row("e", { updated_at: L(2026, 11, 10, 9, 0) })),
      cr(row("f", { updated_at: L(2026, 11, 10, 0, 0, 0) })),
      cr(row("g", { updated_at: L(2026, 11, 9, 23, 59, 59) })),
      cr(row("h", { updated_at: L(2026, 11, 8, 12, 0) })),
      cr(row("i", { updated_at: "" })),
      cr(row("j", { updated_at: L(2026, 11, 11, 1, 0) })),
    ];

    expect(sectionsOf(groupChats(rows, NOW, false))).toEqual([
      { kind: "running", label: "RUNNING NOW", ids: ["a", "b"] },
      { kind: "live", label: "LIVE", ids: ["c", "d"] },
      { kind: "today", label: "TODAY", ids: ["j", "e", "f"] },
      { kind: "yesterday", label: "YESTERDAY", ids: ["g"] },
      { kind: "earlier", label: "EARLIER", ids: ["h", "i"] },
    ]);
  });

  it("running wins over live", () => {
    const rows = [cr(row("x", { running: true, active: false }))];
    expect(sectionsOf(groupChats(rows, NOW, false))).toEqual([
      { kind: "running", label: "RUNNING NOW", ids: ["x"] },
    ]);
  });

  it("a row with no running field (old server) lands by date", () => {
    const x = row("x");
    delete x.running;
    expect(sectionsOf(groupChats([cr(x)], NOW, false))).toEqual([
      { kind: "today", label: "TODAY", ids: ["x"] },
    ]);
  });

  it("ties on updated_at break by id ascending", () => {
    const rows = [
      cr(row("b", { updated_at: L(2026, 11, 10, 12, 0) })),
      cr(row("a", { updated_at: L(2026, 11, 10, 12, 0) })),
    ];
    expect(sectionsOf(groupChats(rows, NOW, false))).toEqual([
      { kind: "today", label: "TODAY", ids: ["a", "b"] },
    ]);
  });

  it("no rows, no sections", () => {
    expect(groupChats([], NOW, false)).toEqual([]);
  });

  it("searching collapses to one matches section ordered running, live, rest", () => {
    const rows = [
      cr(row("e", { updated_at: L(2026, 11, 10, 9, 0) })),
      cr(row("c", { active: true })),
      cr(row("a", { running: true })),
    ];
    expect(sectionsOf(groupChats(rows, NOW, true))).toEqual([
      { kind: "matches", label: "3 MATCHES", ids: ["a", "c", "e"] },
    ]);
  });

  it("one match labels the section 1 MATCH", () => {
    const rows = [cr(row("a", { running: true }))];
    expect(sectionsOf(groupChats(rows, NOW, true))).toEqual([
      { kind: "matches", label: "1 MATCH", ids: ["a"] },
    ]);
  });

  it("searching with no rows is empty", () => {
    expect(groupChats([], NOW, true)).toEqual([]);
  });
});

// ─── §3.2 relative time + meta line ────────────────────────────────────────

describe("relativeTime", () => {
  const cases: [string, string][] = [
    [L(2026, 11, 10, 18, 59, 30), "just now"],
    [L(2026, 11, 10, 19, 5), "just now"], // future
    [L(2026, 11, 10, 18, 59, 0), "1m ago"],
    [L(2026, 11, 10, 18, 0, 1), "59m ago"],
    [L(2026, 11, 10, 18, 0, 0), "1h ago"],
    [L(2026, 11, 10, 0, 0), "19h ago"],
    [L(2026, 11, 9, 23, 59), "Yesterday"],
    [L(2026, 11, 9, 0, 0), "Yesterday"],
    [L(2026, 11, 8, 12, 0), "8 Nov"],
    [L(2025, 12, 31, 12, 0), "31 Dec 2025"],
    ["garbage", ""],
  ];
  it.each(cases)("renders %s as %s", (updatedAt, expected) => {
    expect(relativeTime(updatedAt, NOW)).toBe(expected);
  });

  it("minutes beat the calendar when the day differs", () => {
    expect(relativeTime(L(2026, 11, 9, 23, 58), new Date(2026, 10, 10, 0, 3))).toBe("5m ago");
  });
});

// `metaLine` is deleted in batch 2 (plan 2.15): the row's second line is
// built in the component from `profile · [machine tag] · relativeTime`.

// ─── §3.3 search + profile filter ──────────────────────────────────────────

describe("filterChats", () => {
  const A = row("a", { name: "Debug WS relay timeout", profile: "home" });
  const B = row("b", { name: "Tune WS keepalive", profile: "home" });
  const C = row("c", { name: "ws ws" });
  const D = row("d", { name: "Clippy a.b", profile: "dev" });

  const idsOf = (rows: ChatRow[]): [string, readonly Range[]][] =>
    rows.map((r) => [r.chat.id, r.titleMatches]);

  it("matches case-insensitively and reports every occurrence", () => {
    expect(idsOf(filterChats([A, B, C, D], "ws", null))).toEqual([
      ["a", [[6, 8]]],
      ["b", [[5, 7]]],
      ["c", [[0, 2], [3, 5]]],
    ]);
  });

  it("trims the query", () => {
    expect(idsOf(filterChats([A, B, C, D], "  WS ", null))).toEqual([
      ["a", [[6, 8]]],
      ["b", [[5, 7]]],
      ["c", [[0, 2], [3, 5]]],
    ]);
  });

  it("an empty query keeps every row with no matches", () => {
    expect(idsOf(filterChats([A, B, C, D], "", null))).toEqual([
      ["a", []],
      ["b", []],
      ["c", []],
      ["d", []],
    ]);
  });

  it("a null profile keeps all; a named one keeps only that profile", () => {
    expect(idsOf(filterChats([A, B, C, D], "", "home"))).toEqual([["a", []], ["b", []]]);
  });

  it("query and profile combine", () => {
    expect(filterChats([A, B, C, D], "ws", "dev")).toEqual([]);
  });

  it("an unknown profile keeps nothing", () => {
    expect(filterChats([A, B, C, D], "", "nope")).toEqual([]);
  });

  it("the query is a literal substring, not a pattern", () => {
    expect(idsOf(filterChats([D], "a.b", null))).toEqual([["d", [[7, 10]]]]);
    expect(filterChats([D], "axb", null)).toEqual([]);
  });

  it("a name that changes length when lowercased matches without highlights", () => {
    expect(idsOf(filterChats([row("i", { name: "İstanbul ws" })], "ws", null))).toEqual([
      ["i", []],
    ]);
  });

  it("legacy (profile-less) rows stay under All only", () => {
    expect(filterChats([C], "", "dev")).toEqual([]);
  });

  it("keeps the input order", () => {
    expect(filterChats([D, C, B, A], "", null).map((r) => r.chat.id)).toEqual(["d", "c", "b", "a"]);
  });
});

// ─── §3.5 running / unread fold + back badge ───────────────────────────────

describe("foldActivity", () => {
  const asSets = (a: Activity): [string[], string[]] => [
    [...a.running].sort(),
    [...a.unread].sort(),
  ];
  const A = (running = false) => row("a", { running });
  const withRunning = (ids: string[]): Activity => ({ running: new Set(ids), unread: new Set() });
  const withUnread = (ids: string[]): Activity => ({ running: new Set(), unread: new Set(ids) });

  it("picks up newly running chats from an empty prev", () => {
    expect(asSets(foldActivity(NO_ACTIVITY, [A(true), row("b")], null))).toEqual([["a"], []]);
  });

  it("a running→idle edge sets unread", () => {
    expect(asSets(foldActivity(withRunning(["a"]), [A(false)], null))).toEqual([[], ["a"]]);
  });

  it("the edge is suppressed for the chat being viewed", () => {
    expect(asSets(foldActivity(withRunning(["a"]), [A(false)], "a"))).toEqual([[], []]);
  });

  it("opening a chat clears its unread", () => {
    expect(asSets(foldActivity(withUnread(["a"]), [A(false)], "a"))).toEqual([[], []]);
  });

  it("a running chat that is not listed stays running (unknown is not finished)", () => {
    expect(asSets(foldActivity(withRunning(["a"]), [row("b")], null))).toEqual([["a"], []]);
  });

  it("returns the same reference when nothing changed", () => {
    const prev = withRunning(["a"]);
    expect(foldActivity(prev, [A(true)], null)).toBe(prev);
  });

  it("a listed running chat joins running while keeping unread", () => {
    expect(asSets(foldActivity(withUnread(["a"]), [A(true)], null))).toEqual([["a"], ["a"]]);
  });
});


describe("NO_ACTIVITY", () => {
  it("is an empty running/unread activity", () => {
    expect(NO_ACTIVITY.running.size).toBe(0);
    expect(NO_ACTIVITY.unread.size).toBe(0);
  });
});

// ─── P1: machine derivation + filter (mobile-redesign design §2.4) ────────
//
// A "machine" is this device (id `null`) or one remote origin. The helpers
// are pure: derived from the catalog (profile → machine) and the optional
// `machines` statuses of the last `conversations_list`.

const HS = "https://home-server.local:7823";
const WB = "https://10.0.0.5:7823";

const prof = (name: string, machine: string | null): CatalogProfile => ({
  name,
  origin: machine === null ? "global" : "remote",
  overridesGlobal: false,
  machine,
  agents: [],
});

const CAT: CatalogProfile[] = [
  prof("dev", null),
  prof("home-dev", HS),
  prof("media", HS),
  prof("work", WB),
];

describe("machineOf", () => {
  it("returns the catalog entry's machine for a local and a remote profile", () => {
    expect(machineOf(CAT, "dev")).toBeNull();
    expect(machineOf(CAT, "home-dev")).toBe(HS);
    expect(machineOf(CAT, "work")).toBe(WB);
  });

  it("a null profile (legacy row) is local", () => {
    expect(machineOf(CAT, null)).toBeNull();
  });

  it("an unknown profile is local", () => {
    expect(machineOf(CAT, "nope")).toBeNull();
  });

  it("an empty catalog makes everything local", () => {
    expect(machineOf([], "home-dev")).toBeNull();
  });
});

describe("machineName", () => {
  it("null is 'This device' (LOCAL_MACHINE_NAME)", () => {
    expect(LOCAL_MACHINE_NAME).toBe("This device");
    expect(machineName(null)).toBe("This device");
  });

  // Batch 2, item 2.20 (A39): display-only host WITHOUT the port. Ids and
  // filters still use the full origin; two servers on one host (different
  // ports) show the same name — the documented trade-off (§9 Q3).
  it("shows the host without the port", () => {
    expect(machineName("https://h:7823")).toBe("h");
    expect(machineName("https://10.0.0.5:7823")).toBe("10.0.0.5");
  });

  it("a default-port origin is the bare host", () => {
    expect(machineName("https://h")).toBe("h");
    expect(machineName("https://bot.example.ts.net")).toBe("bot.example.ts.net");
  });
});

describe("machineName — cloud slugs (P4 §F.2)", () => {
  const cloud: Device[] = [
    { id: "aa".repeat(32), name: "Laptop", platform: "linux", online: true, slug: "laptop", self: true },
    { id: "bb".repeat(32), name: "Desk", platform: "macos", online: null, slug: "desk", self: false },
  ];

  it("a slug with a matching cloud device → the device display name", () => {
    expect(machineName("laptop", cloud)).toBe("Laptop");
    expect(machineName("desk", cloud)).toBe("Desk");
  });

  it("a slug with NO match → the slug itself (never throws)", () => {
    expect(machineName("ghost", cloud)).toBe("ghost");
  });

  it("a bare slug with no cloudDevices → the slug, and it must NEVER throw", () => {
    // Regression: the old `new URL("laptop")` threw `TypeError: Invalid URL`.
    expect(() => machineName("laptop")).not.toThrow();
    expect(machineName("laptop")).toBe("laptop");
  });

  it("an origin URL ignores cloudDevices → the hostname (unchanged)", () => {
    expect(machineName("https://h:7823", cloud)).toBe("h");
    expect(machineName("https://bot.example.ts.net", cloud)).toBe("bot.example.ts.net");
  });
});

describe("deriveMachines", () => {
  const st = (id: string, online: boolean): WireMachineStatus => ({ id, online });
  const ids = (m: Machine[]): (string | null)[] => m.map((x) => x.id);

  it("local first, then remotes in first-catalog-appearance order", () => {
    const got = deriveMachines(CAT, []);
    expect(ids(got)).toEqual([null, HS, WB]);
  });

  it("groups profiles by origin: one machine per distinct origin, profiles in catalog order", () => {
    const got = deriveMachines(CAT, []);
    expect(got.find((m) => m.id === HS)!.profiles).toEqual(["home-dev", "media"]);
    expect(got.find((m) => m.id === WB)!.profiles).toEqual(["work"]);
    expect(got.find((m) => m.id === null)!.profiles).toEqual(["dev"]);
  });

  it("names come from machineName", () => {
    const got = deriveMachines(CAT, []);
    expect(got.map((m) => m.name)).toEqual(["This device", "home-server.local", "10.0.0.5"]);
  });

  it("no local entry when the catalog has no local profile", () => {
    const remoteOnly = [prof("a", HS), prof("b", WB)];
    expect(ids(deriveMachines(remoteOnly, []))).toEqual([HS, WB]);
  });

  it("local is always online; a remote follows its status", () => {
    const got = deriveMachines(CAT, [st(HS, true), st(WB, false)]);
    expect(got.find((m) => m.id === null)!.online).toBe(true);
    expect(got.find((m) => m.id === HS)!.online).toBe(true);
    expect(got.find((m) => m.id === WB)!.online).toBe(false);
  });

  it("a remote with no status is online (unknown is not offline)", () => {
    const got = deriveMachines(CAT, [st(WB, false)]);
    expect(got.find((m) => m.id === HS)!.online).toBe(true);
  });

  it("a status for an origin the catalog does not carry is ignored", () => {
    const got = deriveMachines(CAT, [st("https://ghost:1", false)]);
    expect(ids(got)).toEqual([null, HS, WB]);
  });

  it("a cloud machine (bare slug id) is named by its device name (P4 §F.2)", () => {
    const cloud: Device[] = [
      { id: "aa".repeat(32), name: "Laptop", platform: "linux", online: true, slug: "laptop", self: false },
    ];
    const got = deriveMachines([prof("work@laptop", "laptop")], [], cloud);
    expect(got.map((m) => m.name)).toEqual(["Laptop"]);
  });

  it("an empty catalog derives no machines", () => {
    expect(deriveMachines([], [st(HS, true)])).toEqual([]);
  });

  it("a catalog of local profiles only is one machine", () => {
    expect(ids(deriveMachines([prof("a", null), prof("b", null)], []))).toEqual([null]);
  });

  it("every machine has a non-empty profile list", () => {
    for (const m of deriveMachines(CAT, [])) expect(m.profiles.length).toBeGreaterThan(0);
  });
});

describe("resolveFilter", () => {
  // Lazy: a describe-body call would fail the whole FILE at collection while
  // `deriveMachines` is absent, hiding the pre-existing tests above.
  let machines: Machine[] = [];
  beforeEach(() => {
    machines = deriveMachines(CAT, []);
  });
  const one = (id: string | null): MachineFilter => ({ kind: "one", id });

  it("a valid selection passes through", () => {
    expect(resolveFilter(one(HS), machines)).toEqual(one(HS));
    expect(resolveFilter(one(null), machines)).toEqual(one(null));
  });

  it("All stays All", () => {
    expect(resolveFilter(ALL_MACHINES, machines)).toEqual({ kind: "all" });
  });

  it("a stale id (machine no longer exists) resolves to All", () => {
    expect(resolveFilter(one("https://gone:1"), machines)).toEqual({ kind: "all" });
  });

  it("fewer than 2 machines resolves to All even for a valid id", () => {
    const single = deriveMachines([prof("a", null)], []);
    expect(resolveFilter(one(null), single)).toEqual({ kind: "all" });
    expect(resolveFilter(one(null), [])).toEqual({ kind: "all" });
  });

  it("ALL_MACHINES is the all filter", () => {
    expect(ALL_MACHINES).toEqual({ kind: "all" });
  });
});

describe("filterByMachine", () => {
  const rows = [
    row("l1", { profile: "dev" }),
    row("h1", { profile: "home-dev" }),
    row("h2", { profile: "media" }),
    row("w1", { profile: "work" }),
    row("legacy", { profile: null }),
  ];
  const idsOf = (list: ConversationSummary[]): string[] => list.map((r) => r.id);

  it("All returns every row, unchanged", () => {
    expect(idsOf(filterByMachine(rows, CAT, ALL_MACHINES))).toEqual(["l1", "h1", "h2", "w1", "legacy"]);
  });

  it("one remote machine keeps the rows of all its profiles", () => {
    expect(idsOf(filterByMachine(rows, CAT, { kind: "one", id: HS }))).toEqual(["h1", "h2"]);
  });

  it("this device keeps local rows, including legacy profile-less ones", () => {
    expect(idsOf(filterByMachine(rows, CAT, { kind: "one", id: null }))).toEqual(["l1", "legacy"]);
  });

  it("keeps the input order", () => {
    const shuffled = [rows[2], rows[1], rows[0]];
    expect(idsOf(filterByMachine(shuffled, CAT, { kind: "one", id: HS }))).toEqual(["h2", "h1"]);
  });

  it("an empty list stays empty", () => {
    expect(filterByMachine([], CAT, { kind: "one", id: HS })).toEqual([]);
  });

  it("composes with filterChats", () => {
    const list = filterByMachine(rows, CAT, { kind: "one", id: HS });
    expect(filterChats(list, "", "media").map((r) => r.chat.id)).toEqual(["h2"]);
  });
});
