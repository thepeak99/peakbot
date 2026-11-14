// P4 — the one seam between the cloud UI and the Settings shell (design §A.4).
//
// The TrustBanner must open Settings at the Cloud pane without importing the
// shell (and without any fetch — it is navigation, not a mutation). The
// banner dispatches this window event; `App.tsx` listens, opens the Settings
// view, and focuses the Cloud section.

export const CLOUD_SETTINGS_EVENT = "peakbot:open-cloud-settings";

export function openCloudSettings(): void {
  window.dispatchEvent(new CustomEvent(CLOUD_SETTINGS_EVENT));
}
