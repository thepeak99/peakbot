//! `classify(url, known) -> Nav` — the webview's security boundary: it only ever displays the app page or a known profile origin.

use url::Url;

/// What a webview navigation may do. First match wins.
#[derive(Debug, PartialEq)]
pub enum Nav {
    /// The page may display (app or known profile origin).
    Allow,
    /// Open in the system browser.
    External,
    /// Navigate to the mobile-only profiles screen.
    Profiles,
    /// Ask the app to restart the Local core.
    RestartLocal,
    /// Refuse.
    Block,
}

pub const PROFILES_SENTINEL: &str = "peakbot-mobile:profiles";
pub const RESTART_SENTINEL: &str = "peakbot-mobile:restart";

/// The server's CA download. The Profiles page's [Install certificate…]
/// opens it in the system browser — it lives on the known
/// remote origin, so the origin rule would display it in the webview,
/// where a certificate can't be installed.
pub const CA_PATH: &str = "/peakbot-ca.crt";

/// Origins the app may display: the Local loopback origin + paired remotes.
/// Built per navigation so back/forward re-asks the guard (N1).
#[derive(Debug, PartialEq, Eq)]
pub struct Known<'a> {
    pub local: Option<&'a url::Origin>,
    pub remotes: &'a [url::Origin],
}

/// Classify a navigation (first match wins, in rule order).
pub fn classify(url: &Url, known: &Known<'_>) -> Nav {
    // (1)+(2): exactly the two sentinels; any other `peakbot-*:` URL falls
    // through to (6) Block — it is not an open namespace.
    match url.as_str() {
        PROFILES_SENTINEL => return Nav::Profiles,
        RESTART_SENTINEL => return Nav::RestartLocal,
        _ => {}
    }
    // (3): the app's own origin (local assets + dev server).
    if is_app_origin(url) {
        return Nav::Allow;
    }
    // (4a): the CA download on a known profile origin goes to the system
    // browser — the webview can't install a certificate.
    let o = url.origin();
    if url.path() == CA_PATH && (known.local.is_some_and(|l| o == *l) || known.remotes.contains(&o))
    {
        return Nav::External;
    }
    // (4): any known origin. The only way http: is allowed (the Local loopback).
    if known.local.is_some_and(|l| o == *l) || known.remotes.contains(&o) {
        return Nav::Allow;
    }
    // (5): open externally.
    if matches!(url.scheme(), "http" | "https" | "mailto" | "tel") {
        return Nav::External;
    }
    // (6): intent:, file:, data:, javascript:, other — refuse.
    Nav::Block
}

