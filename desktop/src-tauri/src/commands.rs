//! The two IPC commands for the bundled status page (DESIGN §5.4).
//! The remote PeakBot web UI has no permissions at all (capability ACL).

use std::sync::Arc;
use std::sync::mpsc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::phase::{StatusView, UiAction};

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
        // The settings page's background row (design #377 §5.2); the
        // status page never emits it, so this arm is only reachable
        // from the (later) settings window.
        UiAction::ToggleBackground => {
            tx.0.send(Action::ToggleBackground)
                .map_err(|e| e.to_string())
        }
    }
}
