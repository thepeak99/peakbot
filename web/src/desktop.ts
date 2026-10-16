// The single desktop-detection seam. Tauri sets `window.isTauri = true` itself
// (main-frame init script, every origin), so a strict comparison is the whole
// check — no UA sniffing, no `__TAURI__`, no IPC.

export function isDesktop(): boolean {
  return (globalThis as { isTauri?: unknown }).isTauri === true;
}
