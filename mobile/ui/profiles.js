/* PeakBot Mobile — the bundled Profiles page.
 *
 * Every profile open goes through `profile_open`, and Rust navigates
 * the webview (WebviewWindow::navigate). This file never steers the page
 * to a profile URL — a script-initiated load would make Android drop the
 * Profiles page from history, so Back would exit the app. The one anchor
 * in this UI (Install certificate…) is a top-level navigation the nav
 * guard classifies External → system browser.
 *
 * The always-first Local row ("On this phone"), its consent sheet and
 * the Local cold-start open.
 */
"use strict";

const { invoke } = window.__TAURI__.core;
const IS_IOS = /iPad|iPhone|iPod/.test(navigator.userAgent);
const PROBE_TIMEOUT_MS = 8000;
// Locked consent text: shown once, before Local first opens.
const LOCAL_CONSENT_TEXT =
  "Your messages are sent to the AI provider you configure, using your own API key.";

let view = null; // the last profiles_get() result
let unreachable = null; // the remote in the can't-reach panel, if any
let menuRemote = null; // the remote behind the open ⋯ menu

const $ = (id) => document.getElementById(id);
const show = (el) => {
  el.hidden = false;
};
const hide = (el) => {
  el.hidden = true;
};

/* ── Rendering ──────────────────────────────────────────────────────── */

function render() {
  const list = $("list");
  list.replaceChildren();
  const local = renderLocalRow(view);
  if (local) list.append(local);
  if (view.remotes.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No servers yet. Add the link PeakBot prints, or scan its QR code.";
    list.append(li);
  }
  for (const r of view.remotes) list.append(remoteRow(r));
}

// The Local row ("On this phone"): always first, never deletable or
// renamable, so no ⋯ menu. Tapping it runs the same path as a cold start.
function renderLocalRow(_view) {
  const li = document.createElement("li");
  li.className = "row";

  const open = document.createElement("button");
  open.className = "row-open";
  open.id = "local-open";
  open.setAttribute("aria-label", "Open PeakBot on this phone");
  const labels = document.createElement("span");
  labels.className = "row-labels";
  const name = document.createElement("span");
  name.className = "row-name";
  name.textContent = "On this phone";
  const sub = document.createElement("span");
  sub.className = "row-host";
  sub.textContent = "Uses your own API key";
  labels.append(name, sub);
  const badge = document.createElement("span");
  badge.className = "badge";
  badge.textContent = "Local";
  const chev = document.createElement("span");
  chev.className = "chevron";
  chev.setAttribute("aria-hidden", "true");
  chev.textContent = "›";
  open.append(labels, badge, chev);
  open.addEventListener("click", openLocal);

  li.append(open);
  return li;
}

function remoteRow(r) {
  const li = document.createElement("li");
  li.className = "row";

  const open = document.createElement("button");
  open.className = "row-open";
  open.setAttribute("aria-label", `Open ${r.name} (${r.host_label})`);
  const labels = document.createElement("span");
  labels.className = "row-labels";
  const name = document.createElement("span");
  name.className = "row-name";
  name.textContent = r.name;
  const host = document.createElement("span");
  host.className = "row-host";
  host.textContent = r.host_label;
  labels.append(name, host);
  const chev = document.createElement("span");
  chev.className = "chevron";
  chev.setAttribute("aria-hidden", "true");
  chev.textContent = "›";
  open.append(labels, chev);
  open.addEventListener("click", () => openRemote(r));

  const menu = document.createElement("button");
  menu.className = "row-menu";
  menu.setAttribute("aria-label", `More options for ${r.name}`);
  menu.setAttribute("aria-haspopup", "true");
  menu.textContent = "⋯";
  menu.addEventListener("click", (e) => {
    e.stopPropagation();
    rowMenu(r);
  });

  li.append(open, menu);
  return li;
}

function setStatus(text) {
  $("status").textContent = text;
  show($("status"));
}

function clearStatus() {
  hide($("status"));
}

/* ── Opening a remote (tap and cold start share this path) ──────────── */

