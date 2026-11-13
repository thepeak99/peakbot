// Tests for `sheetDrag.ts` — the pure detent math (mobile-redesign design
// §3, P4). No React, no DOM: the whole drag story must be unit-testable as
// functions so the RTL drag cases in Sheet.test.tsx only exercise wiring.
//
// RED before implementation: `./sheetDrag` does not exist yet, so this file
// fails at import — the right reason (feature absent).
//
// The numbers are the locked spec (design §3): `vh = 844` → medium 464px,
// large 776px; close below 130px or a flick (dy > 36 AND dy/dt > 0.55 px/ms);
// settle midpoint (medium + large) / 2 = 620 at 844.

import { describe, expect, it } from "vitest";
import {
  CLOSE_BELOW_PX,
  FLICK_MIN_DY,
  FLICK_PX_PER_MS,
  LARGE,
  MEDIUM,
  detentHeight,
  dragHeight,
  settleDrag,
} from "./sheetDrag";

// ─── constants ────────────────────────────────────────────────────────────

describe("sheetDrag — constants", () => {
  it("are the design §3 thresholds", () => {
    expect(MEDIUM).toBe(0.55);
    expect(LARGE).toBe(0.92);
    expect(CLOSE_BELOW_PX).toBe(130);
    expect(FLICK_PX_PER_MS).toBe(0.55);
    expect(FLICK_MIN_DY).toBe(36);
  });
});

// ─── detentHeight ─────────────────────────────────────────────────────────

describe("detentHeight", () => {
  it("is 55% / 92% of the viewport, rounded", () => {
    expect(detentHeight("medium", 844)).toBe(464); // 464.2 → 464
    expect(detentHeight("large", 844)).toBe(776); // 776.48 → 776
  });

  it("scales with the viewport height", () => {
    expect(detentHeight("medium", 768)).toBe(Math.round(768 * 0.55)); // 422
    expect(detentHeight("large", 768)).toBe(Math.round(768 * 0.92)); // 707
  });

  it("rounds half-up at the boundary", () => {
    // 845 * 0.55 = 464.75 → 465; 843 * 0.55 = 463.65 → 464.
    expect(detentHeight("medium", 845)).toBe(465);
    expect(detentHeight("medium", 843)).toBe(464);
  });
});

// ─── dragHeight ───────────────────────────────────────────────────────────

describe("dragHeight", () => {
  it("subtracts the drag delta from the starting height (up is negative dy)", () => {
    expect(dragHeight(464, -300, 844)).toBe(764);
    expect(dragHeight(464, 100, 844)).toBe(364);
  });

  it("clamps to 0 at the bottom", () => {
    expect(dragHeight(464, 464, 844)).toBe(0);
    expect(dragHeight(464, 800, 844)).toBe(0);
  });

  it("clamps to the large detent at the top", () => {
    expect(dragHeight(464, -300, 844)).toBe(764);
    expect(dragHeight(464, -400, 844)).toBe(776); // 864 → clamped to 776
    expect(dragHeight(776, -10, 844)).toBe(776);
  });
});

// ─── settleDrag ───────────────────────────────────────────────────────────

describe("settleDrag", () => {
  // vh 844: medium 464, large 776, midpoint 620.

  it("closes when the height is below the close threshold", () => {
    expect(settleDrag({ h: 0, dy: 464, dtMs: 1000, vh: 844 })).toBe("close");
    expect(settleDrag({ h: 129, dy: 400, dtMs: 1000, vh: 844 })).toBe("close");
  });

  it("does NOT close at exactly the close threshold (the > comparison)", () => {
    // h 130, a slow drag: settles, it does not close.
    expect(settleDrag({ h: 130, dy: 334, dtMs: 1000, vh: 844 })).toBe("medium");
  });

  it("closes on a fast downward flick above the minimum delta", () => {
    // dy 40 in 50 ms → 0.8 px/ms > 0.55: closes even at a tall height.
    expect(settleDrag({ h: 424, dy: 40, dtMs: 50, vh: 844 })).toBe("close");
    // dy 56 in 100 ms → 0.56 px/ms > 0.55.
    expect(settleDrag({ h: 408, dy: 56, dtMs: 100, vh: 844 })).toBe("close");
  });

  it("a slow drag past the minimum delta does not close", () => {
    // dy 400 in 1000 ms → 0.4 px/ms: below the flick speed.
    expect(settleDrag({ h: 64, dy: 400, dtMs: 1000, vh: 844 })).toBe("close"); // h 64 < 130
    // Same drag from a taller start: settles to medium, not close.
    expect(settleDrag({ h: 400, dy: 64, dtMs: 1000, vh: 844 })).toBe("medium");
  });

  it("the flick needs BOTH the minimum delta and the speed", () => {
    // dy 36 in 10 ms → 3.6 px/ms, but dy is not > 36: no flick.
    expect(settleDrag({ h: 428, dy: 36, dtMs: 10, vh: 844 })).toBe("medium");
    // dy 37 in 68 ms → 0.544 px/ms, below 0.55: no flick.
    expect(settleDrag({ h: 427, dy: 37, dtMs: 68, vh: 844 })).toBe("medium");
    // dy 37 in 67 ms → 0.552 px/ms > 0.55: flick closes.
    expect(settleDrag({ h: 427, dy: 37, dtMs: 67, vh: 844 })).toBe("close");
  });

  it("settles to medium below the midpoint and large above it", () => {
    // Midpoint at 844 is (464 + 776) / 2 = 620.
    expect(settleDrag({ h: 619, dy: 155, dtMs: 1000, vh: 844 })).toBe("medium");
    expect(settleDrag({ h: 464, dy: 0, dtMs: 1000, vh: 844 })).toBe("medium");
    expect(settleDrag({ h: 620, dy: 156, dtMs: 1000, vh: 844 })).toBe("large");
    expect(settleDrag({ h: 764, dy: -300, dtMs: 1000, vh: 844 })).toBe("large");
    expect(settleDrag({ h: 776, dy: -312, dtMs: 1000, vh: 844 })).toBe("large");
  });

  it("the midpoint scales with the viewport", () => {
    // vh 768: medium 422, large 707, midpoint 564.5.
    expect(settleDrag({ h: 564, dy: 0, dtMs: 1000, vh: 768 })).toBe("medium");
    expect(settleDrag({ h: 565, dy: 0, dtMs: 1000, vh: 768 })).toBe("large");
  });

  it("a zero-length drag at the opening detent settles back to it", () => {
    expect(settleDrag({ h: 464, dy: 0, dtMs: 1, vh: 844 })).toBe("medium");
  });
});
