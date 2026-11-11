// Unit tests for `lastSeen.ts` — the in-memory LRU of the 5 most recently
// viewed conversations' last live snapshots (convo-state-cache design §3, L1–L8).
//
// Written FIRST (TDD). RED until `./lastSeen` exists: the import below fails to
// resolve, the one right reason (feature absent). Interface under test:
//
//   export const LAST_SEEN_MAX = 5;
//   export interface LastSeen {
//     remember(profile: string | null, state: AppState): void;
//     recall(profile: string | null, convo: string | null): AppState | null;
//   }
//   export function createLastSeen(): LastSeen;
//
// Pure module, no React, no DOM: runs under the default `node` environment.

import { describe, expect, it } from "vitest";
import { LAST_SEEN_MAX, createLastSeen } from "./lastSeen";
import type { AppState, WireChatMessage, WireSubAgentRun } from "./state";

const msg = (content: string): WireChatMessage => ({
  role: "agent",
  content,
  timestamp: "2026-01-01T00:00:00Z",
});

/** A minimal, valid-enough `AppState` for conversation `id`. */
function snap(id: string | null, over: Partial<AppState> = {}): AppState {
  return {
    chat: { messages: [msg(`hello ${id}`)], auto_scroll: true, scroll_offset: 0 },
    todo: { visible: false, items: [] },
    stats: {
      total_input_tokens: 1,
      total_output_tokens: 2,
      total_api_calls: 3,
      total_cost: 0.5,
      model: "m",
      provider_name: "p",
      model_alias: "a",
    },
    context: {
      current_usage: 0,
      window_size: 1000,
      compaction_enabled: true,
      compaction_threshold: 0.8,
    },
    conversation: id === null ? null : { id, name: `name ${id}` },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    ...over,
  } as unknown as AppState;
}

const SUB: WireSubAgentRun = { role: "researcher", pausable: true, pause: "running" };

/** A snapshot caught mid-turn: every in-flight field set. */
const busy = (id: string): AppState =>
  snap(id, {
    is_running: true,
    command_running: true,
    stop_pending: true,
    pending_input_count: 3,
    status_message: "x",
    sub_agent: SUB,
  });

describe("lastSeen — recall misses", () => {
  it("L1: recall of an unseen convo, or of null, is null", () => {
    const ls = createLastSeen();
    expect(ls.recall(null, "never-seen")).toBeNull();
    expect(ls.recall(null, null)).toBeNull();
    // `null` stays a miss even when other conversations are held.
    ls.remember(null, snap("c1"));
    expect(ls.recall(null, null)).toBeNull();
    expect(ls.recall(null, "c2")).toBeNull();
  });

  it("L1: LAST_SEEN_MAX is 5 (the design constant)", () => {
    expect(LAST_SEEN_MAX).toBe(5);
  });
});

