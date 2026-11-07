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

/// The single init script injected on every origin, both OSes (plan §2.3).
/// Declares the mobile shell marker and shims `window.open` as a top-level
/// navigation (I-D8) the nav guard can classify. Locked bytes — no trailing newline.
pub const INIT_SCRIPT: &str = "Object.defineProperty(window,'peakbotShell',{value:'mobile'});\nwindow.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};";
// TESTER'S LOCK — Phase 2, lock 4 (RED until M4/M5 lands `INIT_SCRIPT`):
// the init script is ONE const in lib.rs, injected on every origin, both
// OSes (plan §2.3). The locked bytes are pinned below; M5 injects it via
// `.initialization_script(INIT_SCRIPT)`.
#[cfg(test)]
mod init_script_tests {
    use super::INIT_SCRIPT;

    /// The locked bytes (plan §2.3 + the I-D8 `window.open` shim): two
    /// lines, ONE `\n` between them, no trailing newline — same style as the
    /// desktop `SHELL_MARKER_SCRIPT`
    /// (`Object.defineProperty(window,'peakbotShell',{value:'desktop'});`).
    const EXPECTED: &str = "Object.defineProperty(window,'peakbotShell',{value:'mobile'});\n\
window.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};";

    /// `web/src/desktop.test.ts` (F1) sets `window.peakbotShell` to exactly
    /// `"desktop"` / `"mobile"` — read at compile time so the cross-side pin
    /// is deterministic.
    const WEB_DESKTOP_TEST: &str = include_str!("../../../web/src/desktop.test.ts");

    #[test]
    fn init_script_is_the_locked_string() {
        assert_eq!(
            INIT_SCRIPT, EXPECTED,
            "INIT_SCRIPT must be exactly the two locked lines (marker + window.open shim)"
        );
    }

    #[test]
    fn init_script_declares_the_mobile_shell_marker() {
        // Same shape as the desktop marker, value 'mobile'.
        assert!(
            INIT_SCRIPT.contains("Object.defineProperty(window,'peakbotShell',{value:'mobile'});")
        );
    }

    #[test]
    fn init_script_shims_window_open_as_top_level_navigation() {
        // I-D8: every window.open becomes a top-level navigation the guard
        // classifies (foreign → External → system browser).
        assert!(INIT_SCRIPT.contains(
            "window.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};"
        ));
    }

    #[test]
    fn shell_marker_value_matches_the_web_tests() {
        // Cross-side pin: the marker our init script declares is one of the
        // exactly two values the web `shell()` seam accepts.
        assert!(WEB_DESKTOP_TEST.contains(r#"peakbotShell = "mobile""#));
        assert!(WEB_DESKTOP_TEST.contains(r#"peakbotShell = "desktop""#));
        assert!(INIT_SCRIPT.contains("{value:'mobile'}"));
        assert!(!INIT_SCRIPT.contains("{value:'desktop'}"));
    }
}
