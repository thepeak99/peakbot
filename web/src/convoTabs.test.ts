// Tests for `convoTabs.ts` — the pure, immutable tab store (design §3/§4/§5).
//
// Contract under test (all functions pure; invalid index → state returned
// unchanged, *same reference*):
//
//   NEW_TAB_LABEL, initialTabs, activeTab, newTab, activate, closeTab,
//   cycle, jumpTo, openConvo, bound, retitle, applyList, enter, leave,
//   tabViews
//
// Invariants every op must keep:
//   I1 tabs.length >= 1 — closing the last tab gives one fresh null-convo tab
//   I2 0 <= active < tabs.length
//   I3 non-null convo values are unique (several null tabs allowed)
//   I4 pure + structural sharing: unchanged tabs keep object identity
//
//   I5 (desktop-tab-composers) ids are unique and never change for the life
//      of a tab. `freshTab(convo?, title?)` is the ONLY place ids are minted.
//
// Fixtures below carry LITERAL ids (not `freshTab`) so the pre-existing
// behaviour tests don't depend on the new minting code; the "tab ids (I5)"
// block at the bottom is the executable contract for the new behaviour.
//
// Runs under `node` (pure logic, no DOM).

import { describe, expect, it } from "vitest";
import type { ConversationSummary } from "./state";
import type { ConvoTab, StatusMap, TabsState } from "./convoTabs";
import {
  NEW_TAB_LABEL,
  activate,
  activeTab,
  applyList,
  bound,
  closeTab,
  cycle,
  enter,
  freshTab,
  initialTabs,
  jumpTo,
  leave,
  newTab,
  openConvo,
  retitle,
  tabViews,
} from "./convoTabs";

// ─── fixtures ────────────────────────────────────────────────────────────

/** Literal-id fixture: the id defaults to `id-<convo>` (unique per convo). */
const tab = (convo: string | null, title = "", id = `id-${convo}`): ConvoTab => ({
  id,
  convo,
  title,
});
const state = (tabs: ConvoTab[], active = 0): TabsState => ({ tabs, active });

const A = tab("a", "Fix login bug", "id-A");
const B = tab("b", "Refactor parser", "id-B");
const C = tab("c", "Write tests", "id-C");
const FRESH = tab(null, "", "id-FRESH");

/** A conversation summary; `running` is the field this feature adds to
 * `state.ts` (design §5 backend), so fixtures may carry it while the type
 * catch-up lands. */
const convo = (
  over: Partial<ConversationSummary> & { running?: boolean } = {},
): ConversationSummary =>
  ({
    id: "c",
    name: "n",
    updated_at: "2026-10-16T00:00:00Z",
    message_count: 0,
    model: "m",
    active: false,
    ...over,
  } as ConversationSummary);

// ─── NEW_TAB_LABEL ───────────────────────────────────────────────────────

describe("NEW_TAB_LABEL", () => {
  it("is the untitled-tab label 'New conversation'", () => {
    expect(NEW_TAB_LABEL).toBe("New conversation");
  });
});

// ─── initialTabs ─────────────────────────────────────────────────────────
// Design §4: no stored state → one tab for the URL convo; no URL convo →
// stored unchanged; URL convo in tabs → activate; not in tabs → append+activate.

describe("initialTabs", () => {
  it("gives one tab for the URL convo when nothing is stored", () => {
    const s = initialTabs(null, "abc-123");
    expect(s).toEqual({
      tabs: [{ id: expect.any(String), convo: "abc-123", title: "" }],
      active: 0,
    });
  });

  it("gives one fresh null-convo tab when nothing is stored and no URL convo", () => {
    const s = initialTabs(null, null);
    expect(s).toEqual({
      tabs: [{ id: expect.any(String), convo: null, title: "" }],
      active: 0,
    });
  });

  it("leaves stored state unchanged (same reference) when there is no URL convo", () => {
    const stored = state([A, B], 1);
    expect(initialTabs(stored, null)).toBe(stored);
  });

  it("activates the existing tab when the URL convo is already open", () => {
    const stored = state([A, B, C], 0);
    const s = initialTabs(stored, "b");
    expect(s.active).toBe(1);
    expect(s.tabs).toHaveLength(3);
    // Stored tabs are reused, not rebuilt.
    expect(s.tabs).toEqual(stored.tabs);
  });

  it("appends and activates the URL convo when it is not open", () => {
    const stored = state([A, B], 0);
    const s = initialTabs(stored, "d");
    expect(s.tabs).toHaveLength(3);
    expect(s.active).toBe(2);
    expect(s.tabs[2]).toEqual({ id: expect.any(String), convo: "d", title: "" });
    // Earlier tabs keep their identity (I4).
    expect(s.tabs[0]).toBe(A);
    expect(s.tabs[1]).toBe(B);
  });
});

