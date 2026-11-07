//! The main window and its navigation guard (DESIGN §4.8/§5.4). Built in
//! Rust (not from `tauri.conf.json`) so `on_navigation` / `on_new_window`
//! can be attached.

use std::sync::{Arc, RwLock};

use tauri::webview::{NewWindowFeatures, NewWindowResponse};
use tauri::{AppHandle, Manager, WebviewUrl, WindowEvent};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::nav::{NavDecision, classify_navigation};
use peakbot_desktop_core::phase::close_app_dialog;

use crate::commands::ActionTx;

pub const MAIN_LABEL: &str = "main";

/// The init script that tags this window's origin as the desktop shell. Byte-for-byte
/// the mobile init-script's `peakbotShell` definition with `"desktop"` in place of
/// `"mobile"` (mobile plan §2.3). Injected on every load, including the remote
/// :7823 web UI, via `.initialization_script` on the main window builder.
pub const SHELL_MARKER_SCRIPT: &str = "Object.defineProperty(window,'peakbotShell',{value:'desktop'});";

/// Create the `main` window on the bundled status page.
pub fn create_main(app: &AppHandle, nav: Arc<RwLock<Option<String>>>) -> tauri::Result<()> {
    let win =
        tauri::WebviewWindowBuilder::new(app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
            .title("PeakBot")
            .inner_size(1200.0, 800.0)
            .min_inner_size(800.0, 500.0)
            // Tauri re-runs init scripts on every load in this window, so one
            // call covers the bundled page and the remote :7823 web UI alike.
            .initialization_script(SHELL_MARKER_SCRIPT)
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
    // (T-B4).
    #[cfg(target_os = "linux")]
    crate::tls_trust::install(&win, nav.clone());

    // ✕ keeps PeakBot running in the tray; with no tray to come back
    // from, it asks, since quitting stops the server (DESIGN §5.4). The
    // dialog must be the async `show`: this runs on the main thread.
    win.on_window_event({
        let app = app.clone();
        let win = win.clone();
        move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if crate::tray::is_visible(&app) {
                    let _ = win.hide();
                } else {
                    let exit = app.clone();
                    crate::bridge::native_dialog(&app, &close_app_dialog())
                        .parent(&win)
                        .show(move |ok| {
                            if ok {
                                exit.exit(0);
                            }
                        });
                }
            }
        }
    });

    Ok(())
}

/// Show + unminimize + focus the main window (single-instance callback,
/// `Open PeakBot`, Windows tray left-click, macOS `Reopen`).
pub fn focus_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(MAIN_LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

// ─── shell marker (mobile plan §2.5, task F1) ────────────────────────────
//
// The desktop webview gets the SAME init-script seam as the mobile shell:
// `window.peakbotShell = 'desktop'`. The SPA's `shell()` (web/src/desktop.ts)
// reads the marker and stops consulting `window.isTauri`; a Tauri window
// without the marker must read as a plain browser.
//
// RED: SHELL_MARKER_SCRIPT does not exist yet — F1 adds it here and injects
// it on the main window via `.initialization_script(SHELL_MARKER_SCRIPT)`.

#[cfg(test)]
mod shell_marker_tests {
    use super::SHELL_MARKER_SCRIPT;

    /// The exact marker definition — byte-for-byte the mobile init script's
    /// definition (plan §2.3) with 'desktop' in place of 'mobile'.
    const MARKER_DEF: &str = "Object.defineProperty(window,'peakbotShell',{value:'desktop'});";

    #[test]
    fn desktop_init_script_defines_peakbot_shell_as_desktop() {
        assert!(
            SHELL_MARKER_SCRIPT.contains(MARKER_DEF),
            "the init script must define peakbotShell as 'desktop'; got: {SHELL_MARKER_SCRIPT}"
        );
    }

    #[test]
    fn desktop_init_script_never_carries_the_mobile_marker() {
        assert!(
            !SHELL_MARKER_SCRIPT.contains("{value:'mobile'}"),
            "desktop must not carry the mobile marker; got: {SHELL_MARKER_SCRIPT}"
        );
    }
}
