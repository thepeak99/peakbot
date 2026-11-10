// @vitest-environment jsdom
//
// Liveness watchdog tests for useAgent (acceptance criteria 11–20, 22).
//
// Written FIRST. Behaviour under test: the server sends `{"type":"heartbeat"}`
// every 20 s; once a socket has produced one, silence for > 45 s (wall clock)
// makes the hook close it and reconnect. Resume signals (`visibilitychange`,
// `pageshow`, `online`, `peakbot-resume`) reconnect at once if the socket is
// not provably healthy; `offline` drops the socket and schedules a retry.
//
// Harness contract the hook must satisfy for these tests to drive it:
//   - sockets are made with `new WebSocket(url)` read from the global at call
//     time; the fake starts CONNECTING (0) and the test calls `open()`.
//   - the hook reads `ws.readyState` (numeric compare with 1) and registers
//     the four `on*` handler properties (never addEventListener on the socket).
//   - time comes from `Date.now()`; timers are `setTimeout` / `setInterval`,
//     all faked here (together with Date) so a test can jump the wall clock
//     without running ticks.
//   - the fake dispatches events only to the handlers CURRENTLY attached, like
//     a browser, so an old socket whose handlers were detached stays silent.

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useAgent, type AgentConnection } from "./useAgent";

// ─── fake WebSocket ─────────────────────────────────────────────────────

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  readyState = 0; // CONNECTING until the test opens it
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  sent: string[] = [];
  closeCalls = 0;

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  /** Our own close(): like a browser, a no-op once CLOSED; the close event is
   * queued and reaches a handler only if one is still attached. */
  close(): void {
    this.closeCalls += 1;
    if (this.readyState === 3) return;
    this.readyState = 3;
    queueMicrotask(() => this.onclose?.());
  }

  /** The handshake completes. */
  open(): void {
    this.readyState = 1;
    this.onopen?.();
  }

  /** The peer or the network ends the socket. */
  serverClose(): void {
    this.readyState = 3;
    this.onclose?.();
  }

  /** An error event (a failed handshake or a broken pipe). */
  fail(): void {
    this.onerror?.();
  }

  receive(frame: object): void {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

  get handlersDetached(): boolean {
    return !this.onopen && !this.onmessage && !this.onclose && !this.onerror;
  }
}

const sockets = (): FakeWebSocket[] => FakeWebSocket.instances;
const sentFrames = (ws: FakeWebSocket): Record<string, unknown>[] => ws.sent.map((s) => JSON.parse(s));

// ─── mount + time plumbing ──────────────────────────────────────────────

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let conn: AgentConnection | null = null;
let visibility: "visible" | "hidden" = "visible";

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
}

async function unmountAgent(): Promise<void> {
  await act(async () => {
    root?.unmount();
  });
  root = null;
  container?.remove();
  container = null;
}

/** Advance the faked clock (timers + Date) and let React settle. */
async function advance(ms: number): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

/** Advance in steps so interval ticks interleave with test actions. */
async function advanceInSteps(totalMs: number, stepMs: number, each?: () => void): Promise<void> {
  for (let elapsed = 0; elapsed < totalMs; elapsed += stepMs) {
    await advance(Math.min(stepMs, totalMs - elapsed));
    each?.();
  }
}

/** Run a synchronous interaction inside act so state updates flush. */
async function run(fn: () => void): Promise<void> {
  await act(async () => {
    fn();
  });
}

/** Open the newest socket and let the server bind it. */
async function openAndAttach(ws: FakeWebSocket = sockets().at(-1)!): Promise<void> {
  await run(() => {
    ws.open();
    ws.receive({ type: "attached", convo: "c-777" });
    ws.receive({ type: "state", state: { conversation: { id: "c-777" } } });
  });
}

const heartbeat = (ws: FakeWebSocket) => run(() => ws.receive({ type: "heartbeat" }));

function setVisibility(v: "visible" | "hidden"): void {
  visibility = v;
}

function dispatchPageshow(persisted: boolean): void {
  const ev = new Event("pageshow");
  Object.defineProperty(ev, "persisted", { value: persisted });
  window.dispatchEvent(ev);
}

