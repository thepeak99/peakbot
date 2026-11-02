// CSS contract for the browser-style shrinking conversation tabs
// (fix/tab-shrink). jsdom has no layout and does not load index.css, so the
// shrink/degrade behaviour cannot be observed in a render test. Instead we
// read `index.css` as TEXT and pin down the declarations that produce it.
// Reference prototype: /tmp/peakbot/tabs-dummy/index.html.
//
//   .ctab         flex: 0 1 200px; width: 200px; min-width: 36px;
//                 container-type: inline-size
//   .ctab-scroll  flex: 0 1 auto; min-width: 0; overflow-x: auto
//   @container (max-width: A)  → title hidden        (tab OUTER width <= 79px)
//   @container (max-width: B)  → inactive tab ✕ hidden (OUTER width <= 47px), B < A
//   no .ctab-spacer / .ctab-count rules
//
// A container query measures the container's CONTENT box, so A and B must
// be 79 / 47 minus the tab's horizontal padding and border, which this test
// reads back out of the `.ctab` rule rather than hard-coding.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { MIN_TAB_PX, NAV_PADDING_PX, TAB_GAP_PX } from "./components/ConvoTabBar";

// Vitest stubs CSS imports to "" (even with ?raw), so read the file directly.
const css = readFileSync(decodeURIComponent(new URL("./index.css", import.meta.url).pathname), "utf8");
const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");

const esc = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Body of a top-level rule whose selector is exactly `selector`. */
function ruleBody(selector: string): string | null {
  const m = new RegExp(`(?:^|\\})\\s*${esc(selector)}\\s*\\{([^{}]*)\\}`).exec(stripped);
  return m ? m[1] : null;
}

/** Value of a declaration inside a rule body, whitespace-normalised. */
function decl(body: string | null, prop: string): string | null {
  if (body === null) return null;
  const m = new RegExp(`(?:^|[;\\s])${esc(prop)}\\s*:\\s*([^;]+)`).exec(body);
  return m ? m[1].trim().replace(/\s+/g, " ") : null;
}

interface ContainerBlock {
  maxWidth: number;
  rules: { selector: string; body: string }[];
}

/** Every `@container (max-width: Npx) { ... }` block with its inner rules. */
function containerBlocks(): ContainerBlock[] {
  const out: ContainerBlock[] = [];
  const re = /@container\s*(?:[\w-]+\s*)?\(\s*max-width\s*:\s*(\d+(?:\.\d+)?)px\s*\)\s*\{((?:[^{}]*\{[^{}]*\})*)\s*\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(stripped))) {
    const rules = [...m[2].matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((r) => ({
      selector: r[1].trim().replace(/\s+/g, " "),
      body: r[2],
    }));
    out.push({ maxWidth: parseFloat(m[1]), rules });
  }
  return out;
}

const hidesWithDisplayNone = (body: string) => /display\s*:\s*none/.test(body);

/** Horizontal padding + border of `.ctab`, in px. */
function horizontalChrome(): number {
  const body = ruleBody(".ctab");
  const px = (v: string | undefined) => (v ? parseFloat(v) || 0 : 0);
  let left = 0;
  let right = 0;
  const pad = decl(body, "padding");
  if (pad) {
    const p = pad.split(" ");
    const [t, r, b, l] = [p[0], p[1] ?? p[0], p[2] ?? p[0], p[3] ?? p[1] ?? p[0]];
    void t; void b;
    left = px(l);
    right = px(r);
  }
  left = decl(body, "padding-left") ? px(decl(body, "padding-left")!) : left;
  right = decl(body, "padding-right") ? px(decl(body, "padding-right")!) : right;
  const border = px(decl(body, "border")?.split(" ")[0]);
  return left + right + 2 * border;
}

describe("index.css — .ctab shrinks like Chrome tabs", () => {
  it("has a top-level .ctab rule", () => {
    expect(ruleBody(".ctab")).not.toBeNull();
  });

  it("declares flex: 0 1 200px (starts at 200px, shrinks evenly)", () => {
    expect(decl(ruleBody(".ctab"), "flex")).toBe("0 1 200px");
  });

  it("declares width: 200px (the strip's intrinsic size needs it under container-type)", () => {
    expect(decl(ruleBody(".ctab"), "width")).toBe("200px");
  });

  it("declares min-width: 36px (was 120px)", () => {
    expect(decl(ruleBody(".ctab"), "min-width")).toBe("36px");
  });

  it("declares container-type: inline-size so @container rules key off the tab's own width", () => {
    expect(decl(ruleBody(".ctab"), "container-type")).toBe("inline-size");
  });

  it("keeps the MIN_TAB_PX constant used by the overflow predicate in sync with min-width", () => {
    expect(`${MIN_TAB_PX}px`).toBe(decl(ruleBody(".ctab"), "min-width"));
  });

  it("keeps TAB_GAP_PX in sync with the strip's gap", () => {
    expect(decl(ruleBody(".ctab-scroll"), "gap")).toBe(`${TAB_GAP_PX}px`);
  });
});

