// Tests for `desktop.ts` — the single desktop-detection seam (design §1/§5):
//
//   export function isDesktop(): boolean;
//   // (globalThis as {isTauri?: unknown}).isTauri === true
//
// Tauri sets `window.isTauri` itself on every origin (main-frame init
// script), so the check is a strict `=== true` comparison — no UA sniffing,
// no `__TAURI__`, no `invoke`. In a browser the property is absent.
//
// Runs under `node` (no jsdom needed): the only globals involved are
// `globalThis.isTauri` (set and deleted per test) and the
// `globalThis.__TAURI__.core.invoke` stub for the single theme command.

import { afterEach, describe, expect, it, vi } from "vitest";
import { isDesktop, setDesktopTheme } from "./desktop";

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

// ── desktop IPC seam (one command: theme_set) ─────────────────────────────
// The web page's ONLY way to talk to the desktop app is the seam in
// desktop.ts: setDesktopTheme(theme) → invoke("theme_set", { theme }). The
// stub is installed by setting globalThis.__TAURI__ = { core: { invoke } }
// and deleted after every test.

type TauriGlobal = { __TAURI__?: { core?: { invoke?: unknown } } };
const tg = globalThis as TauriGlobal;

/** Install a fresh `__TAURI__.core.invoke` stub and return the mock. */
function stubInvoke(): ReturnType<typeof vi.fn> {
  const invoke = vi.fn(async () => undefined);
  tg.__TAURI__ = { core: { invoke } };
  return invoke;
}

describe("desktop IPC seam", () => {
  afterEach(() => {
    delete tg.__TAURI__;
  });

  it("without __TAURI__ the seam rejects (browser)", async () => {
    // No property at all — the normal browser case.
    await expect(setDesktopTheme("dark")).rejects.toThrow("no desktop IPC");
  });

  it("setDesktopTheme invokes theme_set with the theme argument", async () => {
    const invoke = stubInvoke();

    await expect(setDesktopTheme("light")).resolves.toBeUndefined();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("theme_set", { theme: "light" });
  });

  it("a stub rejection passes through unchanged", async () => {
    const invoke = vi.fn(async () => {
      throw new Error("boom");
    });
    tg.__TAURI__ = { core: { invoke } };

    await expect(setDesktopTheme("dark")).rejects.toThrow("boom");
    expect(invoke).toHaveBeenCalledTimes(1);
  });
});

// ── module surface (close-to-tray removal) ─────────────────────────────────
// The Desktop pane is gone: `DesktopView`, `getDesktopSettings` and
// `toggleDesktopBackground` are no longer exported; `setDesktopTheme` is
// the only remaining command.

describe("module surface", () => {
  it("exports only isDesktop and setDesktopTheme", async () => {
    const mod = await import("./desktop");
    expect(Object.keys(mod).sort()).toEqual(["isDesktop", "setDesktopTheme"]);
  });
});
