//! The IPC commands (DESIGN §5.4, amendment #377, desktop-pane design):
//! `status`/`act` for the status page; `settings_get`, `toggle_background`
//! and `theme_get` for the bundled settings page. The remote PeakBot web
//! UI (loopback :7823 only, `capabilities/desktop-pane.json`) may invoke
//! exactly `settings_get`, `toggle_background` and `theme_set` — never
//! `status`, `act` (it carries `quit`) or `theme_get`.

use std::sync::Arc;
use std::sync::mpsc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::phase::{StatusView, UiAction};
use peakbot_desktop_core::settings::{self, SettingsView, Theme};

use crate::bridge::UiState;

/// The action channel into the controller thread.
#[derive(Clone)]
pub struct ActionTx(pub mpsc::Sender<Action>);

/// The current `StatusView` (polled by `ui/status.js` every 500 ms).
#[tauri::command]
pub fn status(state: State<'_, Arc<UiState>>) -> StatusView {
    state.snapshot.lock().expect("snapshot poisoned").clone()
}

/// A local-page button press (DESIGN §4.2/§5.4): status-page actions
/// and the settings page's `← Back to PeakBot` (0.24, spec §9.10).
#[tauri::command]
pub fn act(
    action: UiAction,
    app: AppHandle,
    tx: State<'_, ActionTx>,
    ui: State<'_, Arc<UiState>>,
) -> Result<(), String> {
    match action {
        UiAction::Retry => tx.0.send(Action::Start).map_err(|e| e.to_string()),
        UiAction::ReinstallService => {
            tx.0.send(Action::ReinstallService)
                .map_err(|e| e.to_string())
        }
        UiAction::OpenLogs => {
            let Some(paths) = app.try_state::<DesktopPaths>() else {
                return Err("paths not initialised".into());
            };
            app.opener()
                .open_path(paths.log_dir.to_string_lossy().into_owned(), None::<&str>)
                .map_err(|e| e.to_string())
        }
        UiAction::Quit => {
            app.exit(0);
            Ok(())
        }
        // The settings page's `← Back to PeakBot` (0.24, spec §9.10):
        // the main window goes back to the live backend URL, or the
        // status page if the backend is not Ready.
        UiAction::Back => {
            let backend = ui.backend_url.lock().expect("backend_url poisoned").clone();
            let target = match backend {
                Some(url) => url.parse::<tauri::Url>().map_err(|e| e.to_string())?,
                None => crate::bridge::local_page_url(),
            };
            let Some(win) = app.get_webview_window(crate::window::MAIN_LABEL) else {
                return Err("main window not found".into());
            };
            win.navigate(target).map_err(|e| e.to_string())
        }
    }
}

/// The settings page's current view: the live tray/status state only
/// (0.24 config, spec §8.4 — no settings.json, no web-token).
#[tauri::command]
pub fn settings_get(state: State<'_, Arc<UiState>>) -> SettingsView {
    let tray = state
        .tray_model
        .lock()
        .expect("tray_model poisoned")
        .clone();
    let snapshot = state.snapshot.lock().expect("snapshot poisoned").clone();
    settings::settings_view(&tray, &snapshot)
}

/// Toggle the background service (design #377 §5.2; desktop-pane D3). The
/// controller decides enable vs disable from its own `ServiceState` and
/// confirms with a dialog, so a caller can only *ask*. A dedicated
/// command (not an `act` action) so the remote web UI can be granted it
/// without ever getting `act`.
#[tauri::command]
pub fn toggle_background(tx: State<'_, ActionTx>) -> Result<(), String> {
    tx.0.send(Action::ToggleBackground)
        .map_err(|e| e.to_string())
}

/// The web UI reports its current theme (desktop-pane design §3); the
/// bundled pages read it back with `theme_get` so they match the app.
#[tauri::command]
pub fn theme_set(theme: Theme, state: State<'_, Arc<UiState>>) {
    *state.theme.lock().expect("theme poisoned") = Some(theme);
}

/// The last reported theme, `None` until the web UI has reported one.
/// Local pages only — the remote web UI never reads it.
#[tauri::command]
pub fn theme_get(state: State<'_, Arc<UiState>>) -> Option<Theme> {
    *state.theme.lock().expect("theme poisoned")
}
