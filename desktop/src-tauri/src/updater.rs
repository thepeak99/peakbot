//! The auto-update glue (plan "Glue", spike S2): bundle detection at
//! the edge, `UpdateSys` over tauri-plugin-updater, and the updater
//! thread. All blocking work runs on that thread, and `restart()` only
//! ever happens from it — a main-thread restart skips the Exit event,
//! so the single-instance lock would stay held (spike S2 Q3/Q6).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::SystemTime;

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_updater::{Updater, UpdaterExt};

use peakbot_desktop_core::update::{
    self, Bundle, Dialog, InstallOutcome, State, UpdateSys, run_loop,
};

use crate::bridge::UiState;
use crate::commands::UpdateTx;

/// The updatable bundle of this running binary, or `None` when it
/// cannot be updated in place (spike S2 Q2: left to itself, the
/// plugin would happily install a second copy).
pub fn detect_bundle() -> Option<Bundle> {
    // `config::BundleType` is the (public) type `platform::bundle_type()`
    // returns.
    #[cfg(target_os = "linux")]
    {
        (tauri::utils::platform::bundle_type() == Some(tauri::utils::config::BundleType::AppImage))
            .then_some(Bundle::AppImage)
    }
    #[cfg(windows)]
    {
        (tauri::utils::platform::bundle_type() == Some(tauri::utils::config::BundleType::Nsis))
            .then_some(Bundle::Nsis)
    }
    #[cfg(target_os = "macos")]
    {
        // `bundle_type()` is always `App` on macOS (useless as a gate) —
        // the exe path decides (spike S2 rec 4).
        let exe = std::env::current_exe().ok()?;
        macos_bundle(&exe).then_some(Bundle::MacApp)
    }
}

/// The macOS gate: an exe directly inside `*.app/Contents/MacOS`, and
/// not translocated (a translocated bundle is read-only — the install
/// would fail).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn macos_bundle(exe: &std::path::Path) -> bool {
    let in_macos_dir = exe
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "MacOS");
    // Three levels up from the exe: MacOS → Contents → the `.app`.
    let inside_app = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .is_some_and(|n| n.to_string_lossy().ends_with(".app"));
    in_macos_dir && inside_app && !exe.to_string_lossy().contains("/AppTranslocation/")
}

/// This build is updatable: a release build of an updatable bundle.
pub fn supported() -> bool {
    update::supported(detect_bundle(), cfg!(debug_assertions))
}

/// The updater thread's I/O (core `UpdateSys` over the plugin, the
/// banner state and the `update` menu items).
struct TauriUpdateSys {
    app: AppHandle,
    state: Arc<UiState>,
}

impl TauriUpdateSys {
    /// The plugin's pre-filled builder (endpoints + pubkey from
    /// `tauri.conf.json`). Our `on_before_exit` REPLACES the plugin's
    /// default hook, so it must do the sidecar shutdown AND the tray
    /// cleanup itself (spike S2 Q5).
    fn updater(&self) -> Result<Updater, String> {
        let h = self.app.clone();
        self.app
            .updater_builder()
            .on_before_exit(move || {
                crate::shutdown(&h);
                h.cleanup_before_exit();
            })
            .build()
            .map_err(|e| e.to_string())
    }

    fn current_version(&self) -> String {
        self.app.package_info().version.to_string()
    }
}

