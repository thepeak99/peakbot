import type { Theme } from "./useTheme";

// The single desktop-detection seam. Tauri sets `window.isTauri = true` itself
// (main-frame init script, every origin), so a strict comparison is the whole
// check — no UA sniffing, no `__TAURI__`, no IPC.

export function isDesktop(): boolean {
  return (globalThis as { isTauri?: unknown }).isTauri === true;
}

// ── Desktop IPC seam ────────────────────────────────────────────────────
// The desktop app grants this page (on its default loopback backend only)
// exactly three Tauri commands: settings_get, toggle_background, theme_set
// (desktop/src-tauri/capabilities/desktop-pane.json). Everything goes
// through `globalThis.__TAURI__.core.invoke`; ANY rejection — including no
// IPC at all (a non-default address, an older desktop app) — means "the
// desktop options are unavailable here".

/** The desktop's `settings_get` view (desktop/core/src/settings.rs). */
export interface DesktopView {
  background_visible: boolean;
  background: boolean;
  background_enabled: boolean;
  busy: boolean;
}

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const core = (globalThis as { __TAURI__?: { core?: { invoke?: Invoke } } }).__TAURI__?.core;
  return core?.invoke ? core.invoke(cmd, args) : Promise.reject(new Error("no desktop IPC"));
}

export const getDesktopSettings = () => invoke("settings_get") as Promise<DesktopView>;
export const toggleDesktopBackground = () => invoke("toggle_background").then(() => undefined);
export const setDesktopTheme = (theme: Theme) =>
  invoke("theme_set", { theme }).then(() => undefined);