/** The four "come back" signals, each ready to fire. */
const KICKS: { name: string; fire: () => void }[] = [
  {
    name: "visibilitychange (visible)",
    fire: () => {
      setVisibility("visible");
      document.dispatchEvent(new Event("visibilitychange"));
    },
  },
  { name: "pageshow (persisted: true)", fire: () => dispatchPageshow(true) },
  { name: "online", fire: () => window.dispatchEvent(new Event("online")) },
  { name: "peakbot-resume", fire: () => window.dispatchEvent(new Event("peakbot-resume")) },
];
const ALL_EVENTS: (() => void)[] = [
  ...KICKS.map((k) => k.fire),
  () => dispatchPageshow(false),
  () => window.dispatchEvent(new Event("offline")),
];

const T0 = 1_700_000_000_000;

beforeAll(() => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

beforeEach(() => {
  FakeWebSocket.instances = [];
  conn = null;
  visibility = "visible";
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => visibility,
  });
  vi.useFakeTimers({
    toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"],
  });
  vi.setSystemTime(T0);
  vi.stubGlobal("WebSocket", FakeWebSocket);
  vi.stubGlobal("fetch", vi.fn(() => Promise.resolve({ ok: false, json: () => [] })));
  window.history.replaceState(null, "", "/?convo=c-777&profile=bonsol-dev");
});

afterEach(async () => {
  if (root) await unmountAgent();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  Reflect.deleteProperty(document, "visibilityState");
  window.history.replaceState(null, "", "/");
});

// ─── 11 — an old server never arms the watchdog ─────────────────────────

describe("useAgent liveness — old server (11)", () => {
  it("11: never reconnects a socket whose server never sends a heartbeat", async () => {
    await mountAgent();
    await openAndAttach();

    await advance(10 * 60_000);

    expect(sockets(), "still exactly one socket after 10 quiet minutes").toHaveLength(1);
    expect(conn!.connected, "still connected").toBe(true);
    expect(sockets()[0].closeCalls, "never closed by the watchdog").toBe(0);
  });
});

// ─── 12 — the watchdog ──────────────────────────────────────────────────

describe("useAgent liveness — watchdog (12)", () => {
  it("12: does not reconnect at +44 s of silence after a heartbeat", async () => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);

    await advance(44_000);

    expect(sockets()).toHaveLength(1);
    expect(sockets()[0].closeCalls).toBe(0);
  });

  it("12: closes the silent socket and dials exactly one new one by +51 s", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    await heartbeat(first);

    await advance(51_000);

    expect(first.closeCalls, "the silent socket was closed").toBeGreaterThanOrEqual(1);
    expect(sockets(), "exactly one replacement").toHaveLength(2);
    expect(conn!.connected, "not connected while the replacement handshakes").toBe(false);
  });

  it("12: the replacement's first frame is attach with the same convo and profile", async () => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);
    await advance(51_000);

    const second = sockets()[1];
    await run(() => second.open());

    expect(sentFrames(second)[0]).toStrictEqual({
      type: "attach",
      convo: "c-777",
      profile: "bonsol-dev",
    });
    expect(conn!.connected, "connected once the replacement opens").toBe(true);
  });

  it("12: the replacement starts disarmed — a quiet old-style stream is left alone", async () => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);
    await advance(51_000);
    await openAndAttach(sockets()[1]);

    await advance(10 * 60_000);

    expect(sockets(), "no heartbeat on the new socket → never re-armed → never restarted").toHaveLength(2);
  });

  it("12: restart detaches the old socket's handlers", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    await heartbeat(first);

    await advance(51_000);

    expect(first.handlersDetached, "an intentional close detaches first").toBe(true);
    expect(sockets()[1].handlersDetached, "the live socket keeps its handlers").toBe(false);
  });

  it("12: staleness is wall-clock — a suspended tab that wakes after one tick still restarts", async () => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);

    // The clock jumps 10 min with no ticks in between (a suspended webview).
    vi.setSystemTime(T0 + 10 * 60_000);
    await advance(5_000); // a single watchdog tick

    expect(sockets(), "one tick sees the wall-clock gap").toHaveLength(2);
  });

  it("12: a late frame on the replacement-less socket resets the clock (no restart at +50 s)", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    await heartbeat(first);

    await advance(40_000);
    await run(() => first.receive({ type: "state", state: { conversation: { id: "c-777" } } }));
    await advance(40_000); // +80 s overall, 40 s since the last frame

    expect(sockets()).toHaveLength(1);
  });
});