impl UpdateSys for TauriUpdateSys {
    fn check(&self) -> Result<Option<update::Version>, String> {
        let updater = self.updater()?;
        match tauri::async_runtime::block_on(updater.check()) {
            Ok(Some(u)) => Ok(Some(u.version)),
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn install(&self, on_installing: &mut dyn FnMut(update::Version)) -> InstallOutcome {
        // Re-check: download exactly the version the state machine
        // knows about (single-flight).
        let update = match self
            .updater()
            .and_then(|u| tauri::async_runtime::block_on(u.check()).map_err(|e| e.to_string()))
        {
            Ok(Some(u)) => u,
            Ok(None) => return InstallOutcome::Gone,
            Err(e) => return InstallOutcome::Failed(e.to_string()),
        };
        let version = update.version.clone();
        let bytes =
            match tauri::async_runtime::block_on(update.download(|_chunk, _total| {}, || {})) {
                Ok(b) => b,
                Err(e) => return InstallOutcome::Failed(e.to_string()),
            };
        on_installing(version);
        match update.install(bytes) {
            // Success never returns: Windows exit(0)s inside the
            // plugin (spike S2 Q5), and on mac/linux `restart()`
            // below never returns (Q3) — from THIS thread, never the
            // main one (Q6).
            Ok(()) => {
                crate::shutdown(&self.app);
                self.app.restart();
            }
            Err(e) => InstallOutcome::Failed(e.to_string()),
        }
    }

    fn notify(&self, d: &Dialog) {
        let (title, body) = match d {
            Dialog::UpToDate => (
                "Up to date".to_string(),
                format!("You're up to date (v{})", self.current_version()),
            ),
            Dialog::CheckFailed(e) => (
                "Update check failed".to_string(),
                format!("Couldn't check for updates: {e}"),
            ),
            Dialog::InstallFailed(e) => {
                ("Update failed".to_string(), format!("Update failed: {e}"))
            }
        };
        // Fire-and-forget: `show` dispatches to the main thread and
        // returns immediately, so this thread never blocks on the
        // dialog (during exit the main thread is in the Exit callback
        // — a blocking show would deadlock).
        self.app
            .dialog()
            .message(&body)
            .title(&title)
            .show(|_ok| {});
    }

    fn render(&self, s: &State) {
        // The menu setters round-trip to the main thread (muda) and
        // would deadlock against the Exit callback — the same guard
        // as `bridge::phase_changed` (spike S2 Q7).
        if self
            .app
            .try_state::<Arc<AtomicBool>>()
            .is_some_and(|q| q.load(Ordering::SeqCst))
        {
            return;
        }
        *self.state.update.lock().expect("update poisoned") = update::view(s);
        let (label, enabled) = update::menu_label(s);
        for item in self
            .state
            .update_items
            .lock()
            .expect("update_items poisoned")
            .iter()
        {
            let _ = item.set_text(&label);
            let _ = item.set_enabled(enabled);
        }
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// Start the updater thread when this build is updatable (plan
/// "Glue"): manages `UpdateTx` and runs `core::update::run_loop` on a
/// dedicated thread. Otherwise a no-op (logged) — no channel, no
/// banner, no checks.
pub fn spawn(app: &AppHandle) {
    if !supported() {
        log::info!(
            "updater disabled: bundle={:?}, debug={}",
            detect_bundle(),
            cfg!(debug_assertions)
        );
        return;
    }
    let state = app.state::<Arc<UiState>>().inner().clone();
    let (tx, rx) = mpsc::channel::<update::Event>();
    app.manage(UpdateTx(tx));
    let sys = TauriUpdateSys {
        app: app.clone(),
        state,
    };
    std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || run_loop(sys, rx))
        .expect("updater thread starts");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_bundle_gate_accepts_an_exe_inside_a_app() {
        assert!(macos_bundle(std::path::Path::new(
            "/Applications/PeakBot Desktop.app/Contents/MacOS/peakbot-desktop"
        )));
        assert!(macos_bundle(std::path::Path::new(
            "/tmp/x.app/Contents/MacOS/x"
        )));
    }

    #[test]
    fn macos_bundle_gate_rejects_bare_translocated_and_sideloaded_paths() {
        for p in [
            "/tmp/peakbot-desktop",
            "/tmp/xyz/Contents/MacOS/exe",
            "/Applications/x.app/Contents/Resources/exe",
            "/Users/u/Library/Containers/com.peakbot.desktop/Data/tmp/AppTranslocation/ABC123/d/x.app/Contents/MacOS/peakbot-desktop",
        ] {
            assert!(!macos_bundle(std::path::Path::new(p)), "{p}");
        }
    }
}