/// Whether the URL is the app's own webview origin (`tauri://localhost`,
/// `http(s)://tauri.localhost`).
fn is_app_origin(u: &Url) -> bool {
    if u.scheme() == "tauri" {
        return u.host_str() == Some("localhost");
    }
    matches!(u.scheme(), "http" | "https") && u.host_str() == Some("tauri.localhost")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap_or_else(|e| panic!("test URL {s:?} must parse: {e}"))
    }

    // ── the sentinels ────────────────────────────────────────────────────

    #[test]
    fn restart_sentinel_is_restart_local() {
        assert_eq!(classify(&u(RESTART_SENTINEL)), Nav::RestartLocal);
    }

    #[test]
    fn profiles_sentinel_is_blocked() {
        // The native Profiles page is gone: its sentinel is no longer
        // recognised and falls through to Block like any other.
        assert_eq!(classify(&u("peakbot-mobile:profiles")), Nav::Block);
    }

    #[test]
    fn unknown_mobile_sentinel_is_blocked() {
        // `peakbot-mobile:*` is NOT an open namespace: only the restart
        // sentinel is recognised.
        assert_eq!(classify(&u("peakbot-mobile:report")), Nav::Block);
    }

    #[test]
    fn desktop_sentinel_is_blocked() {
        assert_eq!(classify(&u("peakbot-desktop:restart")), Nav::Block);
    }

    // ── the app's own origin (the bundled error page) ────────────────────

    #[test]
    fn app_origin_tauri_localhost_is_allow() {
        assert_eq!(classify(&u("tauri://localhost")), Nav::Allow);
    }

    #[test]
    fn app_origin_http_tauri_localhost_is_allow() {
        assert_eq!(classify(&u("http://tauri.localhost/")), Nav::Allow);
    }

    #[test]
    fn app_origin_https_tauri_localhost_is_allow() {
        assert_eq!(classify(&u("https://tauri.localhost/settings")), Nav::Allow);
    }

    #[test]
    fn app_origin_with_error_page_hash_is_allow() {
        // The window's failure state is the bundled page with its
        // URL-encoded reason in the hash.
        assert_eq!(
            classify(&u("http://tauri.localhost/index.html#it%20failed")),
            Nav::Allow
        );
    }

    // ── the one origin: the loopback core, on any port ───────────────────

    #[test]
    fn loopback_core_origin_is_allow() {
        // The ONLY way http: is ever allowed: the on-device core.
        assert_eq!(classify(&u("http://127.0.0.1:47823/")), Nav::Allow);
    }

    #[test]
    fn loopback_any_port_is_allow() {
        // The core may have fallen back to an ephemeral port; the guard
        // must not remember a particular one.
        for port in [1, 47823, 53211, 9999, 65535] {
            let url = format!("http://127.0.0.1:{port}/");
            assert_eq!(classify(&u(&url)), Nav::Allow, "{url}");
        }
    }

    #[test]
    fn loopback_subpath_and_query_is_allow() {
        assert_eq!(
            classify(&u("http://127.0.0.1:47823/settings?token=ab12")),
            Nav::Allow
        );
    }

    #[test]
    fn localhost_name_is_not_the_core_origin() {
        // The core binds 127.0.0.1; `localhost` is a different host name
        // (and the NSC blocks its cleartext), so it is a foreign server.
        assert_eq!(classify(&u("http://localhost:47823/")), Nav::External);
    }

    #[test]
    fn https_on_loopback_is_external() {
        // The core is plain http; an https server on loopback is a
        // foreign origin like any other.
        assert_eq!(classify(&u("https://127.0.0.1:8443/")), Nav::External);
    }

    // ── remotes: every http(s) URL that is not the loopback core ─────────

    #[test]
    fn https_remote_is_external() {
        // Remote servers are reached through the core's relay, never
        // loaded in the webview.
        assert_eq!(classify(&u("https://nas.lan:7823/")), Nav::External);
    }

    #[test]
    fn https_remote_subpath_is_external() {
        assert_eq!(
            classify(&u("https://nas.lan:7823/conversations")),
            Nav::External
        );
    }

    #[test]
    fn http_remote_is_external() {
        assert_eq!(classify(&u("http://nas.lan:7823/")), Nav::External);
    }

    #[test]
    fn unknown_https_is_external() {
        assert_eq!(classify(&u("https://evil.example/")), Nav::External);
    }

    // ── plain external schemes ───────────────────────────────────────────

    #[test]
    fn mailto_is_external() {
        assert_eq!(classify(&u("mailto:alice@example.com")), Nav::External);
    }

    #[test]
    fn tel_is_external() {
        assert_eq!(classify(&u("tel:+15551234567")), Nav::External);
    }

    // ── everything else is blocked ───────────────────────────────────────

    #[test]
    fn about_blank_is_block() {
        assert_eq!(classify(&u("about:blank")), Nav::Block);
    }

    #[test]
    fn data_is_block() {
        assert_eq!(classify(&u("data:text/html,<script>")), Nav::Block);
    }

    #[test]
    fn javascript_is_block() {
        assert_eq!(classify(&u("javascript:alert(1)")), Nav::Block);
    }

    #[test]
    fn intent_is_block() {
        assert_eq!(
            classify(&u("intent:https://x.example#Intent;end")),
            Nav::Block
        );
    }

    #[test]
    fn file_is_block() {
        assert_eq!(classify(&u("file:///etc/passwd")), Nav::Block);
    }
}
