// @vitest-environment node
//
// Stage 3 — shared remote conversation cache: delta sync + moving anchor.
// Pure-function tests; compile-RED until `./deltaSync` is implemented.
//
// Planned interface (the senior must land this module or equivalents):
//   parseAnchor(raw: unknown) -> Anchor | null         // lenient wire parse
//   validateAnchor(a: Anchor, epoch, msgs) -> Anchor | null  // 1<=n<=len, exact raw ts at n-1, epoch
//   anchorFor(msgs, epoch) -> Anchor | null            // cursor for current base
//   mergeSuffix(cached, frame, messages_from) -> AppState | null
//   shouldSyncAnchor(applied, every, deltaSupported) -> boolean
//   syncAnchorFrame(a: Anchor) -> { type:"sync_anchor", since: Anchor }
//   ANCHOR_EVERY (constant 10)
//
// All message shapes carry only the fields the tests care about; the real
// AppState has many more. Tests never Date.parse a timestamp — raw strings
// are compared byte-for-byte per the design.

import { describe, expect, it } from "vitest";
import {
  ANCHOR_EVERY,
  anchorFor,
  mergeSuffix,
  parseAnchor,
  shouldSyncAnchor,
  syncAnchorFrame,
  validateAnchor,
} from "./deltaSync";
import type { AppState } from "./state";

const msg = (ts: string) => ({ role: "user" as const, content: "x", timestamp: ts });

function cachedState(messages: ReturnType<typeof msg>[], epoch: number): AppState {
  return {
    chat: { messages, auto_scroll: false, scroll_offset: 0 },
    todo: { visible: false, items: [] },
    stats: { total_input_tokens: 0, total_output_tokens: 0, total_api_calls: 0, total_cost: 0, model: "m", provider_name: "p", model_alias: "a" },
    context: { current_usage: 0, window_size: 1000, compaction_enabled: true, compaction_threshold: 0.8 },
    conversation: { id: "c", name: "c" },
    history_epoch: epoch,
    is_running: false,
    is_command: false,
    command_running: false,
    stop_requested: false,
    stop_pending: false,
    pending: false,
    status: null,
    status_message: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    is_loading: false,
    catalog: [],
    selection: { profile: "p", agent: "a" },
    profile_pinned: false,
    settings_available: true,
    sub_agent: null,
    visible_tabs: [],
    supports_vision: false,
    upload_limits: { max_file_mb: 10, max_files: 5 },
  } as unknown as AppState;
}

// ─── parseAnchor (lenient wire parse) ────────────────────────────────────

describe("parseAnchor", () => {
  it("accepts a well-formed cursor", () => {
    const a = parseAnchor({ n: 2, timestamp: "2026-01-01T00:00:00Z", epoch: 7 });
    expect(a).toEqual({ n: 2, timestamp: "2026-01-01T00:00:00Z", epoch: 7 });
  });

  it.each([
    null,
    "",
    42,
    ["array"],
    { n: 2 },                                    // missing timestamp
    { timestamp: "t" },                          // bare timestamp (not enough)
    { n: 2, timestamp: "t" },                    // missing epoch
    { n: 2, timestamp: "t", epoch: "seven" },    // epoch string
    { n: 2.5, timestamp: "t", epoch: 1 },        // n float
    { n: "two", timestamp: "t", epoch: 1 },      // n string
    { n: 0, timestamp: "t", epoch: 1 },          // n must be > 0
    { n: -1, timestamp: "t", epoch: 1 },         // n negative
    { n: 2, timestamp: 123, epoch: 1 },          // timestamp number
  ])("rejects malformed cursor: %j", (raw) => {
    expect(parseAnchor(raw)).toBeNull();
  });
});

// ─── validateAnchor (1<=n<=len, exact raw ts at n-1, epoch) ─────────────

