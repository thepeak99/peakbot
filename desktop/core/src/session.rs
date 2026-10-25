//! Session sidecar helpers (DESIGN §5.2).

use std::path::PathBuf;

/// The session child's argv (spec §8.4): empty — no `--bind`, no
/// `--tls`, no `--token`. The child reads `general.web` from
/// config.yaml itself (single source of truth).
pub fn session_args() -> Vec<String> {
    Vec::new()
}

/// The session child's env: the parent env with any inherited
/// `PEAKBOT_WEB_TOKEN` REMOVED and `PEAKBOT_NO_OPEN=1` (spec §8.4) —
/// so the server and the desktop read the same token from config.yaml.
/// A pre-existing `PEAKBOT_NO_OPEN` is normalized to exactly `1`, once.
pub fn session_env(parent_env: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = parent_env
        .iter()
        .filter(|(k, _)| k != "PEAKBOT_WEB_TOKEN")
        .cloned()
        .collect::<Vec<_>>();
    env.retain(|(k, _)| k != "PEAKBOT_NO_OPEN");
    env.push(("PEAKBOT_NO_OPEN".to_string(), "1".to_string()));
    env
}

/// `pidfile` content: `pid=<u32>\nexe=<sidecar path>\n`. The exe is
/// recorded at spawn, so the reaper can identify the child even when the
/// app now runs from a different path (a new AppImage mount).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: u32,
    pub exe: PathBuf,
}

impl PidRecord {
    pub fn render(&self) -> String {
        format!("pid={}\nexe={}\n", self.pid, self.exe.display())
    }

    /// Both keys required; an unknown line (e.g. a pre-0.24 `port=`)
    /// rejects the record.
    pub fn parse(s: &str) -> Option<PidRecord> {
        let mut pid: Option<u32> = None;
        let mut exe: Option<PathBuf> = None;
        for line in s.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(v) = line.strip_prefix("pid=") {
                pid = Some(v.trim().parse().ok()?);
            } else {
                let v = line.strip_prefix("exe=")?;
                if v.is_empty() {
                    return None;
                }
                exe = Some(PathBuf::from(v));
            }
        }
        Some(PidRecord {
            pid: pid?,
            exe: exe?,
        })
    }
}

/// Identity check for the stale-pidfile reaper (DESIGN §4.6, spec
/// §8.4): the process's cmdline contains the recorded sidecar path. The
/// session child has no args to match on, and the path is what tells it
/// apart from a hand-started `peakbot` or the service binary.
pub fn is_our_sidecar(cmdline: &str, rec: &PidRecord) -> bool {
    let exe = rec.exe.to_string_lossy();
    !exe.is_empty() && cmdline.contains(exe.as_ref())
}

