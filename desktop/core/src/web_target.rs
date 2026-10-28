//! `WebTarget` — the desktop's lenient view of `general.web` (0.24
//! config, spec §8.4). `config.yaml` is the single source of truth for
//! the desktop's backend URL; the desktop owns no token file and passes
//! no competing web config to the child.
//!
//! `read_web_target` deserializes the config.yaml text into
//! `serde_yaml::Value` and picks out ONLY `general.web.{bind, tls,
//! token}` — unknown keys are ignored on purpose (validation is
//! PeakBot's job). A missing, legacy (no `general:`), unparsable, or
//! empty file gives the defaults `127.0.0.1:7823`, `tls: false`, no
//! token; a malformed per-key value (bad bind, non-bool tls,
//! unparseable token) falls back to THAT key's default. The desktop
//! never refuses to start — the child reports the real problem at boot
//! (`needs_setup`).

use std::net::SocketAddr;

use url::Url;

use crate::token::Token;

/// The lenient `general.web` view the desktop builds its backend URL
/// from (spec §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebTarget {
    /// Effective bind; default `127.0.0.1:7823` (the peakbot server's
    /// default).
    pub bind: SocketAddr,
    /// `general.web.tls`; default `false`.
    pub tls: bool,
    /// `general.web.token`; default `None` (blank/unparseable ⇒ absent).
    pub token: Option<Token>,
}

impl WebTarget {
    /// `"<scheme>://<host>:<port>"` — no trailing slash, no token (`url`
    /// and `probe_url` add the `?token=` query). Scheme is
    /// `https` iff `tls`; the host is `127.0.0.1` when the bind IP is
    /// loopback or unspecified (`0.0.0.0`, `::`), otherwise the bind IP
    /// (spec §8.4 "When it reads it").
    pub fn url_base(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.host_port())
    }

    /// `"<host>:<port>"` — the authority part of `url_base`.
    pub fn host_port(&self) -> String {
        let ip = self.bind.ip();
        let host = if ip.is_loopback() || ip.is_unspecified() {
            "127.0.0.1".to_string()
        } else if ip.is_ipv6() {
            // A bare IPv6 literal is not a valid URL host.
            format!("[{ip}]")
        } else {
            ip.to_string()
        };
        format!("{host}:{}", self.bind.port())
    }

    /// `{url_base}/` [+ `?token=<percent-encoded>`].
    pub fn url(&self) -> Url {
        self.with_token(&format!("{}/", self.url_base()))
    }

    /// `{url_base}/commands` [+ `?token=…`].
    pub fn probe_url(&self) -> Url {
        self.with_token(&format!("{}/commands", self.url_base()))
    }

    fn with_token(&self, base: &str) -> Url {
        let mut u = Url::parse(base).expect("url_base is always a valid URL");
        if let Some(tok) = &self.token {
            u.query_pairs_mut().append_pair("token", tok.as_str());
        }
        u
    }
}

/// The peakbot server's default bind (spec §8.4: the desktop mirrors it).
const DEFAULT_BIND: &str = "127.0.0.1:7823";

