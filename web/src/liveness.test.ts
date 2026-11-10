// Contract tests for the pure liveness module (no DOM, no React).
//
// Written FIRST: `./liveness` does not exist yet, so this file is RED at the
// import until the module lands. The numbers match the acceptance criteria.

import { describe, expect, it } from "vitest";
import {
  HEARTBEAT_STALE_MS,
  WATCHDOG_TICK_MS,
  heard,
  isStale,
  shouldReconnect,
  type LastHeard,
  type Trigger,
} from "./liveness";

// WebSocket readyState values per the spec; the hook compares against the
// numeric literal because test fakes define no static constants.
const CONNECTING = 0;
const OPEN = 1;
const CLOSING = 2;
const CLOSED = 3;

describe("liveness constants (10)", () => {
  it("pins the stale window to 45 s", () => {
    expect(HEARTBEAT_STALE_MS).toBe(45_000);
  });

  it("pins the watchdog tick to 5 s", () => {
    expect(WATCHDOG_TICK_MS).toBe(5_000);
  });
});

describe("heard (7)", () => {
  it("stays null for a non-heartbeat frame before the first heartbeat", () => {
    expect(heard(null, "state", 5)).toBeNull();
  });

  it("arms on the first heartbeat, stamping now", () => {
    expect(heard(null, "heartbeat", 5)).toBe(5);
  });

  it("refreshes on any frame once armed", () => {
    expect(heard(5, "state", 9)).toBe(9);
  });

  it("refreshes on a heartbeat once armed", () => {
    expect(heard(5, "heartbeat", 9)).toBe(9);
  });

  it("keeps a zero timestamp armed (0 is a time, not 'unset')", () => {
    expect(heard(0, "state", 7)).toBe(7);
  });

  it.each([undefined, null, 42, {}, "Heartbeat", "heartbeat "])(
    "does not arm on a non-matching frame type %j",
    (frameType) => {
      expect(heard(null, frameType, 5)).toBeNull();
    },
  );

  it("does not arm on an absent type even when armed state is null", () => {
    expect(heard(null, undefined, 123)).toBeNull();
  });
});

describe("isStale (8)", () => {
  it("is never stale before the first heartbeat, however late", () => {
    expect(isStale(null, 1e12)).toBe(false);
  });

  it("is not stale exactly at the 45 s boundary", () => {
    expect(isStale(0, 45_000)).toBe(false);
  });

  it("is stale one millisecond past the boundary", () => {
    expect(isStale(0, 45_001)).toBe(true);
  });

  it("is not stale right after a frame", () => {
    expect(isStale(1_000_000, 1_000_000)).toBe(false);
  });

  it("is a wall-clock difference, independent of the absolute time", () => {
    const t = 1_700_000_000_000;
    expect(isStale(t, t + 45_000)).toBe(false);
    expect(isStale(t, t + 45_001)).toBe(true);
  });
});

describe("shouldReconnect (9)", () => {
  const NOW = 1_000_000;
  const fresh: LastHeard = NOW - 1_000;
  const stale: LastHeard = NOW - 45_001;

  const cases: {
    name: string;
    readyState: number | null;
    last: LastHeard;
    tick: boolean;
    kick: boolean;
  }[] = [
    { name: "OPEN + fresh", readyState: OPEN, last: fresh, tick: false, kick: false },
    { name: "OPEN + stale", readyState: OPEN, last: stale, tick: true, kick: true },
    // Old server: never armed → never stale.
    { name: "OPEN + never heard", readyState: OPEN, last: null, tick: false, kick: false },
    { name: "CONNECTING + fresh", readyState: CONNECTING, last: fresh, tick: false, kick: true },
    { name: "CONNECTING + stale", readyState: CONNECTING, last: stale, tick: false, kick: true },
    { name: "CONNECTING + never heard", readyState: CONNECTING, last: null, tick: false, kick: true },
    { name: "CLOSING + fresh", readyState: CLOSING, last: fresh, tick: false, kick: true },
    { name: "CLOSING + stale", readyState: CLOSING, last: stale, tick: false, kick: true },
    { name: "CLOSING + never heard", readyState: CLOSING, last: null, tick: false, kick: true },
    { name: "CLOSED + fresh", readyState: CLOSED, last: fresh, tick: false, kick: true },
    { name: "CLOSED + stale", readyState: CLOSED, last: stale, tick: false, kick: true },
    { name: "CLOSED + never heard", readyState: CLOSED, last: null, tick: false, kick: true },
    { name: "no socket + fresh", readyState: null, last: fresh, tick: false, kick: true },
    { name: "no socket + stale", readyState: null, last: stale, tick: false, kick: true },
    { name: "no socket + never heard", readyState: null, last: null, tick: false, kick: true },
  ];

  it.each(cases)("tick is $tick for $name", ({ readyState, last, tick }) => {
    expect(shouldReconnect("tick" satisfies Trigger, readyState, last, NOW)).toBe(tick);
  });

  it.each(cases)("kick is $kick for $name", ({ readyState, last, kick }) => {
    expect(shouldReconnect("kick" satisfies Trigger, readyState, last, NOW)).toBe(kick);
  });

  it("treats an OPEN socket exactly at the 45 s boundary as healthy", () => {
    expect(shouldReconnect("tick", OPEN, NOW - 45_000, NOW)).toBe(false);
    expect(shouldReconnect("kick", OPEN, NOW - 45_000, NOW)).toBe(false);
  });

  it("treats an OPEN socket one millisecond past the boundary as dead", () => {
    expect(shouldReconnect("tick", OPEN, NOW - 45_001, NOW)).toBe(true);
    expect(shouldReconnect("kick", OPEN, NOW - 45_001, NOW)).toBe(true);
  });
});