describe("index.css — .ctab-scroll strip", () => {
  it("is flex: 0 1 auto with min-width: 0 and horizontal auto overflow", () => {
    const body = ruleBody(".ctab-scroll");
    expect(body).not.toBeNull();
    expect(decl(body, "flex")).toBe("0 1 auto");
    expect(["0", "0px"]).toContain(decl(body, "min-width"));
    expect(decl(body, "overflow-x")).toBe("auto");
  });
});

describe("index.css — removed chrome", () => {
  it("no longer styles a .ctab-spacer", () => {
    expect(stripped).not.toMatch(/\.ctab-spacer/);
  });

  it("no longer styles an 'N tabs' .ctab-count", () => {
    expect(stripped).not.toMatch(/\.ctab-count/);
  });
});

describe("index.css — progressive degradation via @container", () => {
  const blocks = () => containerBlocks();

  it("has at least two max-width @container blocks", () => {
    expect(blocks().length).toBeGreaterThanOrEqual(2);
  });

  it("hides .title (icon-only) when the tab's OUTER width is <= 79px", () => {
    const titleBlock = blocks().find((b) =>
      b.rules.some((r) => /\.title\b/.test(r.selector) && hidesWithDisplayNone(r.body)),
    );
    expect(titleBlock, "a @container block must `display: none` the .title").toBeDefined();
    expect(titleBlock!.maxWidth + horizontalChrome()).toBe(79);
  });

  it("hides the ✕ of INACTIVE tabs when the tab's OUTER width is <= 47px", () => {
    const xBlock = blocks().find((b) =>
      b.rules.some(
        (r) =>
          /\.x\b/.test(r.selector) &&
          /:not\(\.active\)/.test(r.selector) &&
          hidesWithDisplayNone(r.body),
      ),
    );
    expect(xBlock, "a @container block must hide `.ctab:not(.active) .x`").toBeDefined();
    expect(xBlock!.maxWidth + horizontalChrome()).toBe(47);
  });

  it("degrades title first, then ✕: the title threshold is wider than the ✕ threshold", () => {
    const t = blocks().find((b) =>
      b.rules.some((r) => /\.title\b/.test(r.selector) && hidesWithDisplayNone(r.body)),
    );
    const x = blocks().find((b) =>
      b.rules.some((r) => /\.x\b/.test(r.selector) && hidesWithDisplayNone(r.body)),
    );
    expect(t).toBeDefined();
    expect(x).toBeDefined();
    expect(t!.maxWidth).toBeGreaterThan(x!.maxWidth);
    expect(x!.maxWidth).toBeGreaterThan(0);
  });

  it("never hides the ACTIVE tab's ✕ at any width", () => {
    for (const b of blocks()) {
      for (const r of b.rules) {
        if (/\.x\b/.test(r.selector) && hidesWithDisplayNone(r.body)) {
          expect(r.selector, "every ✕-hiding selector must exclude .active").toMatch(
            /:not\(\.active\)/,
          );
        }
      }
    }
  });

  it("never hides the .icon at any width", () => {
    for (const b of blocks()) {
      for (const r of b.rules) {
        if (/\.icon\b/.test(r.selector)) {
          expect(hidesWithDisplayNone(r.body), `${r.selector} must stay visible`).toBe(false);
        }
      }
    }
  });
});

describe("index.css — .ctab .icon", () => {
  it("styles the icon as a fixed-size, non-shrinking element", () => {
    const body = ruleBody(".ctab .icon");
    expect(body, "a `.ctab .icon` rule must exist").not.toBeNull();
    expect(decl(body, "flex")).toBe("none");
  });
});

// ─── per-profile tab colour (remote-profiles design T2) ──────────────────
//
// The tab accent is a 2px top edge painted from `--ctab-accent` (set
// inline per tab by ConvoTabBar), at 60% strength when the tab is
// inactive. Each palette colour has a theme variable `--profile-<c>`
// defined in BOTH themes (dark is the default; light = `html.light`).
// RED before implementation: none of these declarations exist yet.

const PALETTE = ["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink"];

/** Every rule (top-level or nested) as `{selector, body}`, comments out. */
const allRules = (): { selector: string; body: string }[] => {
  const out: { selector: string; body: string }[] = [];
  const re = /(?:^|\})\s*([^@{}]+)\{([^{}]*)\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(stripped))) {
    out.push({ selector: m[1].trim(), body: m[2] });
  }
  return out;
};

