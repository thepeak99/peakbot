//! Shell detection for cross-platform command execution.
//!
//! Detects the best available shell at runtime, prioritising:
//! 1. `PEAKBOT_SHELL` environment override
//! 2. WSL environments (treat as Linux)
//! 3. Git Bash on Windows
//! 4. PowerShell on Windows
//! 5. Unix `/bin/sh` fallback

use std::path::Path;

/// PS 5.1 pipes in the OEM codepage; this forces UTF-8 both ways, on the same
/// line as the command so error line numbers don't shift.
pub const PS_UTF8_SETUP: &str =
    "$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ";

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

    /// Tool/shell name; matches the registered tool's `NAME`.
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

    /// Arguments that make this shell run `command` and exit. The single
    /// place that knows each shell's command flag and per-shell setup.
    pub fn args(&self, command: &str) -> [String; 2] {
        match self {
            ShellKind::Bash { .. } => ["-c".to_string(), command.to_string()],
            ShellKind::PowerShell { .. } => {
                ["-Command".to_string(), format!("{PS_UTF8_SETUP}{command}")]
            }
        }
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

    /// PR2: `cmd_arg()` / `is_bash()` are removed; the command flag lives
    /// in `args()`. This test is the migrated version of
    /// `shell_kind_name_matches_variant` — it pins name + first arg per
    /// variant.
    #[test]
    fn shell_kind_name_and_command_flag_match_variant() {
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        assert_eq!(bash.name(), "bash");
        assert_eq!(bash.args("echo hi")[0], "-c");

        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        assert_eq!(ps.name(), "powershell");
        assert_eq!(ps.args("Get-Process")[0], "-Command");
    }

    // ── PR2: `args()` + `PS_UTF8_SETUP` (RED until implemented) ─────────

    /// Bash-style shells pass the command verbatim after `-c`.
    #[test]
    fn bash_args_are_c_flag_and_command() {
        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        assert_eq!(
            bash.args("echo hi"),
            ["-c".to_string(), "echo hi".to_string()]
        );

        // A Git Bash path on Windows is still Bash-shaped.
        let git_bash = ShellKind::Bash {
            path: r"C:\Program Files\Git\bin\bash.exe".to_string(),
        };
        assert_eq!(
            git_bash.args("ls -la"),
            ["-c".to_string(), "ls -la".to_string()]
        );
    }

    /// pwsh.exe (PowerShell 7+) gets `-Command` with the UTF-8 setup
    /// prefixed and the command appended.
    #[test]
    fn powershell_pwsh_args_prepend_utf8_setup() {
        let ps = ShellKind::PowerShell {
            path: r"C:\Program Files\PowerShell\7\pwsh.exe".to_string(),
        };
        let args = ps.args("Write-Host hi");
        assert_eq!(args[0], "-Command");
        assert!(
            args[1].starts_with(PS_UTF8_SETUP),
            "args[1] must start with the UTF-8 setup, was: {0:?}",
            args[1]
        );
        assert!(
            args[1].ends_with("Write-Host hi"),
            "args[1] must end with the command, was: {0:?}",
            args[1]
        );
    }

    /// powershell.exe (legacy 5.1) gets the SAME treatment as pwsh — the
    /// codepage fix must not depend on which binary is installed.
    #[test]
    fn powershell_legacy_51_args_prepend_utf8_setup() {
        let ps = ShellKind::PowerShell {
            path: r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
        };
        let args = ps.args("Get-Process");
        assert_eq!(args[0], "-Command");
        assert!(
            args[1].starts_with(PS_UTF8_SETUP),
            "legacy powershell.exe must get the UTF-8 setup too, was: {0:?}",
            args[1]
        );
        assert!(
            args[1].ends_with("Get-Process"),
            "args[1] must end with the command, was: {0:?}",
            args[1]
        );
    }

    /// The setup must force BOM-less UTF-8 (`UTF8Encoding]::new($false)`)
    /// and stay a single line: the whole thing rides on one `-Command`
    /// argument. A multi-line command keeps its own newlines — only the
    /// *setup portion* must be newline-free.
    #[test]
    fn powershell_utf8_setup_is_single_line_and_bomless() {
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        let cmd = "line one\nline two";
        let args = ps.args(cmd);
        assert!(
            args[1].contains("UTF8Encoding]::new($false)"),
            "setup must construct UTF8Encoding with $false (no BOM), was: {0:?}",
            args[1]
        );
        let setup = &args[1][..PS_UTF8_SETUP.len()];
        assert!(
            !setup.contains('\n'),
            "the setup portion must contain no newline, was: {setup:?}"
        );
        assert!(
            args[1].ends_with(cmd),
            "a multi-line command must keep its own newlines verbatim, was: {0:?}",
            args[1]
        );
        assert_eq!(
            PS_UTF8_SETUP,
            "$OutputEncoding = [Console]::OutputEncoding = \
             [System.Text.UTF8Encoding]::new($false); "
        );
    }

    /// `name()` must stay in lockstep with the tool names so the model
    /// sees one shell under one name.
    #[test]
    fn shell_kind_name_matches_tool_names() {
        use super::super::{BashTool, PowerShellTool};
        use rig_core::tool::Tool;

        let bash = ShellKind::Bash {
            path: "/bin/sh".to_string(),
        };
        let ps = ShellKind::PowerShell {
            path: "pwsh.exe".to_string(),
        };
        assert_eq!(bash.name(), BashTool::NAME);
        assert_eq!(ps.name(), PowerShellTool::NAME);
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
        // PR2: `is_bash()` is removed — match on the variant directly.
        assert!(
            matches!(sk, ShellKind::Bash { .. }),
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
