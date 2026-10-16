// peakbot-desktop-status-page
// Polls the `status` command every 500 ms and renders the StatusView.
// No build step: talks to the window through `window.__TAURI__`
// (withGlobalTauri). On the remote PeakBot UI these calls are denied
// by the capability ACL — this file is only loaded on the local page.

const $ = (id) => document.getElementById(id);

const LABELS = {
  retry: "Retry",
  open_logs: "Open logs",
  reinstall_service: "Reinstall background service",
  quit: "Quit",
};

const tauri = window.__TAURI__;

function render(view) {
  $("title").textContent = view.title;
  $("title").classList.toggle("error", view.kind === "failed");
  $("spinner").hidden = view.kind === "failed";

  const detail = $("detail");
  detail.hidden = view.detail == null;
  if (view.detail != null) {
    detail.textContent = view.detail;
  }

  const actions = $("actions");
  actions.textContent = "";
  for (const action of view.actions) {
    const button = document.createElement("button");
    button.textContent = LABELS[action] ?? action;
    if (action === "retry") {
      button.classList.add("primary");
    }
    button.onclick = () => tauri.core.invoke("act", { action });
    actions.appendChild(button);
  }
}

async function tick() {
  try {
    render(await tauri.core.invoke("status"));
  } catch (err) {
    console.error("status poll failed", err);
  }
}

if (!tauri) {
  $("title").textContent = "Internal error: no Tauri API";
  $("title").classList.add("error");
  $("spinner").hidden = true;
} else {
  tick();
  setInterval(tick, 500);
}
