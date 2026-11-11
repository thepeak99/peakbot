// Telegram-style slide + fade for a mobile overlay: the screen
// parks 48 px toward its edge over 180 ms, and the unmount is held (inert)
// through the WAAPI exit until `finished` settles.

import { useLayoutEffect, useState, type RefObject } from "react";
import { isMobile } from "./desktop";

export type SlideState = "shown" | "leaving" | "gone";

const DURATION_MS = 180;
const EASING = "cubic-bezier(0.215, 0.61, 0.355, 1)"; // easeOutCubic
// CSS px are density-independent (= dp in the WebView): 48 px is the same
// physical distance on every phone.
const SLIDE_PX = 48;

/** The motion policy, read per transition (no listener, no state). */
function motionAllowed(): boolean {
  return (
    isMobile() &&
    typeof Element.prototype.animate === "function" &&
    !window.matchMedia?.("(prefers-reduced-motion: reduce)").matches
  );
}

/** Telegram-style slide for a mobile overlay. `side` is the edge it parks
 *  toward. Instant (shown ⇄ gone) off the mobile shell, without WAAPI, or
 *  under prefers-reduced-motion. */
export function useScreenSlide(
  shown: boolean,
  ref: RefObject<HTMLElement | null>,
  side: "left" | "right",
): SlideState {
  const [state, setState] = useState<SlideState>(shown ? "shown" : "gone");
  // Decided during render (React's adjust-state pattern): a layout effect
  // would run after the commit that already unmounted the overlay.
  if (shown && state !== "shown") setState("shown");
  if (!shown && state === "shown") setState(motionAllowed() ? "leaving" : "gone");

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const parked = `translateX(${side === "left" ? -SLIDE_PX : SLIDE_PX}px)`;
    const home = "translateX(0px)";
    if (state === "leaving") {
      // fill "forwards" holds the end frame until the unmount commit.
      const anim = el.animate(
        [{ opacity: 1, transform: home }, { opacity: 0, transform: parked }],
        { duration: DURATION_MS, easing: EASING, fill: "forwards" },
      );
      // A rejection means the exit was cancelled (re-shown, unmount): the
      // newer state owns the overlay, so nothing to release.
      anim.finished.then(
        () => setState((s) => (s === "leaving" ? "gone" : s)),
        () => undefined,
      );
      return () => anim.cancel();
    }
    if (!motionAllowed()) return;
    // No fill: the layout effect runs before paint, so the first painted
    // frame is the animation's first frame.
    const anim = el.animate(
      [{ opacity: 0, transform: parked }, { opacity: 1, transform: home }],
      { duration: DURATION_MS, easing: EASING },
    );
    return () => anim.cancel();
  }, [state, ref, side]);

  return state;
}