// ─── activeTab ───────────────────────────────────────────────────────────

describe("activeTab", () => {
  it("returns the tab at the active index", () => {
    const s = state([A, B], 1);
    expect(activeTab(s)).toBe(B);
  });

  it("tracks activation", () => {
    const s = state([A, B, C], 0);
    expect(activeTab(activate(s, 2))).toBe(C);
  });
});

// ─── newTab ──────────────────────────────────────────────────────────────

describe("newTab", () => {
  it("appends a null-convo untitled tab and activates it", () => {
    const s = newTab(state([A, B], 0));
    expect(s.tabs).toHaveLength(3);
    expect(s.tabs[2]).toEqual({ id: expect.any(String), convo: null, title: "" });
    expect(s.active).toBe(2);
  });

  it("keeps unchanged tabs by identity (I4)", () => {
    const prev = state([A, B], 0);
    const next = newTab(prev);
    expect(next.tabs[0]).toBe(A);
    expect(next.tabs[1]).toBe(B);
    // The old active tab is untouched — "same active tab" by reference.
    expect(activeTab(next) !== activeTab(prev)).toBe(true);
  });
});

// ─── activate ────────────────────────────────────────────────────────────

describe("activate", () => {
  it("switches to the given index", () => {
    expect(activate(state([A, B, C], 0), 2).active).toBe(2);
  });

  it("returns the same state reference for an out-of-range index", () => {
    const s = state([A, B], 0);
    expect(activate(s, 2)).toBe(s);
    expect(activate(s, -1)).toBe(s);
  });

  it("returns the same state reference for a non-integer index", () => {
    const s = state([A, B], 0);
    expect(activate(s, 1.5)).toBe(s);
  });

  it("keeps the same active tab object when re-activating the current tab", () => {
    const s = state([A, B], 1);
    const next = activate(s, 1);
    expect(next.active).toBe(1);
    expect(activeTab(next)).toBe(activeTab(s));
  });
});

// ─── closeTab ────────────────────────────────────────────────────────────

describe("closeTab", () => {
  it("removes an inactive tab and keeps the active one", () => {
    const s = closeTab(state([A, B, C], 1), 2);
    expect(s.tabs.map((t: ConvoTab) => t.convo)).toEqual(["a", "b"]);
    expect(s.active).toBe(1);
  });

  it("moves to the right neighbour when the active tab is closed", () => {
    const s = closeTab(state([A, B, C], 0), 0);
    expect(s.tabs).toHaveLength(2);
    expect(activeTab(s)).toBe(B);
  });

  it("moves to the left neighbour when there is no right one", () => {
    const s = closeTab(state([A, B, C], 2), 2);
    expect(s.tabs).toHaveLength(2);
    expect(activeTab(s)).toBe(B);
  });

  it("gives a fresh empty tab when the last tab is closed (I1)", () => {
    const s = closeTab(state([A], 0), 0);
    expect(s.tabs).toEqual([{ id: expect.any(String), convo: null, title: "" }]);
    expect(s.active).toBe(0);
  });

  it("returns the same state reference for an out-of-range index", () => {
    const s = state([A, B], 0);
    expect(closeTab(s, 5)).toBe(s);
    expect(closeTab(s, -3)).toBe(s);
  });

  it("keeps surviving tabs by identity (I4)", () => {
    const prev = state([A, B, C], 0);
    const next = closeTab(prev, 1);
    expect(next.tabs[0]).toBe(A);
    expect(next.tabs[1]).toBe(C);
  });
});

// ─── cycle ───────────────────────────────────────────────────────────────

describe("cycle", () => {
  it("advances with +1 and wraps from last to first", () => {
    const s = state([A, B, C], 0);
    expect(cycle(s, 1).active).toBe(1);
    expect(cycle(state([A, B, C], 2), 1).active).toBe(0);
  });

  it("goes back with -1 and wraps from first to last", () => {
    expect(cycle(state([A, B, C], 0), -1).active).toBe(2);
    expect(cycle(state([A, B, C], 1), -1).active).toBe(0);
  });

  it("stays put on a single tab", () => {
    const s = state([A], 0);
    expect(cycle(s, 1).active).toBe(0);
    expect(cycle(s, -1).active).toBe(0);
  });
});

