//! Shell detection for cross-platform command execution.
//!
//! Detects the best available shell at runtime, prioritising:
//! 1. `PEAKBOT_SHELL` environment override
//! 2. WSL environments (treat as Linux)
//! 3. Git Bash on Windows
//! 4. PowerShell on Windows
//! 5. Unix `/bin/sh` fallback

use std::path::Path;

/// The kind of shell detected on this system.
#[derive(Debug, Clone, PartialEq)]
pub enum ShellKind {
    /// Bash or POSIX sh (Linux, macOS, WSL, Git Bash).
    Bash { path: String },
    /// PowerShell (Windows, pwsh or powershell).
    PowerShell { path: String },
}

impl ShellKind {
    /// Detect the best available shell on this system.
    ///
    /// Returns `None` only on Windows when no suitable shell is found.
    /// On Unix-like systems this always returns `Some(ShellKind::Bash)`.
    pub fn detect() -> Option<Self> {
        // 1. User override takes absolute precedence.
        if let Ok(override_shell) = std::env::var("PEAKBOT_SHELL") {
            let path = override_shell.trim().to_string();
            if !path.is_empty() {
                // Infer kind from the executable name.
                let lower = path.to_lowercase();
                if lower.contains("pwsh") || lower.contains("powershell") {
                    return Some(ShellKind::PowerShell { path });
                }
                return Some(ShellKind::Bash { path });
            }
        }

        // 2. WSL — treat as Linux even though the kernel reports Windows.
        if is_wsl() {
            return Some(ShellKind::Bash {
                path: "/bin/sh".to_string(),
            });
        }

        // 3. Non-Windows — assume POSIX.
        if !is_windows() {
            return Some(ShellKind::Bash {
                path: "/bin/sh".to_string(),
            });
        }

        // 4. Windows — try Git Bash first.
        if let Some(git_bash) = find_git_bash() {
            return Some(ShellKind::Bash { path: git_bash });
        }

        // 5. Windows — PowerShell 7+ (pwsh).
        if let Some(pwsh) = find_on_path("pwsh.exe") {
            return Some(ShellKind::PowerShell { path: pwsh });
        }

        // 6. Windows — PowerShell 5.1 (legacy).
        if let Some(ps) = find_on_path("powershell.exe") {
            return Some(ShellKind::PowerShell { path: ps });
        }

        // Nothing found on Windows.
        None
    }

    /// Human-readable name for diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            ShellKind::Bash { .. } => "bash",
            ShellKind::PowerShell { .. } => "powershell",
        }
    }

    /// The shell executable path.
    pub fn executable(&self) -> &str {
        match self {
            ShellKind::Bash { path } | ShellKind::PowerShell { path } => path.as_str(),
        }
    }

    /// The argument used to pass a command string.
    pub fn cmd_arg(&self) -> &'static str {
        match self {
            ShellKind::Bash { .. } => "-c",
            ShellKind::PowerShell { .. } => "-Command",
        }
    }

    /// Whether this is a Bash-like shell.
    pub fn is_bash(&self) -> bool {
        matches!(self, ShellKind::Bash { .. })
    }
}

/// Check if we're running inside WSL.
fn is_wsl() -> bool {
    // WSL_DISTRO_NAME is set in all WSL distributions.
    if std::env::var("WSL_DISTRO_NAME").is_ok() {
        return true;
    }
    // WSLInterop file exists only in WSL.
    if Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists() {
        return true;
    }
    // Check /proc/version for Microsoft/WSL markers.
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let v = version.to_lowercase();
        if v.contains("microsoft") || v.contains("wsl") {
            return true;
        }
    }
    false
}

/// Check if we're on Windows.
fn is_windows() -> bool {
    std::env::consts::OS == "windows"
}

/// Look for Git Bash on Windows.
fn find_git_bash() -> Option<String> {
    let candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    for candidate in &candidates {
        if Path::new(candidate).is_file() {
            return Some(candidate.to_string());
        }
    }
    // Also try PATH.
    find_on_path("bash.exe")
}

/// Search for an executable on the system PATH.
fn find_on_path(name: &str) -> Option<String> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(if is_windows() { ';' } else { ':' }) {
            let full = Path::new(dir).join(name);
            if full.is_file() {
                return Some(full.to_string_lossy().to_string());
            }
        }
    }
    None
}

