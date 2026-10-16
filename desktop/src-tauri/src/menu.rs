// macOS app menu (issue #373): Tauri's default menu **minus "Close
// Window"**. The default File submenu is only `CloseWindow`, so dropping it
// removes the menu's Cmd+W; the JS keydown (capture) then closes the active
// conversation tab instead of quitting the app. The app (PeakBot) submenu
// and Edit/View/Help are unchanged — Edit must stay, copy/paste breaks
// without it — and the Window submenu keeps only minimize and maximize.
//
// The PeakBot app submenu is built here (not in `tray::create`) because the
// builder menu is set at `build()` time and `tray::create` runs in `setup`,
// where a `set_menu` would overwrite it and bring "Close Window" back.
//
// Never on Linux: `Builder::menu` would add a GTK menubar there.

use tauri::AppHandle;
use tauri::menu::{Menu, PredefinedMenuItem, Submenu};

/// The macOS menu: the PeakBot app submenu, then Tauri 2's default submenus
/// with File dropped and `Window` reduced to minimize/maximize (no "Close
/// Window", so Cmd+W reaches the page and the tab bar closes a conversation
/// tab).
pub fn app_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let mut menu = Menu::new(app)?;
    menu.append_items(&[&crate::tray::build_app_submenu(app)?])?;

    for item in Menu::default(app)?.items()? {
        // The default menu's top level is submenus only (App, File, Edit,
        // View, Window, Help); `items()` hands them back as `MenuItemKind`.
        let Some(sub) = item.as_submenu() else {
            continue;
        };
        match sub.text()?.as_str() {
            // The default File submenu is exactly [CloseWindow]: drop it.
            "File" => {}
            // Drop the default "Close Window" from Window.
            "Window" => {
                let window = Submenu::with_items(
                    app,
                    "Window",
                    true,
                    &[
                        PredefinedMenuItem::minimize(app, None)?,
                        PredefinedMenuItem::maximize(app, None)?,
                    ],
                )?;
                menu.append_items(&[&window])?;
            }
            // App, Edit, View, Help: unchanged.
            _ => menu.append_items(&[&item])?,
        }
    }
    Ok(menu)
}
