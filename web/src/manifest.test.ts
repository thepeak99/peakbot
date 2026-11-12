// PR-A test T20 (/tmp/peakbot/design/notify-pr-a.md §7): the PWA manifest
// exists, is valid, is standalone, and index.html links it with
// `crossorigin="use-credentials"` (browsers fetch manifests WITHOUT cookies by
// default, which a token-gated server would answer with 401).
//
// RED today: `web/public/manifest.webmanifest` does not exist and index.html
// has no manifest link.

import { readFileSync, statSync } from "node:fs";
import { describe, expect, it } from "vitest";

// Vitest stubs HTML/CSS imports, so read from disk (same approach as
// indexHtmlViewport.test.ts).
const PUBLIC_DIR = decodeURIComponent(new URL("../public/", import.meta.url).pathname);
const MANIFEST = decodeURIComponent(
  new URL("../public/manifest.webmanifest", import.meta.url).pathname,
);
const INDEX_HTML = decodeURIComponent(new URL("../index.html", import.meta.url).pathname);

interface Manifest {
  name?: unknown;
  short_name?: unknown;
  start_url?: unknown;
  display?: unknown;
  icons?: unknown;
}

function loadManifest(): Manifest {
  return JSON.parse(readFileSync(MANIFEST, "utf8")) as Manifest;
}

function fileExists(path: string): boolean {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

describe("web/public/manifest.webmanifest", () => {
  it("T20 exists and is valid JSON", () => {
    expect(fileExists(MANIFEST)).toBe(true);
    expect(() => loadManifest()).not.toThrow();
  });

  it("T20 is a standalone app named PeakBot starting at /", () => {
    const m = loadManifest();
    expect(m.display).toBe("standalone");
    expect(m.name).toBe("PeakBot");
    expect(m.start_url).toBe("/");
  });

  it("T20 has a non-empty icon list whose files all exist under web/public", () => {
    const m = loadManifest();
    expect(Array.isArray(m.icons)).toBe(true);
    const icons = m.icons as { src?: unknown; sizes?: unknown; type?: unknown }[];
    expect(icons.length).toBeGreaterThan(0);
    for (const icon of icons) {
      expect(typeof icon.src).toBe("string");
      const src = icon.src as string;
      expect(src.startsWith("/")).toBe(true);
      expect(fileExists(PUBLIC_DIR + decodeURIComponent(src.slice(1)))).toBe(true);
    }
  });
});

describe("web/index.html manifest link", () => {
  it("T20 links the manifest with crossorigin=use-credentials", () => {
    const html = readFileSync(INDEX_HTML, "utf8");
    expect(html).toMatch(
      /<link\s+rel="manifest"\s+href="\/manifest\.webmanifest"\s+crossorigin="use-credentials"\s*\/?>/,
    );
  });
});
