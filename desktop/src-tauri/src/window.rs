//! The main window and its navigation guard (DESIGN §4.8/§5.4), plus the
//! settings window (design #377 §4.1). Built in Rust (not from
//! `tauri.conf.json`) so `on_navigation` / `on_new_window` can be attached.

use std::sync::{Arc, RwLock};

use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Manager, WebviewUrl, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::nav::{NavDecision, classify_navigation};

pub const MAIN_LABEL: &str = "main";
pub const SETTINGS_LABEL: &str = "settings";

/// The Ctrl/Cmd+Shift+S handler (design #377 §4.1): an init script on the
/// main webview (every origin, incl. the remote PeakBot UI) that navigates
/// to the reserved sentinel; the nav guard turns it into the settings
/// window — no IPC, no global-shortcut plugin (works on Wayland).
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
                        // navigation; open the settings window (design
                        // #377 §4.1).
                        NavDecision::OpenSettings => {
                            request_settings(&app);
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
                        NavDecision::OpenSettings => request_settings(&app),
                        // file:, data:, javascript:, …
                        NavDecision::Block => {}
                    }
                    NewWindowResponse::Deny
                }
            })
            .build()?;

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

/// The settings window (design #377 §4.1): a second trigger focuses the
/// existing one. Local origin only — `OpenExternal` goes to the system
/// browser; the settings sentinel and everything else is blocked.
fn open_settings(app: &AppHandle) -> tauri::Result<()> {
    if let Some(win) = app.get_webview_window(SETTINGS_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }

    tauri::WebviewWindowBuilder::new(app, SETTINGS_LABEL, WebviewUrl::App("settings.html".into()))
        .title("PeakBot Settings")
        .inner_size(520.0, 600.0)
        .min_inner_size(420.0, 480.0)
        .on_navigation({
            let app = app.clone();
            move |url: &tauri::Url| match classify_navigation(url, None) {
                // The bundled settings page origin.
                NavDecision::Allow => true,
                NavDecision::OpenExternal => {
                    let _ = app.opener().open_url(url.to_string(), None::<&str>);
                    false
                }
                // The window never leaves the local origin (I6).
                NavDecision::OpenSettings | NavDecision::Block => false,
            }
        })
        .build()?;
    Ok(())
}

/// Entry points (tray, macOS menu, the sentinel nav guard) call this:
/// window construction must not run inside a nav/menu callback (deadlock
/// risk), so it is spawned on the async runtime (design #377 §4.1).
pub fn request_settings(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = open_settings(&app) {
            log::error!("settings window not created: {e}");
        }
    });
}
