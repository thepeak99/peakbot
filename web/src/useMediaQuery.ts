// Reactive `matchMedia` hook. Returns `true` while the media query matches.
//
// The initial value is read from matchMedia during the first render (lazy
// useState), so a desktop-width first paint doesn't flash the mobile layout
// for a frame. A missing matchMedia (jsdom, very old engines) falls back to
// false. The listener is attached once per query string, cleaned up on
// unmount.

import { useEffect, useState } from "react";

const initialMatches = (query: string): boolean =>
  typeof window.matchMedia === "function" ? window.matchMedia(query).matches : false;

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => initialMatches(query));

  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia(query);
    setMatches(mql.matches);
    const onChange = (e: MediaQueryListEvent) => setMatches(e.matches);
    // `addEventListener` is the modern API; the deprecated `addListener`
    // shim still exists for Safari < 14 but is irrelevant for our targets.
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);

  return matches;
}
