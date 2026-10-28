//! The IPC commands (DESIGN §5.4): `status`, `act` and `theme_get` for
//! the bundled status page. The remote PeakBot web UI (loopback :7823
//! only, `capabilities/remote-web.json`) may invoke exactly `theme_set`,
//! `update_status` and `update_install` — never `status`, `act` (it
//! carries `quit`) or `theme_get`.

use std::sync::Arc;
use std::sync::mpsc;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::phase::{StatusView, UiAction};
use peakbot_desktop_core::theme::Theme;
use peakbot_desktop_core::update::{self, UpdateView};

use crate::bridge::UiState;

/// The action channel into the controller thread.
#[derive(Clone)]
pub struct ActionTx(pub mpsc::Sender<Action>);

/// The event channel into the updater thread. Managed only when this
/// build is updatable (portable/dev builds never manage it, so the
/// web UI gets a clean "updates unsupported" error).
#[derive(Clone)]
pub struct UpdateTx(pub mpsc::Sender<update::Event>);

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
    }
}

/// The web UI reports its current theme; the bundled pages read it back
/// with `theme_get` so they match the app.
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

/// The update banner view for the web UI; `null` on the wire until the
/// updater has something to show (plan wire format).
#[tauri::command]
pub fn update_status(state: State<'_, Arc<UiState>>) -> Option<UpdateView> {
    state.update.lock().expect("update poisoned").clone()
}

/// The web UI's "Restart to update" / retry button: asks the updater
/// thread to install. `try_state` = `None` when this build is not
/// updatable (the updater thread is never started).
#[tauri::command]
pub fn update_install(app: AppHandle) -> Result<(), String> {
    let Some(tx) = app.try_state::<UpdateTx>() else {
        return Err("updates unsupported".into());
    };
    tx.0.send(update::Event::Install).map_err(|e| e.to_string())
}
