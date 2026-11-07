// Static contract (mobile plan §2.5, task F3): `web/index.html`'s
// viewport meta content is EXACTLY the locked string —
// `viewport-fit=cover` is what lets the SPA read the safe-area insets
// (notch / Dynamic Island / home indicator) and
// `interactive-widget=resizes-content` keeps the composer above the
// virtual keyboard on Android.
//
// RED today: the meta lacks `viewport-fit=cover`.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

// Vitest stubs HTML/CSS imports, so read the file from disk (same approach
// as convoTabBar.css.test.ts).
const INDEX_HTML = decodeURIComponent(new URL("../index.html", import.meta.url).pathname);

const VIEWPORT_CONTENT =
  "width=device-width, initial-scale=1.0, viewport-fit=cover, interactive-widget=resizes-content";

function viewportContent(html: string): string | null {
  const m = html.match(/<meta\s+name="viewport"\s+content="([^"]*)"\s*\/?\s*>/);
  return m ? m[1] : null;
}

describe("web/index.html viewport meta (F3)", () => {
  it("the viewport meta content is exactly the locked §2.5 string", () => {
    const html = readFileSync(INDEX_HTML, "utf8");
    expect(viewportContent(html)).toBe(VIEWPORT_CONTENT);
  });
});
