// Tests for `desktop.ts` — the single desktop-detection seam (design §1/§5):
//
//   export function isDesktop(): boolean;
//   // (globalThis as {isTauri?: unknown}).isTauri === true
//
// Tauri sets `window.isTauri` itself on every origin (main-frame init script),
// so the check is a strict `=== true` comparison — no UA sniffing, no
// `__TAURI__`, no `invoke`. In a browser the property is absent.
//
// RED before implementation: `./desktop` does not exist yet, so this file
// fails at import — the right reason (feature absent).
//
// Runs under `node` (no jsdom needed): the only global involved is
// `globalThis.isTauri`, set and deleted per test.

import { afterEach, describe, expect, it } from "vitest";
import { isDesktop } from "./desktop";

type WithTauri = { isTauri?: unknown };
const g = globalThis as WithTauri;

afterEach(() => {
  delete g.isTauri;
});

describe("isDesktop", () => {
  it("returns true when window.isTauri is exactly true", () => {
    g.isTauri = true;
    expect(isDesktop()).toBe(true);
  });

  it("returns false in a browser (isTauri absent)", () => {
    // No property set at all — the normal browser case.
    expect(isDesktop()).toBe(false);
  });

  it("returns false when isTauri is false", () => {
    g.isTauri = false;
    expect(isDesktop()).toBe(false);
  });

  // The comparison must be strict `=== true`, not truthiness: any other
  // value (a string, a number, an object) is not Tauri's signal.
  it.each([
    ["the string 'true'", "true"],
    ["the number 1", 1],
    ["an object", {}],
    ["null", null],
    ["undefined (explicit)", undefined],
  ] as const)("returns false when isTauri is %s", (_label, value) => {
    g.isTauri = value;
    expect(isDesktop()).toBe(false);
  });
});
