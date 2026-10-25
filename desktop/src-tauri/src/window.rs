//! The main window and its navigation guard (DESIGN §4.8/§5.4). Built in
//! Rust (not from `tauri.conf.json`) so `on_navigation` / `on_new_window`
//! can be attached. 0.24 (spec §9.10): there is ONE window — the
//! settings sentinel navigates it to the bundled `settings.html`.

use std::sync::{Arc, RwLock};

use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Manager, WebviewUrl, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::nav::{NavDecision, classify_navigation};

use crate::commands::ActionTx;

pub const MAIN_LABEL: &str = "main";

/// The Ctrl/Cmd+Shift+S handler (design #377 §4.1, kept for 0.24): an
/// init script on the main webview (every origin, incl. the remote
/// PeakBot UI) that navigates to the reserved sentinel; the nav guard
/// turns it into the in-window settings page — no IPC, no
/// global-shortcut plugin (works on Wayland).
const SETTINGS_SHORTCUT_SCRIPT: &str = r#"
addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.code === "KeyS") {
    e.preventDefault(); e.stopPropagation(); location.href = "peakbot-desktop:settings";
  }
}, true);
"#;

/// Create the `main` window on the bundled status page.
pub fn create_main(app: &AppHandle, nav: Arc<RwLock<Option<String>>>) -> tauri::Result<()> {
    let win =
        tauri::WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
            .title("PeakBot")
            .inner_size(1200.0, 800.0)
            .min_inner_size(800.0, 500.0)
            .initialization_script(SETTINGS_SHORTCUT_SCRIPT)
            .on_navigation({
                let app = app.clone();
                let nav = nav.clone();
                move |url: &tauri::Url| {
                    match classify_navigation(url, nav.read().expect("nav poisoned").as_deref()) {
                        NavDecision::Allow => true,
                        // Other http(s)/mailto: system browser, block in-app.
                        NavDecision::OpenExternal => {
                            let _ = app.opener().open_url(url.to_string(), None::<&str>);
                            false
                        }
                        // The reserved settings sentinel: block the
                        // navigation; show the bundled settings page in
                        // this same window (0.24, spec §9.10).
                        NavDecision::OpenSettings => {
                            request_settings_page(&app);
                            false
                        }
                        // The reserved restart sentinel: block the
                        // navigation; restart only the PeakBot server
                        // (0.24, spec §8.4).
                        NavDecision::Restart => {
                            if let Some(tx) = app.try_state::<ActionTx>() {
                                let _ = tx.0.send(Action::Restart);
                            }
                            false
                        }
                        // file:, data:, javascript:, …
                        NavDecision::Block => false,
                    }
                }
            })
            .on_new_window({
                let app = app.clone();
                let nav = nav.clone();
                move |url: tauri::Url, _features: NewWindowFeatures| {
                    // Never open an in-app popup; non-blocked URLs go to the
                    // system browser (DESIGN §4.8).
                    match classify_navigation(&url, nav.read().expect("nav poisoned").as_deref()) {
                        NavDecision::Allow | NavDecision::OpenExternal => {
                            let _ = app.opener().open_url(url.to_string(), None::<&str>);
                        }
                        NavDecision::OpenSettings => request_settings_page(&app),
                        NavDecision::Restart => {
                            if let Some(tx) = app.try_state::<ActionTx>() {
                                let _ = tx.0.send(Action::Restart);
                            }
                        }
                        // file:, data:, javascript:, …
                        NavDecision::Block => {}
                    }
                    NewWindowResponse::Deny
                }
            })
            .build()?;

    // Linux: trust PeakBot's CA in the webview for the live backend
    // origin (design #377b §4.5) — macOS keeps WebKit's error page
    // and the settings note (T-B4).
    #[cfg(target_os = "linux")]
    crate::tls_trust::install(&win, nav.clone());

    // Closing the window quits the app on every OS (DESIGN §5.4).
    win.on_window_event({
        let app = app.clone();
        move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                app.exit(0);
            }
        }
    });

    Ok(())
}

/// Show + unminimize + focus the main window (single-instance callback,
/// `Open PeakBot`, macOS `Reopen`).
pub fn focus_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(MAIN_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// The bundled settings page (0.24, spec §9.10): the `main` window
/// navigates to `settings.html` — there is no second settings window.
/// Entry points (tray, macOS menu, the sentinel nav guard) call this:
/// webview navigation must not run inside a nav/menu callback (deadlock
/// risk), so it is spawned on the async runtime (design #377 §4.1).
pub fn request_settings_page(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(win) = app.get_webview_window(MAIN_LABEL) else {
            return;
        };
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        if let Err(e) = win.navigate(crate::bridge::settings_page_url()) {
            log::error!("settings page not shown: {e}");
        }
    });
}