// ─── jumpTo ──────────────────────────────────────────────────────────────

describe("jumpTo", () => {
  it("maps n to index n-1 when that tab exists", () => {
    const s = state([A, B, C, tab("d"), tab("e")], 0);
    expect(jumpTo(s, 1).active).toBe(0);
    expect(jumpTo(s, 3).active).toBe(2);
  });

  it("jumps to the LAST tab for 9, regardless of count", () => {
    expect(jumpTo(state([A, B, C], 0), 9).active).toBe(2);
    expect(jumpTo(state(Array.from({ length: 12 }, (_, i) => tab(`t${i}`)), 0), 9).active).toBe(11);
  });

  it("leaves the state unchanged (same reference) when the tab does not exist", () => {
    const s = state([A, B, C], 1);
    expect(jumpTo(s, 4)).toBe(s); // only 3 tabs
    expect(jumpTo(s, 10)).toBe(s);
  });

  it("leaves the state unchanged (same reference) for 0 and non-integers", () => {
    const s = state([A, B, C], 1);
    expect(jumpTo(s, 0)).toBe(s);
    expect(jumpTo(s, 2.5)).toBe(s);
  });
});

// ─── openConvo ───────────────────────────────────────────────────────────

describe("openConvo", () => {
  it("activates an already-open convo without appending", () => {
    const s = openConvo(state([A, B], 0), "b");
    expect(s.tabs).toHaveLength(2);
    expect(s.active).toBe(1);
  });

  it("appends and activates a new convo, keeping the given title", () => {
    const s = openConvo(state([A], 0), "d", "Fresh convo");
    expect(s.tabs).toHaveLength(2);
    expect(s.active).toBe(1);
    expect(s.tabs[1]).toEqual({
      id: expect.any(String),
      convo: "d",
      title: "Fresh convo",
    });
  });

  it("stores an empty title when none is given", () => {
    const s = openConvo(state([A], 0), "d");
    expect(s.tabs[1]).toEqual({ id: expect.any(String), convo: "d", title: "" });
  });
});

// ─── bound ───────────────────────────────────────────────────────────────
// Design §4: the store follows the server's `attached`/state.conversation.id.
//   - active tab already on convo → no change
//   - another tab holds convo → remove that tab, bind the active tab (I3)
//   - otherwise → bind the active tab and reset its title to ""

describe("bound", () => {
  it("leaves the state unchanged (same reference) when the active tab is already on the convo", () => {
    const s = state([A, B], 0);
    expect(bound(s, "a")).toBe(s);
  });

  it("removes the duplicate tab and binds the active tab when another tab holds the convo", () => {
    const s = state([A, B], 0);
    const next = bound(s, "b");
    expect(next.tabs).toHaveLength(1);
    expect(next.tabs[0].convo).toBe("b");
    expect(next.active).toBe(0);
    // I3: no two tabs share a convo.
    expect(
      next.tabs.filter((t: ConvoTab) => t.convo === "b").length,
    ).toBe(1);
  });

  it("binds the active tab and resets its title when no other tab holds the convo", () => {
    const next = bound(state([A, C], 0), "d");
    // Binding keeps the active tab's identity (I5): only convo/title change.
    expect(next.tabs[0]).toEqual({ id: "id-A", convo: "d", title: "" });
    expect(next.tabs).toHaveLength(2);
  });

  it("keeps non-active tabs by identity (I4)", () => {
    const prev = state([A, C], 0);
    const next = bound(prev, "d");
    expect(next.tabs[1]).toBe(C);
  });
});

// ─── retitle ─────────────────────────────────────────────────────────────

describe("retitle", () => {
  it("renames the tab holding the convo", () => {
    const next = retitle(state([A, B], 0), "b", "New title");
    expect(next.tabs[1].title).toBe("New title");
  });

  it("returns the same reference when the title is unchanged", () => {
    const s = state([A, B], 0);
    expect(retitle(s, "b", "Refactor parser")).toBe(s);
  });

  it("returns the same reference when no tab holds the convo", () => {
    const s = state([A, B], 0);
    expect(retitle(s, "zzz", "Ghost")).toBe(s);
  });

  it("keeps other tabs by identity (I4)", () => {
    const prev = state([A, B], 0);
    const next = retitle(prev, "a", "Fixed login");
    expect(next.tabs[1]).toBe(B);
  });
});

