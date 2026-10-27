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

import { afterEach, describe, expect, it, vi } from "vitest";
import { isDesktop } from "./desktop";
import {
  getDesktopSettings,
  setDesktopTheme,
  toggleDesktopBackground,
  type DesktopView,
} from "./desktop";

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

// ── desktop IPC seam (design §5 item 5, D10) ─────────────────────────────
// The DesktopPane's ONLY way to talk to the desktop app is the seam in
// desktop.ts: getDesktopSettings() → invoke("settings_get"),
// toggleDesktopBackground() → invoke("toggle_background"),
// setDesktopTheme(theme) → invoke("theme_set", { theme }). The stub is
// installed by setting globalThis.__TAURI__ = { core: { invoke } } and
// deleted after every test.

type TauriGlobal = { __TAURI__?: { core?: { invoke?: unknown } } };
const tg = globalThis as TauriGlobal;

const VIEW: DesktopView = {
  cert_warning: false,
  background_visible: true,
  background: false,
  background_enabled: true,
  busy: false,
};

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

  it("without __TAURI__ all three seam calls reject (browser)", async () => {
    // No property at all — the normal browser case.
    await expect(getDesktopSettings()).rejects.toThrow("no desktop IPC");
    await expect(toggleDesktopBackground()).rejects.toThrow("no desktop IPC");
    await expect(setDesktopTheme("dark")).rejects.toThrow("no desktop IPC");
  });

  it("getDesktopSettings invokes settings_get with no args", async () => {
    const invoke = stubInvoke();
    invoke.mockResolvedValueOnce(VIEW);

    const view = await getDesktopSettings();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("settings_get", undefined);
    expect(view).toEqual(VIEW);
  });

  it("toggleDesktopBackground invokes toggle_background with no args", async () => {
    const invoke = stubInvoke();

    await expect(toggleDesktopBackground()).resolves.toBeUndefined();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("toggle_background", undefined);
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

    await expect(getDesktopSettings()).rejects.toThrow("boom");
    await expect(toggleDesktopBackground()).rejects.toThrow("boom");
    await expect(setDesktopTheme("dark")).rejects.toThrow("boom");
    expect(invoke).toHaveBeenCalledTimes(3);
  });
});
