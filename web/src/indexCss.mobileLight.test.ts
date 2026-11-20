// Batch 5 light theme (fix-plan items 5.10/5.11, A41/B34, B35/C18, R1, R4):
// the phone light-colour remap — pure-white background, sky-400 #0369a1 and
// emerald-400 #047857 accents — lives in `html.light[data-layout="phone"]`
// (keyed on the unified layout decision, not viewport) and restores the
// ORIGINAL values inside `[data-testid="settings-view"]` (Settings is
// untouched, R4).
//
// jsdom never loads index.css, so — like indexCss.batch1.test.ts and
// convoTabBar.css.test.ts — the file is read as TEXT and the declarations
// are pinned down.
//
// The phone layout uses the data-layout attribute (useLayout.ts) rather than
// a viewport media query, so the remap must live in [data-layout="phone"]
// rules and never in the global html.light block.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

// Vitest stubs CSS imports to "" (even with ?raw), so read the file directly.
const css = readFileSync(decodeURIComponent(new URL("./index.css", import.meta.url).pathname), "utf8");
const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The phone-scoping selector from the unified layout design. */

/** Body of the first rule whose selector is exactly `selector`. */
function ruleBody(selector: string): string | null {
  const m = new RegExp(`${esc(selector)}\\s*\\{([^{}]*)\\}`).exec(stripped);
  return m ? m[1] : null;
}

/** Body of the first rule whose selector is exactly `selector`, in `scope`.
 *  The boundary accepts `;` so a rule right after an `@import` matches too. */

/** Bodies of ALL rules whose selector is exactly `selector`, in `scope`. */
function rulesIn(scope: string, selector: string): string[] {
  const re = new RegExp(`(?:^|[};])\\s*${esc(selector)}\\s*\\{([^{}]*)\\}`, "g");
  const out: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(scope))) out.push(m[1]);
  return out;
}

/** The unscoped `html.light` block — the one that remaps the zinc ramp
 *  (index.css carries a second, profile-palette `html.light` block). */
function globalLight(): string | null {
  return rulesIn(stripped, "html.light").find((b) => b.includes("--color-zinc-950")) ?? null;
}

/** Value of a declaration inside a rule body, whitespace-normalised. */
function decl(body: string | null, prop: string): string | null {
  if (body === null) return null;
  const m = new RegExp(`(?:^|[;\\s])${esc(prop)}\\s*:\\s*([^;]+)`).exec(body);
  return m ? m[1].trim().replace(/\s+/g, " ") : null;
}

/** oklch components, lightness normalised (0.746 ≡ 74.6% ≡ 74.6). */
function oklch(v: string | null): { l: string; c: string; h: string } | null {
  const m = /oklch\(\s*([0-9.]+%?)\s+([0-9.]+)\s+([0-9.]+)\s*\)/.exec(v ?? "");
  if (!m) return null;
  let l = m[1];
  if (l.endsWith("%")) l = l.slice(0, -1);
  if (parseFloat(l) <= 1) l = (parseFloat(l) * 100).toString();
  return { l, c: m[2], h: m[3] };
}

describe("index.css — phone light remap (items 5.10/5.11)", () => {
  const phoneLight = () => ruleBody('html.light[data-layout="phone"]');
  const phoneSettings = () => ruleBody('html.light[data-layout="phone"] [data-testid="settings-view"]');

  it("the remap lives in html.light[data-layout=\"phone\"] (unified layout decision)", () => {
    expect(phoneLight(), "the phone-scoped remap must exist").not.toBeNull();
  });

  it("the phone light background is pure white (A41/B34)", () => {
    expect(
      decl(phoneLight(), "--color-zinc-950"),
      "zinc-950 (the app background) remaps to #ffffff on the phone only",
    ).toBe("#ffffff");
  });

  it("the phone light accents remap sky-400 → #0369a1 and emerald-400 → #047857 (B35/C18)", () => {
    expect(decl(phoneLight(), "--color-sky-400"), "the mock's light --link").toBe("#0369a1");
    expect(decl(phoneLight(), "--color-emerald-400"), "the mock's light chip/dot green").toBe("#047857");
  });

  it("Settings keeps the original colours: the restore rule is nested under [data-layout=\"phone\"] (R4)", () => {
    const body = phoneSettings();
    expect(
      body,
      'html.light[data-layout="phone"] [data-testid="settings-view"] must restore the originals',
    ).not.toBeNull();
    expect(
      decl(body, "--color-zinc-950"),
      "zinc-950 restores the global light original",
    ).toBe(decl(globalLight(), "--color-zinc-950"));
    // The Tailwind originals, as locked in item 5.11.
    expect(oklch(decl(body, "--color-sky-400"))).toEqual({ l: "74.6", c: "0.16", h: "232.661" });
    expect(oklch(decl(body, "--color-emerald-400"))).toEqual({ l: "76.5", c: "0.177", h: "163.223" });
  });
});

describe("index.css — the remap never leaks outside [data-layout=\"phone\"] (R1)", () => {
  it("no --color-sky-400 / --color-emerald-400 definition outside [data-layout=\"phone\"] rules", () => {
    // Remove the two phone-specific rules
    const outside = stripped
      .replace(/html\.light\[data-layout="phone"\]\s*\{[^}]*\}/g, "")
      .replace(/html\.light\[data-layout="phone"\]\s*\[data-testid="settings-view"\]\s*\{[^}]*\}/g, "");
    expect(
      outside,
      "the sky-400 accent remap must not touch the desktop light theme",
    ).not.toMatch(/--color-sky-400\s*:/);
    expect(
      outside,
      "the emerald-400 accent remap must not touch the desktop light theme",
    ).not.toMatch(/--color-emerald-400\s*:/);
  });

  it("the global html.light block keeps its original zinc-950 (the remap is additive)", () => {
    expect(
      decl(globalLight(), "--color-zinc-950"),
      "the unscoped light background stays the zinc ramp remap",
    ).toBe("oklch(98.5% 0 0)");
  });
});
