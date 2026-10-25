//! The desktop settings page's view (0.24 config, spec §8.4/§9.10).
//! Only desktop-only options are left here: the background-service row.
//! Web server options (bind, HTTPS, access token) live in config.yaml
//! `general.web`, edited in PeakBot → Settings → General; the desktop
//! owns no settings file.

use serde::Serialize;

use crate::phase::{StatusView, TrayModel};

/// What the settings page renders: the live tray/status state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    /// The app window can trust PeakBot's CA on Linux only (design
    /// #377b §3.2): the page shows the macOS certificate note when true.
    pub cert_warning: bool,
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
        cert_warning: !cfg!(target_os = "linux"),
        background_visible: tray.background_visible,
        background: tray.background_checked,
        background_enabled: tray.background_enabled,
        busy: status.kind == "starting",
    }
}

#[cfg(test)]
mod tests {
    use crate::phase::{StatusView, TrayModel};
    use crate::settings::settings_view;

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
    // busy ⇔ kind == "starting"; the cert note on non-Linux only.
    #[test]
    fn t74_settings_view_maps_tray_and_status() {
        let tray = tray();

        let v = settings_view(&tray, &status("ready"));
        assert!(v.background_visible, "= tray.background_visible");
        assert!(v.background, "= tray.background_checked");
        assert!(!v.background_enabled, "= tray.background_enabled");
        assert!(!v.busy, "kind \"ready\" ⇒ not busy");
        assert_eq!(v.cert_warning, !cfg!(target_os = "linux"));

        let v = settings_view(&tray, &status("starting"));
        assert!(v.busy, "busy ⇔ status.kind == \"starting\"");

        // The page gets exactly these keys — nothing web-related.
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
                "busy",
                "cert_warning"
            ]
        );
    }
}
