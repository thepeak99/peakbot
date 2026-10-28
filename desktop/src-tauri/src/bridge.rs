//! The `core::Ui` implementation (DESIGN §5.4): phase snapshot for the
//! status page, webview navigation, and native dialogs. Runs on the
//! controller thread — `blocking_show` dialogs are legal here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tauri::menu::MenuItem;
use tauri::{Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogBuilder, MessageDialogButtons};

use peakbot_desktop_core::controller::Ui;
use peakbot_desktop_core::nav::{NavDecision, classify_navigation};
use peakbot_desktop_core::phase::{Dialog, Phase, StartStep, StatusView, status_view};
use peakbot_desktop_core::theme::Theme;
use peakbot_desktop_core::update::UpdateView;

use crate::tray;

/// The embedded local page URL (DESIGN §4.8). Tauri serves embedded
/// assets from `tauri://localhost` on Linux/macOS and
/// `http://tauri.localhost` on Windows/Android.
pub(crate) fn local_page_url() -> tauri::Url {
    const URL: &str = if cfg!(any(target_os = "windows", target_os = "android")) {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    URL.parse().expect("static local page URL parses")
}

/// Shared mutable state between the controller thread, the IPC commands
/// and the menu handlers.
pub struct UiState {
    /// The last `StatusView`, served by the `status` command.
    pub snapshot: Mutex<StatusView>,
    /// The allowed backend origin (`None` on the local status page).
    pub nav: Arc<RwLock<Option<String>>>,
    /// A clone of the tray's status item (2.12's `TrayIcon` has no
    /// `get_menu()`); `None` when the tray could not be created.
    pub tray_status_item: Mutex<Option<MenuItem<Wry>>>,
    /// The web UI's last reported theme (`theme_set`); `None` until it
    /// reports one. Read back by the bundled pages via `theme_get`.
    pub theme: Mutex<Option<Theme>>,
    /// The update banner view (`update_status`); `None` until the
    /// updater thread renders something worth showing.
    pub update: Mutex<Option<UpdateView>>,
    /// The `update` menu items (tray menu + macOS app submenu); the
    /// updater thread's `render` updates text+enabled on all of them.
    pub update_items: Mutex<Vec<MenuItem<Wry>>>,
}

impl UiState {
    pub fn new() -> Self {
        let starting = Phase::Starting(StartStep::CheckingBundled);
        Self {
            snapshot: Mutex::new(status_view(&starting)),
            nav: Arc::new(RwLock::new(None)),
            tray_status_item: Mutex::new(None),
            theme: Mutex::new(None),
            update: Mutex::new(None),
            update_items: Mutex::new(Vec::new()),
        }
    }
}

/// The `core::Ui` the controller drives (DESIGN §5.3/§5.4).
pub struct TauriUi {
    app: tauri::AppHandle,
    state: Arc<UiState>,
}

impl TauriUi {
    pub fn new(app: tauri::AppHandle, state: Arc<UiState>) -> Self {
        Self { app, state }
    }

    /// The shared quitting flag (managed in `setup`).
    fn quitting(&self) -> bool {
        self.app
            .try_state::<Arc<AtomicBool>>()
            .is_some_and(|q| q.load(Ordering::SeqCst))
    }
}

impl Ui for TauriUi {
    fn phase_changed(&self, phase: &Phase, tray_status: &str) {
        *self.state.snapshot.lock().expect("snapshot poisoned") = status_view(phase);
        // No main-thread round-trips while quitting: during
        // `RunEvent::Exit` the main thread is blocked on the shutdown
        // ack (DESIGN §4.6), and both `navigate` (webview) and
        // `tray::apply` (menu item setters run on the main thread via
        // muda) would deadlock against it.
        if !self.quitting() {
            self.navigate(phase);
            tray::apply(&self.state, tray_status);
        }
    }

    fn confirm(&self, d: &Dialog) -> bool {
        native_dialog(&self.app, d).blocking_show()
    }
}

/// A native dialog for `d`, not yet shown. Callers on the main thread
/// must use the async `show`: `blocking_show` there deadlocks.
pub(crate) fn native_dialog(app: &tauri::AppHandle, d: &Dialog) -> MessageDialogBuilder<Wry> {
    let buttons = match d.cancel.as_deref() {
        Some(cancel) => MessageDialogButtons::OkCancelCustom(d.ok.clone(), cancel.to_string()),
        None => MessageDialogButtons::OkCustom(d.ok.clone()),
    };
    app.dialog()
        .message(&d.body)
        .title(&d.title)
        .buttons(buttons)
}

impl TauriUi {
    /// DESIGN §4.8: `Ready` → navigate to the backend URL and remember
    /// its origin; `Starting`/`Failed` → clear the origin and bounce back
    /// to the local status page if we are not already on it.
    fn navigate(&self, phase: &Phase) {
        let Some(win) = self.app.get_webview_window(crate::window::MAIN_LABEL) else {
            return;
        };
        match phase {
            Phase::Ready(target) => {
                *self.state.nav.write().expect("nav poisoned") = Some(target.url_base());
                if let Err(e) = win.navigate(target.url()) {
                    log::warn!("navigate to backend failed: {e}");
                }
            }
            Phase::Starting(_) | Phase::Failed(_) => {
                *self.state.nav.write().expect("nav poisoned") = None;
                let on_local = win
                    .url()
                    .is_ok_and(|u| classify_navigation(&u, None) == NavDecision::Allow);
                if !on_local && let Err(e) = win.navigate(local_page_url()) {
                    log::warn!("navigate to status page failed: {e}");
                }
            }
        }
    }
}