/// Print a startup warning when no shell is available on Windows.
pub fn print_no_shell_warning() {
    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║              ⚠️  No shell found on Windows!                    ║");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");
    eprintln!();
    eprintln!("PeakBot needs a shell to run commands. None of the following were found:");
    eprintln!();
    eprintln!("  • Git Bash (from Git for Windows)");
    eprintln!("  • PowerShell 7+ (pwsh.exe)");
    eprintln!("  • PowerShell 5.1 (powershell.exe)");
    eprintln!();
    eprintln!("To fix this, install one of:");
    eprintln!();
    eprintln!("  1. Git for Windows — https://git-scm.com/download/win");
    eprintln!("     (provides Git Bash with bash, mv, rm, cp, tar, etc.)");
    eprintln!();
    eprintln!("  2. PowerShell 7 — https://aka.ms/powershell");
    eprintln!();
    eprintln!("Or set a custom shell via the PEAKBOT_SHELL environment variable:");
    eprintln!();
    eprintln!("  $env:PEAKBOT_SHELL = \"C:\\path\\to\\bash.exe\"");
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    /// Serialize tests that mutate process-global environment variables.
    static ENV_LOCK: LazyLock<std::sync::Mutex<()>> = LazyLock::new(|| std::sync::Mutex::new(()));

    #[test]
    fn shell_kind_name_matches_variant() {
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        assert_eq!(bash.name(), "bash");
        assert!(bash.is_bash());
        assert_eq!(bash.cmd_arg(), "-c");

        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        assert_eq!(ps.name(), "powershell");
        assert!(!ps.is_bash());
        assert_eq!(ps.cmd_arg(), "-Command");
    }

    #[test]
    fn shell_kind_executable_roundtrips() {
        let bash = ShellKind::Bash {
            path: "/usr/bin/bash".to_string(),
        };
        assert_eq!(bash.executable(), "/usr/bin/bash");
    }

    // ── Environment detection ────────────────────────────────────────────────

    /// On a non-WSL Linux host `is_wsl()` must return false.
    #[test]
    fn is_wsl_false_on_native_linux() {
        // This test runs in the CI/test environment which is native Linux.
        assert!(!is_wsl(), "native Linux should not report WSL");
    }

    /// `find_on_path` must locate a shell that exists on the system PATH.
    #[test]
    fn find_on_path_finds_existing_shell() {
        let found = find_on_path("sh");
        assert!(
            found.is_some(),
            "`sh` should be found on PATH in any Unix-like environment"
        );
        let path = found.unwrap();
        assert!(
            Path::new(&path).is_file(),
            "resolved path must exist: {}",
            path
        );
    }

    /// `find_on_path` returns None for a non-existent executable.
    #[test]
    fn find_on_path_returns_none_for_missing_binary() {
        let found = find_on_path("definitely_not_a_real_binary_12345");
        assert!(found.is_none());
    }

    /// `ShellKind::detect()` on Linux returns Bash with a valid shell path.
    #[test]
    fn detect_returns_bash_on_linux() {
        let _guard = ENV_LOCK.lock().unwrap();
        let kind = ShellKind::detect();
        assert!(
            kind.is_some(),
            "ShellKind::detect() should always return Some on Linux"
        );
        let sk = kind.unwrap();
        assert!(
            sk.is_bash(),
            "Linux detection should yield Bash, got: {:?}",
            sk
        );
        assert!(
            !sk.executable().is_empty(),
            "detected shell path must not be empty"
        );
    }

    // ── PEAKBOT_SHELL override ───────────────────────────────────────────────

    /// When `PEAKBOT_SHELL` is set, it takes precedence over auto-detection.
    #[test]
    fn detect_respects_peakbot_shell_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var("PEAKBOT_SHELL").ok();
        unsafe {
            std::env::set_var("PEAKBOT_SHELL", "/custom/shell");
        }
        let kind = ShellKind::detect();
        // Restore before any assertions that might panic.
        unsafe {
            match original {
                Some(v) => std::env::set_var("PEAKBOT_SHELL", v),
                None => std::env::remove_var("PEAKBOT_SHELL"),
            }
        }
        assert_eq!(
            kind,
            Some(ShellKind::Bash {
                path: "/custom/shell".to_string()
            }),
            "PEAKBOT_SHELL override should be respected"
        );
    }

    /// `PEAKBOT_SHELL` pointing at a PowerShell executable is inferred correctly.
    #[test]
    fn detect_infers_powershell_from_override_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        let original = std::env::var("PEAKBOT_SHELL").ok();
        unsafe {
            std::env::set_var(
                "PEAKBOT_SHELL",
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            );
        }
        let kind = ShellKind::detect();
        unsafe {
            match original {
                Some(v) => std::env::set_var("PEAKBOT_SHELL", v),
                None => std::env::remove_var("PEAKBOT_SHELL"),
            }
        }
        assert!(
            matches!(kind, Some(ShellKind::PowerShell { .. })),
            "override containing 'pwsh' should infer PowerShell, got: {:?}",
            kind
        );
    }
}
