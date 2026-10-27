import type { Theme } from "./useTheme";

// The single desktop-detection seam. Tauri sets `window.isTauri = true` itself
// (main-frame init script, every origin), so a strict comparison is the whole
// check — no UA sniffing, no `__TAURI__`, no IPC.

export function isDesktop(): boolean {
  return (globalThis as { isTauri?: unknown }).isTauri === true;
}

// ── desktop IPC seam ─────────────────────────────────────────────────────
// The desktop app grants this page (on its default loopback backend only)
// exactly one Tauri command: `theme_set`
// (desktop/src-tauri/capabilities/desktop-pane.json). Everything goes
// through `globalThis.__TAURI__.core.invoke`; ANY rejection — including no
// IPC at all (a non-default address, an older desktop app) — means "the
// desktop option is unavailable here".

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const core = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__?.core;
  return core?.invoke ? core.invoke(cmd, args) : Promise.reject(new Error("no desktop IPC"));
}

export const setDesktopTheme = (theme: Theme) =>
  invoke("theme_set", { theme }).then(() => undefined);