// ─── applyList ───────────────────────────────────────────────────────────
// Design §4: for each INACTIVE tab convo found in the list,
//   running = item.running ?? false
//   unread  = prev.unread || (prev.running && !running)   (running→idle edge)
// The active convo is never stored.

describe("applyList", () => {
  const s = state([A, B, C], 0); // A active

  it("records running from the list for inactive tabs", () => {
    const out = applyList(
      {},
      [convo({ id: "a", running: true }), convo({ id: "b", running: true }), convo({ id: "c" })],
      s,
    );
    expect(out["b"]).toEqual({ running: true, unread: false });
    expect(out["c"]).toEqual({ running: false, unread: false });
  });

  it("never stores an entry for the active convo", () => {
    const out = applyList(
      {},
      [convo({ id: "a", running: true }), convo({ id: "b", running: false })],
      s,
    );
    expect("a" in out).toBe(false);
    expect(Object.keys(out)).toEqual(["b"]);
  });

  it("sets unread exactly on the running→idle edge", () => {
    const prev: StatusMap = {
      b: { running: true, unread: false }, // was running in the background
      c: { running: false, unread: false }, // was idle
      d: { running: true, unread: false }, // still running
    };
    // `d` must be an open (inactive) tab to be tracked at all.
    const withD = state([A, B, C, tab("d", "Fourth")], 0);
    const out = applyList(
      prev,
      [
        convo({ id: "b", running: false }),
        convo({ id: "c", running: false }),
        convo({ id: "d", running: true }),
      ],
      withD,
    );
    expect(out["b"]).toEqual({ running: false, unread: true }); // edge fired
    expect(out["c"]).toEqual({ running: false, unread: false }); // no edge
    expect(out["d"]).toEqual({ running: true, unread: false }); // still running
  });

  it("keeps unread sticky once set", () => {
    const prev: StatusMap = { b: { running: false, unread: true } };
    const out = applyList(prev, [convo({ id: "b", running: false })], s);
    expect(out["b"]).toEqual({ running: false, unread: true });
  });

  it("treats a missing running flag as false (old server without the field)", () => {
    const out = applyList({}, [convo({ id: "b" })], s);
    expect(out["b"]).toEqual({ running: false, unread: false });
  });

  it("never stores entries for null-convo tabs", () => {
    const sNull = state([A, FRESH], 0);
    const out = applyList({}, [convo({ id: "b", running: true })], sNull);
    expect(Object.keys(out)).toEqual([]);
  });
});

// ─── enter / leave ───────────────────────────────────────────────────────

describe("enter", () => {
  it("deletes the convo's entry, clearing unread", () => {
    const prev: StatusMap = {
      b: { running: true, unread: true },
      c: { running: false, unread: false },
    };
    const out = enter(prev, "b");
    expect("b" in out).toBe(false);
    expect(out["c"]).toEqual({ running: false, unread: false });
  });

  it("is a no-op for a null convo", () => {
    const prev: StatusMap = { b: { running: true, unread: true } };
    const out = enter(prev, null);
    expect(out).toEqual(prev);
  });
});

describe("leave", () => {
  it("seeds {running, unread:false} for the convo", () => {
    const out = leave({}, "b", true);
    expect(out["b"]).toEqual({ running: true, unread: false });
  });

  it("records an idle leave as not running", () => {
    const out = leave({}, "b", false);
    expect(out["b"]).toEqual({ running: false, unread: false });
  });

  it("overwrites a stale entry (a turn started while away)", () => {
    const prev: StatusMap = { b: { running: false, unread: true } };
    const out = leave(prev, "b", true);
    expect(out["b"]).toEqual({ running: true, unread: false });
  });

  it("is a no-op for a null convo", () => {
    const prev: StatusMap = { b: { running: true, unread: false } };
    const out = leave(prev, null, true);
    expect(out).toEqual(prev);
  });
});

// ─── tabViews ────────────────────────────────────────────────────────────
// key = tab id (I5, replaces convo ?? `new:${i}`); label = title || NEW_TAB_LABEL; tooltip = label;
// active tab: running = activeRunning, unread = false.

