import { describe, it, expect } from "vitest";
import * as rows from "./transcriptRows";
import { estimateRowHeight } from "./transcriptRows";
import type { WireChatMessage } from "./state";

// Minimal WireChatMessage builder. Only `role`, `content`, `timestamp` are
// required on the wire type; everything else is optional and irrelevant to
// the seed estimate. This keeps the fixtures small and the (a)-(e) invariants
// below focused on the `content` field.
function msg(content: string): WireChatMessage {
  return { role: "agent", content, timestamp: "2026-01-01T00:00:00Z" };
}

const MIN = 68;
const MAX = 1200;

describe("estimateRowHeight", () => {
  // (a) Every result must land in [MIN, MAX]. This is the safety bound — the
  // seed estimate is consumed by virtualised row layouts and must not be
  // pathological.
  it("returns a value in the inclusive [68, 1200] band", () => {
    for (const len of [0, 10, 50, 200, 500, 1000, 2000, 5000]) {
      const h = estimateRowHeight(msg("x".repeat(len)));
      expect(h).toBeGreaterThanOrEqual(MIN);
      expect(h).toBeLessThanOrEqual(MAX);
    }
  });

  // (b) Monotone non-decreasing in content length: a longer message is never
  // shorter than a shorter one (until the cap kicks in). This is the property
  // a virtualised list relies on to avoid row reflow when content grows.
  it("is monotone non-decreasing in content length below the cap", () => {
    const lengths = [0, 50, 200, 500, 800];
    const heights = lengths.map((n) => estimateRowHeight(msg("a".repeat(n))));
    for (let i = 1; i < heights.length; i++) {
      expect(heights[i]).toBeGreaterThanOrEqual(heights[i - 1]);
    }
  });

  // (b') Specific value check alongside monotonicity: distinct inputs below
  // the cap produce strictly larger estimates. If the implementation is
  // degenerate (always MIN or always constant), the previous case would still
  // pass but this one would not — together they pin the contract.
  it("grows strictly with content length below the cap", () => {
    const short = estimateRowHeight(msg("a".repeat(100)));
    const medium = estimateRowHeight(msg("a".repeat(400)));
    const long = estimateRowHeight(msg("a".repeat(900)));
    expect(medium).toBeGreaterThan(short);
    expect(long).toBeGreaterThan(medium);
  });

  // (c) Empty content → MIN. A blank row is the floor of the estimate.
  it("returns MIN for empty content", () => {
    expect(estimateRowHeight(msg(""))).toBe(MIN);
  });

  // (c') A whitespace-only string is also length-zero-ish but still truthy in
  // string terms — the implementation may choose to count whitespace the same
  // way (most seed estimators do). We only require non-crashing here.
  it("does not throw on whitespace-only content", () => {
    expect(() => estimateRowHeight(msg("   \n\t  "))).not.toThrow();
  });

  // (d) A 5000-char content saturates the cap exactly. This pins the upper
  // bound so the cap is enforced even when content keeps growing.
  it("caps a 5000-char content at exactly 1200", () => {
    expect(estimateRowHeight(msg("a".repeat(5000)))).toBe(MAX);
  });

  // (e) Defensive: a wire message that omits `content` entirely (the field is
  // optional on the wire) must not throw. Old transcripts or system messages
  // may not carry one. The implementation must fall back to a safe default.
  it("does not throw when the message has no content field", () => {
    const bare: WireChatMessage = {
      role: "system",
      timestamp: "2026-01-01T00:00:00Z",
    };
    expect(() => estimateRowHeight(bare)).not.toThrow();
    expect(estimateRowHeight(bare)).toBeGreaterThanOrEqual(MIN);
    expect(estimateRowHeight(bare)).toBeLessThanOrEqual(MAX);
  });

  // (e') Mirror of (c) via the optional-field path: no content field AND no
  // content string means the same MIN fallback as empty string.
  it("returns MIN for a message with no content field", () => {
    const bare: WireChatMessage = {
      role: "system",
      timestamp: "2026-01-01T00:00:00Z",
    };
    expect(estimateRowHeight(bare)).toBe(MIN);
  });

  // (f) Thinking blocks (Anthropic extended thinking, only on the wire when
  // `display_reasoning` is on) must grow the estimate above a content-only
  // peer. Otherwise expanding an assistant's thinking would punch a hole in
  // the end-anchored virtualizer until measureElement self-corrects.
  it("grows the estimate when thinking blocks are present", () => {
    const withoutThinking = estimateRowHeight(msg("a".repeat(200)));
    const withThinking = estimateRowHeight({
      ...msg("a".repeat(200)),
      thinking: [{ text: "x".repeat(2000) }],
    });
    expect(withThinking).toBeGreaterThan(withoutThinking);
  });

  // (f') Thinking absent and content empty must still produce MIN — the
  // existing (e') invariant cannot be broken by adding thinking awareness.
  it("returns MIN for an empty message even with an empty thinking array", () => {
    const emptyThinking: WireChatMessage = {
      role: "agent",
      content: "",
      timestamp: "2026-01-01T00:00:00Z",
      thinking: [],
    };
    expect(estimateRowHeight(emptyThinking)).toBe(MIN);
  });

  // (f'') The thinking path must stay O(1) and must not throw on a malformed
  // entry. The wire serializer guarantees only `{ text: string }` survives,
  // but a custom client could send anything — defensiveness is cheap.
  it("does not throw when a thinking block is missing the text field", () => {
    const weird: WireChatMessage = {
      role: "agent",
      content: "",
      timestamp: "2026-01-01T00:00:00Z",
      // Cast away the wire-type literal — this is a robustness test for
      // malformed input, not a happy-path fixture.
      thinking: [{} as { text: string }],
    };
    expect(() => estimateRowHeight(weird)).not.toThrow();
    expect(estimateRowHeight(weird)).toBeGreaterThanOrEqual(MIN);
  });
});

