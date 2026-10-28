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
// `globalThis.__TAURI__.core.invoke` stub for the theme and update commands.

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  getUpdateStatus,
  installUpdate,
  isDesktop,
  setDesktopTheme,
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

// ── desktop IPC seam (theme_set + update commands) ───────────────────────
// The web page's ONLY way to talk to the desktop app is the seam in
// desktop.ts: setDesktopTheme(theme) → invoke("theme_set", { theme }),
// getUpdateStatus() → invoke("update_status"), installUpdate() →
// invoke("update_install"). The
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

// ── update IPC seam (desktop auto-update) ────────────────────────────────
// Two more commands ride the same `__TAURI__.core.invoke` seam
// (desktop/src-tauri/capabilities/remote-web.json):
//   update_status  → null | {state:"available";version} | {state:"installing"}
//                    | {state:"failed";version}
//   update_install → resolves (the app is restarting) or rejects with a
//                    string (the failure reason)
//
// RED today: desktop.ts exports neither getUpdateStatus nor installUpdate,
// so this whole file fails at import — the right reason (feature absent).

describe("update IPC seam", () => {
  afterEach(() => {
    delete tg.__TAURI__;
  });

  it.each([
    ["null", null],
    ["available", { state: "available", version: "0.25.0" }],
    ["installing", { state: "installing" }],
    ["failed", { state: "failed", version: "0.25.0" }],
  ] as const)(
    "getUpdateStatus passes through invoke's %s value unchanged",
    async (_label, value) => {
      const invoke = vi.fn(async () => value);
      tg.__TAURI__ = { core: { invoke } };

      await expect(getUpdateStatus()).resolves.toEqual(value);

      expect(invoke).toHaveBeenCalledTimes(1);
      expect(invoke).toHaveBeenCalledWith("update_status");
    },
  );

  it("installUpdate invokes update_install and resolves undefined", async () => {
    const invoke = vi.fn(async () => undefined);
    tg.__TAURI__ = { core: { invoke } };

    await expect(installUpdate()).resolves.toBeUndefined();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("update_install");
  });

  it("installUpdate resolves undefined even when the command resolves with a value", async () => {
    const invoke = vi.fn(async () => "done");
    tg.__TAURI__ = { core: { invoke } };

    await expect(installUpdate()).resolves.toBeUndefined();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("installUpdate rejects with the command's string reason", async () => {
    const invoke = vi.fn(async () => {
      throw "download failed";
    });
    tg.__TAURI__ = { core: { invoke } };

    await expect(installUpdate()).rejects.toBe("download failed");
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it.each([
    ["getUpdateStatus", () => getUpdateStatus()],
    ["installUpdate", () => installUpdate()],
  ] as [string, () => Promise<unknown>][])(
    "%s rejects with 'no desktop IPC' when __TAURI__ is absent",
    async (_name, call) => {
      await expect(call()).rejects.toThrow("no desktop IPC");
    },
  );
});

// ── module surface (close-to-tray removal + auto-update) ─────────────────
// The Desktop pane is gone: `DesktopView`, `getDesktopSettings` and
// `toggleDesktopBackground` are no longer exported. The seam now carries
// exactly four exports: detection, the theme command, and the two update
// commands.

describe("module surface", () => {
  it("exports exactly isDesktop, setDesktopTheme, getUpdateStatus, installUpdate", async () => {
    const mod = await import("./desktop");
    expect(Object.keys(mod).sort()).toEqual([
      "getUpdateStatus",
      "installUpdate",
      "isDesktop",
      "setDesktopTheme",
    ]);
  });
});
