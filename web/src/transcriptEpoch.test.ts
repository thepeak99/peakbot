import { describe, it, expect } from "vitest";
import { nextEpoch, epochKey } from "./transcriptEpoch";
import type { EpochState } from "./transcriptEpoch";

function state(id: string, count: number, epoch: number): EpochState {
  return { id, count, epoch };
}

describe("nextEpoch", () => {
  // Initial state: a fresh EpochState with epoch 0. Switching view/conversation
  // bumps the epoch regardless of count — the new id forces a remount.
  it("bumps the epoch when the id changes (view/conversation switch)", () => {
    const prev = state("alpha", 5, 3);
    const next = nextEpoch(prev, "beta", 0);
    expect(next).toEqual({ id: "beta", count: 0, epoch: 4 });
  });

  // Same id, count shrank — the upstream transcript was truncated or swapped
  // to a shorter one. The virtualised list cannot reconcile this in place, so
  // the epoch must advance.
  it("bumps the epoch when the same id reports a smaller count (truncation)", () => {
    const prev = state("alpha", 10, 2);
    const next = nextEpoch(prev, "alpha", 7);
    expect(next).toEqual({ id: "alpha", count: 7, epoch: 3 });
  });

  // Same id, same count — nothing actually changed. nextEpoch must return the
  // SAME object (reference equality) so React's downstream memoisation can
  // short-circuit on referential identity.
  it("returns the same object reference when id and count are unchanged", () => {
    const prev = state("alpha", 10, 2);
    expect(nextEpoch(prev, "alpha", 10)).toBe(prev);
  });

  // Same id, count grew — normal append. Epoch stays put; only count moves.
  it("keeps the epoch and updates count when the same id grew", () => {
    const prev = state("alpha", 10, 2);
    const next = nextEpoch(prev, "alpha", 11);
    expect(next).toEqual({ id: "alpha", count: 11, epoch: 2 });
    // Reference inequality is fine here — a new object with the new count is
    // expected. Only the no-op case above requires reference preservation.
    expect(next).not.toBe(prev);
  });

  // Same id, count grew from zero — first message on this view. Epoch stays
  // at its current value (no truncation, no switch). Regression guard for the
  // off-by-one where count > prev.count would be mistaken for a switch.
  it("keeps the epoch on the first append (count grows from 0 to 1)", () => {
    const prev = state("alpha", 0, 0);
    const next = nextEpoch(prev, "alpha", 1);
    expect(next).toEqual({ id: "alpha", count: 1, epoch: 0 });
  });

  // Id change combined with a smaller count: still treated as a switch
  // (epoch bumps). The truncation branch must NOT pre-empt the id-change
  // branch even when count would also have shrunk.
  it("treats an id change with a smaller count as a switch (id wins)", () => {
    const prev = state("alpha", 10, 1);
    const next = nextEpoch(prev, "beta", 3);
    expect(next.epoch).toBe(2);
    expect(next.id).toBe("beta");
    expect(next.count).toBe(3);
  });
});

describe("epochKey", () => {
  it("formats as `${id}#${epoch}`", () => {
    expect(epochKey(state("alpha", 10, 0))).toBe("alpha#0");
    expect(epochKey(state("junior#1", 42, 7))).toBe("junior#1#7");
  });

  it("ignores count (only id and epoch participate in the key)", () => {
    // Same id+epoch with different counts must collide on the same key —
    // otherwise the epoch boundary would fragment unnecessarily.
    const a = state("alpha", 5, 3);
    const b = state("alpha", 99, 3);
    expect(epochKey(a)).toBe(epochKey(b));
  });
});
