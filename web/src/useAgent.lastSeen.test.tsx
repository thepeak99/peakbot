// @vitest-environment jsdom
//
// Real `useAgent` + fake WebSocket: the "state always describes convo"
// invariant and the last-seen cache of 5 chats (convo-state-cache design §3,
// §4, §8.2 — U1–U12, plus U6b).
//
// Written FIRST (TDD), against the locked design. Expected on master:
//   RED   U1–U5, U7–U11  — useAgent never swaps `state` on a switch (the
//                          previous conversation's snapshot stays), never
//                          clears error/alias, and has no cache to recall.
//   GREEN U6, U6b, U12   — same-convo reconnect keeps the live state; no
//                          reconnect storm; nothing from the cache is sent.
//                          Locks: they must stay green once the cache lands.
//
// "Before any frame on the new socket" is observed with a SYNCHRONOUS act
// (`switchNow`): the fake socket's `onopen` is a queued microtask, and the
// real `onopen` also clears `error`, so awaiting before asserting would hide
// the switch-time state. Frames are fed by hand, like useAgent.firstFrame.
//
// Harness copied (repo convention) from useAgent.firstFrame.test.tsx /
// useAgent.reattach.test.tsx; profile URLs from useAgent.t9.test.tsx.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent, type AgentConnection } from "./useAgent";
import type { AppState, WireChatMessage } from "./state";

// ─── fake WebSocket (useAgent.firstFrame.test.tsx conventions) ───────────

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 1; // OPEN, like the real socket once `onopen` has fired
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
    queueMicrotask(() => this.onopen?.());
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.());
  }
}

const sockets = (): FakeWebSocket[] => FakeWebSocket.instances;
const framesOf = (ws: FakeWebSocket): object[] => ws.sent.map((s) => JSON.parse(s));

// ─── mount plumbing ─────────────────────────────────────────────────────

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let conn: AgentConnection | null = null;

function Harness() {
  conn = useAgent();
  return null;
}

async function mountAgent(): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(<Harness />);
  });
  // Flush the fake socket's onopen microtask (the attach frame is sent in it).
  await act(async () => {
    await Promise.resolve();
  });
}

async function feed(frame: object): Promise<void> {
  const ws = sockets().at(-1)!;
  await act(async () => {
    ws.onmessage?.({ data: JSON.stringify(frame) });
  });
}

/** Switch WITHOUT yielding: the new socket's onopen has not run yet, so
 * nothing but the switch itself has touched the hook's state. */
function switchNow(id: string | null, profile?: string | null): void {
  act(() => {
    conn!.switchConvo(id, profile);
  });
}

/** Let the fresh socket's onopen microtask run (it sends `attach`). */
async function settle(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  FakeWebSocket.instances = [];
  conn = null;
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  window.history.replaceState(null, "", "/?convo=c1");
});

