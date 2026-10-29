import type { Theme } from "./useTheme";

// The single desktop-detection seam. Tauri sets `window.isTauri = true` itself
// (main-frame init script, every origin), so a strict comparison is the whole
// check — no UA sniffing, no `__TAURI__`, no IPC.

export function isDesktop(): boolean {
  return (globalThis as { isTauri?: unknown }).isTauri === true;
}

// ── desktop IPC seam ─────────────────────────────────────────────────────
// The desktop app grants this page (on its default loopback backend only)
// three Tauri commands (desktop/src-tauri/capabilities/remote-web.json):
// `theme_set`, `update_status`, `update_install`. Everything goes through
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
