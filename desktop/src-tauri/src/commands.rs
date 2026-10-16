//! The IPC commands for the bundled local pages (DESIGN §5.4, amendment
//! #377): `status`/`act` for the status page, `settings_get`/`settings_set`
//! for the settings window. The remote PeakBot web UI has no permissions
//! at all (capability ACL).

use std::sync::Arc;
use std::sync::mpsc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::phase::{StatusView, UiAction};
use peakbot_desktop_core::settings::{self, SaveRequest, SettingsView, SettingsWire};
use peakbot_desktop_core::token::Token;

use crate::bridge::UiState;

/// The action channel into the controller thread.
#[derive(Clone)]
pub struct ActionTx(pub mpsc::Sender<Action>);

/// The current `StatusView` (polled by `ui/status.js` every 500 ms).
#[tauri::command]
pub fn status(state: State<'_, Arc<UiState>>) -> StatusView {
    state.snapshot.lock().expect("snapshot poisoned").clone()
}

/// A status-page button press (DESIGN §4.2/§5.4).
#[tauri::command]
pub fn act(action: UiAction, app: AppHandle, tx: State<'_, ActionTx>) -> Result<(), String> {
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
        // The settings window's background row (design #377 §5.2); the
        // status page never emits it.
        UiAction::ToggleBackground => {
            tx.0.send(Action::ToggleBackground)
                .map_err(|e| e.to_string())
        }
    }
}

/// The settings window's current view (design #377b §4.1): the saved
/// `settings.json` (missing/corrupt ⇒ defaults, read-only here), the
/// current web-token (dropped by the view when the token is off), and
/// the live tray/status state. Only the controller writes the files.
#[tauri::command]
pub fn settings_get(
    state: State<'_, Arc<UiState>>,
    paths: State<'_, DesktopPaths>,
) -> SettingsView {
    let text = std::fs::read_to_string(paths.settings_file()).ok();
    let settings = settings::load_settings(text.as_deref());
    let token = std::fs::read_to_string(paths.web_token())
        .ok()
        .and_then(|t| Token::parse(&t));
    let tray = state
        .tray_model
        .lock()
        .expect("tray_model poisoned")
        .clone();
    let snapshot = state.snapshot.lock().expect("snapshot poisoned").clone();
    settings::settings_view(settings, token, &tray, &snapshot, &paths.web_token())
}

/// Save the settings + the optional new token from the settings window
/// (design #377b §4.3). `SaveRequest::parse` is THE boundary check: an
/// illegal wire value or a bad token is rejected verbatim and nothing
/// reaches the controller or the disk. An empty token keeps the
/// current one (or generates one).
#[tauri::command]
pub fn settings_set(
    settings: SettingsWire,
    token: String,
    tx: State<'_, ActionTx>,
) -> Result<(), String> {
    let req = SaveRequest::parse(settings, &token)?;
    tx.0.send(Action::SaveSettings(req))
        .map_err(|e| e.to_string())
}