describe("tabViews", () => {
  it("derives key, label, tooltip and untitled per tab", () => {
    const views = tabViews(state([A, FRESH], 0), {}, false);
    expect(views[0]).toMatchObject({
      key: "id-A",
      label: "Fix login bug",
      tooltip: "Fix login bug",
      untitled: false,
      active: true,
    });
    expect(views[1]).toMatchObject({
      key: "id-FRESH",
      label: "New conversation",
      tooltip: "New conversation",
      untitled: true,
      active: false,
    });
  });

  it("renders an empty title as the NEW_TAB_LABEL", () => {
    const views = tabViews(state([tab("a", "")], 0), {}, false);
    expect(views[0].label).toBe("New conversation");
    expect(views[0].untitled).toBe(true);
  });

  it("marks the active tab with running=activeRunning and unread=false", () => {
    // Defensive: even a stale unread entry for the active convo must not
    // surface — the active tab's running state comes live from the socket.
    const status: StatusMap = { a: { running: false, unread: true } };
    const views = tabViews(state([A, B], 0), status, true);
    expect(views[0]).toMatchObject({ active: true, running: true, unread: false });
    expect(views[1].active).toBe(false);
  });

  it("reads running/unread from the status map for inactive tabs", () => {
    const status: StatusMap = {
      b: { running: true, unread: true },
      c: { running: false, unread: false },
    };
    const views = tabViews(state([A, B, C], 0), status, false);
    expect(views[1]).toMatchObject({ running: true, unread: true });
    expect(views[2]).toMatchObject({ running: false, unread: false });
  });

  it("defaults a missing status entry to not running, no unread", () => {
    const views = tabViews(state([A, B], 0), {}, false);
    expect(views[1]).toMatchObject({ running: false, unread: false });
  });
});

// ═════════════════════════════════════════════════════════════════════════
// I5 — tab ids (desktop-tab-composers). Every tab has a stable, unique `id`;
// `freshTab` is the only minting site. These are the keep-alive composers'
// React keys, so an id that changes (or is reused) would remount or cross
// the wires of a composer.
// ═════════════════════════════════════════════════════════════════════════

const ids = (s: TabsState): string[] => s.tabs.map((t: ConvoTab) => t.id);
const allUnique = (xs: readonly string[]): boolean => new Set(xs).size === xs.length;

describe("freshTab", () => {
  it("defaults to a null-convo, untitled tab with a non-empty string id", () => {
    const t = freshTab();
    expect(t.convo).toBeNull();
    expect(t.title).toBe("");
    expect(typeof t.id).toBe("string");
    expect(t.id.length).toBeGreaterThan(0);
  });

  it("carries the convo and title it is given", () => {
    expect(freshTab("abc", "My chat")).toMatchObject({ convo: "abc", title: "My chat" });
  });

  it("defaults the title to '' when only a convo is given", () => {
    expect(freshTab("abc")).toMatchObject({ convo: "abc", title: "" });
  });

  it("mints a different id on every call, even for identical arguments", () => {
    const a = freshTab("same", "same");
    const b = freshTab("same", "same");
    expect(a.id).not.toBe(b.id);
    expect(a).not.toBe(b);
  });

  it("never repeats an id across 1000 calls", () => {
    const minted = Array.from({ length: 1000 }, () => freshTab().id);
    expect(allUnique(minted)).toBe(true);
  });
});

