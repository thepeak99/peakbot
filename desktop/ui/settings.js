// peakbot-desktop-settings-page
// Polls `settings_get` every 1000 ms and renders the SettingsView (design
// #377 §4.2). No build step: talks to the window through `window.__TAURI__`
// (withGlobalTauri). This file only runs on the bundled settings window;
// the remote PeakBot UI has no permissions at all (capability ACL).

const $ = (id) => document.getElementById(id);

const tauri = window.__TAURI__;

// The last saved wire value (what `settings.json` holds); the form is
// dirty iff the form values differ from it.
let saved = null;
let busy = false;
let backgroundEnabled = false;
let webTokenPath = "";

function protocolRadio(value) {
  return document.querySelector(`input[name="protocol"][value="${value}"]`);
}

function formValues() {
  return {
    protocol: protocolRadio("http").checked ? "http" : "https",
    token: $("token").checked,
    remote: $("remote").checked,
  };
}

function isDirty() {
  const v = formValues();
  return (
    saved !== null &&
    (v.protocol !== saved.protocol ||
      v.token !== saved.token ||
      v.remote !== saved.remote)
  );
}

function setForm(v) {
  protocolRadio(v.protocol).checked = true;
  $("token").checked = v.token;
  $("remote").checked = v.remote;
}

function updateDynamic() {
  const v = formValues();
  // Remote forces HTTPS + token on and locks both controls (UX only —
  // core still validates at the IPC boundary).
  protocolRadio("http").disabled = v.remote;
  protocolRadio("https").disabled = v.remote;
  $("token").disabled = v.remote;

  $("protocol-note").hidden = v.protocol !== "https";
  $("token-note").textContent = v.token
    ? `Stored in ${webTokenPath}`
    : "Without a token, any program or web page on this computer can control PeakBot.";

  $("save").disabled = !isDirty() || busy;
  $("background").disabled = !(backgroundEnabled && !isDirty() && !busy);
}

// The poll: refresh `saved` (never overwriting a dirty form), the
// background row and `busy`; clear "Applying…" once the file reflects
// the saved values.
async function tick() {
  try {
    const view = await tauri.core.invoke("settings_get");
    webTokenPath = view.web_token_path;
    if (!isDirty()) {
      saved = view.settings;
      setForm(view.settings);
    }
    $("background").checked = view.background;
    backgroundEnabled = view.background_enabled;
    busy = view.busy;
    if (!isDirty() && $("state").textContent === "Applying…") {
      $("state").textContent = "";
    }
    updateDynamic();
  } catch (err) {
    console.error("settings poll failed", err);
  }
}

if (!tauri) {
  $("title").textContent = "Internal error: no Tauri API";
  $("no-tauri").hidden = false;
  $("form").hidden = true;
  document.querySelector("hr").hidden = true;
  $("row-background").hidden = true;
} else {
  $("remote").addEventListener("change", () => {
    // Turning remote on forces HTTPS + token on; turning it off
    // re-enables both, keeping their current values.
    if ($("remote").checked) {
      protocolRadio("https").checked = true;
      $("token").checked = true;
    }
    updateDynamic();
  });

  // Save (enabled iff dirty && !busy). A validation error comes back
  // verbatim from core; on success the poll clears the dirty state
  // once the file reflects the values. Cancelling the native confirm
  // dialog leaves the form dirty with the user's values.
  $("form").addEventListener("submit", async (e) => {
    e.preventDefault();
    if (!isDirty() || busy) return;
    $("error").textContent = "";
    try {
      await tauri.core.invoke("settings_set", { settings: formValues() });
      $("state").textContent = "Applying…";
    } catch (err) {
      $("state").textContent = "";
      $("error").textContent = String(err);
    }
  });

  // The background row surfaces the existing tray toggle; the poll
  // reflects the result (the confirm dialogs run in the controller).
  $("background").addEventListener("change", () => {
    tauri.core
      .invoke("act", { action: "toggle_background" })
      .catch((err) => console.error("toggle_background failed", err));
  });

  tick();
  setInterval(tick, 1000);
}
