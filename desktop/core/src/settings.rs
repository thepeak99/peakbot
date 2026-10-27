//! The desktop settings page's view (0.24 config, spec §8.4/§9.10).
//! Only desktop-only options are left here: the background-service row.
//! Web server options (bind, HTTPS, access token) live in config.yaml
//! `general.web`, edited in PeakBot → Settings → General; the desktop
//! owns no settings file.

use serde::{Deserialize, Serialize};

use crate::phase::{StatusView, TrayModel};

/// The web UI's light/dark theme, as reported over the `theme_set`
/// command and read back by the bundled pages via `theme_get`
/// (desktop-pane design §3). Lowercase wire form; the web page's value is
/// untrusted input, so nothing else deserializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
}

/// What the settings page renders: the live tray/status state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    /// The background row is shown only when true (Q1: hidden on
    /// Windows in v1); = `TrayModel.background_visible`.
    pub background_visible: bool,
    pub background: bool,
    pub background_enabled: bool,
    /// `status.kind == "starting"`.
    pub busy: bool,
}

pub fn settings_view(tray: &TrayModel, status: &StatusView) -> SettingsView {
    SettingsView {
        background_visible: tray.background_visible,
        background: tray.background_checked,
        background_enabled: tray.background_enabled,
        busy: status.kind == "starting",
    }
}

#[cfg(test)]
mod tests {
    use crate::phase::{StatusView, TrayModel};
    use crate::settings::{Theme, settings_view};

    fn tray() -> TrayModel {
        TrayModel {
            status: "PeakBot 0.23.0 · running (port 7823)".into(),
            background_visible: true,
            background_checked: true,
            background_enabled: false,
            background_label: "Keep PeakBot running in background".into(),
        }
    }

    fn status(kind: &'static str) -> StatusView {
        StatusView {
            kind,
            title: String::new(),
            detail: None,
            actions: Vec::new(),
        }
    }

    // Design #377 t74 (0.24: the view no longer carries settings.json,
    // a token or the web-token path): a pure mapping of tray + status;
    // busy ⇔ kind == "starting". CA-trust design §6: `cert_warning` is
    // REMOVED — the CA is now trusted in the OS store on macOS/Windows
    // and by the webview on Linux, so the page shows no certificate note.
    #[test]
    fn t74_settings_view_maps_tray_and_status() {
        let tray = tray();

        let v = settings_view(&tray, &status("ready"));
        assert!(v.background_visible, "= tray.background_visible");
        assert!(v.background, "= tray.background_checked");
        assert!(!v.background_enabled, "= tray.background_enabled");
        assert!(!v.busy, "kind \"ready\" ⇒ not busy");

        let v = settings_view(&tray, &status("starting"));
        assert!(v.busy, "busy ⇔ status.kind == \"starting\"");

        // The page gets exactly these keys — nothing web-related, no
        // cert note.
        let json = serde_json::to_value(&v).expect("the view serialises");
        let mut keys: Vec<&str> = json
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "background",
                "background_enabled",
                "background_visible",
                "busy"
            ]
        );
    }

    // Desktop-pane design §5 item 1: `Theme` is the wire type of the
    // `theme_set` / `theme_get` commands — lowercase "light" | "dark", and
    // nothing else deserializes (the web page's value is untrusted input).
    #[test]
    fn theme_serde() {
        // Round-trip both variants through their exact wire form.
        for (wire, theme) in [("light", Theme::Light), ("dark", Theme::Dark)] {
            let d: Theme = serde_json::from_str(&format!("\"{wire}\""))
                .unwrap_or_else(|e| panic!("{wire}: {e}"));
            assert_eq!(d, theme, "\"{wire}\" deserializes");
            assert_eq!(
                serde_json::to_string(&theme).expect("Theme serialises"),
                format!("\"{wire}\""),
                "{theme:?} serialises lowercase"
            );
        }

        // Anything else is refused: unknown name, wrong case, null, wrong
        // JSON types, empty string, padded string.
        for bad in [
            "\"blue\"",
            "\"Light\"",
            "\"DARK\"",
            "\"\"",
            "\" light\"",
            "null",
            "true",
            "0",
            "[]",
            "{}",
        ] {
            assert!(
                serde_json::from_str::<Theme>(bad).is_err(),
                "{bad} must not deserialize as a Theme"
            );
        }

        // `theme_get` returns Option<Theme>: null is the "not reported yet"
        // answer and must round-trip through Option, not through Theme.
        assert_eq!(
            serde_json::from_str::<Option<Theme>>("null").expect("null is None"),
            None
        );
        assert_eq!(
            serde_json::to_string(&Some(Theme::Dark)).expect("serialises"),
            "\"dark\""
        );
        assert_eq!(
            serde_json::to_string(&None::<Theme>).expect("serialises"),
            "null"
        );
    }
}