describe("lastSeen — recall hits", () => {
  it("L2: recall returns the remembered conversation's messages and meta", () => {
    const ls = createLastSeen();
    const s = snap("c1");
    ls.remember(null, s);

    const got = ls.recall(null, "c1");
    expect(got, "a hit").not.toBeNull();
    expect(got!.chat.messages, "the same messages reference, not a copy").toBe(s.chat.messages);
    expect(got!.conversation?.id).toBe("c1");
    expect(got!.conversation?.name).toBe("name c1");
  });

  it("L3: recall quiesces every in-flight-turn field", () => {
    const ls = createLastSeen();
    const s = busy("c1");
    ls.remember(null, s);

    const got = ls.recall(null, "c1")!;
    expect(got.is_running).toBe(false);
    expect(got.command_running).toBe(false);
    expect(got.stop_pending).toBe(false);
    expect(got.pending_input_count).toBe(0);
    expect(got.status_message).toBeNull();
    expect(got.sub_agent).toBeNull();

    // Read-only panels and conversation meta are shown as last seen.
    expect(got.bg).toBe(s.bg);
    expect(got.bash_panel).toBe(s.bash_panel);
    expect(got.catalog).toBe(s.catalog);
    expect(got.selection).toBe(s.selection);
    expect(got.stats).toBe(s.stats);
  });

  it("L3: bg, bash_panel, catalog and selection are kept by reference when they are set", () => {
    const ls = createLastSeen();
    const bg = { running_count: 2, recent_summaries: [] };
    const bash = { kind: "finished", command: "ls", exit_code: 0, duration_secs: 1, tail: [] } as const;
    const catalog = [{ name: "home", origin: "remote", overrides_global: false, agents: [] }];
    const selection = { profile: "home", agent: "dev" };
    const s = busy("c1");
    Object.assign(s, { bg, bash_panel: bash, catalog, selection });
    ls.remember("home", s);

    const got = ls.recall("home", "c1")!;
    expect(got.bg).toBe(bg);
    expect(got.bash_panel).toBe(bash);
    expect(got.catalog).toBe(catalog);
    expect(got.selection).toBe(selection);
  });

  it("L4: recall never mutates or returns the stored object", () => {
    const ls = createLastSeen();
    const s = busy("c1");
    ls.remember(null, s);

    const got = ls.recall(null, "c1");
    expect(got, "a fresh object, not the stored one").not.toBe(s);
    expect(s.is_running, "the original keeps its live claims").toBe(true);
    expect(s.command_running).toBe(true);
    expect(s.stop_pending).toBe(true);
    expect(s.pending_input_count).toBe(3);
    expect(s.status_message).toBe("x");
    expect(s.sub_agent).toBe(SUB);
  });

  it("L4: mutating a recalled snapshot does not leak into the next recall", () => {
    const ls = createLastSeen();
    ls.remember(null, busy("c1"));

    const first = ls.recall(null, "c1")!;
    first.is_running = true;
    first.status_message = "tampered";

    const second = ls.recall(null, "c1")!;
    expect(second).not.toBe(first);
    expect(second.is_running).toBe(false);
    expect(second.status_message).toBeNull();
  });

  it("L4: a later remember for the same convo replaces the earlier snapshot wholesale", () => {
    const ls = createLastSeen();
    ls.remember(null, snap("c1", { chat: { messages: [msg("old")], auto_scroll: true, scroll_offset: 0 } }));
    ls.remember(null, snap("c1", { chat: { messages: [msg("new")], auto_scroll: true, scroll_offset: 0 } }));

    expect(ls.recall(null, "c1")!.chat.messages.map((m: WireChatMessage) => m.content)).toEqual(["new"]);
  });
});

describe("lastSeen — capacity and recency", () => {
  it("L5: keeps the 5 most recent; the oldest of 6 is a miss", () => {
    const ls = createLastSeen();
    for (const id of ["c1", "c2", "c3", "c4", "c5", "c6"]) ls.remember(null, snap(id));

    expect(ls.recall(null, "c1"), "c1 was evicted").toBeNull();
    for (const id of ["c2", "c3", "c4", "c5", "c6"]) {
      expect(ls.recall(null, id)?.conversation?.id, `${id} is still held`).toBe(id);
    }
  });

  it("L5: exactly 5 snapshots evict nothing", () => {
    const ls = createLastSeen();
    for (const id of ["c1", "c2", "c3", "c4", "c5"]) ls.remember(null, snap(id));
    for (const id of ["c1", "c2", "c3", "c4", "c5"]) {
      expect(ls.recall(null, id), `${id} is held`).not.toBeNull();
    }
  });

  it("L6: re-remembering a conversation refreshes its recency", () => {
    const ls = createLastSeen();
    for (const id of ["c1", "c2", "c3", "c4", "c5"]) ls.remember(null, snap(id));
    ls.remember(null, snap("c1")); // c1 is now the most recent
    ls.remember(null, snap("c6")); // evicts the least recent: c2

    expect(ls.recall(null, "c2"), "c2 was the least recent").toBeNull();
    expect(ls.recall(null, "c1"), "c1 survived thanks to the refresh").not.toBeNull();
    for (const id of ["c3", "c4", "c5", "c6"]) expect(ls.recall(null, id), id).not.toBeNull();
  });

  it("L6: re-remembering the same convo never grows the cache past the cap", () => {
    const ls = createLastSeen();
    for (let i = 0; i < 20; i++) ls.remember(null, snap("c1"));
    for (const id of ["c2", "c3", "c4", "c5"]) ls.remember(null, snap(id));
    expect(ls.recall(null, "c1"), "c1 is one of the 5 held").not.toBeNull();
  });

  it("L6: recall is read-only for recency (it does not refresh)", () => {
    const ls = createLastSeen();
    for (const id of ["c1", "c2", "c3", "c4", "c5"]) ls.remember(null, snap(id));
    ls.recall(null, "c1"); // must NOT make c1 the most recent
    ls.remember(null, snap("c6"));

    expect(ls.recall(null, "c1"), "c1 is still the least recent and is evicted").toBeNull();
  });
});

