//! Session sidecar helpers (DESIGN §5.2).

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

/// `pidfile` content: `pid=<u32>\nport=<u16>\n`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidRecord {
    pub pid: u32,
    pub port: u16,
}

impl PidRecord {
    pub fn render(&self) -> String {
        format!("pid={}\nport={}\n", self.pid, self.port)
    }

    pub fn parse(s: &str) -> Option<PidRecord> {
        let mut pid: Option<u32> = None;
        let mut port: Option<u16> = None;
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(v) = line.strip_prefix("pid=") {
                let p: u32 = v.parse().ok()?;
                pid = Some(p);
            } else {
                let v = line.strip_prefix("port=")?;
                let p: u16 = v.parse().ok()?;
                port = Some(p);
            }
        }
        Some(PidRecord {
            pid: pid?,
            port: port?,
        })
    }
}

/// Identity check for the stale-pidfile reaper (DESIGN §4.6): cmdline
/// contains `peakbot` AND `--bind <host>:<rec.port>` where host is
/// 127.0.0.1 (loopback) or 0.0.0.0 (remote sessions).
pub fn is_our_sidecar(cmdline: &str, rec: &PidRecord) -> bool {
    if !cmdline.contains("peakbot") {
        return false;
    }
    let port = format!(":{}", rec.port);
    cmdline.contains(&format!("--bind 127.0.0.1{port}"))
        || cmdline.contains(&format!("--bind 0.0.0.0{port}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyExit {
    PortTaken,
    Crashed,
}

/// Log tail contains `already in use` → PortTaken, else Crashed.
/// (`code` is kept for the locked signature; the tail is the signal.)
pub fn classify_early_exit(_code: Option<i32>, log_tail: &str) -> EarlyExit {
    if log_tail.contains("already in use") {
        EarlyExit::PortTaken
    } else {
        EarlyExit::Crashed
    }
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

    // §9.1 case 18.
    #[test]
    fn t18_pid_record_roundtrip_and_rejects() {
        let rec = PidRecord {
            pid: 4242,
            port: 7824,
        };
        let rt = PidRecord::parse(&rec.render()).expect("render→parse roundtrip");
        assert_eq!(rt, rec);
        assert_eq!(PidRecord::parse("pid=4242\n"), None, "missing port");
        assert_eq!(
            PidRecord::parse("pid=x\nport=7824\n"),
            None,
            "non-numeric pid"
        );
        assert_eq!(PidRecord::parse(""), None, "empty");
    }

    // §9.1 case 19 (updated for #377: the reaper also accepts the
    // remote 0.0.0.0 bind).
    #[test]
    fn t19_is_our_sidecar() {
        let rec = PidRecord { pid: 1, port: 7824 };
        assert!(
            is_our_sidecar("/tmp/.mount_x/usr/bin/peakbot --bind 127.0.0.1:7824", &rec),
            "matching peakbot + port ⇒ ours"
        );
        assert!(
            is_our_sidecar(
                "/tmp/.mount_x/usr/bin/peakbot --bind 0.0.0.0:7824 --tls",
                &rec
            ),
            "remote sessions bind 0.0.0.0 (with --tls after the bind) ⇒ ours"
        );
        assert!(
            !is_our_sidecar("/tmp/.mount_x/usr/bin/peakbot --bind 127.0.0.1:7825", &rec),
            "port mismatch ⇒ not ours"
        );
        assert!(
            !is_our_sidecar("/usr/bin/vim --bind 127.0.0.1:7824", &rec),
            "not peakbot ⇒ not ours"
        );
        assert!(
            !is_our_sidecar("peakbot service status", &rec),
            "peakbot without `--bind <host>:<port>` ⇒ not ours"
        );
    }

    // §9.1 case 20.
    #[test]
    fn t20_classify_early_exit() {
        assert_eq!(
            classify_early_exit(Some(1), "127.0.0.1:7824 is already in use — try again"),
            EarlyExit::PortTaken
        );
        assert_eq!(
            classify_early_exit(Some(1), "panic: something else"),
            EarlyExit::Crashed
        );
        assert_eq!(classify_early_exit(None, ""), EarlyExit::Crashed);
    }

    // §9.1 case 21.
    #[test]
    fn t21_tail_lines() {
        assert_eq!(tail_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(tail_lines("a\nb", 5), "a\nb", "fewer lines → all");
        assert_eq!(tail_lines("", 3), "", "empty → empty");
    }
}
