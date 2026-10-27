// peakbot-desktop-settings-page
// 0.24 (spec §9.10): the bundled settings page of the main window —
// there is no second settings window. Only the desktop-only "Keep
// PeakBot running in background" row plus `← Back to PeakBot` (act
// "back"); web server settings live in PeakBot → Settings → General
// (config.yaml `general.web`). Polls `settings_get` every 1000 ms. No
// build step: talks to the window through `window.__TAURI__`
// (withGlobalTauri). The remote PeakBot UI has no permissions at all
// (capability ACL).

const $ = (id) => document.getElementById(id);

const tauri = window.__TAURI__;

let backgroundEnabled = false;

function tick() {
  tauri.core
    .invoke("settings_get")
    .then((view) => {
      $("background").checked = view.background;
      backgroundEnabled = view.background_enabled;
      $("background").disabled = !backgroundEnabled;
      // Q1: the row is absent on platforms that don't offer the
      // background service (Windows in v1).
      $("row-background").hidden = !view.background_visible;
      // Un-hide the form: the HTML keeps it hidden so the defaults never
      // flash before the first successful load.
      $("page").hidden = false;
    })
    .catch((err) => console.error("settings poll failed", err));
}

if (!tauri) {
  $("title").textContent = "Internal error: no Tauri API";
  $("no-tauri").hidden = false;
  $("page").hidden = true;
} else {
  // The background row surfaces the existing tray toggle; the poll
  // reflects the result (the confirm dialogs run in the controller).
  $("background").addEventListener("change", () => {
    tauri.core
      .invoke("act", { action: "toggle_background" })
      .catch((err) => console.error("toggle_background failed", err));
  });

  // `← Back to PeakBot`: the main window navigates back to the chat
  // (or the status page if the backend is not Ready).
  $("back").addEventListener("click", () => {
    tauri.core
      .invoke("act", { action: "back" })
      .catch((err) => console.error("back failed", err));
  });

  tick();
  setInterval(tick, 1000);
}
