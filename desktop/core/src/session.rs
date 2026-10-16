//! Session sidecar helpers (DESIGN §5.2).

use crate::settings::{Protocol, Settings};
use crate::token::Token;

/// `["--bind", "{bind_host}:{port}"]` + `["--tls"]` when https. The
/// bind host comes from the settings (remote ⇒ 0.0.0.0).
pub fn session_args(port: u16, s: Settings) -> Vec<String> {
    let mut args = vec!["--bind".to_string(), format!("{}:{}", s.bind_host(), port)];
    if s.protocol() == Protocol::Https {
        args.push("--tls".to_string());
    }
    args
}

/// `[("PEAKBOT_WEB_TOKEN", tok)]?` + `("PEAKBOT_NO_OPEN", "1")` — exactly
/// these (on top of the sanitized base env). No token ⇒ no
/// `PEAKBOT_WEB_TOKEN` at all.
pub fn session_env(token: Option<&Token>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(t) = token {
        env.push(("PEAKBOT_WEB_TOKEN".to_string(), t.as_str().to_string()));
    }
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

    // §9.1 case 17 (updated for #377: args take `Settings` — bind host
    // and `--tls` derive from it; env takes an optional token).
    #[test]
    fn t17_session_args_and_env() {
        use crate::settings::{Protocol, Settings};
        assert_eq!(
            session_args(7824, Settings::default()),
            vec!["--bind".to_string(), "127.0.0.1:7824".to_string()]
        );
        assert_eq!(
            session_args(
                7824,
                Settings::Loopback {
                    protocol: Protocol::Https,
                    token: true
                }
            ),
            vec![
                "--bind".to_string(),
                "127.0.0.1:7824".to_string(),
                "--tls".to_string()
            ],
            "https ⇒ --tls after the bind"
        );
        assert_eq!(
            session_args(7824, Settings::Remote),
            vec![
                "--bind".to_string(),
                "0.0.0.0:7824".to_string(),
                "--tls".to_string()
            ],
            "remote ⇒ 0.0.0.0 bind + --tls"
        );

        let tok = Token::parse("s3cret").expect("parses");
        let env = session_env(Some(&tok));
        assert_eq!(env.len(), 2, "exactly the two vars; got {env:?}");
        assert!(
            env.iter()
                .any(|(k, v)| k == "PEAKBOT_WEB_TOKEN" && v == "s3cret"),
            "got {env:?}"
        );
        assert!(
            env.iter().any(|(k, v)| k == "PEAKBOT_NO_OPEN" && v == "1"),
            "got {env:?}"
        );

        // Token off: only PEAKBOT_NO_OPEN — no PEAKBOT_WEB_TOKEN at all.
        assert_eq!(
            session_env(None),
            vec![("PEAKBOT_NO_OPEN".to_string(), "1".to_string())]
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
