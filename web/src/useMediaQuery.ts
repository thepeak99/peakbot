// Reactive `matchMedia` hook. Returns `true` while the media query matches.
//
// SSR-safe: starts `false` and hydrates on mount, so the first client render
// matches what the server emitted (avoids hydration mismatches). The listener
// is attached once per query string, cleaned up on unmount.

import { useEffect, useState } from "react";

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(false);

  useEffect(() => {
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
