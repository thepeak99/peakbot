// Tests for the Focus-mode core rule (plan-focus-mode.md §4) — written
// FIRST, expected to be RED until `web/src/focus.ts` and
// `web/src/useFocusMode.ts` exist. Both imports below resolve to
// not-yet-created modules, so this whole file fails at collection for the
// right reason (missing feature), not for a setup bug.
//
// The contract under test:
//
//   focusView(messages, isRunning): {
//     visible:  WireChatMessage[]   // transcript rows, original order,
//                                   // SAME object references (no copies)
//     activity: WireChatMessage | null  // newest hidden step while running
//   }
//
// Turn segmentation: a turn starts at every user message (human or
// background) and runs to the next user message. Messages before the first
// user message are "preamble" (always visible). Only the trailing turn is
// running when isRunning is true.
//
// Visibility (finished human turn): opening user message + the last
// non-sub-agent agent message ("the answer") + errors (system content
// starting with ❌/⚠) + non-error system messages after the last work
// message (slash output, stop marker). Everything else is hidden.
// Background turns are hidden entirely (including the reply), except
// errors and trailing system messages.
//
// Environment: node (vitest.config.ts — only src/components/** is jsdom).
// No DOM, no React: both targets are pure TS.

import { describe, expect, it } from "vitest";
import { focusView, type FocusView } from "./focus";
import { readFocusPref } from "./useFocusMode";
import type { WireChatMessage, WireMessageSource } from "./state";

// ─── tiny builders (plan §5) ──────────────────────────────────────────────
// Only the fields focusView is allowed to read (role, content, tool_name,
// source) are set; timestamp is the one required field on the wire type.

const T = "2026-01-01T00:00:00Z";

function u(text: string, source?: WireMessageSource): WireChatMessage {
  return { role: "user", content: text, timestamp: T, ...(source ? { source } : {}) };
}
function a(text: string, source?: WireMessageSource): WireChatMessage {
  return { role: "agent", content: text, timestamp: T, ...(source ? { source } : {}) };
}
function tc(name: string, content?: string, source?: WireMessageSource): WireChatMessage {
  return {
    role: "toolcall",
    tool_name: name,
    content: content ?? name,
    timestamp: T,
    ...(source ? { source } : {}),
  };
}
function tr(name: string, source?: WireMessageSource): WireChatMessage {
  return {
    role: "toolresult",
    tool_name: name,
    content: `result of ${name}`,
    timestamp: T,
    ...(source ? { source } : {}),
  };
}
function sys(text: string): WireChatMessage {
  return { role: "system", content: text, timestamp: T };
}
function sub(role: string): WireMessageSource {
  return { kind: "sub_agent", role };
}
// The wire shape (state.ts) makes proc_ids optional, but the plan's builder
// spec carries it — and a correct focusView must work with it present.
function bg(): WireMessageSource {
  return { kind: "background", proc_ids: [1] };
}

/** Assert `view.visible` is exactly `expected` — same length and SAME
 *  object references (toBe, not toEqual), in the same order. Reference
 *  identity is a hard contract (plan §4.4: Transcript runs adaptMessage on
 *  the very objects the wire produced). */
function expectVisible(view: FocusView, ...expected: WireChatMessage[]): void {
  expect(view.visible).toHaveLength(expected.length);
  for (let i = 0; i < expected.length; i++) {
    expect(view.visible[i], `visible[${i}] must be the same reference as input[${i}]`).toBe(
      expected[i],
    );
  }
}

// ─── focusView — plan §5 cases 1–23 ───────────────────────────────────────