describe("tab ids — minting sites (I5)", () => {
  it("newTab gives the new tab an id distinct from every existing tab", () => {
    const prev = state([A, B], 0);
    const next = newTab(prev);
    expect(typeof next.tabs[2].id).toBe("string");
    expect(allUnique(ids(next))).toBe(true);
    expect(ids(next).slice(0, 2)).toEqual(["id-A", "id-B"]);
  });

  it("repeated newTab calls mint a distinct id each time", () => {
    let s = state([A], 0);
    for (let i = 0; i < 20; i++) s = newTab(s);
    expect(s.tabs).toHaveLength(21);
    expect(allUnique(ids(s))).toBe(true);
  });

  it("openConvo mints a unique id when it adds a tab and leaves existing ids alone", () => {
    const next = openConvo(state([A, B], 0), "d");
    expect(ids(next).slice(0, 2)).toEqual(["id-A", "id-B"]);
    expect(allUnique(ids(next))).toBe(true);
    expect(typeof next.tabs[2].id).toBe("string");
  });

  it("openConvo on an already-open convo keeps every id (it only activates)", () => {
    const next = openConvo(state([A, B, C], 0), "c");
    expect(ids(next)).toEqual(["id-A", "id-B", "id-C"]);
  });

  it("initialTabs mints a string id for the single boot tab", () => {
    const s = initialTabs(null, "u");
    expect(typeof s.tabs[0].id).toBe("string");
    expect(s.tabs[0].id.length).toBeGreaterThan(0);
  });

  it("two independent boots never share an id", () => {
    expect(initialTabs(null, null).tabs[0].id).not.toBe(initialTabs(null, null).tabs[0].id);
  });

  it("initialTabs with a URL convo not yet open mints an id distinct from the stored ones", () => {
    const s = initialTabs(state([A, B], 0), "zzz");
    expect(ids(s).slice(0, 2)).toEqual(["id-A", "id-B"]);
    expect(allUnique(ids(s))).toBe(true);
  });

  it("initialTabs with a URL convo already open keeps the stored ids", () => {
    const s = initialTabs(state([A, B, C], 0), "b");
    expect(ids(s)).toEqual(["id-A", "id-B", "id-C"]);
  });
});

describe("tab ids — closeTab (I5)", () => {
  it("closing a middle tab leaves the other ids unchanged", () => {
    const next = closeTab(state([A, B, C], 0), 1);
    expect(ids(next)).toEqual(["id-A", "id-C"]);
  });

  it("closing the active tab leaves the survivors' ids unchanged", () => {
    const next = closeTab(state([A, B, C], 1), 1);
    expect(ids(next)).toEqual(["id-A", "id-C"]);
  });

  it("closing the LAST remaining tab gives a fresh tab with a NEW id", () => {
    const s = closeTab(state([A], 0), 0);
    expect(s.tabs).toHaveLength(1);
    expect(typeof s.tabs[0].id).toBe("string");
    expect(s.tabs[0].id).not.toBe("id-A");
  });

  it("closing the last remaining fresh tab again still mints a new id each time", () => {
    const first = closeTab(state([FRESH], 0), 0);
    const second = closeTab(first, 0);
    expect(first.tabs[0].id).not.toBe("id-FRESH");
    expect(second.tabs[0].id).not.toBe(first.tabs[0].id);
  });

  it("a closed tab's id is never handed to a later tab", () => {
    let s = state([A, B], 1);
    s = closeTab(s, 1); // B gone
    s = newTab(s);
    s = openConvo(s, "q");
    expect(ids(s)).not.toContain("id-B");
    expect(allUnique(ids(s))).toBe(true);
  });
});

describe("tab ids — retitle / activate / cycle / jumpTo keep ids (I5)", () => {
  it("retitle keeps every id, including the retitled tab's", () => {
    const next = retitle(state([A, B, C], 0), "b", "Renamed");
    expect(ids(next)).toEqual(["id-A", "id-B", "id-C"]);
    expect(next.tabs[1].title).toBe("Renamed");
  });

  it("activate keeps every id", () => {
    expect(ids(activate(state([A, B, C], 0), 2))).toEqual(["id-A", "id-B", "id-C"]);
  });

  it("cycle keeps every id", () => {
    expect(ids(cycle(state([A, B, C], 2), 1))).toEqual(["id-A", "id-B", "id-C"]);
    expect(ids(cycle(state([A, B, C], 0), -1))).toEqual(["id-A", "id-B", "id-C"]);
  });

  it("jumpTo keeps every id", () => {
    expect(ids(jumpTo(state([A, B, C], 0), 9))).toEqual(["id-A", "id-B", "id-C"]);
    expect(ids(jumpTo(state([A, B, C], 0), 2))).toEqual(["id-A", "id-B", "id-C"]);
  });
});

