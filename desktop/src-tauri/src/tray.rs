//! The tray menu (DESIGN §5.4). Item ids are locked: `status`, `open`,
//! `background`, `logs`, `quit`.

use std::sync::Arc;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::controller::Action;
use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::phase::TrayModel;

use crate::bridge::UiState;
use crate::commands::ActionTx;
use crate::window;

pub const TRAY_ID: &str = "peakbot-tray";

/// The items a `TrayModel` updates (everything else is static). Kept as
/// clones: 2.12's `TrayIcon` exposes no `get_menu()`.
pub struct TrayItems {
    pub status: MenuItem<Wry>,
    pub background: CheckMenuItem<Wry>,
}

fn build_dynamic_items(app: &AppHandle) -> tauri::Result<TrayItems> {
    let status = MenuItem::with_id(app, "status", "PeakBot", false, None::<&str>)?;
    let background = CheckMenuItem::with_id(
        app,
        "background",
        "Keep PeakBot running in background",
        true,
        false,
        None::<&str>,
    )?;
    Ok(TrayItems { status, background })
}

/// Build the tray icon + menu and stash the dynamic items in `state`.
pub fn create(app: &AppHandle, state: &Arc<UiState>) -> tauri::Result<()> {
    let items = build_dynamic_items(app)?;
    let open = MenuItem::with_id(app, "open", "Open PeakBot", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Open logs folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit PeakBot Desktop", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &items.status,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &items.background,
            &logs,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

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

    // macOS: the same items as the app (first) menu, keeping Tauri's
    // default Edit/View/Window menus (needed for ⌘C/⌘V/⌘Q).
    #[cfg(target_os = "macos")]
    attach_app_menu(app)?;

    Ok(())
}

/// Update the live tray items from a `TrayModel`.
pub fn apply(state: &Arc<UiState>, model: &TrayModel) {
    let guard = state.tray_items.lock().expect("tray_items poisoned");
    let Some(items) = guard.as_ref() else {
        return;
    };
    let _ = items.status.set_text(model.status.as_str());
    let _ = items.background.set_text(model.background_label.as_str());
    let _ = items.background.set_checked(model.background_checked);
    let _ = items.background.set_enabled(model.background_enabled);
}

/// Shared by the tray menu and the macOS app menu (same locked ids).
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "open" => window::focus_main(app),
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

/// macOS only: a `PeakBot` app submenu with the same items, replacing
/// Tauri's default app menu (DESIGN §5.4).
#[cfg(target_os = "macos")]
fn attach_app_menu(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::Submenu;

    let items = build_dynamic_items(app)?;
    let open = MenuItem::with_id(app, "open", "Open PeakBot", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "Open logs folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit PeakBot Desktop", true, None::<&str>)?;

    let peakbot = Submenu::with_id(app, "peakbot", "PeakBot", true)?;
    peakbot.append_items(&[
        &items.status,
        &open,
        &PredefinedMenuItem::separator(app)?,
        &items.background,
        &logs,
        &PredefinedMenuItem::separator(app)?,
        &quit,
    ])?;

    // Keep the default menus, swap the first (app) item for ours.
    let mut menu = Menu::new(app)?;
    menu.append_items(&[&peakbot])?;
    for item in Menu::default(app)?.items()?.into_iter().skip(1) {
        menu.append_items(&[&item])?;
    }

    app.set_menu(menu)?;
    // The app menu's events (not routed through the tray builder).
    app.on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()));
    Ok(())
}
