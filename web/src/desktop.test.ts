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
// `globalThis.__TAURI__.core.invoke` stub for the theme, update and
// clipboard-image commands.

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  getUpdateStatus,
  installUpdate,
  isDesktop,
  readClipboardImage,
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

// ── shell seam (mobile plan §2.5, task F1) ───────────────────────────────
// `shell()` reads `window.peakbotShell` — the marker the shell's init
// script injects: exactly "desktop" or "mobile". Anything else — missing,
// garbage, the wrong type — is "browser" (SH1 fail-safe). `isDesktop()`
// becomes `shell() === "desktop"`, and `window.isTauri` is NO LONGER
// consulted: old desktop apps still set `isTauri` without the marker, and
// a Tauri window without the marker must read as "browser".
//
// RED today: desktop.ts does not export shell() yet (F1). The dynamic
// import (this file's existing module-surface pattern) keeps the rest of
// this file green until the seam lands.

type ShellModule = { shell?: unknown };

async function loadShell(): Promise<() => string> {
  const mod = (await import("./desktop")) as ShellModule;
  if (typeof mod.shell !== "function") {
    throw new Error("desktop.ts does not export shell() yet (F1)");
  }
  return mod.shell as () => string;
}

type WithShell = { peakbotShell?: unknown };
const sg = globalThis as WithShell;

describe("shell() seam (F1)", () => {
  afterEach(() => {
    delete sg.peakbotShell;
    delete g.isTauri;
  });

  it("returns 'desktop' when window.peakbotShell is exactly 'desktop'", async () => {
    sg.peakbotShell = "desktop";
    const shell = await loadShell();
    expect(shell()).toBe("desktop");
  });

  it("returns 'mobile' when window.peakbotShell is exactly 'mobile'", async () => {
    sg.peakbotShell = "mobile";
    const shell = await loadShell();
    expect(shell()).toBe("mobile");
  });

  it("returns 'browser' when window.peakbotShell is missing", async () => {
    const shell = await loadShell();
    expect(shell()).toBe("browser");
  });

  it.each([
    ["the uppercase 'DESKTOP'", "DESKTOP"],
    ["the mixed-case 'Desktop'", "Desktop"],
    ["the empty string", ""],
    ["the padded marker ' mobile '", " mobile "],
    ["the string 'true'", "true"],
    ["the number 1", 1],
    ["true", true],
    ["null", null],
    ["an object", {}],
  ] as const)("returns 'browser' for the garbage marker %s", async (_label, value) => {
    sg.peakbotShell = value;
    const shell = await loadShell();
    expect(shell()).toBe("browser");
  });

  it("isDesktop() is true exactly when shell() is 'desktop'", async () => {
    const shell = await loadShell();
    for (const marker of ["desktop", "mobile", undefined] as const) {
      if (marker === undefined) delete sg.peakbotShell;
      else sg.peakbotShell = marker;
      expect(isDesktop()).toBe(shell() === "desktop");
    }
  });

  it("a Tauri window WITHOUT the marker reads as 'browser' (isTauri not consulted)", async () => {
    g.isTauri = true; // old desktop apps set this automatically
    const shell = await loadShell();
    expect(shell()).toBe("browser");
    expect(isDesktop()).toBe(false);
  });

  it("a Tauri window WITH the desktop marker stays desktop", async () => {
    g.isTauri = true;
    sg.peakbotShell = "desktop";
    const shell = await loadShell();
    expect(shell()).toBe("desktop");
    expect(isDesktop()).toBe(true);
  });
});

// ── desktop IPC seam (theme_set + update commands) ───────────────────────
// The web page's ONLY way to talk to the desktop app is the seam in
// desktop.ts: setDesktopTheme(theme) → invoke("theme_set", { theme }),
// getUpdateStatus() → invoke("update_status"), installUpdate() →
// invoke("update_install"), readClipboardImage() →
// invoke("clipboard_image_png"). The
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
    ["readClipboardImage", () => readClipboardImage()],
  ] as [string, () => Promise<unknown>][])(
    "%s rejects with 'no desktop IPC' when __TAURI__ is absent",
    async (_name, call) => {
      await expect(call()).rejects.toThrow("no desktop IPC");
    },
  );
});