describe("validateAnchor", () => {
  it("accepts an exact prefix end", () => {
    const msgs = [msg("t1"), msg("t2")];
    const a = validateAnchor({ n: 2, timestamp: "t2", epoch: 3 }, 3, msgs);
    expect(a).toEqual({ n: 2, timestamp: "t2", epoch: 3 });
  });

  it("accepts a mid-point anchor", () => {
    const msgs = [msg("t1"), msg("t2"), msg("t3")];
    expect(validateAnchor({ n: 2, timestamp: "t2", epoch: 3 }, 3, msgs)).not.toBeNull();
  });

  it("rejects n = 0", () => {
    expect(validateAnchor({ n: 0, timestamp: "t", epoch: 1 }, 1, [msg("t")])).toBeNull();
  });

  it("rejects n > len", () => {
    expect(validateAnchor({ n: 5, timestamp: "t", epoch: 1 }, 1, [msg("t")])).toBeNull();
  });

  it("rejects epoch mismatch", () => {
    expect(validateAnchor({ n: 1, timestamp: "t1", epoch: 4 }, 5, [msg("t1")])).toBeNull();
  });

  it("rejects timestamp mismatch (byte-exact, no Date.parse)", () => {
    const a = validateAnchor(
      { n: 1, timestamp: "2026-01-01T00:00:00Z", epoch: 1 },
      1,
      [msg("2026-01-01T00:00:00+00:00")],  // same instant, different string
    );
    expect(a).toBeNull();
  });

  // ─── same-timestamp boundary (Mandatory) ─────────────────────────────
  it("two messages with identical timestamp are disambiguated by n", () => {
    const ts = "2026-11-25T19:00:00.123456789+01:00";
    const msgs = [msg(ts), msg(ts), msg("t3")];
    expect(validateAnchor({ n: 1, timestamp: ts, epoch: 1 }, 1, msgs)).not.toBeNull();
    expect(validateAnchor({ n: 2, timestamp: ts, epoch: 1 }, 1, msgs)).not.toBeNull();
    // n=3 with ts of the first two must fail
    expect(validateAnchor({ n: 3, timestamp: ts, epoch: 1 }, 1, msgs)).toBeNull();
  });
});

// ─── anchorFor (build cursor for current base) ──────────────────────────

describe("anchorFor", () => {
  it("builds a cursor from the last message's raw timestamp", () => {
    const msgs = [msg("t1"), msg("t2")];
    const a = anchorFor(msgs, 7);
    expect(a).toEqual({ n: 2, timestamp: "t2", epoch: 7 });
  });

  it("returns null for empty messages (cannot form n>=1)", () => {
    expect(anchorFor([], 7)).toBeNull();
  });

  it("preserves raw timestamp byte-exact (nanoseconds + offset)", () => {
    const raw = "2026-11-25T19:00:00.123456789+01:00";
    const a = anchorFor([msg(raw)], 1);
    expect(a?.timestamp).toBe(raw);
  });
});

// ─── mergeSuffix (frozen prefix + suffix = full) ────────────────────────

