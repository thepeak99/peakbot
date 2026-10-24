//! The tray menu (DESIGN §5.4). Item ids are locked: `status`, `open`,
//! `settings`, `background`, `logs`, `quit`.

use std::sync::Arc;

use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::phase::{TrayModel, background_offered, host_platform};

use crate::bridge::UiState;
use crate::commands::ActionTx;
use crate::window;

pub const TRAY_ID: &str = "peakbot-tray";

/// The items a `TrayModel` updates (everything else is static). Kept as
/// clones: 2.12's `TrayIcon` exposes no `get_menu()`. `background` is
/// `None` when the platform doesn't offer the background service (Q1:
/// Windows in v1) — the item is then absent from the menu entirely.
pub struct TrayItems {
    pub status: MenuItem<Wry>,
    pub background: Option<CheckMenuItem<Wry>>,
}

fn build_dynamic_items(app: &AppHandle, background_visible: bool) -> tauri::Result<TrayItems> {
    let status = MenuItem::with_id(app, "status", "PeakBot", false, None::<&str>)?;
    let background = (background_visible)
        .then(|| {
            CheckMenuItem::with_id(
                app,
                "background",
                "Keep PeakBot running in background",
                true,
                false,
                None::<&str>,
            )
        })
        .transpose()?;
    Ok(TrayItems { status, background })
}

/// Build the tray icon + menu and stash the dynamic items in `state`.
pub fn create(app: &AppHandle, state: &Arc<UiState>) -> tauri::Result<()> {
    // Q1: the background item is offered per platform (hidden on
    // Windows in v1) — decided once, from the pure core.
    let background_visible = background_offered(host_platform());
    let items = build_dynamic_items(app, background_visible)?;
    let open = MenuItem::with_id(app, "open", "Open PeakBot", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Open logs folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit PeakBot Desktop", true, None::<&str>)?;

    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let sep3 = PredefinedMenuItem::separator(app)?;
    let mut menu_items: Vec<&dyn IsMenuItem<Wry>> =
        vec![&items.status, &sep1, &open, &settings, &sep2];
    if let Some(background) = &items.background {
        menu_items.push(background);
    }
    menu_items.push(&logs);
    menu_items.push(&sep3);
    menu_items.push(&quit);
    let menu = Menu::with_items(app, &menu_items)?;

    let Some(icon) = app.default_window_icon().cloned() else {
        log::error!("no default window icon; tray not created");
        return Ok(());
    };

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("PeakBot")
        .icon(icon)
        .menu(&menu)
        // Left-click shows the menu (Linux appindicator + macOS).
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        .build(app);
    match tray {
        Ok(_tray) => {
            *state.tray_items.lock().expect("tray_items poisoned") = Some(items);
        }
        // No StatusNotifier/XEmbed target (e.g. minimal X session): the
        // app must still work without a tray (DESIGN risk R6).
        Err(e) => {
            log::error!("tray icon not created: {e}");
        }
    }

    // macOS: the app (first) menu is set by `menu::app_menu` at build time
    // (it carries the PeakBot submenu + the default menus minus "Close
    // Window", issue #373) — `set_menu` here would overwrite it.
    Ok(())
}

/// Update the live tray items from a `TrayModel`.
pub fn apply(state: &Arc<UiState>, model: &TrayModel) {
    let guard = state.tray_items.lock().expect("tray_items poisoned");
    let Some(items) = guard.as_ref() else {
        return;
    };
    let _ = items.status.set_text(model.status.as_str());
    if let Some(background) = &items.background {
        let _ = background.set_text(model.background_label.as_str());
        let _ = background.set_checked(model.background_checked);
        let _ = background.set_enabled(model.background_enabled);
    }
}

/// Shared by the tray menu and the macOS app menu (same locked ids).
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "open" => window::focus_main(app),
        "settings" => window::request_settings_page(app),
        "background" => {
            // muda auto-toggles a check item on click, before we get
            // here — re-apply the last model, then act (the controller
            // re-emits the tray on the next phase change).
            if let Some(state) = app.try_state::<Arc<UiState>>() {
                let model = state
                    .tray_model
                    .lock()
                    .expect("tray_model poisoned")
                    .clone();
                apply(&state, &model);
            }
            if let Some(tx) = app.try_state::<ActionTx>() {
                let _ = tx.0.send(Action::ToggleBackground);
            }
        }
        "logs" => {
            if let Some(paths) = app.try_state::<DesktopPaths>() {
                let _ = app
                    .opener()
                    .open_path(paths.log_dir.to_string_lossy().into_owned(), None::<&str>);
            }
        }
        "quit" => app.exit(0),
        _ => {}
    }
}

/// macOS only: the `PeakBot` app submenu (same locked ids as the tray
/// menu), used by `menu::app_menu` (issue #373).
#[cfg(target_os = "macos")]
pub fn build_app_submenu(app: &AppHandle) -> tauri::Result<tauri::menu::Submenu<tauri::Wry>> {
    use tauri::menu::Submenu;

    let items = build_dynamic_items(app, true)?; // macOS ⇒ Unix ⇒ always offered
    let open = MenuItem::with_id(app, "open", "Open PeakBot", true, None::<&str>)?;
    // The macOS accelerator (design #377 §5.3); the same id flows through
    // `handle_menu_event` on both the tray and the app menu.
    let settings = MenuItem::with_id(
        app,
        "settings",
        "Settings…",
        true,
        Some("CmdOrCtrl+Shift+S"),
    )?;
    let logs = MenuItem::with_id(app, "logs", "Open logs folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit PeakBot Desktop", true, None::<&str>)?;

    let peakbot = Submenu::with_id(app, "peakbot", "PeakBot", true)?;
    peakbot.append_items(&[
        &items.status,
        &open,
        &settings,
        &PredefinedMenuItem::separator(app)?,
        &items
            .background
            .as_ref()
            .expect("background is offered on macOS (Unix)"),
        &logs,
        &PredefinedMenuItem::separator(app)?,
        &quit,
    ])?;
    Ok(peakbot)
}