// no-cors: any HTTP answer resolves, TLS/network failures reject — the
// same TLS stack and trust store as the real page load (T6).
async function probe(origin) {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), PROBE_TIMEOUT_MS);
  try {
    await fetch(origin, { mode: "no-cors", signal: ctrl.signal });
    return true;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

async function openRemote(r) {
  hideCantReach();
  setStatus(`Connecting to ${r.host_label}…`);
  if (await probe(r.origin)) {
    clearStatus();
    try {
      // Rust navigates the webview; this page stays in history.
      await invoke("profile_open", { target: { remote: r.id } });
    } catch (e) {
      setStatus(String(e));
    }
    return;
  }
  clearStatus();
  showCantReach(r);
}

/* ── Opening Local (tap and cold start share this path) ─────────────── */

// First use: the consent sheet. Continue records it (`local_consent_set`),
// Cancel leaves the list untouched. Rust refuses `profile_open("local")`
// without the recorded consent too — this sheet is not the only gate.
async function openLocal() {
  hideCantReach();
  clearStatus();
  try {
    if (!view.local.consented) {
      const ok = await openDialog({
        title: "On this phone",
        text: LOCAL_CONSENT_TEXT,
        okLabel: "Continue",
      });
      if (!ok) return;
      await invoke("local_consent_set");
      view = await invoke("profiles_get");
      render();
    }
    setStatus("Starting PeakBot on this phone…");
    // Rust navigates the webview; this page stays in history.
    await invoke("profile_open", { target: "local" });
    clearStatus();
  } catch (e) {
    // "Couldn't start PeakBot on this phone: …" / "Accept the notice first."
    setStatus(String(e));
  }
}

/* ── Can't-reach help (locked strings) ─────────────────────── */

function showCantReach(r) {
  unreachable = r;
  $("cr-host").textContent = r.host_label;
  $("cr-steps").textContent = IS_IOS
    ? "Install the certificate: download the profile in Safari, then Settings → VPN & Device Management → install, and Certificate Trust Settings → full trust."
    : "Install the certificate: Settings → Security → Install a certificate → CA.";
  // Top-level navigation: the nav guard sees it and opens the system
  // browser (External). Not the opener IPC — that permission is not
  // granted to this page.
  $("cr-install").setAttribute("href", `${r.origin}/peakbot-ca.crt`);
  show($("cant-reach"));
}

function hideCantReach() {
  unreachable = null;
  hide($("cant-reach"));
}

/* ── Add server (paste a link, or scan the pairing QR) ──────────────── */

function openAdd() {
  hide($("add-error"));
  show($("add"));
  hide($("add-server"));
  $("add-link").focus();
}

function closeAdd() {
  hide($("add"));
  hide($("add-error"));
  show($("add-server"));
}

async function submitAdd() {
  const text = $("add-link").value.trim();
  const name = $("add-name").value.trim();
  hide($("add-error"));
  try {
    await invoke("profile_add", { text, name: name || null });
    $("add-link").value = "";
    $("add-name").value = "";
    view = await invoke("profiles_get");
    render();
    closeAdd();
  } catch (e) {
    // The locked LinkError string, verbatim from Rust.
    const err = $("add-error");
    err.textContent = String(e);
    show(err);
  }
}

// The barcode-scanner plugin's commands, straight through core.invoke
// (no bundler, no guest JS package).
async function scanQR() {
  const core = window.__TAURI__.core;
  let camera = (await core.checkPermissions("barcode-scanner")).camera;
  if (!camera) camera = (await core.requestPermissions("barcode-scanner")).camera;
  const err = $("add-error");
  if (!camera) {
    err.textContent =
      "Camera access is needed to scan the QR code. Allow it in your phone's settings, then try again.";
    show(err);
    return;
  }
  try {
    const text = await core.invoke("plugin:barcode-scanner|scan", {
      formats: ["QR_CODE"],
    });
    $("add-link").value = text;
    submitAdd();
  } catch (e) {
    err.textContent = String(e);
    show(err);
  }
}

/* ── Dialogs: ⋯ menu, rename, delete ────────────────────────────────── */

let dialogResolve = null;
let lastFocused = null;

function openDialog(opts) {
  return new Promise((resolve) => {
    dialogResolve = resolve;
    lastFocused = document.activeElement;
    $("dialog-title").textContent = opts.title;
    const text = $("dialog-text");
    if (opts.text) {
      text.textContent = opts.text;
      show(text);
    } else {
      hide(text);
    }
    const input = $("dialog-input");
    if (opts.input !== undefined) {
      input.value = opts.input;
      show(input);
    } else {
      hide(input);
    }
    const ok = $("dialog-ok");
    ok.textContent = opts.okLabel || "OK";
    ok.className = "btn " + (opts.danger ? "danger" : "primary");
    $("dialog-menu").hidden = !opts.menu;
    $("dialog-actions").hidden = Boolean(opts.menu);
    show($("dialog"));
    (opts.menu ? $("menu-rename") : opts.input !== undefined ? input : ok).focus();
  });
}

function closeDialog(result) {
  hide($("dialog"));
  const resolve = dialogResolve;
  dialogResolve = null;
  if (lastFocused && lastFocused.focus) lastFocused.focus();
  lastFocused = null;
  if (resolve) resolve(result);
}

$("dialog-ok").addEventListener("click", () => closeDialog(true));
$("dialog-cancel").addEventListener("click", () => closeDialog(false));
$("dialog-input").addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    closeDialog(true);
  }
});
$("menu-rename").addEventListener("click", () => {
  const r = menuRemote;
  closeDialog("rename");
  if (r) renameRemote(r);
});
$("menu-delete").addEventListener("click", () => {
  const r = menuRemote;
  closeDialog("delete");
  if (r) deleteRemote(r);
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("dialog").hidden) closeDialog(false);
});

