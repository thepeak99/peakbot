//! The main window and its navigation guard (DESIGN §4.8/§5.4).
//! Built in Rust (not from `tauri.conf.json`) so `on_navigation` /
//! `on_new_window` can be attached.

use std::sync::{Arc, RwLock};

use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Manager, WebviewUrl, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::nav::{NavDecision, classify_navigation};

pub const MAIN_LABEL: &str = "main";

/// Create the `main` window on the bundled status page.
pub fn create_main(app: &AppHandle, nav: Arc<RwLock<Option<String>>>) -> tauri::Result<()> {
    let win =
        tauri::WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
            .title("PeakBot")
            .inner_size(1200.0, 800.0)
            .min_inner_size(800.0, 500.0)
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
                    if classify_navigation(&url, nav.read().expect("nav poisoned").as_deref())
                        != NavDecision::Block
                    {
                        let _ = app.opener().open_url(url.to_string(), None::<&str>);
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