// ── clipboard image IPC seam (Linux desktop paste fallback) ──────────────
// `readClipboardImage()` → invoke("clipboard_image_png") returns the
// clipboard image as PNG bytes; 0 bytes = no image. Tauri delivers the
// raw bytes as an ArrayBuffer, or as a number[] when its custom-protocol
// IPC is blocked — the seam takes both.
//
// RED today: desktop.ts does not export readClipboardImage, so this
// describe block fails for exactly the "feature absent" reason.

describe("clipboard image IPC seam", () => {
  afterEach(() => {
    delete tg.__TAURI__;
  });

  it("readClipboardImage invokes clipboard_image_png with the one-arg form", async () => {
    const invoke = vi.fn(async () => new Uint8Array([1, 2, 3]).buffer);
    tg.__TAURI__ = { core: { invoke } };

    await readClipboardImage();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("clipboard_image_png");
  });

  it("ArrayBuffer bytes become a pasted-image.png image/png File with the same bytes", async () => {
    const invoke = vi.fn(async () => new Uint8Array([137, 80, 78, 71]).buffer);
    tg.__TAURI__ = { core: { invoke } };

    const file = (await readClipboardImage()) as File;

    expect(file).toBeInstanceOf(File);
    expect(file.name).toBe("pasted-image.png");
    expect(file.type).toBe("image/png");
    expect(file.size).toBe(4);
    expect(new Uint8Array(await file.arrayBuffer())).toEqual(
      new Uint8Array([137, 80, 78, 71]),
    );
  });

  it("number[] bytes (custom-protocol IPC blocked) become the same File", async () => {
    const invoke = vi.fn(async () => [137, 80, 78, 71]);
    tg.__TAURI__ = { core: { invoke } };

    const file = (await readClipboardImage()) as File;

    expect(file).toBeInstanceOf(File);
    expect(file.name).toBe("pasted-image.png");
    expect(file.type).toBe("image/png");
    expect(file.size).toBe(4);
    expect(new Uint8Array(await file.arrayBuffer())).toEqual(
      new Uint8Array([137, 80, 78, 71]),
    );
  });

  it.each([
    ["an empty ArrayBuffer", new ArrayBuffer(0)],
    ["an empty number[]", []],
  ] as const)("empty payload %s resolves null", async (_label, raw) => {
    const invoke = vi.fn(async () => raw);
    tg.__TAURI__ = { core: { invoke } };

    await expect(readClipboardImage()).resolves.toBeNull();
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("readClipboardImage rejects with the command's string reason", async () => {
    const invoke = vi.fn(async () => {
      throw "clipboard image is larger than 50 MB";
    });
    tg.__TAURI__ = { core: { invoke } };

    await expect(readClipboardImage()).rejects.toBe(
      "clipboard image is larger than 50 MB",
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });
});

// ── module surface (close-to-tray removal + auto-update) ─────────────────
// The Desktop pane is gone: `DesktopView`, `getDesktopSettings` and
// `toggleDesktopBackground` are no longer exported. The seam carries
// exactly six exports: the shell detection pair (F1), the theme command,
// the two update commands, and the clipboard-image read.
//
// RED today: `shell` is not exported yet (F1).

describe("module surface", () => {
  it("exports exactly isDesktop, shell, setDesktopTheme, getUpdateStatus, installUpdate, readClipboardImage", async () => {
    const mod = (await import("./desktop")) as Record<string, unknown>;
    expect(Object.keys(mod).sort()).toEqual([
      "getUpdateStatus",
      "installUpdate",
      "isDesktop",
      "readClipboardImage",
      "setDesktopTheme",
      "shell",
    ]);
  });
});