async function rowMenu(r) {
  menuRemote = r;
  const choice = await openDialog({ title: r.name, menu: true });
  menuRemote = null;
  if (choice === "rename") renameRemote(r);
  else if (choice === "delete") deleteRemote(r);
}

async function renameRemote(r) {
  let ok = await openDialog({ title: "Rename", text: r.name, input: r.name, okLabel: "Save" });
  while (ok) {
    const name = $("dialog-input").value.trim();
    try {
      await invoke("profile_rename", { id: r.id, name });
      view = await invoke("profiles_get");
      render();
      break;
    } catch (e) {
      // Keep the dialog open, show the locked error, let the user fix it.
      ok = await openDialog({ title: "Rename", text: String(e), input: name, okLabel: "Save" });
    }
  }
}

async function deleteRemote(r) {
  let ok = await openDialog({
    title: "Delete server",
    text: `Delete ${r.name}? This clears the saved logins on this phone, so you'll be signed out of your other servers too.`,
    okLabel: "Delete",
    danger: true,
  });
  while (ok) {
    try {
      await invoke("profile_remove", { id: r.id });
      if (unreachable && unreachable.id === r.id) hideCantReach();
      view = await invoke("profiles_get");
      render();
      break;
    } catch (e) {
      ok = await openDialog({
        title: "Delete server",
        text: String(e),
        okLabel: "Delete",
        danger: true,
      });
    }
  }
}

/* ── Wiring + cold start ────────────────────────────────── */

$("add-server").addEventListener("click", openAdd);
$("add-cancel").addEventListener("click", closeAdd);
$("add-submit").addEventListener("click", submitAdd);
$("add-scan").addEventListener("click", scanQR);
$("cr-retry").addEventListener("click", () => {
  if (unreachable) openRemote(unreachable);
});
$("cr-edit").addEventListener("click", () => {
  if (unreachable) renameRemote(unreachable);
});
$("cr-close").addEventListener("click", hideCantReach);

async function init() {
  try {
    view = await invoke("profiles_get");
  } catch (e) {
    setStatus(String(e));
    return;
  }
  render();
  // Cold start: a fresh process has an empty sessionStorage. Set the flag
  // first, then open `last` exactly like a tap (a remote that still
  // exists, or Local once the notice is accepted — never the consent sheet
  // unprompted) — so Back from the server lands here, and a later return
  // to this page never auto-opens again.
  if (sessionStorage.opened === undefined) {
    sessionStorage.opened = "1";
    const last = view.last;
    if (last === "local") {
      if (view.local.consented) openLocal();
    } else if (last && typeof last === "object" && typeof last.remote === "number") {
      const r = view.remotes.find((x) => x.id === last.remote);
      if (r) openRemote(r);
    }
  }
}

init();
