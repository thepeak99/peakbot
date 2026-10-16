// peakbot-desktop-settings-page
// Polls `settings_get` every 1000 ms and renders the SettingsView (design
// #377 §4.2). No build step: talks to the window through `window.__TAURI__`
// (withGlobalTauri). This file only runs on the bundled settings window;
// the remote PeakBot UI has no permissions at all (capability ACL).

const $ = (id) => document.getElementById(id);

const tauri = window.__TAURI__;

// The last saved wire value (what `settings.json` holds); the form is
// dirty iff the form values differ from it. `savedToken` is the last
// saved token value ("" when off or absent); an emptied field is NOT
// dirty ("keep the current token, or generate one").
let saved = null;
let savedToken = "";
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

function tokenDirty() {
  const v = $("token-value").value.trim();
  return $("token").checked && v !== "" && v !== savedToken;
}

function isDirty() {
  const v = formValues();
  return (
    saved !== null &&
    (v.protocol !== saved.protocol ||
      v.token !== saved.token ||
      v.remote !== saved.remote ||
      tokenDirty())
  );
}

function setForm(v) {
  protocolRadio(v.protocol).checked = true;
  $("token").checked = v.token;
  $("remote").checked = v.remote;
}

// The HTTPS note (design #377b §4.2, locked strings): the app window
// trusts PeakBot's CA on Linux only; on macOS it shows a warning until
// the certificate is trusted.
function protocolNote(certWarning) {
  return certWarning
    ? "PeakBot uses its own certificate. On macOS the app window shows a certificate warning until that certificate is trusted; browsers and phones work after you accept it once."
    : "PeakBot uses its own certificate. This window trusts it automatically; other browsers and phones need to accept it once.";
}

function updateDynamic() {
  const v = formValues();
  // Remote forces HTTPS + token on and locks both controls (UX only —
  // core still validates at the IPC boundary).
  protocolRadio("http").disabled = v.remote;
  protocolRadio("https").disabled = v.remote;
  $("token").disabled = v.remote;

  // The token field + buttons only exist while the token is on; Copy
  // needs a non-empty value.
  $("token-field").hidden = !v.token;
  $("token-copy").disabled = !v.token || $("token-value").value.trim() === "";

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
    $("protocol-note").textContent = protocolNote(view.cert_warning);
    if (!isDirty()) {
      saved = view.settings;
      setForm(view.settings);
      // Refill the token field from the view — never over a field the
      // user is editing (cleared it to type, or mid-generation).
      if (document.activeElement !== $("token-value")) {
        savedToken = view.token ?? "";
        $("token-value").value = savedToken;
      }
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

  $("token").addEventListener("change", updateDynamic);

  // Generate: 32 random bytes → 64 hex chars (the same shape as
  // `RealSys::random_token`). Core still validates the value on Save.
  $("token-generate").addEventListener("click", () => {
    const bytes = crypto.getRandomValues(new Uint8Array(32));
    $("token-value").value = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
    updateDynamic();
  });

  // Copy: the async clipboard first, the select+execCommand fallback
  // second (R-A3: `tauri://` is a secure scheme, but this covers it if
  // not).
  $("token-copy").addEventListener("click", async () => {
    const field = $("token-value");
    try {
      await navigator.clipboard.writeText(field.value);
    } catch {
      field.select();
      document.execCommand("copy");
    }
  });

  $("token-value").addEventListener("input", updateDynamic);

  // Save (enabled iff dirty && !busy). A validation error comes back
  // verbatim from core; on success the poll clears the dirty state
  // once the file reflects the values (token included: the field is
  // dirty until `view.token === field.trim()`). Cancelling the native
  // confirm dialog leaves the form dirty with the user's values.
  $("form").addEventListener("submit", async (e) => {
    e.preventDefault();
    if (!isDirty() || busy) return;
    $("error").textContent = "";
    try {
      await tauri.core.invoke("settings_set", {
        settings: formValues(),
        token: $("token-value").value,
      });
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