/// Lenient read of ONLY `general.web.{bind, tls, token}` from the
/// config.yaml text. `text` is `None` when the file is missing (file
/// I/O happens in the controller; this is pure). See the module docs
/// for the leniency contract.
pub fn read_web_target(text: Option<&str>) -> WebTarget {
    let mut bind: SocketAddr = DEFAULT_BIND.parse().expect("static default is valid");
    let mut tls = false;
    let mut token: Option<Token> = None;
    if let Some(text) = text
        && let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(text)
        && let Some(web) = doc.get("general").and_then(|g| g.get("web"))
    {
        if let Some(b) = web.get("bind").and_then(serde_yaml::Value::as_str)
            && let Ok(addr) = b.parse::<SocketAddr>()
        {
            bind = addr;
        }
        tls = web
            .get("tls")
            .and_then(serde_yaml::Value::as_bool)
            .unwrap_or(false);
        token = web
            .get("token")
            .and_then(serde_yaml::Value::as_str)
            .and_then(Token::parse);
    }
    WebTarget { bind, tls, token }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;
    use std::net::SocketAddr;

    fn defaults() -> WebTarget {
        WebTarget {
            bind: "127.0.0.1:7823"
                .parse::<SocketAddr>()
                .expect("static default is valid"),
            tls: false,
            token: None,
        }
    }

    fn token(s: &str) -> Token {
        Token::parse(s).expect("test token parses")
    }

    // T15a — spec §8.4/§11.6: missing file, legacy file, unparsable file
    // → defaults. The desktop never refuses; PeakBot reports real problems.
    #[test]
    fn missing_legacy_unparsable_give_defaults() {
        // Missing file.
        assert_eq!(read_web_target(None), defaults(), "missing file ⇒ defaults");

        // Legacy 0.23 layout: no `general:` key (web bind/tls were CLI
        // flags, the token lived in a separate file the desktop no longer
        // reads).
        let legacy = "default_model: gpt\nauto_save: true\npipelines:\n  - name: team\n";
        assert_eq!(
            read_web_target(Some(legacy)),
            defaults(),
            "legacy file (no general:) ⇒ defaults"
        );

        // Unparsable YAML.
        assert_eq!(
            read_web_target(Some(":::\n- [\n  bad")),
            defaults(),
            "unparsable file ⇒ defaults"
        );

        // Empty file.
        assert_eq!(
            read_web_target(Some("")),
            defaults(),
            "empty file ⇒ defaults"
        );

        // `general:` present but no `web:` block.
        assert_eq!(
            read_web_target(Some("general:\n  cost_tracking: true\n")),
            defaults(),
            "general without web ⇒ defaults"
        );
    }

    // T15a — spec §8.4: the URL host is 127.0.0.1 when the bind IP is
    // loopback or unspecified (`0.0.0.0`, `::`).
    #[test]
    fn unspecified_bind_maps_to_loopback() {
        let t = read_web_target(Some("general:\n  web:\n    bind: 0.0.0.0:9000\n"));
        assert_eq!(
            t.bind,
            "0.0.0.0:9000".parse::<SocketAddr>().unwrap(),
            "the bind itself stays 0.0.0.0:9000"
        );
        assert_eq!(
            t.url_base(),
            "http://127.0.0.1:9000",
            "0.0.0.0 bind ⇒ URL host 127.0.0.1, port kept"
        );

        let t = read_web_target(Some("general:\n  web:\n    bind: \"[::]:9000\"\n"));
        assert_eq!(
            t.url_base(),
            "http://127.0.0.1:9000",
            "IPv6 unspecified bind ⇒ URL host 127.0.0.1"
        );

        let t = read_web_target(Some("general:\n  web:\n    bind: 127.0.0.1:9000\n"));
        assert_eq!(
            t.url_base(),
            "http://127.0.0.1:9000",
            "explicit loopback bind passes through"
        );
    }

    // T15a — spec §8.4: the scheme is `https` iff `tls`; the token is read
    // from `general.web.token`.
    #[test]
    fn tls_gives_https() {
        let t = read_web_target(Some("general:\n  web:\n    tls: true\n"));
        assert!(t.tls, "tls: true is read");
        assert_eq!(t.url_base(), "https://127.0.0.1:7823");

        let t = read_web_target(Some("general:\n  web:\n    tls: false\n"));
        assert!(!t.tls, "tls: false is read");
        assert_eq!(t.url_base(), "http://127.0.0.1:7823");

        // tls absent ⇒ http (default).
        assert_eq!(
            read_web_target(None).url_base(),
            "http://127.0.0.1:7823",
            "defaults ⇒ http"
        );

        // https + unspecified bind combine.
        let t = read_web_target(Some(
            "general:\n  web:\n    bind: 0.0.0.0:9000\n    tls: true\n",
        ));
        assert_eq!(t.url_base(), "https://127.0.0.1:9000");

        // The token is read verbatim into a Token.
        let t = read_web_target(Some(
            "general:\n  web:\n    token: 0123456789abcdef0123456789abcdef\n",
        ));
        assert_eq!(
            t.token,
            Some(token("0123456789abcdef0123456789abcdef")),
            "general.web.token is read"
        );
    }

    // T15a — spec §8.4: unknown keys are ignored on purpose (validation is
    // PeakBot's job); a realistic full 0.24 config parses.
    #[test]
    fn unknown_keys_ignored() {
        let yaml = "\
providers:
  openrouter:
    type: openrouter
    api_key: sk-test
mcps:
  - name: linear
    type: streamable-http
profiles:
  - name: dev
    agents:
      - name: main
        model: openrouter/default
general:
  cost_tracking: true
  session_ttl_secs: 600
  web:
    bind: 192.168.1.5:9000
    tls: true
    token: abcdefabcdefabcdefabcdefabcdef
    some_future_key: whatever
";
        let t = read_web_target(Some(yaml));
        assert_eq!(
            t.bind,
            "192.168.1.5:9000".parse::<SocketAddr>().unwrap(),
            "bind read from a full config"
        );
        assert!(t.tls, "tls read from a full config");
        assert_eq!(
            t.token,
            Some(token("abcdefabcdefabcdefabcdefabcdef")),
            "token read from a full config"
        );
        assert_eq!(t.url_base(), "https://192.168.1.5:9000");
    }

    // Edge — spec §8.4: only loopback/unspecified map to 127.0.0.1; a LAN
    // bind IP passes through the URL verbatim.
    #[test]
    fn lan_bind_passes_through_in_url_base() {
        let t = read_web_target(Some("general:\n  web:\n    bind: 192.168.1.5:9000\n"));
        assert_eq!(t.url_base(), "http://192.168.1.5:9000");
    }

    // Edge — leniency: a malformed per-key value falls back to THAT key's
    // default (the desktop never refuses to start over a config typo; the
    // server reports the real problem at boot).
    #[test]
    fn malformed_values_fall_back_to_defaults() {
        let t = read_web_target(Some(
            "general:\n  web:\n    bind: not-an-addr\n    tls: \"yes\"\n    token: \"a b\"\n",
        ));
        assert_eq!(
            t.bind,
            "127.0.0.1:7823".parse::<SocketAddr>().unwrap(),
            "bad bind ⇒ default bind"
        );
        assert!(!t.tls, "non-bool tls ⇒ false");
        assert_eq!(t.token, None, "unparseable token ⇒ None");
    }

    // Edge — spec §8.4 resolve_token semantics: a blank token collapses to
    // absent.
    #[test]
    fn blank_token_is_absent() {
        let t = read_web_target(Some("general:\n  web:\n    token: \"\"\n"));
        assert_eq!(t.token, None, "blank token ⇒ None");
    }

    fn target(bind: &str, tls: bool, tok: Option<Token>) -> WebTarget {
        WebTarget {
            bind: bind.parse().expect("test bind parses"),
            tls,
            token: tok,
        }
    }

    // §9.1 case 28, ported from the deleted `Mode` to
    // `WebTarget::{url, probe_url}` (close-to-tray plan §C): the token must
    // be percent-encoded in the query.
    #[test]
    fn t28_web_target_url_and_probe_url() {
        let t = target(
            "127.0.0.1:7824",
            false,
            Some(Token::parse("tok+en&1").expect("token with + & parses")),
        );
        assert_eq!(t.bind.port(), 7824);
        assert_eq!(
            t.url().to_string(),
            "http://127.0.0.1:7824/?token=tok%2Ben%261",
            "the token must be percent-encoded"
        );
        let probe = t.probe_url();
        assert_eq!(probe.path(), "/commands");
        assert_eq!(
            probe.to_string(),
            "http://127.0.0.1:7824/commands?token=tok%2Ben%261"
        );
    }

    // Design #377 t73, ported to `WebTarget`: the https variants and the
    // tokenless target.
    #[test]
    fn t73_web_target_https_urls_and_tokenless() {
        let tok = Token::parse("tok+en&1").expect("token with + & parses");

        let t = target("127.0.0.1:7824", true, Some(tok));
        assert_eq!(
            t.url().to_string(),
            "https://127.0.0.1:7824/?token=tok%2Ben%261"
        );
        assert_eq!(
            t.probe_url().to_string(),
            "https://127.0.0.1:7824/commands?token=tok%2Ben%261"
        );
        assert_eq!(t.url_base(), "https://127.0.0.1:7824");

        // Tokenless: no `?token=` anywhere.
        let t = target("127.0.0.1:7824", false, None);
        assert_eq!(t.url().to_string(), "http://127.0.0.1:7824/");
        assert_eq!(t.probe_url().to_string(), "http://127.0.0.1:7824/commands");
        assert_eq!(t.url_base(), "http://127.0.0.1:7824");

        let t = target("127.0.0.1:7823", true, None);
        assert_eq!(t.bind.port(), 7823);
        assert_eq!(t.url().to_string(), "https://127.0.0.1:7823/");
        assert_eq!(t.probe_url().to_string(), "https://127.0.0.1:7823/commands");
    }

    // §9.1 case 29, ported to `WebTarget`: the host follows
    // `general.web.bind` — unspecified ⇒ 127.0.0.1, a concrete LAN IP ⇒ that
    // IP.
    #[test]
    fn t29_web_target_url_follows_bind_host() {
        let t = target("127.0.0.1:7823", false, None);
        assert_eq!(t.url().to_string(), "http://127.0.0.1:7823/");
        assert_eq!(t.url_base(), "http://127.0.0.1:7823");

        let t = target("0.0.0.0:9000", false, None);
        assert_eq!(t.url().to_string(), "http://127.0.0.1:9000/");
        assert_eq!(t.probe_url().to_string(), "http://127.0.0.1:9000/commands");

        let t = target("192.168.1.5:9000", true, None);
        assert_eq!(
            t.probe_url().to_string(),
            "https://192.168.1.5:9000/commands"
        );
        assert_eq!(t.url_base(), "https://192.168.1.5:9000");

        // A bracketed IPv6 LAN literal stays a valid URL host.
        let t = target("[2001:db8::1]:9000", false, None);
        assert_eq!(t.url().to_string(), "http://[2001:db8::1]:9000/");
    }

    // The `url()` the webview navigates to is `{url_base}/` + the token
    // query, so the nav guard's allowed origin (`url_base()`) always matches.
    #[test]
    fn url_starts_with_url_base_and_carries_token_query() {
        let t = target(
            "192.168.1.5:9000",
            true,
            Some(Token::parse("abc123").expect("parses")),
        );
        let url = t.url();
        assert!(
            url.as_str().starts_with(&format!("{}/", t.url_base())),
            "{url} must start with {}/",
            t.url_base()
        );
        assert_eq!(url.query(), Some("token=abc123"));
    }
}
