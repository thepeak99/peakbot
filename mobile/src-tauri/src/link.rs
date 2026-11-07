//! `ServerLink` — THE pairing-link parser. `parse` is the only
//! constructor, so downstream code holds a valid, normalized link whose token
//! is reachable only through `url()`.

use std::fmt;
use url::Url;

/// The five user-facing rejections for a pairing link. `Display` wording is
/// locked; these are what the UI surfaces verbatim.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LinkError {
    NotAUrl,
    NotHttps,
    NoHost,
    NoToken,
    HasCredentials,
}

impl std::error::Error for LinkError {}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                LinkError::NotAUrl => "That isn't a link.",
                LinkError::NotHttps =>
                    "PeakBot Mobile only connects over HTTPS — start PeakBot with general.web.tls: true.",
                LinkError::NoHost => "The link has no server address.",
                LinkError::NoToken =>
                    "The link has no access token — copy the full link PeakBot prints (…/?token=…).",
                LinkError::HasCredentials => "Links with a username or password aren't supported.",
            }
        )
    }
}

/// A validated PeakBot pairing link: `https://host[:port]/?token=…`.
/// `parse` is the sole constructor; the token is only ever visible through
/// `url()` (and the persisted profiles.json). `Debug`/`host_label()` drop it.
#[derive(Clone, PartialEq, Eq)]
pub struct ServerLink(url::Url);

impl ServerLink {
    /// Sanctioned token carrier — the normalized URL.
    pub fn url(&self) -> &url::Url {
        &self.0
    }

    /// What origins/cookie jars compare against.
    pub fn origin(&self) -> url::Origin {
        self.0.origin()
    }

    /// `host[:port]` (explicit port only); IPv6 stays bracketed. Never the token.
    pub fn host_label(&self) -> String {
        let host = self.0.host_str().expect("ServerLink always has a host");
        match self.0.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        }
    }

    /// Trim + apply the rules in order: parse (NotAUrl) → scheme https
    /// (NotHttps) → host (NoHost) → no userinfo (HasCredentials) → exactly one
    /// non-empty `token` (NoToken), then normalize (path `/`, raw `token=…`,
    /// no fragment, port kept).
    pub fn parse(text: &str) -> Result<ServerLink, LinkError> {
        let url = Url::parse(text.trim()).map_err(|_| LinkError::NotAUrl)?;

        if url.scheme() != "https" {
            return Err(LinkError::NotHttps);
        }
        if !url.has_host() {
            return Err(LinkError::NoHost);
        }
        if !url.username().is_empty() {
            return Err(LinkError::HasCredentials);
        }
        let raw_token = match extract_raw_token(url.query()) {
            Some(t) => t,
            None => return Err(LinkError::NoToken),
        };

        let mut normalized = url.clone();
        normalized.set_path("/");
        normalized.set_query(Some(&format!("token={raw_token}")));
        normalized.set_fragment(None);
        Ok(ServerLink(normalized))
    }
}

/// The raw (percent-encoding preserved) value of the unique, non-empty `token`
/// query param, or `None` if there isn't exactly one.
fn extract_raw_token(query: Option<&str>) -> Option<String> {
    let mut count = 0usize;
    let mut token: Option<String> = None;
    for part in query?.split('&') {
        if let Some((key, value)) = part.split_once('=')
            && key == "token"
        {
            count += 1;
            token = Some(value.to_string());
        }
    }
    if count != 1 {
        return None;
    }
    let token = token?;
    if token.is_empty() {
        return None;
    }
    Some(token)
}

