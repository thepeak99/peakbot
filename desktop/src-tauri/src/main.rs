//! PeakBot Desktop — the thin Tauri shell over `peakbot-desktop-core`
//! (DESIGN §5.4). All decisions live in core; this wires the window,
//! tray, dialogs and the controller thread together.

mod bridge;
mod commands;
mod tray;
mod window;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use tauri::Manager;

use peakbot_desktop_core::controller::{Action, Controller, run_loop};
use peakbot_desktop_core::paths::DesktopPaths;

/// Bounds `RunEvent::Exit` against a controller stuck in a modal dialog.
const SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_secs(6);

/// SIGINT handling (the DESIGN §4.6 shutdown path on `kill`/Ctrl+C).
/// The current toolchain has no `std::os::unix::signal`, so a C handler
/// (via `libc`) funnels each SIGINT through a self-pipe — the only
/// async-signal-safe operation — to a dedicated reader thread.
#[cfg(unix)]
mod sigint {
    use std::io::{ErrorKind, PipeReader, PipeWriter, pipe};
    use std::os::fd::AsRawFd;
    use std::sync::OnceLock;

    /// The self-pipe: the write end is used by the C signal handler,
    /// the read end stays open for the process lifetime.
    static PIPES: OnceLock<(PipeReader, PipeWriter)> = OnceLock::new();

    /// C handler: async-signal-safe — one `write`, nothing else.
    extern "C" fn on_sigint(_sig: libc::c_int) {
        if let Some((_, w)) = PIPES.get() {
            let one: u8 = 1;
            let _ =
                unsafe { libc::write(w.as_raw_fd(), &one as *const u8 as *const libc::c_void, 1) };
        }
    }

    /// Install the SIGINT handler; `on_signal` runs once per SIGINT on
    /// the reader thread.
    pub fn install(on_signal: impl Fn() + Send + 'static) -> std::io::Result<()> {
        let (r, w) = pipe()?;
        PIPES.set((r, w)).expect("self-pipe stored once");
        if let Some((_, w)) = PIPES.get() {
            unsafe { libc::fcntl(w.as_raw_fd(), libc::FD_CLOEXEC) };
        }
        unsafe {
            libc::signal(libc::SIGINT, on_sigint as usize as libc::sighandler_t);
        }
        std::thread::Builder::new()
            .name("sigint".into())
            .spawn(move || {
                let mut buf = [0u8; 1];
                loop {
                    let Some((r, _)) = PIPES.get() else {
                        break;
                    };
                    let n = unsafe { libc::read(r.as_raw_fd(), buf.as_mut_ptr().cast(), 1) };
                    if n > 0 {
                        on_signal();
                    } else if n < 0
                        && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted
                    {
                        break;
                    }
                }
            })
            .expect("sigint reader thread starts");
        Ok(())
    }
}

fn main() {
    // DESIGN §4.10: first statement, before any thread.
    if cfg!(target_os = "linux")
        && let Some((key, value)) = peakbot_desktop_core::env::webkit_dmabuf_default(
            std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").as_deref(),
        )
    {
        unsafe { std::env::set_var(key, value) };
    }

    let app = tauri::Builder::default()
        // FIRST: a second launch focuses the existing window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            window::focus_main(app);
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("desktop".into()),
                    },
                ))
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Stdout,
                ))
                .level(log::LevelFilter::Info)
                .build(),
        )
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![commands::status, commands::act])
        .setup(|app| {
            let handle = app.handle().clone();

            let paths = build_paths(&handle)?;
            let ui_state = Arc::new(bridge::UiState::new());

            app.manage(ui_state.clone());
            app.manage(paths.clone());

            window::create_main(&handle, ui_state.nav.clone())?;
            tray::create(&handle, &ui_state)?;

            // Controller thread (DESIGN §4.1): one owner of the session
            // child + phase; all long work is serialized there.
            let (tx, rx) = mpsc::channel::<Action>();
            let quitting = Arc::new(AtomicBool::new(false));
            app.manage(commands::ActionTx(tx.clone()));
            app.manage(quitting.clone());

            let ui = bridge::TauriUi::new(handle.clone(), ui_state);
            let sys = peakbot_desktop_core::sys::RealSys::new();
            let controller = Controller::new(sys, ui, paths, quitting);
            std::thread::Builder::new()
                .name("controller".into())
                .spawn(move || run_loop(controller, rx))
                .expect("controller thread starts");

            let _ = tx.send(Action::Start);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("tauri app builds");

    // SIGINT (Ctrl+C, `kill`) takes the same graceful shutdown path as a
    // window close (DESIGN §4.6): the controller stops the session
    // sidecar and removes the pidfile before the process exits.
    #[cfg(unix)]
    {
        let handle = app.handle().clone();
        sigint::install(move || handle.exit(0)).expect("SIGINT handler installs");
    }

    app.run(|app, event| match event {
        tauri::RunEvent::Exit => shutdown(app),
        // macOS only: Dock-icon click after the window closed.
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => window::focus_main(app),
        _ => {}
    });
}

/// `RunEvent::Exit` (DESIGN §4.6): set `quitting`, send `Shutdown(ack)`,
/// wait at most 6 s for the ack, then let the process exit.
fn shutdown(app: &tauri::AppHandle) {
    if let Some(quitting) = app.try_state::<Arc<AtomicBool>>() {
        quitting.store(true, Ordering::SeqCst);
    }
    let Some(tx) = app.try_state::<commands::ActionTx>() else {
        return;
    };
    let (ack_tx, ack_rx) = mpsc::channel::<()>();
    if tx.0.send(Action::Shutdown(ack_tx)).is_err() {
        return; // the controller thread already exited
    }
    if ack_rx.recv_timeout(SHUTDOWN_ACK_TIMEOUT).is_err() {
        log::warn!(
            "controller did not ack shutdown within {} s; exiting anyway",
            SHUTDOWN_ACK_TIMEOUT.as_secs()
        );
    }
}

/// `DesktopPaths` for this app (DESIGN §3.6/§3.7): sidecar next to the
/// executable when bundled, `binaries/peakbot-<triple>` in a dev build.
fn build_paths(app: &tauri::AppHandle) -> tauri::Result<DesktopPaths> {
    let exe = std::env::current_exe()?;
    let sidecar = {
        let bundled = peakbot_desktop_core::paths::sidecar_path(&exe);
        if bundled.exists() {
            bundled
        } else {
            app.path()
                .resource_dir()?
                .join("binaries")
                .join(format!("peakbot-{}", env!("DESKTOP_TARGET_TRIPLE")))
        }
    };
    Ok(DesktopPaths {
        sidecar,
        data_dir: app.path().app_data_dir()?,
        log_dir: app.path().app_log_dir()?,
        peakbot_config_dir: peakbot_desktop_core::paths::peakbot_config_dir().ok_or_else(|| {
            tauri::Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no peakbot config dir",
            ))
        })?,
    })
}