afterEach(async () => {
  // A test may end on a synchronous `switchNow`: let the new socket's queued
  // onopen run inside act instead of leaking an out-of-act state update.
  await settle();
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

// ─── fixtures ───────────────────────────────────────────────────────────

const msg = (content: string): WireChatMessage => ({
  role: "agent",
  content,
  timestamp: "2026-01-01T00:00:00Z",
});

/** A minimal, valid-enough `AppState` frame for conversation `id`. */
function stateFor(id: string, texts: string[], over: Partial<AppState> = {}): AppState {
  return {
    chat: { messages: texts.map(msg), auto_scroll: true, scroll_offset: 0 },
    todo: { visible: false, items: [] },
    stats: {
      total_input_tokens: 0,
      total_output_tokens: 0,
      total_api_calls: 0,
      total_cost: 0,
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
    conversation: { id, name: `name ${id}` },
    is_running: false,
    is_loading: false,
    welcome: null,
    exit_requested: false,
    bg: { running_count: 0, recent_summaries: [] },
    bash_panel: { kind: "idle" },
    ...over,
  } as unknown as AppState;
}

const contents = (): string[] | null =>
  conn!.state === null ? null : conn!.state.chat.messages.map((m) => m.content ?? "");

/** The live handshake of the newest socket: attached + one state frame. */
async function live(id: string, texts: string[], over: Partial<AppState> = {}): Promise<void> {
  await feed({ type: "attached", convo: id });
  await feed({ type: "state", state: stateFor(id, texts, over) });
}

/** Mount on `?convo=c1` (set by beforeEach) and land c1's first live frame. */
async function boot(texts: string[] = ["A1"], over: Partial<AppState> = {}): Promise<void> {
  await mountAgent();
  await live("c1", texts, over);
}

/** Switch to `id` and let its (fresh) socket attach and receive a frame. */
async function visit(id: string, texts: string[], over: Partial<AppState> = {}): Promise<void> {
  switchNow(id);
  await settle();
  await live(id, texts, over);
}

/** Every in-flight-turn field set, as a turn caught mid-flight. */
const RUNNING = {
  is_running: true,
  command_running: true,
  stop_pending: true,
  pending_input_count: 3,
  status_message: "compiling",
  sub_agent: { role: "researcher", pausable: true, pause: "running" },
} as const;

// ─── U1–U5: the switch swaps `state` at once ─────────────────────────────

describe("useAgent last-seen — a switch exposes the target, never the previous chat", () => {
  it("U1: switching to an unseen convo exposes no state before its first frame", async () => {
    await boot(["A1"]);
    expect(contents(), "precondition: c1 is live").toEqual(["A1"]);

    switchNow("c2"); // before ANY frame on the new socket

    expect(conn!.convo).toBe("c2");
    expect(conn!.state, "state must not describe c1 while bound to c2").toBeNull();
    await settle();
  });

  it("U2: switching back to a recently viewed convo exposes its last-seen messages immediately", async () => {
    await boot(["A1"]);
    await visit("c2", ["B1"]);
    expect(contents(), "precondition: c2 is live").toEqual(["B1"]);

    switchNow("c1"); // no frame fed

    expect(conn!.convo).toBe("c1");
    expect(conn!.state?.conversation?.id).toBe("c1");
    expect(contents(), "c1's last-seen transcript, not c2's").toEqual(["A1"]);
    await settle();
  });

  it("U3: the live frame replaces the recalled snapshot wholesale", async () => {
    await boot(["A1"]);
    await visit("c2", ["B1"]);
    switchNow("c1");
    expect(contents(), "precondition: the recalled snapshot is c1's").toEqual(["A1"]);
    await settle();
    await feed({ type: "attached", convo: "c1" });

    await feed({
      type: "state",
      state: stateFor("c1", ["A1", "A2"], { is_running: true }),
    });

    expect(contents()).toEqual(["A1", "A2"]);
    expect(conn!.state!.is_running, "live claims come from the live frame").toBe(true);
  });

  it("U4: only the 5 most recent convos are recalled", async () => {
    await boot(["T1"]);
    for (const id of ["c2", "c3", "c4", "c5", "c6"]) await visit(id, [`T-${id}`]);

    switchNow("c1"); // the 6th-oldest: evicted
    expect(conn!.convo).toBe("c1");
    expect(conn!.state, "c1 fell out of the 5-entry cache").toBeNull();

    switchNow("c2"); // still held
    expect(conn!.state?.conversation?.id).toBe("c2");
    expect(contents()).toEqual(["T-c2"]);
    await settle();
  });

  it("U5: a recalled snapshot is quiesced (messages and bg are kept)", async () => {
    const bg = {
      running_count: 1,
      recent_summaries: [{ id: 7, command: "sleep 99", status: "running" }],
    };
    await boot(["A1"], { ...RUNNING, bg });
    await visit("c2", ["B1"]);

    switchNow("c1");

    const s = conn!.state;
    expect(s, "a hit").not.toBeNull();
    expect(s!.conversation?.id, "the recalled snapshot is c1's, not c2's").toBe("c1");
    expect(s!.is_running).toBe(false);
    expect(s!.command_running).toBe(false);
    expect(s!.stop_pending).toBe(false);
    expect(s!.pending_input_count).toBe(0);
    expect(s!.status_message).toBeNull();
    expect(s!.sub_agent).toBeNull();
    expect(contents(), "the transcript is shown as last seen").toEqual(["A1"]);
    expect(s!.bg, "read-only panels are shown as last seen").toEqual(bg);
    await settle();
  });
});

// ─── U6: same-convo reconnect (GREEN lock) ───────────────────────────────

describe("useAgent last-seen — a same-convo reconnect never touches state (I5)", () => {
  it("U6: an unasked close + timer reconnect keeps the live state until a new frame", async () => {
    await boot(["A1"], { is_running: true });
    const before = conn!.state;
    expect(before!.is_running).toBe(true);

    await act(async () => {
      sockets().at(-1)!.close(); // unasked: the handlers are still attached
    });
    await act(async () => {
      vi.advanceTimersByTime(600); // the 500 ms backoff
      await Promise.resolve(); // the new socket's onopen
    });
    expect(sockets(), "a replacement socket was dialed").toHaveLength(2);
    await feed({ type: "attached", convo: "c1" });

    expect(conn!.state, "the very same live snapshot object").toBe(before);
    expect(conn!.state!.is_running, "not quiesced").toBe(true);
    expect(contents()).toEqual(["A1"]);
  });

  it("U6: a peakbot-resume kick on a dead socket keeps the live state until a new frame", async () => {
    await boot(["A1"], { is_running: true });
    const before = conn!.state;
    sockets().at(-1)!.readyState = 3; // dead in place; no close event delivered

    await act(async () => {
      window.dispatchEvent(new Event("peakbot-resume"));
      await Promise.resolve(); // the replacement's onopen
    });
    expect(sockets(), "the kick dialed a replacement").toHaveLength(2);
    await feed({ type: "attached", convo: "c1" });

    expect(conn!.state).toBe(before);
    expect(conn!.state!.is_running).toBe(true);
    expect(contents()).toEqual(["A1"]);
  });

  it("U6b: a switch dials exactly one new socket and later frames add none (no reconnect storm, R1)", async () => {
    await boot(["A1"]);
    expect(sockets()).toHaveLength(1);

    switchNow("c2");
    await settle();
    await live("c2", ["B1"]);
    await feed({ type: "state", state: stateFor("c2", ["B1", "B2"]) });
    await act(async () => {
      vi.advanceTimersByTime(10_000);
    });

    expect(sockets(), "one socket for the boot, one for the switch").toHaveLength(2);
  });
});

// ─── U7–U9: the binding moves without a live frame ───────────────────────

describe("useAgent last-seen — every binding move without a live frame drops the held state", () => {
  it("U7: an attached reply for a different convo drops the held state", async () => {
    await boot(["A1"]);
    await visit("c2", ["B1"]);
    await visit("c1", ["A1"]);
    switchNow("c2"); // a hit: c2's snapshot is shown (pinned by U2)
    await settle();

    // The server fell back to a fresh conversation: a different id.
    await feed({ type: "attached", convo: "c9" });

    expect(conn!.convo).toBe("c9");
    expect(conn!.state, "nothing held describes c9").toBeNull();
  });

  it("U8: the profile is part of the cache key", async () => {
    window.history.replaceState(null, "", "/?convo=c1&profile=home");
    await boot(["A1"]);
    await visit("c2", ["B1"]); // still under "home": switchConvo keeps the profile

    switchNow("c1", null); // c1 under the LOCAL profile was never seen
    expect(conn!.convo).toBe("c1");
    expect(conn!.state, "same convo id, different profile → a miss").toBeNull();

    switchNow("c1", "home"); // c1 was seen under "home"
    expect(conn!.state?.conversation?.id).toBe("c1");
    expect(contents()).toEqual(["A1"]);
    await settle();
  });

  it("U9: a new chat exposes no state", async () => {
    await boot(["A1"]);

    switchNow(null, null);

    expect(conn!.convo).toBeNull();
    expect(conn!.state).toBeNull();
    await settle();
  });
});

// ─── U10: error + alias belong to the old socket ─────────────────────────

describe("useAgent last-seen — a switch drops the previous socket's error and model alias", () => {
  it("U10: error and activeAlias are cleared at the switch", async () => {
    await boot(["A1"]);
    await feed({ type: "error", message: "boom" });
    await feed({ type: "models_available", active: "sonnet", models: [] });
    expect(conn!.error, "precondition").toBe("boom");
    expect(conn!.activeAlias, "precondition").toBe("sonnet");

    // Synchronous: asserted before the new socket's onopen (which clears
    // `error` on its own) can run.
    switchNow("c2");

    expect(conn!.error, "c1's error is not c2's").toBeNull();
    expect(conn!.activeAlias, "c1's model is not c2's").toBe("");
    await settle();
  });
});

// ─── U11: a server-side move keeps the live frame ────────────────────────

describe("useAgent last-seen — a server-side move (/new) is not a switch", () => {
  it("U11: the live frame is kept as fed, and the old convo stays recallable", async () => {
    await boot(["A1"]);

    // /new: the same socket now carries a state frame for another id.
    await feed({ type: "state", state: stateFor("c7", ["C1"], { is_running: true }) });

    expect(conn!.convo).toBe("c7");
    expect(conn!.state?.conversation?.id).toBe("c7");
    expect(conn!.state!.is_running, "a live frame is never quiesced").toBe(true);

    switchNow("c1"); // the old convo is still held as last seen
    expect(conn!.state?.conversation?.id).toBe("c1");
    expect(contents()).toEqual(["A1"]);
    await settle();
  });
});

// ─── U12: nothing from the cache is ever sent (GREEN lock, I4) ───────────

describe("useAgent last-seen — the cache never leaves memory", () => {
  it("U12: after a recalling switch the new socket sends exactly [attach{convo}]", async () => {
    await boot(["SECRET-A1"]);
    await visit("c2", ["SECRET-B1"]);

    switchNow("c1");
    await settle();

    const ws = sockets().at(-1)!;
    expect(framesOf(ws)).toEqual([{ type: "attach", convo: "c1" }]);
    expect(ws.sent.join(""), "no snapshot text rides the socket").not.toContain("SECRET");
  });
});