// ─── 13 — keep-alive ────────────────────────────────────────────────────

describe("useAgent liveness — keep-alive (13)", () => {
  it.each(["heartbeat", "state"])("13: a %s frame every 20 s for 5 min never reconnects", async (type) => {
    await mountAgent();
    await openAndAttach();
    const ws = sockets()[0];
    await heartbeat(ws); // arm

    await advanceInSteps(5 * 60_000, 20_000, () => {
      act(() => {
        ws.receive(type === "state" ? { type, state: { conversation: { id: "c-777" } } } : { type });
      });
    });

    expect(sockets()).toHaveLength(1);
    expect(ws.closeCalls).toBe(0);
    expect(conn!.connected).toBe(true);
  });
});

// ─── 14 — hidden tabs ───────────────────────────────────────────────────

describe("useAgent liveness — hidden (14)", () => {
  it("14: the tick does nothing while hidden, even 10 min past the stale window", async () => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);
    setVisibility("hidden");

    await advance(10 * 60_000);

    expect(sockets()).toHaveLength(1);
    expect(sockets()[0].closeCalls).toBe(0);
  });

  it("14: becoming visible reconnects a stale socket at once, with no timer advance", async () => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);
    setVisibility("hidden");
    await advance(10 * 60_000);

    await run(() => {
      setVisibility("visible");
      document.dispatchEvent(new Event("visibilitychange"));
    });

    expect(sockets(), "a second socket exists immediately").toHaveLength(2);
    expect(sockets()[0].closeCalls).toBeGreaterThanOrEqual(1);
  });

  it("14: a visibilitychange to hidden does nothing, even for a dead socket", async () => {
    await mountAgent();
    await openAndAttach();
    await run(() => sockets()[0].serverClose()); // dead, backoff pending

    await run(() => {
      setVisibility("hidden");
      document.dispatchEvent(new Event("visibilitychange"));
    });

    expect(sockets()).toHaveLength(1);
  });
});

// ─── 15 / 16 / 17 — resume kicks ────────────────────────────────────────

describe("useAgent liveness — kicks on a healthy socket (15)", () => {
  it.each([true, false])(
    "15: no kick event touches an OPEN, fresh socket (armed: %s)",
    async (armed) => {
      await mountAgent();
      await openAndAttach();
      const ws = sockets()[0];
      if (armed) await heartbeat(ws);
      const sentBefore = ws.sent.length;

      for (const kick of KICKS) await run(kick.fire);

      expect(sockets(), "still one socket").toHaveLength(1);
      expect(ws.closeCalls).toBe(0);
      expect(ws.sent.length, "nothing extra sent").toBe(sentBefore);
      expect(conn!.connected).toBe(true);
    },
  );

  it.each(KICKS)("15: $name reconnects at once when the socket is dead (control)", async ({ fire }) => {
    await mountAgent();
    await openAndAttach();
    await run(() => sockets()[0].serverClose()); // dead; a 500 ms backoff is pending

    await run(fire);

    expect(sockets(), "a new socket is dialed without waiting for the backoff").toHaveLength(2);
  });

  it.each(KICKS)("15: $name restarts a stale OPEN socket", async ({ fire }) => {
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);
    // Jump past the stale window without letting a tick run first.
    vi.setSystemTime(T0 + 60_000);

    await run(fire);

    expect(sockets()).toHaveLength(2);
    expect(sockets()[0].closeCalls).toBeGreaterThanOrEqual(1);
  });
});

