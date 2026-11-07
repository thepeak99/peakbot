//! PeakBot Mobile — Tauri shell glue (plan §2.2).
//!
//! Scaffold only: plugins, window, init script, nav wiring and commands land
//! in M5; the pure modules below are filled in M2–M4 and L8.
mod link;
mod local;
mod nav;
mod profiles;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running PeakBot Mobile");
}
