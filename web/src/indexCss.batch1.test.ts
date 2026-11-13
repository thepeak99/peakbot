// Batch 1 shared tokens and primitives (fix-plan §2, item 1.1): the
// `phoneUi.ts` class-token module plus the `light:` custom variant and the
// `--radius-sheet` theme token in index.css.
//
// jsdom never loads index.css, so — like convoTabBar.css.test.ts — the file
// is read as TEXT and the declarations are pinned down.
//
// RED before implementation: `phoneUi.ts` does not exist yet (the import
// below fails to resolve, so this file fails at collection for the right
// reason), and index.css has neither the variant nor the radius token.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import {
  ACTION_PILL,
  IB,
  IBI,
  IBI_HI,
  IBI_ON,
  MTAG,
  PANEL_HEAD,
  PANEL_SUB,
  PANEL_TITLE,
  PILL_OFF,
  PILL_ON,
  ROW,
} from "./components/phoneUi";

// Vitest stubs CSS imports to "" (even with ?raw), so read the file directly.
const css = readFileSync(
  decodeURIComponent(new URL("./index.css", import.meta.url).pathname),
  "utf8",
);
const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");

describe("phoneUi.ts — the mock's class tokens live in one module", () => {
  it("IB/IBI* are 34px r11 square surfaces (the .ibi family, proto:126-129)", () => {
    for (const c of [IBI, IBI_HI, IBI_ON]) {
      expect(c).toContain("h-[34px]");
      expect(c).toContain("w-[34px]");
      expect(c).toContain("rounded-[11px]");
      expect(c).toContain("border");
    }
    // .ib — the 44×44 hit area around an .ibi.
    expect(IB).toContain("h-11");
    expect(IB).toContain("w-11");
    // .ibi.on is the emerald-tinted pressed face; the plain .ibi is not.
    expect(IBI_ON).toContain("emerald");
    expect(IBI).not.toContain("emerald");
  });

  it("PILL_OFF/PILL_ON are the unselected/selected pill faces (colours only)", () => {
    expect(PILL_OFF).toContain("font-medium");
    expect(PILL_ON).toContain("font-semibold");
    expect(PILL_OFF, "the two faces differ").not.toBe(PILL_ON);
  });

  it("MTAG is the 10.5px machine-tag chip (proto:81)", () => {
    expect(MTAG).toContain("text-[10.5px]");
    expect(MTAG).toContain("rounded-[5px]");
  });

  it("PANEL_* are the .ph2 header tokens (proto:230-231)", () => {
    expect(PANEL_HEAD).toContain("flex");
    expect(PANEL_HEAD).toContain("justify-between");
    expect(PANEL_TITLE).toContain("text-[15px]");
    expect(PANEL_SUB).toContain("text-xs");
  });

  it("ROW is the 52px list row and ACTION_PILL the 34px r17 pill (proto:234, 247)", () => {
    expect(ROW).toContain("min-h-[52px]");
    expect(ROW).toContain("border-b");
    expect(ACTION_PILL).toContain("h-[34px]");
    expect(ACTION_PILL).toContain("rounded-[17px]");
  });
});

describe("index.css — batch 1 tokens (item 1.1)", () => {
  it("declares the `light:` custom variant for the html.light theme", () => {
    expect(
      stripped,
      "the `light:` variant (portaled sheets included) must be declared",
    ).toMatch(/@custom-variant\s+light/);
  });

  it("adds --radius-sheet: 26px inside the @theme block (rounded-sheet for sheet + composer pill)", () => {
    const theme = /@theme\s*\{([\s\S]*?)\}/.exec(stripped);
    expect(theme, "the existing @theme block must stay").not.toBeNull();
    expect(
      theme![1],
      "the 26px sheet radius token must live in @theme",
    ).toMatch(/--radius-sheet\s*:\s*26px/);
  });
});