describe("tab ids — bound (I5)", () => {
  it("null → uuid keeps the ACTIVE tab's id (the composer must not remount)", () => {
    const pending = tab(null, "", "id-pending");
    const next = bound(state([A, pending], 1), "uuid-1");
    expect(next.tabs[1]).toEqual({ id: "id-pending", convo: "uuid-1", title: "" });
    expect(next.active).toBe(1);
  });

  it("X → Y without a merge keeps the ACTIVE tab's id and every other id", () => {
    const next = bound(state([A, B, C], 1), "brand-new");
    expect(ids(next)).toEqual(["id-A", "id-B", "id-C"]);
    expect(next.tabs[1].convo).toBe("brand-new");
  });

  it("a merge takes the OTHER tab's id at the active index and removes the other tab", () => {
    // Active A (index 0) binds to "b", which tab B (index 1) already shows.
    const next = bound(state([A, B], 0), "b");
    expect(next.tabs).toHaveLength(1);
    expect(next.tabs[0]).toEqual({ id: "id-B", convo: "b", title: "Refactor parser" });
    expect(next.active).toBe(0);
  });

  it("a merge with the other tab BEFORE the active one: the active slot takes the other id and indices shift", () => {
    // [A, B, C], active C (2) binds to "a" which tab A (0) shows.
    const next = bound(state([A, B, C], 2), "a");
    expect(next.tabs).toHaveLength(2);
    expect(ids(next)).toEqual(["id-B", "id-A"]);
    expect(next.tabs[1]).toMatchObject({ convo: "a", title: "Fix login bug" });
    expect(next.active).toBe(1);
  });

  it("a merge never leaves two tabs sharing an id, and the active tab's old id is retired", () => {
    const next = bound(state([A, B, C], 0), "c");
    expect(allUnique(ids(next))).toBe(true);
    expect(ids(next)).not.toContain("id-A");
    expect(ids(next)).toContain("id-C");
    expect(ids(next)).toContain("id-B");
  });

  it("bound on an already-bound convo returns the same state (ids trivially unchanged)", () => {
    const s = state([A, B], 1);
    expect(bound(s, "b")).toBe(s);
  });
});

describe("tabViews — key is the tab id (I5)", () => {
  it("uses each tab's id as its key", () => {
    const views = tabViews(state([A, B, FRESH], 0), {}, false);
    expect(views.map((v) => v.key)).toEqual(["id-A", "id-B", "id-FRESH"]);
  });

  it("gives two null-convo tabs distinct keys without the old 'new:<i>' scheme", () => {
    const n1 = tab(null, "", "id-n1");
    const n2 = tab(null, "", "id-n2");
    const keys = tabViews(state([n1, n2], 0), {}, false).map((v) => v.key);
    expect(keys).toEqual(["id-n1", "id-n2"]);
    expect(keys.some((k) => k.startsWith("new:"))).toBe(false);
  });

  it("keeps the active tab's key across null → uuid binding (the old scheme changed it)", () => {
    const pending = tab(null, "", "id-pending");
    const before = tabViews(state([pending], 0), {}, false)[0].key;
    const after = tabViews(bound(state([pending], 0), "uuid-1"), {}, false)[0].key;
    expect(after).toBe(before);
  });

  it("does not use the convo as the key", () => {
    const views = tabViews(state([A], 0), {}, false);
    expect(views[0].key).not.toBe("a");
  });
});

describe("tab ids — invariant I5 over a deterministic op sequence", () => {
  it("ids stay unique and a retired id never reappears", () => {
    // Small LCG so the sequence is deterministic and reproducible.
    let seed = 12345;
    const rnd = (n: number): number => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed % n;
    };
    let s = initialTabs(null, null);
    const retired = new Set<string>();
    let convoSeq = 0;
    for (let step = 0; step < 300; step++) {
      const before = new Set(ids(s));
      const op = rnd(7);
      if (op === 0) s = newTab(s);
      else if (op === 1) s = closeTab(s, rnd(s.tabs.length));
      else if (op === 2) s = openConvo(s, `c${rnd(6)}`);
      else if (op === 3) s = bound(s, `c${rnd(6)}`);
      else if (op === 4) s = retitle(s, `c${rnd(6)}`, `t${step}`);
      else if (op === 5) s = activate(s, rnd(s.tabs.length));
      else s = bound(s, `fresh${(convoSeq += 1)}`);

      const now = ids(s);
      expect(allUnique(now), `duplicate ids after step ${step} (op ${op})`).toBe(true);
      for (const id of now) {
        expect(retired.has(id), `retired id ${id} reappeared at step ${step}`).toBe(false);
      }
      const nowSet = new Set(now);
      for (const id of before) if (!nowSet.has(id)) retired.add(id);
      expect(s.tabs.length).toBeGreaterThanOrEqual(1);
      expect(s.active).toBeGreaterThanOrEqual(0);
      expect(s.active).toBeLessThan(s.tabs.length);
    }
  });
});
