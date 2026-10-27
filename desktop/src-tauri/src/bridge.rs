//! The `core::Ui` implementation (DESIGN §5.4): phase snapshot for the
//! status page, webview navigation, and native dialogs. Runs on the
//! controller thread — `blocking_show` dialogs are legal here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

use peakbot_desktop_core::controller::Ui;
use peakbot_desktop_core::nav::{NavDecision, classify_navigation};
use peakbot_desktop_core::phase::{Dialog, Phase, StartStep, StatusView, TrayModel, status_view};
use peakbot_desktop_core::settings::Theme;

use crate::tray::{self, TrayItems};

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

/// The embedded settings page URL (0.24, spec §9.10): the local page
/// origin plus `settings.html` — the `main` window navigates there.
pub(crate) fn settings_page_url() -> tauri::Url {
    local_page_url()
        .join("settings.html")
        .expect("local page origin joins with settings.html")
}

/// Shared mutable state between the controller thread, the IPC commands
/// and the menu handlers.
pub struct UiState {
    /// The last `StatusView`, served by the `status` command.
    pub snapshot: Mutex<StatusView>,
    /// The allowed backend origin (`None` on the local status page).
    pub nav: Arc<RwLock<Option<String>>>,
    /// The live backend URL the phase machine last navigated to
    /// (`None` while not Ready) — where `← Back to PeakBot` returns
    /// (0.24, spec §9.10).
    pub backend_url: Mutex<Option<String>>,
    /// The last tray model the controller emitted.
    pub tray_model: Mutex<TrayModel>,
    /// Clones of the tray items that `TrayModel` updates (2.12's
    /// `TrayIcon` has no `get_menu()`, so the handles are kept here).
    pub tray_items: Mutex<Option<TrayItems>>,
    /// The web UI's last reported theme (`theme_set`); `None` until it
    /// reports one. Read back by the bundled pages via `theme_get`.
    pub theme: Mutex<Option<Theme>>,
}

impl UiState {
    pub fn new() -> Self {
        let starting = Phase::Starting(StartStep::CheckingBundled);
        Self {
            snapshot: Mutex::new(status_view(&starting)),
            nav: Arc::new(RwLock::new(None)),
            backend_url: Mutex::new(None),
            tray_model: Mutex::new(peakbot_desktop_core::phase::tray_model(
                &starting,
                None,
                None,
                peakbot_desktop_core::phase::host_platform(),
            )),
            tray_items: Mutex::new(None),
            theme: Mutex::new(None),
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
    fn phase_changed(&self, phase: &Phase, tray: &TrayModel) {
        *self.state.snapshot.lock().expect("snapshot poisoned") = status_view(phase);
        *self.state.tray_model.lock().expect("tray_model poisoned") = tray.clone();
        // No main-thread round-trips while quitting: during
        // `RunEvent::Exit` the main thread is blocked on the shutdown
        // ack (DESIGN §4.6), and both `navigate` (webview) and
        // `tray::apply` (menu item setters run on the main thread via
        // muda) would deadlock against it.
        if !self.quitting() {
            self.navigate(phase);
            tray::apply(&self.state, tray);
        }
    }

    fn confirm(&self, d: &Dialog) -> bool {
        let buttons = match d.cancel.as_deref() {
            Some(cancel) => MessageDialogButtons::OkCancelCustom(d.ok.clone(), cancel.to_string()),
            None => MessageDialogButtons::OkCustom(d.ok.clone()),
        };
        self.app
            .dialog()
            .message(&d.body)
            .title(&d.title)
            .buttons(buttons)
            .blocking_show()
    }

    fn message(&self, d: &Dialog) {
        let _ = self
            .app
            .dialog()
            .message(&d.body)
            .title(&d.title)
            .buttons(MessageDialogButtons::OkCustom(d.ok.clone()))
            .blocking_show();
    }
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
            Phase::Ready(mode) => {
                *self.state.nav.write().expect("nav poisoned") = Some(mode.origin());
                *self.state.backend_url.lock().expect("backend_url poisoned") =
                    Some(mode.url().to_string());
                if let Err(e) = win.navigate(mode.url()) {
                    log::warn!("navigate to backend failed: {e}");
                }
            }
            Phase::Starting(_) | Phase::Failed(_) => {
                *self.state.nav.write().expect("nav poisoned") = None;
                *self.state.backend_url.lock().expect("backend_url poisoned") = None;
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