describe("index.css — per-profile tab colour (remote-profiles T2)", () => {
  it("paints a 2px top accent on .ctab from --ctab-accent", () => {
    const accent = allRules().find(
      (r) =>
        /^\.ctab(\.active)?$/.test(r.selector) &&
        r.body.includes("var(--ctab-accent)") &&
        /2px/.test(r.body),
    );
    expect(
      accent,
      "a .ctab (or .ctab.active) rule must paint a 2px accent from var(--ctab-accent)",
    ).toBeDefined();
  });

  it("renders the accent at 60% on inactive tabs", () => {
    const inactive = allRules().find(
      (r) =>
        /\.ctab/.test(r.selector) &&
        /:not\(\.active\)/.test(r.selector) &&
        r.body.includes("--ctab-accent"),
    );
    expect(
      inactive,
      "an inactive .ctab rule (:not(.active)) must reference --ctab-accent",
    ).toBeDefined();
    expect(inactive!.body, "the inactive accent must be at 60%").toMatch(/60%|0\.6/);
  });

  it("defines --profile-<c> for all 8 colours in the dark theme", () => {
    const dark = [ruleBody(":root"), ruleBody("html")].find(
      (b) => b !== null && PALETTE.every((c) => b.includes(`--profile-${c}:`)),
    );
    expect(
      dark,
      "the dark theme (:root or html) must define all 8 --profile-<c> variables",
    ).toBeDefined();
  });

  it("defines --profile-<c> for all 8 colours in the light theme", () => {
    const light = [ruleBody("html.light"), ruleBody(":root.light")].find(
      (b) => b !== null && PALETTE.every((c) => b.includes(`--profile-${c}:`)),
    );
    expect(
      light,
      "the light theme (html.light) must define all 8 --profile-<c> variables",
    ).toBeDefined();
  });
});

// ─── .ctabbar right-edge clearance for the TabbedDrawer rail ─────────────
//
// The drawer's handle rail (fixed, right-0, z-40) overlaps the tab bar
// (z-20). The bar's right padding must clear the rail, else the "+" button
// is hidden behind it. RED before the fix: padding is 8px, rail is 40px.

const drawer = readFileSync(
  decodeURIComponent(new URL("./components/TabbedDrawer.tsx", import.meta.url).pathname),
  "utf8",
);

/** Rail handle width in px: the role="tab" button's Tailwind w-<n> class (1 unit = 4px). */
function railHandleWidthPx(): number {
  const btn = /<button[\s\S]*?role="tab"[\s\S]*?<\/button>/.exec(drawer);
  expect(btn, 'TabbedDrawer must render a role="tab" handle button').not.toBeNull();
  const w = /\bw-(\d+(?:\.\d+)?)\b/.exec(btn![0]);
  expect(w, "the handle button must carry a Tailwind w-<n> width class").not.toBeNull();
  return parseFloat(w![1]) * 4;
}

/** Horizontal padding of `.ctabbar` in px, left and right kept separate. */
function ctabbarPadding(): { left: number; right: number } {
  const body = ruleBody(".ctabbar");
  expect(body, "index.css must have a .ctabbar rule").not.toBeNull();
  const px = (v: string | null) => (v ? parseFloat(v) || 0 : 0);
  let left = 0;
  let right = 0;
  const pad = decl(body, "padding");
  if (pad) {
    const p = pad.split(" ");
    const r = p[1] ?? p[0];
    const l = p[3] ?? r;
    left = px(l);
    right = px(r);
  }
  left = decl(body, "padding-left") ? px(decl(body, "padding-left")) : left;
  right = decl(body, "padding-right") ? px(decl(body, "padding-right")) : right;
  return { left, right };
}

describe("index.css — .ctabbar clears the drawer rail", () => {
  it("has right padding ≥ rail handle width + 8px so the \"+\" stays visible and clickable", () => {
    const rail = railHandleWidthPx();
    const { right } = ctabbarPadding();
    expect(
      right,
      `.ctabbar padding-right is ${right}px; the rail handle is ${rail}px wide and needs ${rail + 8}px (rail + 8px gap) of clearance`,
    ).toBeGreaterThanOrEqual(rail + 8);
  });

  it("keeps NAV_PADDING_PX in sync with the .ctabbar left+right padding (overflow math)", () => {
    const { left, right } = ctabbarPadding();
    expect(
      NAV_PADDING_PX,
      "NAV_PADDING_PX feeds tabsOverflow and must equal the bar's left+right padding",
    ).toBe(left + right);
  });
});
