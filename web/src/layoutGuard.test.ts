// Layout class migration guard (N4).
//
// The old layout split used lg:/max-lg: viewport breakpoints that disagreed
// with the JS layout decision. The new design unifies layout under
// html[data-layout="phone"] vs html[data-layout="desktop"] via Tailwind's
// @custom-variant phone and @custom-variant desktop.
//
// This guard ensures:
// - Zero `max-lg:` classes remain in web/src non-test sources
// - `lg:` classes only survive in the allow-listed files (TopBar sessions
//   trio, BottomBar close button, settings providers grid)
// - index.css has no `@media (width < 64rem)` block (replaced by layout attr)
// - index.css defines both @custom-variant phone and @custom-variant desktop
//   keyed on data-layout
//
// RED before implementation: the CSS variants don't exist yet, so the
// in-block assertions fail on the missing block (feature absent).

import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const WEB_SRC = join(__dirname, "..", "src");

/** Recursively read directory, return list of file paths. */
function listFiles(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, name.name);
    if (name.isDirectory()) {
      out.push(...listFiles(full, exts));
    } else if (exts.some((e) => name.name.endsWith(e))) {
      out.push(full);
    }
  }
  return out;
}

/** Read all non-test .ts/.tsx files in web/src. */
function sourceFiles(): string[] {
  return listFiles(WEB_SRC, [".ts", ".tsx"])
    .filter((f) => !f.includes(".test.") && !f.includes(".spec."));
}

// Allowed files that can still use `lg:` (per the design: TopBar sessions
// trio, BottomBar close button, settings providers grid)
const LG_ALLOWLIST = new Set([
  "TopBar.tsx",
  "BottomBar.tsx",
  "settings/providers.tsx",
  "useLayout.ts", // comments reference the old breakpoint names
]);

// ═══ N4a: zero max-lg: in non-test sources ════════════════════════════════

describe("layout guard — N4a: zero max-lg: in non-test sources", () => {
  it("no file uses max-lg: classes", () => {
    const offenders: { file: string; line: number; text: string }[] = [];

    for (const f of sourceFiles()) {
      const rel = f.replace(WEB_SRC, "").slice(1);
      // useLayout.ts references the old breakpoint names in comments
      if (rel.endsWith("useLayout.ts")) continue;

      const content = readFileSync(f, "utf8");
      const lines = content.split("\n");
      lines.forEach((line, i) => {
        if (/\bmax-lg:/i.test(line)) {
          offenders.push({ file: rel, line: i + 1, text: line.trim() });
        }
      });
    }

    expect(offenders, "found max-lg: usage (replaced by phone: variant)").toEqual([]);
  });
});

// ═══ N4b: lg: only in allow-listed files ══════════════════════════════════

describe("layout guard — N4b: lg: only in allow-listed files", () => {
  it("lg: classes appear only in the allow-listed files", () => {
    const offenders: { file: string; line: number; text: string }[] = [];

    for (const f of sourceFiles()) {
      const rel = f.replace(WEB_SRC, "").slice(1);
      // Skip allow-listed files
      if ([...LG_ALLOWLIST].some((allowed) => rel.endsWith(allowed))) continue;

      const content = readFileSync(f, "utf8");
      const lines = content.split("\n");
      lines.forEach((line, i) => {
        if (/\blg:/i.test(line)) {
          offenders.push({ file: rel, line: i + 1, text: line.trim() });
        }
      });
    }

    expect(
      offenders,
      "found lg: usage outside the allow-list (replaced by desktop: variant)",
    ).toEqual([]);
  });
});

// ═══ N4c: index.css has no 64rem media query ═════════════════════════════

describe("layout guard — N4c: index.css layout media query removed", () => {
  const css = () => readFileSync(join(WEB_SRC, "index.css"), "utf8");
  const stripped = () => css().replace(/\/\*[\s\S]*?\*\//g, "");

  it("no @media (width < 64rem) block in index.css", () => {
    expect(stripped(), "the layout-scoping media query is replaced by data-layout")
      .not.toMatch(/@media\s*\(\s*width\s*<\s*64rem\s*\)/);
  });
});

// ═══ N4d: @custom-variant phone and desktop defined ══════════════════════

describe("layout guard — N4d: @custom-variant phone and desktop", () => {
  const css = () => readFileSync(join(WEB_SRC, "index.css"), "utf8");
  const stripped = () => css().replace(/\/\*[\s\S]*?\*\//g, "");

  it("defines @custom-variant phone keyed on data-layout=phone", () => {
    expect(stripped()).toMatch(/@custom-variant\s+phone\s*\(/);
    expect(stripped()).toMatch(/phone[\s\S]*?\[data-layout="phone"\]/);
  });

  it("defines @custom-variant desktop keyed on data-layout=desktop", () => {
    expect(stripped()).toMatch(/@custom-variant\s+desktop\s*\(/);
    expect(stripped()).toMatch(/desktop[\s\S]*?\[data-layout="desktop"\]/);
  });
});