describe("mergeSuffix", () => {
  it("reconstructs the full message array from prefix + suffix", () => {
    const cached = cachedState([msg("t1"), msg("t2")], 1);
    const frame = cachedState([msg("t3")], 1);
    const merged = mergeSuffix(cached, frame, 2);
    expect(merged).not.toBeNull();
    expect(merged!.chat.messages.length).toBe(3);
    expect((merged!.chat.messages[0] as { content: string }).content).toBe("x");
    expect((merged!.chat.messages[2] as { content: string }).content).toBe("x");
  });

  it("takes all non-message fields from the latest frame", () => {
    const cached = cachedState([msg("t1")], 1);
    const frame = cachedState([msg("t2")], 1);
    frame.is_running = true;
    frame.status_message = "working";
    const merged = mergeSuffix(cached, frame, 1);
    expect(merged).not.toBeNull();
    expect(merged!.is_running).toBe(true);
    expect(merged!.status_message).toBe("working");
  });

  it("accepts an empty tail (messages_from == len)", () => {
    const cached = cachedState([msg("t1")], 1);
    const frame = cachedState([], 1);
    const merged = mergeSuffix(cached, frame, 1);
    expect(merged).not.toBeNull();
    expect(merged!.chat.messages.length).toBe(1);
  });

  it("returns null when messages_from is undefined (full frame path)", () => {
    expect(mergeSuffix(null, cachedState([msg("t")], 1), undefined)).toBeNull();
  });

  it("returns null when the cached base is missing (never render bare tail)", () => {
    expect(mergeSuffix(null, cachedState([msg("t")], 1), 1)).toBeNull();
  });

  it("returns null when messages_from exceeds the cached length (bare tail)", () => {
    const cached = cachedState([msg("t1")], 1);
    expect(mergeSuffix(cached, cachedState([msg("t")], 1), 5)).toBeNull();
  });

  it("returns null on epoch mismatch", () => {
    const cached = cachedState([msg("t1")], 1);
    const frame = cachedState([msg("t2")], 2);
    expect(mergeSuffix(cached, frame, 1)).toBeNull();
  });

  it("returns null for messages_from = 0", () => {
    const cached = cachedState([msg("t1")], 1);
    expect(mergeSuffix(cached, cachedState([msg("t")], 1), 0)).toBeNull();
  });

  it("does not mutate the cached or frame states", () => {
    const cached = cachedState([msg("t1")], 1);
    const frame = cachedState([msg("t2")], 1);
    mergeSuffix(cached, frame, 1);
    expect(cached.chat.messages.length).toBe(1);
    expect(frame.chat.messages.length).toBe(1);
  });

  it("preserves raw timestamps byte-exact in the merged array", () => {
    const raw = "2026-11-25T19:00:00.123456789+01:00";
    const cached = cachedState([msg("t1")], 1);
    const frame = cachedState([msg(raw)], 1);
    const merged = mergeSuffix(cached, frame, 1);
    expect((merged!.chat.messages[1] as { timestamp: string }).timestamp).toBe(raw);
  });
});

// ─── shouldSyncAnchor (moving-anchor cadence) ───────────────────────────

describe("shouldSyncAnchor", () => {
  it("fires exactly every `every` newly applied rows", () => {
    expect(shouldSyncAnchor(9, 10, true)).toBe(false);
    expect(shouldSyncAnchor(10, 10, true)).toBe(true);
    expect(shouldSyncAnchor(11, 10, true)).toBe(false);
    expect(shouldSyncAnchor(20, 10, true)).toBe(true);
  });

  it("never fires when delta support was not observed", () => {
    expect(shouldSyncAnchor(10, 10, false)).toBe(false);
  });

  it("supports an injectable cadence (test constant)", () => {
    expect(shouldSyncAnchor(2, 3, true)).toBe(false);
    expect(shouldSyncAnchor(3, 3, true)).toBe(true);
  });

  it("does not fire at zero", () => {
    expect(shouldSyncAnchor(0, 10, true)).toBe(false);
  });
});

// ─── syncAnchorFrame (additive rebase frame) ────────────────────────────

describe("syncAnchorFrame", () => {
  it("builds the sync_anchor inbound frame with the same cursor", () => {
    const a = { n: 12, timestamp: "t", epoch: 7 };
    const f = syncAnchorFrame(a);
    expect(f.type).toBe("sync_anchor");
    expect(f.since).toEqual(a);
  });

  it("preserves raw timestamp byte-exact", () => {
    const raw = "2026-11-25T19:00:00.123456789+01:00";
    const f = syncAnchorFrame({ n: 1, timestamp: raw, epoch: 1 });
    expect(f.since.timestamp).toBe(raw);
  });
});

// ─── constant: rebase cadence ───────────────────────────────────────────

describe("ANCHOR_EVERY", () => {
  it("is the documented 10", () => {
    expect(ANCHOR_EVERY).toBe(10);
  });
});