/// Last `n` lines; fewer lines → all; empty → empty.
pub fn tail_lines(text: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // T15a (0.24 config, spec §8.4): the session child is launched with
    // NO web flags — no `--bind`, no `--tls`, no `--token`. The child
    // reads `general.web` from config.yaml itself (single source of
    // truth). ASSUMED new signature: `pub fn session_args() -> Vec<String>`
    // (the old `(port, Settings)` form is deleted by T15b).
    #[test]
    fn session_argv_has_no_web_flags() {
        assert_eq!(
            session_args(),
            Vec::<String>::new(),
            "no --bind/--tls/--token in argv; general.web is the source"
        );
    }

    // T15a (spec §8.4): the child env is the parent env with an inherited
    // `PEAKBOT_WEB_TOKEN` REMOVED and `PEAKBOT_NO_OPEN=1` — so the server
    // and the desktop read the same token from config.yaml. ASSUMED new
    // signature: `pub fn session_env(parent_env: &[(String, String)])
    // -> Vec<(String, String)>` (the old `Option<&Token>` form is deleted
    // by T15b).
    #[test]
    fn env_strips_inherited_token() {
        let parent = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            (
                "PEAKBOT_WEB_TOKEN".to_string(),
                "inherited-secret".to_string(),
            ),
        ];
        let env = session_env(&parent);
        assert!(
            !env.iter().any(|(k, _)| k == "PEAKBOT_WEB_TOKEN"),
            "inherited PEAKBOT_WEB_TOKEN must be removed; got {env:?}"
        );
        assert!(
            env.iter().any(|(k, v)| k == "PEAKBOT_NO_OPEN" && v == "1"),
            "PEAKBOT_NO_OPEN=1 must be present; got {env:?}"
        );
        assert!(
            env.iter().any(|(k, v)| k == "PATH" && v == "/usr/bin"),
            "unrelated parent vars are kept; got {env:?}"
        );

        // No inherited token: PEAKBOT_NO_OPEN=1 on top of the parent.
        let env = session_env(&[("PATH".to_string(), "/usr/bin".to_string())]);
        assert!(
            !env.iter().any(|(k, _)| k == "PEAKBOT_WEB_TOKEN"),
            "got {env:?}"
        );
        assert!(
            env.iter().any(|(k, v)| k == "PEAKBOT_NO_OPEN" && v == "1"),
            "got {env:?}"
        );

        // A pre-existing PEAKBOT_NO_OPEN is normalized to exactly "1"
        // (once, no duplicates).
        let env = session_env(&[("PEAKBOT_NO_OPEN".to_string(), "0".to_string())]);
        assert_eq!(
            env,
            vec![("PEAKBOT_NO_OPEN".to_string(), "1".to_string())],
            "PEAKBOT_NO_OPEN must be exactly 1, once; got {env:?}"
        );

        // Empty parent env.
        assert_eq!(
            session_env(&[]),
            vec![("PEAKBOT_NO_OPEN".to_string(), "1".to_string())],
            "empty parent ⇒ just PEAKBOT_NO_OPEN=1"
        );
    }

    // §9.1 case 18 (0.24: the record carries the exe path, not a port).
    #[test]
    fn t18_pid_record_roundtrip_and_rejects() {
        let rec = PidRecord {
            pid: 4242,
            exe: PathBuf::from("/Applications/PeakBot Desktop.app/Contents/MacOS/peakbot"),
        };
        let rt = PidRecord::parse(&rec.render()).expect("render→parse roundtrip");
        assert_eq!(rt, rec, "paths with spaces survive");
        assert_eq!(PidRecord::parse("pid=4242\n"), None, "missing exe");
        assert_eq!(
            PidRecord::parse("pid=x\nexe=/app/peakbot\n"),
            None,
            "non-numeric pid"
        );
        assert_eq!(
            PidRecord::parse("pid=4242\nport=7824\n"),
            None,
            "a pre-0.24 port record is rejected"
        );
        assert_eq!(PidRecord::parse(""), None, "empty");
    }

    // §9.1 case 19 (0.24, spec §8.4: identity by executable path — the
    // session child is launched with no `--bind`).
    #[test]
    fn t19_is_our_sidecar() {
        let rec = PidRecord {
            pid: 1,
            exe: PathBuf::from("/tmp/.mount_x/usr/bin/peakbot"),
        };
        assert!(
            is_our_sidecar("/tmp/.mount_x/usr/bin/peakbot", &rec),
            "the recorded exe, no args ⇒ ours"
        );
        assert!(
            is_our_sidecar("\"/tmp/.mount_x/usr/bin/peakbot\" ", &rec),
            "a quoted cmdline (wmic) ⇒ ours"
        );
        assert!(
            !is_our_sidecar("/home/u/.local/bin/peakbot", &rec),
            "another peakbot binary (the service, a hand-started one) ⇒ not ours"
        );
        assert!(
            !is_our_sidecar("/usr/bin/vim", &rec),
            "PID reuse by another program ⇒ not ours"
        );
    }

    // §9.1 case 21.
    #[test]
    fn t21_tail_lines() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail_lines("a\nb", 5), "a\nb", "fewer lines → all");
        assert_eq!(tail_lines("", 3), "", "empty → empty");
    }
}
