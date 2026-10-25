//! Paths (DESIGN §3.7).

use std::path::{Path, PathBuf};

/// The directories/files the desktop app owns (DESIGN §3.6/§3.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPaths {
    /// The sidecar the app runs: next to the exe in Linux/macOS bundles,
    /// `binaries/peakbot-<triple>` in dev, the extracted payload on
    /// Windows (`extract::payload_path`).
    pub sidecar: PathBuf,
    /// tauri `app_data_dir()`.
    pub data_dir: PathBuf,
    /// tauri `app_log_dir()`.
    pub log_dir: PathBuf,
    /// `peakbot_config_dir()`.
    pub peakbot_config_dir: PathBuf,
}

/// `current_exe.parent() / sidecar_file_name()`.
pub fn sidecar_path(current_exe: &Path) -> PathBuf {
    match current_exe.parent() {
        Some(dir) => dir.join(sidecar_file_name()),
        None => PathBuf::from(sidecar_file_name()),
    }
}

/// `"peakbot"` (unix) | `"peakbot.exe"` (windows).
pub fn sidecar_file_name() -> &'static str {
    if cfg!(windows) {
        "peakbot.exe"
    } else {
        "peakbot"
    }
}

/// `directories_next::ProjectDirs::from("com", "peakbot", "peakbot")
/// .config_dir()` — SAME crate + args as peakbot's `get_config_dir()`,
/// so the paths match by construction.
pub fn peakbot_config_dir() -> Option<PathBuf> {
    directories_next::ProjectDirs::from("com", "peakbot", "peakbot")
        .map(|p| p.config_dir().to_path_buf())
}

/// `directories_next::BaseDirs::cache_dir()/peakbot/tls/ca-cert.pem` —
/// SAME base dir and file name as the backend's `dirs::cache_dir()` CA
/// (`src/ui/web/tls.rs`): the Linux webview verifies the presented leaf
/// against this file (design #377b §3.3).
pub fn peakbot_ca_cert() -> Option<PathBuf> {
    directories_next::BaseDirs::new().map(|b| {
        b.cache_dir()
            .join("peakbot")
            .join("tls")
            .join("ca-cert.pem")
    })
}

impl DesktopPaths {
    /// `data_dir/sidecar.pid`.
    pub fn pidfile(&self) -> PathBuf {
        self.data_dir.join("sidecar.pid")
    }

    /// `data_dir/declined-replace`.
    pub fn declined_file(&self) -> PathBuf {
        self.data_dir.join("declined-replace")
    }

    /// `log_dir/peakbot-session.log`.
    pub fn session_log(&self) -> PathBuf {
        self.log_dir.join("peakbot-session.log")
    }

    /// `peakbot_config_dir/config.yaml`.
    pub fn peakbot_config(&self) -> PathBuf {
        self.peakbot_config_dir.join("config.yaml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> DesktopPaths {
        DesktopPaths {
            sidecar: PathBuf::from("/app/peakbot"),
            data_dir: PathBuf::from("/data"),
            log_dir: PathBuf::from("/logs"),
            peakbot_config_dir: PathBuf::from("/cfg"),
        }
    }

    // §9.1 case 26 (0.24: no web-token / settings.json helpers).
    #[test]
    fn t26_sidecar_path_and_helper_file_names() {
        let p = sidecar_path(Path::new("/tmp/.mount_x/usr/bin/peakbot-desktop"));
        assert_eq!(p, PathBuf::from("/tmp/.mount_x/usr/bin/peakbot"));

        let d = paths();
        assert_eq!(d.pidfile(), PathBuf::from("/data/sidecar.pid"));
        assert_eq!(d.declined_file(), PathBuf::from("/data/declined-replace"));
        assert_eq!(d.session_log(), PathBuf::from("/logs/peakbot-session.log"));
        assert_eq!(d.peakbot_config(), PathBuf::from("/cfg/config.yaml"));
    }

    // §9.1 case 27.
    #[cfg(target_os = "linux")]
    #[test]
    fn t27_peakbot_config_dir_ends_with_peakbot() {
        let dir = peakbot_config_dir().expect("config dir resolves on Linux");
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("peakbot"),
            "got {dir:?}"
        );
    }

    // Design #377b t102: the CA lives in the same base dir as the
    // backend's `dirs::cache_dir()` — `peakbot/tls/ca-cert.pem` (the
    // file name the backend's tls.rs mints it under).
    #[cfg(target_os = "linux")]
    #[test]
    fn t102_peakbot_ca_cert_ends_with_peakbot_tls_ca_cert_pem() {
        let p = peakbot_ca_cert().expect("the cache dir resolves on Linux");
        assert!(p.ends_with("peakbot/tls/ca-cert.pem"), "got {p:?}");
    }
}
