// Batch 5 light theme (fix-plan items 5.10/5.11, A41/B34, B35/C18, R1, R4):
// the phone light-colour remap — pure-white background, sky-400 #0369a1 and
// emerald-400 #047857 accents — lives in ONE `@media (width < 64rem)` block
// (64rem = 1024px, so desktop light keeps its tokens, R1) and restores the
// ORIGINAL values inside `[data-testid="settings-view"]` (Settings is
// untouched, R4).
//
// jsdom never loads index.css, so — like indexCss.batch1.test.ts and
// convoTabBar.css.test.ts — the file is read as TEXT and the declarations
// are pinned down.
//
// RED before implementation: the `@media (width < 64rem)` block does not
// exist in index.css yet, so the in-block assertions fail on the missing
// block (feature absent). The two leak guards already pass and must keep
// passing: the remap must not land anywhere else.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

// Vitest stubs CSS imports to "" (even with ?raw), so read the file directly.
const css = readFileSync(decodeURIComponent(new URL("./index.css", import.meta.url).pathname), "utf8");
const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The phone-scoping media query from items 5.10/5.11. */
const MOBILE_MEDIA = /@media\s*\(\s*width\s*<\s*64rem\s*\)/;

/** Body of the first block opened by `marker` (brace-matched). */
function blockAfter(marker: RegExp): string | null {
  const m = marker.exec(stripped);
  if (!m) return null;
  const open = stripped.indexOf("{", m.index);
  let depth = 0;
  for (let i = open; i < stripped.length; i++) {
    if (stripped[i] === "{") depth++;
    else if (stripped[i] === "}") {
      depth--;
      if (depth === 0) return stripped.slice(open + 1, i);
    }
  }
  return null;
}

/** `stripped` with the first block opened by `marker` removed (with braces). */
function withoutBlock(marker: RegExp): string {
  const m = marker.exec(stripped);
  if (!m) return stripped;
  const open = stripped.indexOf("{", m.index);
  let depth = 0;
  for (let i = open; i < stripped.length; i++) {
    if (stripped[i] === "{") depth++;
    else if (stripped[i] === "}") {
      depth--;
      if (depth === 0) return stripped.slice(0, m.index) + stripped.slice(i + 1);
    }
  }
  return stripped;
}

/** Body of the first rule whose selector is exactly `selector`, in `scope`.
 *  The boundary accepts `;` so a rule right after an `@import` matches too. */
function ruleIn(scope: string | null, selector: string): string | null {
  if (scope === null) return null;
  const m = new RegExp(`(?:^|[};])\\s*${esc(selector)}\\s*\\{([^{}]*)\\}`).exec(scope);
  return m ? m[1] : null;
}

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

describe("index.css — batch 5 mobile light remap (items 5.10/5.11)", () => {
  const mobileLight = () => blockAfter(MOBILE_MEDIA);
  const light = () => ruleIn(mobileLight(), "html.light");
  const settings = () => ruleIn(mobileLight(), 'html.light [data-testid="settings-view"]');

  it("the remap sits inside @media (width < 64rem), scoped below 1024px (R1)", () => {
    expect(mobileLight(), "the phone-scoped @media block must exist").not.toBeNull();
    expect(light(), "the html.light remap must live inside it").not.toBeNull();
  });

  it("the phone light background is pure white (A41/B34)", () => {
    expect(
      decl(light(), "--color-zinc-950"),
      "zinc-950 (the app background) remaps to #ffffff on the phone only",
    ).toBe("#ffffff");
  });

  it("the phone light accents remap sky-400 → #0369a1 and emerald-400 → #047857 (B35/C18)", () => {
    expect(decl(light(), "--color-sky-400"), "the mock's light --link").toBe("#0369a1");
    expect(decl(light(), "--color-emerald-400"), "the mock's light chip/dot green").toBe("#047857");
  });

  it("Settings keeps the original colours: the restore rule sits in the same media block (R4)", () => {
    const body = settings();
    expect(
      body,
      'html.light [data-testid="settings-view"] must restore the originals inside the media block',
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

describe("index.css — the remap never leaks outside the phone media block (R1)", () => {
  it("no --color-sky-400 / --color-emerald-400 definition outside @media (width < 64rem)", () => {
    const outside = withoutBlock(MOBILE_MEDIA);
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
