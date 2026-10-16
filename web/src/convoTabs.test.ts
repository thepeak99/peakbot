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
//   I1 tabs.length >= 1 — closing the last tab gives [{convo:null,title:""}]
//   I2 0 <= active < tabs.length
//   I3 non-null convo values are unique (several null tabs allowed)
//   I4 pure + structural sharing: unchanged tabs keep object identity
//
// RED before implementation: `./convoTabs` does not exist yet, so this file
// fails at import — the right reason (feature absent). The type-only import
// is erased at runtime and adds nothing to the failure.
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
  initialTabs,
  jumpTo,
  leave,
  newTab,
  openConvo,
  retitle,
  tabViews,
} from "./convoTabs";

// ─── fixtures ────────────────────────────────────────────────────────────

const tab = (convo: string | null, title = ""): ConvoTab => ({ convo, title });
const state = (tabs: ConvoTab[], active = 0): TabsState => ({ tabs, active });

const A = tab("a", "Fix login bug");
const B = tab("b", "Refactor parser");
const C = tab("c", "Write tests");
const FRESH = tab(null, "");

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
    expect(s).toEqual({ tabs: [{ convo: "abc-123", title: "" }], active: 0 });
  });

  it("gives one fresh null-convo tab when nothing is stored and no URL convo", () => {
    const s = initialTabs(null, null);
    expect(s).toEqual({ tabs: [{ convo: null, title: "" }], active: 0 });
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
    expect(s.tabs[2]).toEqual({ convo: "d", title: "" });
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
    expect(s.tabs[2]).toEqual({ convo: null, title: "" });
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
    expect(s.tabs).toEqual([{ convo: null, title: "" }]);
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
    expect(s.tabs[1]).toEqual({ convo: "d", title: "Fresh convo" });
  });

  it("stores an empty title when none is given", () => {
    const s = openConvo(state([A], 0), "d");
    expect(s.tabs[1]).toEqual({ convo: "d", title: "" });
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
    expect(next.tabs[0]).toEqual({ convo: "d", title: "" });
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
// key = convo ?? `new:${i}`; label = title || NEW_TAB_LABEL; tooltip = label;
// active tab: running = activeRunning, unread = false.

describe("tabViews", () => {
  it("derives key, label, tooltip and untitled per tab", () => {
    const views = tabViews(state([A, FRESH], 0), {}, false);
    expect(views[0]).toMatchObject({
      key: "a",
      label: "Fix login bug",
      tooltip: "Fix login bug",
      untitled: false,
      active: true,
    });
    expect(views[1]).toMatchObject({
      key: "new:1",
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