describe("focusView", () => {
  it("case 1: empty transcript → no visible rows, no activity", () => {
    const view = focusView([], false);
    expect(view.visible).toEqual([]);
    expect(view.activity).toBeNull();
  });

  it("case 2: system banners before the first user message (preamble) stay visible", () => {
    const w1 = sys("Welcome to Shifu");
    const w2 = sys("model: gpt-5");
    const view = focusView([w1, w2], false);
    expectVisible(view, w1, w2);
    expect(view.activity).toBeNull();

    // Preamble is visible unconditionally — even mid-run.
    const running = focusView([w1, w2], true);
    expectVisible(running, w1, w2);
    expect(running.activity).toBeNull();
  });

  it("case 3: simple finished turn → user message + answer only", () => {
    const m1 = u("hello");
    const m2 = tc("bash");
    const m3 = tr("bash");
    const m4 = a("hi there");
    const view = focusView([m1, m2, m3, m4], false);
    expectVisible(view, m1, m4);
    expect(view.activity).toBeNull();
  });

  it("case 4: multiple agent texts in a finished turn → only the last one is the answer (D3)", () => {
    const m1 = u("q");
    const a1 = a("interim musing");
    const m3 = tc("bash");
    const m4 = tr("bash");
    const a2 = a("final answer");
    const view = focusView([m1, a1, m3, m4, a2], false);
    expectVisible(view, m1, a2);
    expect(view.activity).toBeNull();
  });

  it("case 5: running turn → only the user message, activity = the newest step", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const m3 = tr("bash");
    const view = focusView([m1, m2, m3], true);
    expectVisible(view, m1);
    expect(view.activity).toBe(m3);
  });

  it("case 6: running turn with intermediate agent text → activity tracks the newest step; the answer stays hidden until finished", () => {
    const m1 = u("q");
    const a1 = a("interim");
    const m3 = tc("bash");
    const view1 = focusView([m1, a1, m3], true);
    expectVisible(view1, m1);
    expect(view1.activity).toBe(m3);

    const a2 = a("more interim");
    const view2 = focusView([m1, a1, m3, a2], true);
    expectVisible(view2, m1);
    expect(view2.activity).toBe(a2);
  });

  it("case 7: running turn with no steps yet → user message only, activity null", () => {
    const m1 = u("q");
    const view = focusView([m1], true);
    expectVisible(view, m1);
    expect(view.activity).toBeNull();
  });

  it("case 8: earlier finished turns stay intact while the newest turn runs", () => {
    const u1 = u("first");
    const t1 = tc("bash");
    const a1 = a("first answer");
    const u2 = u("second");
    const t2 = tc("ls");
    const view = focusView([u1, t1, a1, u2, t2], true);
    expectVisible(view, u1, a1, u2);
    expect(view.activity).toBe(t2);
  });

  it("case 9: mid-turn error stays visible even in a finished turn (D4)", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const err = sys("❌ tool failed");
    const m4 = tc("bash");
    const m5 = a("answer");
    const view = focusView([m1, m2, err, m4, m5], false);
    expectVisible(view, m1, err, m5);
    expect(view.activity).toBeNull();
  });

  it("case 10: error in a running turn is visible; the hidden step before it is the activity", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const err = sys("⚠ internal error: model went quiet — turn aborted");
    const view = focusView([m1, m2, err], true);
    expectVisible(view, m1, err);
    // The error itself is visible, so it must NOT be the activity row.
    expect(view.activity).toBe(m2);
  });

  it("case 11: LLM failure → user message + the error, nothing else", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const m3 = tr("bash");
    const err = sys("❌ LLM request failed: 502 from provider");
    const view = focusView([m1, m2, m3, err], false);
    expectVisible(view, m1, err);
    expect(view.activity).toBeNull();
  });

  it("case 12: the stop marker stays visible (D10)", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const stop = sys("Agent stopped by user");
    const view = focusView([m1, m2, stop], false);
    expectVisible(view, m1, stop);
    expect(view.activity).toBeNull();
  });

  it("case 13: a mid-turn non-error system message is hidden", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const m3 = sys("Context compacted: 12 messages summarized");
    const m4 = tc("bash");
    const m5 = a("answer");
    const view = focusView([m1, m2, m3, m4, m5], false);
    expectVisible(view, m1, m5);
    expect(view.activity).toBeNull();
  });

  it("case 14: trailing slash output after a finished turn stays visible (D9)", () => {
    const m1 = u("q");
    const m2 = a("answer");
    const m3 = sys("Session stats: 3 api calls, $0.02");
    const view = focusView([m1, m2, m3], false);
    expectVisible(view, m1, m2, m3);
    expect(view.activity).toBeNull();
  });

  it("case 15: a finished background turn is hidden entirely, reply included (D5)", () => {
    const u1 = u("foreground");
    const a1 = a("foreground answer");
    const uBg = u("[bg output] npm test", bg());
    const t1 = tc("bash", undefined, bg());
    const aBg = a("background answer", bg());
    const view = focusView([u1, a1, uBg, t1, aBg], false);
    expectVisible(view, u1, a1);
    expect(view.activity).toBeNull();
  });

  it("case 16: a running background turn stays hidden, but its newest step feeds the activity row", () => {
    const u1 = u("foreground");
    const a1 = a("foreground answer");
    const uBg = u("[bg output] make build", bg());
    const t1 = tc("bash", undefined, bg());
    const view = focusView([u1, a1, uBg, t1], true);
    expectVisible(view, u1, a1);
    expect(view.activity).toBe(t1);
  });

  it("case 17: slash output after a background turn stays visible (D9)", () => {
    const uBg = u("[bg output] x", bg());
    const aBg = a("bg answer", bg());
    const out = sys("Session stats: 1 api call");
    const view = focusView([uBg, aBg, out], false);
    expectVisible(view, out);
    expect(view.activity).toBeNull();
  });

  it("case 18: an error inside a background turn stays visible (D4)", () => {
    const uBg = u("[bg output] x", bg());
    const t1 = tc("bash", undefined, bg());
    const err = sys("❌ bg task failed: exit 1");
    const view = focusView([uBg, t1, err], false);
    expectVisible(view, err);
    expect(view.activity).toBeNull();
  });

  it("case 19: sub-agent messages never appear in the visible list, and a sub-agent's agent message is not the answer (D6)", () => {
    const m1 = u("research that");
    const delegate = tc("delegate");
    const aSub = a("sub-agent findings", sub("researcher"));
    const tSub = tc("bash", "ls -la", sub("researcher"));
    const trSub = tr("bash", sub("researcher"));
    const trDel = tr("delegate");
    const aFin = a("final answer");
    const view = focusView([m1, delegate, aSub, tSub, trSub, trDel, aFin], false);
    expectVisible(view, m1, aFin);
    expect(view.activity).toBeNull();
  });

  it("case 20: the newest step from a sub-agent lane becomes the activity, with its source preserved (for the 🧩 prefix)", () => {
    const m1 = u("research that");
    const delegate = tc("delegate");
    const tSub = tc("bash", "curl https://example.com", sub("researcher"));
    const view = focusView([m1, delegate, tSub], true);
    expectVisible(view, m1);
    expect(view.activity).toBe(tSub);
    expect(view.activity?.source).toEqual({ kind: "sub_agent", role: "researcher" });
  });

  it("case 21: a turn whose only agent message is a sub-agent's → no answer, user message only", () => {
    const m1 = u("do the thing");
    const t1 = tc("bash");
    const aSub = a("sub-agent text", sub("worker"));
    const view = focusView([m1, t1, aSub], false);
    expectVisible(view, m1);
    expect(view.activity).toBeNull();
  });

  it("case 22: visible rows are the SAME object references as the input (no copies)", () => {
    const u1 = u("q1");
    const t1 = tc("bash");
    const a1 = a("a1");
    const u2 = u("q2");
    const t2 = tc("ls");
    const a2 = a("a2");
    const err = sys("❌ boom");
    const input = [u1, t1, a1, u2, t2, a2, err];
    const view = focusView(input, false);
    // Every visible row must be === to the exact input object.
    expectVisible(view, u1, a1, u2, a2, err);
    for (const m of view.visible) {
      expect(input.indexOf(m), "visible row must be a reference into the input array").not.toBe(-1);
    }
    // The input array itself is left untouched.
    expect(input).toHaveLength(7);
  });

  it("case 23: isRunning=false → the trailing turn is finished (answer visible, no activity)", () => {
    const m1 = u("q");
    const m2 = tc("bash");
    const m3 = a("answer");
    const view = focusView([m1, m2, m3], false);
    expectVisible(view, m1, m3);
    expect(view.activity).toBeNull();
  });
});

// ─── readFocusPref — plan §5 case 26 ──────────────────────────────────────
// Pure preference reader for localStorage key "peakbot-focus" (values
// "on"/"off"): default ON, only the exact string "off" disables, anything
// else (including garbage from older versions) is treated as ON.

describe("readFocusPref", () => {
  it("case 26: no stored value (null) → focus on by default", () => {
    expect(readFocusPref(null)).toBe(true);
  });

  it("case 26: stored 'off' → focus off", () => {
    expect(readFocusPref("off")).toBe(false);
  });

  it("case 26: stored 'on' → focus on", () => {
    expect(readFocusPref("on")).toBe(true);
  });

  it("case 26: any garbage value → focus on (only the exact string 'off' disables)", () => {
    expect(readFocusPref("banana")).toBe(true);
    expect(readFocusPref("")).toBe(true);
    // Case-sensitive per the contract: raw !== "off".
    expect(readFocusPref("OFF")).toBe(true);
  });
});