describe("useAgent liveness — kick during backoff (16)", () => {
  it("16: `online` mid-backoff dials now, cancels the timer, and resets the backoff", async () => {
    await mountAgent();
    await openAndAttach();

    // Drive the backoff to 4 s: 500 → 1000 → 2000 → 4000 (handshakes keep failing).
    await run(() => sockets()[0].serverClose()); // waits 500
    await advance(500);
    await run(() => sockets()[1].serverClose()); // waits 1000
    await advance(1_000);
    await run(() => sockets()[2].serverClose()); // waits 2000
    await advance(2_000);
    await run(() => sockets()[3].serverClose()); // waits 4000
    expect(sockets()).toHaveLength(4);

    await run(() => window.dispatchEvent(new Event("online")));
    expect(sockets(), "an immediate fifth socket").toHaveLength(5);

    // The cancelled 4 s timer must not fire a sixth.
    await advance(10_000);
    expect(sockets(), "the cancelled timer never creates another").toHaveLength(5);

    // The kick reset the backoff: the next failure retries after 500 ms.
    await run(() => sockets()[4].serverClose());
    await advance(499);
    expect(sockets()).toHaveLength(5);
    await advance(1);
    expect(sockets(), "next attempt came after 500 ms").toHaveLength(6);
  });
});

describe("useAgent liveness — pageshow (17)", () => {
  it("17: pageshow with persisted: false does nothing, even for a dead socket", async () => {
    await mountAgent();
    await openAndAttach();
    await run(() => sockets()[0].serverClose());

    await run(() => dispatchPageshow(false));

    expect(sockets()).toHaveLength(1);
  });

  it("17: pageshow with persisted: true (bfcache restore) does reconnect a dead socket", async () => {
    await mountAgent();
    await openAndAttach();
    await run(() => sockets()[0].serverClose());

    await run(() => dispatchPageshow(true));

    expect(sockets()).toHaveLength(2);
  });
});

// ─── 18 — offline ───────────────────────────────────────────────────────

describe("useAgent liveness — offline (18)", () => {
  it("18: `offline` closes the socket, flips connected, and retries after 500 ms", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    expect(conn!.connected).toBe(true);

    await run(() => window.dispatchEvent(new Event("offline")));

    expect(first.closeCalls, "the socket was closed on offline").toBeGreaterThanOrEqual(1);
    expect(conn!.connected).toBe(false);
    expect(sockets(), "no instant redial").toHaveLength(1);

    await advance(499);
    expect(sockets()).toHaveLength(1);
    await advance(1);
    expect(sockets(), "retry after the 500 ms backoff").toHaveLength(2);
  });

  it("18: the old socket's late close after `offline` schedules nothing extra", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];

    await run(() => window.dispatchEvent(new Event("offline")));
    await run(() => first.onclose?.()); // still attached? then it would add a timer
    await advance(10_000);

    expect(sockets(), "one retry, not two").toHaveLength(2);
  });

  it("18: `online` right after `offline` dials immediately and cancels the retry timer", async () => {
    await mountAgent();
    await openAndAttach();
    await run(() => window.dispatchEvent(new Event("offline")));

    await run(() => window.dispatchEvent(new Event("online")));
    expect(sockets()).toHaveLength(2);

    await run(() => sockets()[1].open());
    await advance(10_000);
    expect(sockets(), "the offline retry timer was cancelled").toHaveLength(2);
  });
});

// ─── 19 — late events from a replaced socket ────────────────────────────

describe("useAgent liveness — late events (19)", () => {
  it("19: close/error/message from the OLD socket after a watchdog restart change nothing", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    await heartbeat(first);
    await advance(51_000);
    expect(sockets()).toHaveLength(2);
    const second = sockets()[1];
    await run(() => second.open());
    await heartbeat(second);

    await run(() => {
      first.serverClose();
      first.fail();
      first.receive({ type: "state", state: { conversation: { id: "c-777" } } });
      first.receive({ type: "error", message: "late" });
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    await advanceInSteps(10_000, 1_000, () => {
      act(() => second.receive({ type: "heartbeat" }));
    });

    expect(sockets(), "no third socket").toHaveLength(2);
    expect(conn!.connected, "the live socket still counts as connected").toBe(true);
    expect(conn!.error, "a late error frame is not surfaced").toBeNull();
    expect(second.closeCalls).toBe(0);
  });

  it("19: a late close from a socket replaced by a kick does not schedule a reconnect", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    await heartbeat(first);
    vi.setSystemTime(T0 + 60_000);
    await run(() => window.dispatchEvent(new Event("peakbot-resume")));
    expect(sockets()).toHaveLength(2);
    await run(() => sockets()[1].open());

    await run(() => first.serverClose());
    await act(async () => {
      await Promise.resolve();
    });
    await advance(10_000);

    expect(sockets()).toHaveLength(2);
  });
});

