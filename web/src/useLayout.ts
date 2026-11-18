// The single layout decision seam.
//
// Replaces the old split where JS (isDesktopShell || min-width:1024) and CSS
// (Tailwind's large-breakpoint viewport rules) disagreed — causing the 40px rail to
// overlap transcript/composer in a narrow desktop window.
//
// New rule: phone layout ONLY for the mobile shell (window.peakbotShell ===
// "mobile") or a browser where both the viewport is narrow AND the pointer
// is coarse (a real phone). Everything else — including a narrow desktop
// window and a desktop app at any width — is desktop layout.
//
// This hook mirrors the decision onto <html data-layout> so Tailwind's
// @custom-variant phone and desktop selectors follow it, replacing all
// old viewport-width layout classes.

import { useLayoutEffect } from "react";
import { shell, type Shell } from "./desktop";
import { useMediaQuery } from "./useMediaQuery";

export type Layout = "phone" | "desktop";

/** A phone *browser*: narrow AND touch-first. Desktop at any width fails this. */
export const PHONE_BROWSER_QUERY = "(max-width: 1023px) and (pointer: coarse)";

/** Pure decision matrix: shell + phone viewport → layout. */
export function layoutOf(sh: Shell, phoneBrowserViewport: boolean): Layout {
  if (sh === "mobile") return "phone";
  if (sh === "browser" && phoneBrowserViewport) return "phone";
  return "desktop";
}

/**
 * Call ONCE, in App. Computes the layout from shell() + PHONE_BROWSER_QUERY,
 * subscribes to the query, and mirrors the decision onto
 * document.documentElement.dataset.layout so CSS @custom-variant phone/desktop
 * follow it. Returns the layout string.
 */
export function useLayout(): Layout {
  const phoneViewport = useMediaQuery(PHONE_BROWSER_QUERY);
  const sh = shell();
  const layout = layoutOf(sh, phoneViewport);

  useLayoutEffect(() => {
    document.documentElement.dataset.layout = layout;
    return () => {
      delete document.documentElement.dataset.layout;
    };
  }, [layout]);

  return layout;
}