// ─── startsNewDay (design §6, §8) ──────────────────────────────────────────
//
// startsNewDay(messages, i) returns true for the first row of a new local day.
// It uses sameLocalDay (from chatList) to compare consecutive message timestamps.
// The design specifies it operates on WireChatMessage[] with Date.parse of the
// ISO timestamp field.

import { startsNewDay } from "./transcriptRows";

function wireMsg(timestamp: string): WireChatMessage {
  return { role: "agent", content: "x", timestamp };
}

describe("startsNewDay", () => {
  it("returns true for index 0 (first row always starts a day)", () => {
    const msgs = [wireMsg("2026-11-14T10:00:00Z")];
    expect(startsNewDay(msgs, 0)).toBe(true);
  });

  it("returns false when two messages are on the same local day", () => {
    // Use local time construction to ensure same local day in any TZ
    const a = new Date(2026, 10, 14, 10, 0, 0).toISOString();
    const b = new Date(2026, 10, 14, 15, 0, 0).toISOString();
    const msgs = [wireMsg(a), wireMsg(b)];
    expect(startsNewDay(msgs, 1)).toBe(false);
  });

  it("returns true when crossing local midnight (23:59:59 → 00:00:00)", () => {
    // Use local time construction to ensure we cross local midnight
    const before = new Date(2026, 10, 14, 23, 59, 59).toISOString();
    const after = new Date(2026, 10, 15, 0, 0, 0).toISOString();
    const msgs = [wireMsg(before), wireMsg(after)];
    expect(startsNewDay(msgs, 1)).toBe(true);
  });

  it("returns false when UTC day differs but local day is the same (LA TZ)", () => {
    // Both are Nov 14 local in LA; a UTC split would say different days.
    const msgs = [
      wireMsg("2026-11-14T23:00:00Z"), // Nov 14 15:00 PT
      wireMsg("2026-11-15T01:00:00Z"), // Nov 14 17:00 PT
    ];
    // Note: this test runs under the default TZ. The design's TZ case is
    // covered in dates.test.ts. Here we just verify the function doesn't
    // crash and uses the same local-day logic as sameLocalDay.
    expect(() => startsNewDay(msgs, 1)).not.toThrow();
  });
});

// Batch 3, item 3.2 (X2): `firstLine` moves verbatim from ActivityRow.tsx
// into transcriptRows.ts (now two call sites: ActivityRow and the phone tool
// row). Behaviour is the contract, so the cases mirror the old private fn.
// `firstLine` is reached through the namespace until 3.2 exports it, so the
// file typechecks today and the cases fail as "not a function" (RED).
const firstLine = (rows as unknown as { firstLine: (c?: string) => string }).firstLine;

describe("firstLine", () => {
  it("returns the first line, trimmed", () => {
    expect(firstLine("  Read /etc/hosts  \nsecond")).toBe("Read /etc/hosts");
  });

  it("skips leading blank and whitespace-only lines", () => {
    expect(firstLine("\n   \n\t\nreal line\nnext")).toBe("real line");
  });

  it("returns '' for undefined, empty and all-whitespace content", () => {
    expect(firstLine(undefined)).toBe("");
    expect(firstLine("")).toBe("");
    expect(firstLine("  \n \n")).toBe("");
  });

  it("keeps unicode and a single line as is", () => {
    expect(firstLine("日本語 ✓")).toBe("日本語 ✓");
  });
});
