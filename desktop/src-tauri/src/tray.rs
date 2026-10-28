//! The tray menu (DESIGN §5.4). Item ids are locked: `status`,
//! `update`, `open`, `logs`, `quit`.

use std::sync::Arc;

use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

use peakbot_desktop_core::paths::DesktopPaths;
use peakbot_desktop_core::update;

use crate::bridge::UiState;
use crate::commands::UpdateTx;
use crate::window;

pub const TRAY_ID: &str = "peakbot-tray";

/// The fixed items, in menu order after the status item.
fn static_items(app: &AppHandle) -> tauri::Result<(MenuItem<Wry>, MenuItem<Wry>, MenuItem<Wry>)> {
    Ok((
        MenuItem::with_id(app, "open", "Open PeakBot", true, None::<&str>)?,
        MenuItem::with_id(app, "logs", "Open logs folder", true, None::<&str>)?,
        MenuItem::with_id(app, "quit", "Quit PeakBot", true, None::<&str>)?,
    ))
}

fn status_item(app: &AppHandle) -> tauri::Result<MenuItem<Wry>> {
    MenuItem::with_id(app, "status", "PeakBot", false, None::<&str>)
}

/// The `update` item (initial label per `core::update::menu_label(Idle)`;
/// the updater thread rewrites it on every state change).
fn update_item(app: &AppHandle) -> tauri::Result<MenuItem<Wry>> {
    MenuItem::with_id(app, "update", "Check for updates…", true, None::<&str>)
}

/// Build the tray icon + menu and stash the status + update items in
/// `state` (the `update` item only when this build is updatable).
pub fn create(app: &AppHandle, state: &Arc<UiState>) -> tauri::Result<()> {
    let status = status_item(app)?;
    let (open, logs, quit) = static_items(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let update = crate::updater::supported()
        .then(|| update_item(app))
        .transpose()?;
    let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![&status];
    if let Some(ref u) = update {
        items.push(u);
    }
    items.push(&sep1);
    items.push(&open);
    items.push(&logs);
    items.push(&sep2);
    items.push(&quit);
    let menu = Menu::with_items(app, &items)?;

    let Some(icon) = app.default_window_icon().cloned() else {
        log::error!("no default window icon; tray not created");
        return Ok(());
    };

    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("PeakBot")
        .icon(icon)
        .menu(&menu)
        // Windows convention: left-click opens the app, right-click the
        // menu. Linux appindicator + macOS show the menu on left-click.
        .show_menu_on_left_click(!cfg!(windows))
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()));
    #[cfg(windows)]
    let builder = builder.on_tray_icon_event(|tray, event| {
        use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            window::focus_main(tray.app_handle());
        }
    });
    match builder.build(app) {
        Ok(_tray) => {
            *state
                .tray_status_item
                .lock()
                .expect("tray_status_item poisoned") = Some(status);
            if let Some(u) = update {
                state
                    .update_items
                    .lock()
                    .expect("update_items poisoned")
                    .push(u);
            }
        }
        // The app must still work without a tray (DESIGN risk R6); ✕ then
        // asks before quitting (`is_visible`).
        Err(e) => {
            log::error!("tray icon not created: {e}");
        }
    }

    // macOS: the app (first) menu is set by `menu::app_menu` at build time
    // (it carries the PeakBot submenu + the default menus minus "Close
    // Window", issue #373) — `set_menu` here would overwrite it.
    Ok(())
}

/// Update the tray's status item.
pub fn apply(state: &Arc<UiState>, status: &str) {
    let guard = state
        .tray_status_item
        .lock()
        .expect("tray_status_item poisoned");
    if let Some(item) = guard.as_ref() {
        let _ = item.set_text(status);
    }
}

/// Can the user get the hidden window back from the tray? Probed on every
/// ✕ so a tray host that starts later counts.
pub fn is_visible(app: &AppHandle) -> bool {
    app.tray_by_id(TRAY_ID).is_some() && host_present()
}

/// Linux: appindicator "creates" the icon even with no StatusNotifier host
/// (stock GNOME), so ask the session bus whether a watcher is running.
/// Any D-Bus error counts as no host.
#[cfg(target_os = "linux")]
fn host_present() -> bool {
    use gio::glib::{ToVariant, VariantTy};

    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) else {
        return false;
    };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
        Some(VariantTy::new("(b)").expect("static variant type")),
        gio::DBusCallFlags::NONE,
        500,
        None::<&gio::Cancellable>,
    )
    .ok()
    .and_then(|reply| reply.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}

/// macOS menu bar / Windows notification area: always there.
#[cfg(not(target_os = "linux"))]
fn host_present() -> bool {
    true
}

/// Shared by the tray menu and the macOS app menu (same locked ids).
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "open" => window::focus_main(app),
        "update" => {
            // `UpdateTx` is managed only on updatable builds.
            if let Some(tx) = app.try_state::<UpdateTx>() {
                let _ = tx.0.send(update::Event::Click);
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
/// menu), used by `menu::app_menu` (issue #373). Its status item is a
/// static placeholder: only the tray's copy is updated. Its `update`
/// item is kept in sync — the updater thread renders every stored
/// item.
#[cfg(target_os = "macos")]
pub fn build_app_submenu(
    app: &AppHandle,
    state: &Arc<UiState>,
) -> tauri::Result<tauri::menu::Submenu<tauri::Wry>> {
    use tauri::menu::Submenu;

    let status = status_item(app)?;
    let (open, logs, quit) = static_items(app)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let update = crate::updater::supported()
        .then(|| update_item(app))
        .transpose()?;
    let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![&status];
    if let Some(ref u) = update {
        items.push(u);
    }
    items.push(&open);
    items.push(&logs);
    items.push(&sep);
    items.push(&quit);
    let peakbot = Submenu::with_id(app, "peakbot", "PeakBot", true)?;
    peakbot.append_items(&items)?;
    if let Some(u) = update {
        state
            .update_items
            .lock()
            .expect("update_items poisoned")
            .push(u);
    }
    Ok(peakbot)
}
