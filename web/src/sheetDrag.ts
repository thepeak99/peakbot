// Pure detent / drag math for the Panels sheet (mobile-redesign design §3).
// No React, no DOM: the whole drag story is unit-testable as functions, so
// the RTL drag cases in Sheet.test.tsx only exercise wiring. `vh` is the
// viewport height (window.innerHeight), read by the caller.

export const MEDIUM = 0.55;
export const LARGE = 0.92;
/** Below this height a release dismisses the sheet. */
export const CLOSE_BELOW_PX = 130;
/** A flick needs BOTH a downward delta above FLICK_MIN_DY and a speed
 * above FLICK_PX_PER_MS (px/ms). */
export const FLICK_PX_PER_MS = 0.55;
export const FLICK_MIN_DY = 36;

/** The pixel height of a detent: 55% / 92% of the viewport, rounded. */
export function detentHeight(which: "medium" | "large", vh: number): number {
  const frac = which === "medium" ? MEDIUM : LARGE;
  return Math.round(vh * frac);
}

/** The live height while dragging: start minus the pointer delta, clamped
 * to [0, large]. Up is a negative delta. */
export function dragHeight(startH: number, dy: number, vh: number): number {
  const h = startH - dy;
  return Math.max(0, Math.min(detentHeight("large", vh), h));
}

/** Where a release settles: "close" below CLOSE_BELOW_PX or on a fast
 * downward flick; else "medium" below the (medium+large)/2 midpoint,
 * "large" at or above it. */
export function settleDrag(p: {
  h: number;
  dy: number;
  dtMs: number;
  vh: number;
}): "close" | "medium" | "large" {
  const { h, dy, dtMs, vh } = p;
  if (h < CLOSE_BELOW_PX) return "close";
  if (dy > FLICK_MIN_DY && dy / dtMs > FLICK_PX_PER_MS) return "close";
  const midpoint = (detentHeight("medium", vh) + detentHeight("large", vh)) / 2;
  return h < midpoint ? "medium" : "large";
}
