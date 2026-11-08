// @vitest-environment jsdom
//
// Tests for the `switchConvo` `firstFrame` seam (CONTRACT 2) in useAgent.ts.
//
// Written FIRST, expected to be RED until useAgent's `switchConvo` accepts an
// optional 4th `firstFrame` parameter and sends the held frame exactly once on
// `attached`. The behaviour is absent today, so the assertions fail at runtime
// for the RIGHT reason (missing behaviour).
//
// Contract (locked):
//   - switchConvo gains an optional 4th parameter `firstFrame` (a
//     client→server frame object).
//   - It is stored in a pendingFrameRef, after the existing early-return guard.
//   - In the handler for the `attached` server message, the held frame is sent
//     once and then cleared.
//   - Every call to switchConvo overwrites the ref with `firstFrame ?? null`,
//     so a held frame never reaches a different conversation.
//
// Cases:
//   9.  after switchConvo(null,…,F): F is NOT sent when the socket opens, IS
//       sent right after `attached`, and is sent only once (a second attached
//       does not resend it).
//   10. if switchConvo(id) is called after a held F, F is never sent.
//   11. a reconnect before `attached` still sends F exactly once, on the
//       attach that succeeds.
//
// Harness modeled on useAgent.reattach.test.tsx / useAgent.t9.test.tsx
// (FakeWebSocket replaces the global; the backoff timer is faked for the
// deterministic reconnect in case 11). No network is touched.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent, type AgentConnection } from "./useAgent";

// ─── fake WebSocket (useAgent.t9.test.tsx conventions) ──────────────────

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

// ─── frame helpers ───────────────────────────────────────────────────────

const framesOf = (ws: FakeWebSocket): object[] => ws.sent.map((s) => JSON.parse(s));
const countOf = (ws: FakeWebSocket, frame: object): number =>
  framesOf(ws).filter((f) => JSON.stringify(f) === JSON.stringify(frame)).length;

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
  const ws = FakeWebSocket.instances.at(-1)!;
  await act(async () => {
    ws.onmessage?.({ data: JSON.stringify(frame) });
  });
}

/** Fully tear down the current mount so a single test can run two
 * independent sessions (used by case 10's positive control). */
async function resetAgent(): Promise<void> {
  act(() => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
  FakeWebSocket.instances = [];
  conn = null;
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
});

afterEach(() => {
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

const CONVO = "77777777-7777-7777-7777-777777777777";
const F = { type: "switch_cwd", path: "/r" };

// Seed `?convo=` with a real id so a busy session is bound to a non-null
// convo — that is what makes `switchConvo(null, …, F)` pass the existing
// early-return guard (a busy session always has a minted convo id).
async function mountOn(convo: string): Promise<void> {
  window.history.replaceState(null, "", `/?convo=${convo}`);
  await mountAgent();
  await feed({ type: "attached", convo });
}

// ─── case 9 ─────────────────────────────────────────────────────────────

describe("useAgent — switchConvo firstFrame (case 9)", () => {
  it("a held frame is sent once on attached, never on open, and never on a second attached", async () => {
    await mountOn(CONVO);

    await act(async () => {
      conn!.switchConvo(null, undefined, undefined, F);
      await Promise.resolve(); // the fresh socket's onopen microtask
    });

    const ws = FakeWebSocket.instances.at(-1)!;
    // F is NOT sent when the socket opens — only the attach frame rides the
    // fresh socket.
    expect(framesOf(ws), "only the attach frame is sent on open").toEqual([
      { type: "attach", convo: null },
    ]);
    expect(countOf(ws, F), "no held frame before attached").toBe(0);

    // The server binds the fresh conversation → the held frame is sent once.
    await feed({ type: "attached", convo: "fresh-1" });
    expect(countOf(ws, F), "the held frame is sent on attached").toBe(1);

    // A second `attached` must NOT resend it.
    await feed({ type: "attached", convo: "fresh-1" });
    expect(countOf(ws, F), "no resend on a second attached").toBe(1);
  });
});

// ─── case 10 ────────────────────────────────────────────────────────────

describe("useAgent — a later switchConvo clears a held frame (case 10)", () => {
  it("switchConvo(id) after a held F clears it, so F is never sent", async () => {
    // CONTROL (makes this test discriminating): with ONLY the first switch, a
    // held F IS sent on the attach of its own socket. RED until the feature
    // exists, so the treatment below can't pass trivially.
    await mountOn(CONVO);
    await act(async () => {
      conn!.switchConvo(null, undefined, undefined, F);
      await Promise.resolve();
    });
    const wsControl = FakeWebSocket.instances.at(-1)!;
    await feed({ type: "attached", convo: "fresh" });
    expect(
      countOf(wsControl, F),
      "control: a held F is sent on its own attach",
    ).toBe(1);

    await resetAgent();

    // TREATMENT: a second switchConvo(id) (no firstFrame) clears the held F
    // (firstFrame ?? null) before that socket ever attaches.
    await mountOn(CONVO);
    await act(async () => {
      conn!.switchConvo(null, undefined, undefined, F);
      await Promise.resolve();
    });
    await act(async () => {
      conn!.switchConvo("other-id");
      await Promise.resolve();
    });

    const ws = FakeWebSocket.instances.at(-1)!;
    expect(framesOf(ws), "the socket attaches to the target id").toEqual([
      { type: "attach", convo: "other-id" },
    ]);

    // When it attaches, the earlier held F must NOT be sent.
    await feed({ type: "attached", convo: "other-id" });
    expect(countOf(ws, F), "the cleared frame is never sent").toBe(0);
  });
});

// ─── case 11 ────────────────────────────────────────────────────────────

describe("useAgent — reconnect before attached keeps the held frame (case 11)", () => {
  it("a reconnect before attached still sends F exactly once, on the attach that succeeds", async () => {
    await mountOn(CONVO);

    await act(async () => {
      conn!.switchConvo(null, undefined, undefined, F);
      await Promise.resolve();
    });
    const ws2 = FakeWebSocket.instances.at(-1)!;

    // Drop the fresh socket BEFORE the server ever attaches it.
    await act(async () => {
      ws2.close();
    });

    // Reconnect after the 500 ms backoff.
    await act(async () => {
      vi.advanceTimersByTime(600);
      await Promise.resolve(); // the reconnected socket's onopen microtask
    });
    const ws3 = FakeWebSocket.instances.at(-1)!;

    // F was never sent on the dropped socket (it never attached).
    expect(countOf(ws2, F), "no frame on the dropped socket").toBe(0);

    // The reconnected socket re-attaches to the fresh convo (null)…
    expect(framesOf(ws3)[0], "reconnect re-attaches to the fresh convo").toEqual({
      type: "attach",
      convo: null,
    });

    // …and when it attaches, the still-held F is sent exactly once.
    await feed({ type: "attached", convo: "fresh-1" });
    expect(countOf(ws3, F), "the held frame survives the reconnect and is sent once").toBe(1);
  });
});
