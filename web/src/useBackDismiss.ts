// The OS Back gesture dismisses an overlay instead of leaving the page
// (at the SPA root, Back would exit the app). One invariant: while
// `open`, exactly one history entry exists above the baseline —
//   * opening pushes it (from the render after the open state lands);
//   * a popstate while open closes the overlay — the user's traversal
//     already consumed the entry, so no back() of our own;
//   * a close that is NOT the popstate (UI button, Esc) issues the one
//     `history.back()` that consumes it.
// `close` must be stable for the open's duration (a new identity would
// push a second entry).

import { useEffect, useRef } from "react";

export function useBackDismiss(open: boolean, close: () => void): void {
  const closedByPop = useRef(false);

  useEffect(() => {
    if (!open) return;
    closedByPop.current = false;
    // Keep the current URL (incl. `?convo=`): the entry is a marker only,
    // and the convo updates rewrite it in place with replaceState.
    window.history.pushState(window.history.state, "");
    const onPop = () => {
      closedByPop.current = true;
      close();
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      if (!closedByPop.current) window.history.back();
    };
  }, [open, close]);
}
