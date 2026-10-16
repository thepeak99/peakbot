//! Paths (DESIGN §3.7).

use std::path::{Path, PathBuf};

/// The directories/files the desktop app owns (DESIGN §3.6/§3.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPaths {
    /// `sidecar_path(current_exe)`.
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

    /// `peakbot_config_dir/web-token`.
    pub fn web_token(&self) -> PathBuf {
        self.peakbot_config_dir.join("web-token")
    }

    /// `peakbot_config_dir/config.yaml`.
    pub fn peakbot_config(&self) -> PathBuf {
        self.peakbot_config_dir.join("config.yaml")
    }

    /// `data_dir/settings.json` (design #377 §3.1: launch parameters only).
    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
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

    // §9.1 case 26 (updated for #377: + settings_file).
    #[test]
    fn t26_sidecar_path_and_helper_file_names() {
        let p = sidecar_path(Path::new("/tmp/.mount_x/usr/bin/peakbot-desktop"));
        assert_eq!(p, PathBuf::from("/tmp/.mount_x/usr/bin/peakbot"));

        let d = paths();
        assert_eq!(d.pidfile(), PathBuf::from("/data/sidecar.pid"));
        assert_eq!(d.declined_file(), PathBuf::from("/data/declined-replace"));
        assert_eq!(d.session_log(), PathBuf::from("/logs/peakbot-session.log"));
        assert_eq!(d.web_token(), PathBuf::from("/cfg/web-token"));
        assert_eq!(d.peakbot_config(), PathBuf::from("/cfg/config.yaml"));
        assert_eq!(d.settings_file(), PathBuf::from("/data/settings.json"));
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
}