describe("lastSeen — keying", () => {
  it("L7: the profile is part of the key", () => {
    const ls = createLastSeen();
    ls.remember("home", snap("c1"));

    expect(ls.recall(null, "c1"), "same convo, different profile → miss").toBeNull();
    expect(ls.recall("other", "c1")).toBeNull();
    expect(ls.recall("home", "c1")?.conversation?.id).toBe("c1");
  });

  it("L7: the same convo under two profiles holds two independent entries", () => {
    const ls = createLastSeen();
    ls.remember(null, snap("c1", { chat: { messages: [msg("local")], auto_scroll: true, scroll_offset: 0 } }));
    ls.remember("home", snap("c1", { chat: { messages: [msg("remote")], auto_scroll: true, scroll_offset: 0 } }));

    expect(ls.recall(null, "c1")!.chat.messages[0].content).toBe("local");
    expect(ls.recall("home", "c1")!.chat.messages[0].content).toBe("remote");
  });

  it("L7: profile names that would collide under naive concatenation stay distinct", () => {
    const ls = createLastSeen();
    // ("a:b", "c") vs ("a", "b:c") collide under `${profile}:${convo}`.
    ls.remember("a:b", snap("c"));
    expect(ls.recall("a", "b:c")).toBeNull();
    ls.remember("a", snap("b:c"));
    expect(ls.recall("a:b", "c")?.conversation?.id).toBe("c");
    expect(ls.recall("a", "b:c")?.conversation?.id).toBe("b:c");
  });

  it("L7: a unicode profile name and an empty-string profile round-trip", () => {
    const ls = createLastSeen();
    ls.remember("日本語-プロフィール", snap("c1"));
    ls.remember("", snap("c2"));
    expect(ls.recall("日本語-プロフィール", "c1")).not.toBeNull();
    expect(ls.recall("", "c2")).not.toBeNull();
    expect(ls.recall(null, "c2"), "empty string is not null").toBeNull();
  });
});

describe("lastSeen — snapshots without a conversation id", () => {
  it("L8: a snapshot without conversation.id is not remembered and evicts nothing", () => {
    const ls = createLastSeen();
    for (const id of ["c1", "c2", "c3", "c4", "c5"]) ls.remember(null, snap(id));

    ls.remember(null, snap(null)); // conversation: null

    for (const id of ["c1", "c2", "c3", "c4", "c5"]) {
      expect(ls.recall(null, id), `${id} was not evicted by the id-less snapshot`).not.toBeNull();
    }
  });

  it("L8: a conversation meta with no id field is ignored too", () => {
    const ls = createLastSeen();
    for (const id of ["c1", "c2", "c3", "c4", "c5"]) ls.remember(null, snap(id));

    ls.remember(null, snap("x", { conversation: {} }));
    ls.remember(null, snap("x", { conversation: { name: "nameless" } }));

    for (const id of ["c1", "c2", "c3", "c4", "c5"]) {
      expect(ls.recall(null, id), `${id} still held`).not.toBeNull();
    }
  });
});

describe("lastSeen — instances are independent", () => {
  it("each createLastSeen() has its own storage", () => {
    const a = createLastSeen();
    const b = createLastSeen();
    a.remember(null, snap("c1"));
    expect(b.recall(null, "c1")).toBeNull();
  });
});
