// Tests for `chatList.ts` — the pure list view-model (design §3.1/3.2/3.3/3.5).
//
// No DOM, no storage, no `Date.now()`: every time table passes an explicit
// `now`, so the suite is timezone-independent. Fixture convention per the
// design: `L(y,mo,d,h,mi,s)` is a local-date ISO string, `NOW` is a fixed
// local instant, `row(id, over)` is a `ConversationSummary` with sane
// defaults.
//
// RED before implementation: `./chatList` does not exist yet, so this file
// fails at import — the right reason (feature absent).

import { describe, expect, it } from "vitest";
import {
  NO_ACTIVITY,
  backBadge,
  filterChats,
  foldActivity,
  groupChats,
  metaLine,
  relativeTime,
  type Activity,
  type ChatRow,
  type ChatSection,
  type Range,
} from "./chatList";
import type { ConversationSummary } from "./state";

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

describe("metaLine", () => {
  it("assembles count, model and relative time", () => {
    expect(
      metaLine(row("a", { message_count: 24, updated_at: L(2026, 11, 10, 18, 59, 40) }), NOW),
    ).toBe("24 msgs · sonnet · just now");
  });

  it("singularises one message", () => {
    expect(metaLine(row("a", { message_count: 1 }), NOW)).toBe("1 msg · sonnet · 7h ago");
  });

  it("drops the relative segment when unparseable", () => {
    expect(metaLine(row("a", { updated_at: "" }), NOW)).toBe("3 msgs · sonnet");
  });
});

// ─── §3.3 search + profile filter ──────────────────────────────────────────

describe("filterChats", () => {
  const A = row("a", { name: "Debug WS relay timeout", profile: "home" });
  const B = row("b", { name: "Tune WS keepalive", profile: "home" });
  const C = row("c", { name: "ws ws" });
  const D = row("d", { name: "Clippy a.b", profile: "dev" });

  const idsOf = (rows: ChatRow[]): [string, Range[]][] =>
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

describe("backBadge", () => {
  const A = row("a");
  const B = row("b");

  it("no badge when only the bound chat is active", () => {
    expect(backBadge([row("a", { running: true }), B], new Set(), "a")).toBe(false);
    expect(backBadge([A, B], new Set(["a"]), "a")).toBe(false);
    expect(backBadge([A], new Set(["Z"]), "a")).toBe(false);
  });

  it("badge when an unbound chat is running or unread", () => {
    expect(backBadge([A, row("b", { running: true })], new Set(), "a")).toBe(true);
    expect(backBadge([A, B], new Set(["b"]), "a")).toBe(true);
  });

  it("no badge with an empty list", () => {
    expect(backBadge([], new Set(), null)).toBe(false);
  });
});

describe("NO_ACTIVITY", () => {
  it("is an empty running/unread activity", () => {
    expect(NO_ACTIVITY.running.size).toBe(0);
    expect(NO_ACTIVITY.unread.size).toBe(0);
  });
});
