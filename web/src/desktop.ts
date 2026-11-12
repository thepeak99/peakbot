import type { Theme } from "./useTheme";

// The single shell-detection seam. Both Tauri shells (desktop `window.rs`,
// mobile `src-tauri/lib.rs`) inject `window.peakbotShell = "desktop" | "mobile"`
// via a main-frame init script on every origin. Read with a strict comparison
// — never `window.isTauri` (old shells set it without the marker), no UA
// sniffing, no IPC. Missing or any other value is the fail-safe "browser" (SH1).
declare global {
  interface Window {
    peakbotShell?: "desktop" | "mobile";
    /** Defined by the SPA while desktop tabs are mounted (useConvoTabs); called by
     *  the desktop app (notifier.rs) via webview eval on a notification click:
     *  activate that conversation's tab, or open it in a new one. */
    peakbotOpenConvo?: (convo: string) => void;
  }
}

export type Shell = "desktop" | "mobile" | "browser";

export function shell(): Shell {
  const marker = (globalThis as { peakbotShell?: unknown }).peakbotShell;
  return marker === "desktop" || marker === "mobile" ? marker : "browser";
}

export function isDesktop(): boolean {
  return shell() === "desktop";
}

export function isMobile(): boolean {
  // True only when the mobile shell's init-script marker is set.
  return shell() === "mobile";
}

// ── edition signal ────────────────────────────────────────────────────────
// What THIS server can do: `boot.edition` on `GET /api/settings`.
// `"full"` (host build) is the default until the first boot read; a read
// WITHOUT the field (old servers) resets it back to `"full"`. `getSettings()`
// (settings/api.ts) is the single read point — Boot, Settings and the
// wizard all go through it.
export type Edition = "full" | "local";

let bootEdition: Edition = "full";

export function edition(): Edition {
  return bootEdition;
}

export function setBootEdition(e: Edition | undefined): void {
  bootEdition = e ?? "full";
}

// ── desktop IPC seam ─────────────────────────────────────────────────────
// The desktop app grants this page (on its default loopback backend only)
// four Tauri commands (desktop/src-tauri/capabilities/remote-web.json):
// `theme_set`, `update_status`, `update_install`, `clipboard_image_png`.
// Everything goes through
// `globalThis.__TAURI__.core.invoke`; ANY rejection — including no IPC at
// all (a non-default address, an older desktop app) — means "the desktop
// option is unavailable here".

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const core = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__?.core;
  if (!core?.invoke) return Promise.reject(new Error("no desktop IPC"));
  // Omit the args slot when absent so payload-less commands match the
  // one-arg `invoke(cmd)` form the IPC tests assert.
  return args === undefined ? core.invoke(cmd) : core.invoke(cmd, args);
}

export const setDesktopTheme = (theme: Theme) =>
  invoke("theme_set", { theme }).then(() => undefined);

// ── desktop auto-update ──────────────────────────────────────────────────
// `update_status` returns null (up to date) or one of three states (plan §Web);
// `update_install` starts the download + restart — it resolves on success and
// rejects with a string reason on failure. The app owns restart; the web
// page only surfaces state.
export type UpdateStatus =
  | null
  | { state: "available"; version: string }
  | { state: "installing" }
  | { state: "failed"; version: string };

export const getUpdateStatus = (): Promise<UpdateStatus> =>
  invoke("update_status") as Promise<UpdateStatus>;

export const installUpdate = (): Promise<void> =>
  invoke("update_install").then(() => undefined);

// ── clipboard image (Linux desktop paste fallback) ──────────────────────
// WebKitGTK ≤ 2.50 never exposes a clipboard image to DOM paste events
// (WebKit Bug 218519), so the app reads it: `clipboard_image_png` returns PNG
// bytes, 0 bytes = no image. Tauri delivers raw bytes as an ArrayBuffer, or as
// a number[] when its custom-protocol IPC is blocked — Uint8Array takes both.
export const readClipboardImage = (): Promise<File | null> =>
  invoke("clipboard_image_png").then((raw) => {
    const bytes = new Uint8Array(raw as ArrayBuffer | number[]);
    return bytes.length === 0
      ? null
      : new File([bytes], "pasted-image.png", { type: "image/png" });
  });
