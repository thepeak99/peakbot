// The OS Back gesture dismisses an overlay instead of leaving the page
// (at the SPA root, Back would exit the app). backStack arbitrates, so
// nested overlays never both react to one popstate. Keep `close` stable
// while open: a new identity releases the marker and pushes a fresh one.

import { useEffect } from "react";
import { pushBackEntry } from "./backStack";

export function useBackDismiss(open: boolean, close: () => void): void {
  useEffect(() => (open ? pushBackEntry(close) : undefined), [open, close]);
}