impl fmt::Debug for ServerLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Origin only — never the token (secrecy P4).
        let port = self.0.port().map(|p| format!(":{p}")).unwrap_or_default();
        let host = self.0.host_str().unwrap_or("<no host>");
        write!(f, "ServerLink({0}://{1}{2})", self.0.scheme(), host, port)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ServerLink, LinkError> {
        ServerLink::parse(text)
    }

    fn ok(text: &str) -> ServerLink {
        parse(text).unwrap_or_else(|e| panic!("expected {text:?} to parse, got {e:?}"))
    }

    // ── parse rules, in order (first failure wins) ───────────────────────

    #[test]
    fn parse_rejects_garbage_as_not_a_url() {
        for bad in [
            "not a url",
            "https://", // empty host: the url crate rejects it at parse
            "",
            "   ",
            "://nope",
            "ht!tp://h/?token=t",
        ] {
            match parse(bad) {
                Err(LinkError::NotAUrl) => {}
                other => panic!("expected NotAUrl for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_trims_surrounding_whitespace() {
        let link = ok("  \nhttps://h:7823/?token=t  \r\n");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
    }

    #[test]
    fn parse_rejects_http_scheme_as_not_https() {
        match parse("http://h:7823/?token=t") {
            Err(LinkError::NotHttps) => {}
            other => panic!("expected NotHttps, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_non_https_scheme_as_not_https() {
        for bad in ["ftp://h:7823/?token=t", "tauri://localhost"] {
            match parse(bad) {
                Err(LinkError::NotHttps) => {}
                other => panic!("expected NotHttps for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_rule_order_scheme_before_credentials() {
        // http AND user:pass → the scheme check must fire first.
        match parse("http://user:pass@h:7823/?token=t") {
            Err(LinkError::NotHttps) => {}
            other => panic!("expected NotHttps (scheme before userinfo), got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_userinfo_as_has_credentials() {
        for bad in [
            "https://user:pass@h:7823/?token=t",
            "https://user@h:7823/?token=t",
        ] {
            match parse(bad) {
                Err(LinkError::HasCredentials) => {}
                other => panic!("expected HasCredentials for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_rule_order_credentials_before_token() {
        // user:pass AND no token → the userinfo check must fire first.
        match parse("https://user@h:7823/") {
            Err(LinkError::HasCredentials) => {}
            other => panic!("expected HasCredentials (userinfo before token), got {other:?}"),
        }
    }

    #[test]
    fn parse_requires_exactly_one_nonempty_token() {
        for bad in [
            "https://h:7823/",
            "https://h:7823/?token=", // empty token
            "https://h:7823/?other=1",
            "https://h:7823/?token=a&token=b", // two tokens
        ] {
            match parse(bad) {
                Err(LinkError::NoToken) => {}
                other => panic!("expected NoToken for {bad:?}, got {other:?}"),
            }
        }
    }

    // ── normalization ────────────────────────────────────────────────────

    #[test]
    fn parse_normalizes_path_to_root() {
        let link = ok("https://h:7823/old/deep/path?token=t");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
    }

    #[test]
    fn parse_drops_fragment() {
        let link = ok("https://h:7823/?token=t#frag");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
    }

    #[test]
    fn parse_keeps_explicit_port() {
        let link = ok("https://h:7823/?token=t");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
        assert_eq!(link.url().port(), Some(7823));
    }

    #[test]
    fn parse_accepts_https_with_default_port() {
        let link = ok("https://h/?token=t");
        assert_eq!(link.url().as_str(), "https://h/?token=t");
        assert_eq!(link.url().port(), None);
    }

    #[test]
    fn parse_drops_extra_params() {
        let link = ok("https://h:7823/?token=t&junk=1&more=2");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
    }

    #[test]
    fn parse_accepts_027_link_with_ca_and_profile_and_drops_both() {
        // 0.27.0 entry links carry `&ca=sha256:<64-hex>` and may carry
        // `&profile=<name>` — accepted, then dropped like any extra param.
        let ca = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
        let link = ok(&format!(
            "https://h:7823/?token=t&ca=sha256:{ca}&profile=Home"
        ));
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");

        // parameter order must not matter
        let link = ok(&format!(
            "https://h:7823/?ca=sha256:{ca}&token=t&profile=Home"
        ));
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
    }

    #[test]
    fn parse_query_token_is_raw_byte_for_byte() {
        // `a.b-c_d`: legal unescaped in a query — must survive verbatim.
        let link = ok("https://h:7823/?token=a.b-c_d");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=a.b-c_d");
    }

    #[test]
    fn parse_query_token_percent_escapes_stay_raw() {
        // A token the server percent-escaped stays EXACTLY as pasted:
        // re-encoding or decoding (`%2B` → `+`) would break the server's
        // raw-query token comparison.
        let link = ok("https://h:7823/?token=%2B%3Dx");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=%2B%3Dx");
    }

    #[test]
    fn parse_scheme_is_case_insensitive() {
        let link = ok("HTTPS://h:7823/?token=t");
        assert_eq!(link.url().as_str(), "https://h:7823/?token=t");
    }

    // ── origin ───────────────────────────────────────────────────────────

    #[test]
    fn origin_is_scheme_host_port() {
        let link = ok("https://h:7823/?token=t");
        assert_eq!(
            link.origin(),
            url::Url::parse("https://h:7823").unwrap().origin()
        );
    }

    #[test]
    fn origin_is_port_sensitive() {
        let a = ok("https://h:7823/?token=t");
        let b = ok("https://h:7824/?token=t");
        assert_ne!(a.origin(), b.origin());
    }

    #[test]
    fn same_origin_across_tokens() {
        // The basis of P3: two entries for one origin would share a cookie jar.
        let a = ok("https://h:7823/?token=first");
        let b = ok("https://h:7823/?token=second");
        assert_eq!(a.origin(), b.origin());
    }

    // ── secrecy: the token lives only in url() (and profiles.json) ───────

    const SECRET: &str = "deadbeefcafe1234deadbeefcafe1234";

    #[test]
    fn url_carries_the_token() {
        let link = ok(&format!("https://h:7823/?token={SECRET}"));
        assert!(
            link.url().as_str().contains(SECRET),
            "url() is the sanctioned token carrier"
        );
    }

    #[test]
    fn debug_never_contains_the_token() {
        let link = ok(&format!("https://h:7823/?token={SECRET}"));
        let dbg = format!("{link:?}");
        assert!(!dbg.contains(SECRET), "Debug leaked the token: {dbg}");
        // Debug shows the origin (the only permitted content).
        assert!(
            dbg.contains("https://h:7823"),
            "Debug should show the origin: {dbg}"
        );
    }

    #[test]
    fn host_label_never_contains_the_token() {
        let link = ok(&format!("https://h:7823/?token={SECRET}"));
        let label = link.host_label();
        assert!(
            !label.contains(SECRET),
            "host_label leaked the token: {label}"
        );
    }

    #[test]
    fn host_label_is_host_port() {
        assert_eq!(ok("https://h:7823/?token=t").host_label(), "h:7823");
        // default port is not spelled out
        assert_eq!(ok("https://h/?token=t").host_label(), "h");
        assert_eq!(ok("https://[::1]:7823/?token=t").host_label(), "[::1]:7823");
    }

    // ── the locked user-facing strings ─────────────────────────

    #[test]
    fn link_error_display_strings_are_locked() {
        assert_eq!(LinkError::NotAUrl.to_string(), "That isn't a link.");
        assert_eq!(
            LinkError::NotHttps.to_string(),
            "PeakBot Mobile only connects over HTTPS — start PeakBot with general.web.tls: true."
        );
        assert_eq!(
            LinkError::NoHost.to_string(),
            "The link has no server address."
        );
        assert_eq!(
            LinkError::NoToken.to_string(),
            "The link has no access token — copy the full link PeakBot prints (…/?token=…)."
        );
        assert_eq!(
            LinkError::HasCredentials.to_string(),
            "Links with a username or password aren't supported."
        );
    }
}