// ─── 20 — cleanup ───────────────────────────────────────────────────────

describe("useAgent liveness — unmount (20)", () => {
  it("20: unmount removes every listener it added and the watchdog interval", async () => {
    const winAdd = vi.spyOn(window, "addEventListener");
    const winRemove = vi.spyOn(window, "removeEventListener");
    const docAdd = vi.spyOn(document, "addEventListener");
    const docRemove = vi.spyOn(document, "removeEventListener");
    await mountAgent();
    await openAndAttach();
    await heartbeat(sockets()[0]);

    const added = (spy: typeof winAdd, type: string) =>
      spy.mock.calls.filter((c) => c[0] === type).map((c) => c[1]);
    const winTypes = ["pageshow", "online", "offline", "peakbot-resume"];
    for (const type of winTypes) {
      expect(added(winAdd, type), `window listener for ${type} registered`).toHaveLength(1);
    }
    expect(added(docAdd, "visibilitychange"), "document listener for visibilitychange registered").toHaveLength(1);
    expect(vi.getTimerCount(), "the watchdog interval is running while mounted").toBeGreaterThanOrEqual(1);

    await unmountAgent();

    for (const type of winTypes) {
      expect(added(winRemove, type), `window ${type} listener removed`).toEqual(added(winAdd, type));
    }
    expect(added(docRemove, "visibilitychange"), "document listener removed").toEqual(
      added(docAdd, "visibilitychange"),
    );
    expect(vi.getTimerCount(), "no timer survives unmount").toBe(0);
  });

  it("20: after unmount no event or time creates a socket", async () => {
    await mountAgent();
    await openAndAttach();
    const first = sockets()[0];
    await heartbeat(first);
    await unmountAgent();

    for (const fire of ALL_EVENTS) fire();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(10 * 60_000);
    });

    expect(sockets(), "no new socket after unmount").toHaveLength(1);
    expect(first.closeCalls, "the live socket was closed").toBeGreaterThanOrEqual(1);
    expect(first.handlersDetached, "its handlers were detached").toBe(true);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("20: unmount while a backoff timer is pending leaves no timer and no socket", async () => {
    await mountAgent();
    await openAndAttach();
    await run(() => sockets()[0].serverClose()); // 500 ms retry pending

    await unmountAgent();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });

    expect(sockets()).toHaveLength(1);
    expect(vi.getTimerCount()).toBe(0);
  });
});

// ─── 22 — forward compatibility (GUARD) ─────────────────────────────────

describe("useAgent liveness — unknown frames (22, guard)", () => {
  it("22: an unknown frame type changes nothing and does not throw", async () => {
    await mountAgent();
    await openAndAttach();
    const ws = sockets()[0];
    const before = { state: conn!.state, error: conn!.error, convo: conn!.convo, connected: conn!.connected };

    expect(() => {
      act(() => ws.receive({ type: "some_future_frame", payload: { x: 1 } }));
    }).not.toThrow();

    expect(conn!.state).toBe(before.state);
    expect(conn!.error).toBe(before.error);
    expect(conn!.convo).toBe(before.convo);
    expect(conn!.connected).toBe(before.connected);
    expect(sockets()).toHaveLength(1);
  });

  it("22: a heartbeat frame is a no-op for the visible state", async () => {
    await mountAgent();
    await openAndAttach();
    const ws = sockets()[0];
    const before = { state: conn!.state, error: conn!.error, convo: conn!.convo };

    expect(() => {
      act(() => ws.receive({ type: "heartbeat" }));
    }).not.toThrow();

    expect(conn!.state).toBe(before.state);
    expect(conn!.error).toBe(before.error);
    expect(conn!.convo).toBe(before.convo);
  });
});
