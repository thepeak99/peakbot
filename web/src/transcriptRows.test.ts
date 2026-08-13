import { describe, it, expect } from "vitest";
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
