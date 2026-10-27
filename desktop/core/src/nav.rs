//! Webview navigation guard (DESIGN §4.8).

use url::Url;

/// Reserved URL that restarts only the PeakBot server (0.24, spec
/// §8.4): the navigation is blocked and the desktop sends
/// `Action::Restart` (session child relaunch — never the desktop app
/// itself).
pub const RESTART_URL: &str = "peakbot-desktop:restart";

/// What to do about a webview navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDecision {
    /// In-app: the local status page origin, or the allowed backend origin.
    Allow,
    /// Open in the external browser and block in-app (other http(s)/mailto).
    OpenExternal,
    /// The reserved restart sentinel: restart only the PeakBot server
    /// (`Action::Restart`), block the navigation (0.24, spec §8.4).
    Restart,
    /// `file:`, `data:`, `javascript:` …
    Block,
}

pub fn classify_navigation(url: &Url, backend_origin: Option<&str>) -> NavDecision {
    // The sentinel wins over everything, incl. the allowed backend origin.
    if url.as_str() == RESTART_URL {
        return NavDecision::Restart;
    }
    // Local app origins (the bundled status page) are always in-app.
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or("");
    if scheme == "tauri" && host == "localhost"
        || (scheme == "http" || scheme == "https") && host == "tauri.localhost"
    {
        return NavDecision::Allow;
    }
    match scheme {
        "http" | "https" | "mailto" => {
            if backend_origin
                .and_then(|o| Url::parse(o).ok())
                .is_some_and(|allowed| allowed.origin() == url.origin())
            {
                NavDecision::Allow
            } else {
                NavDecision::OpenExternal
            }
        }
        // file:, data:, javascript:, other peakbot-desktop: sentinels, …
        _ => NavDecision::Block,
    }
}

/// The pure trust gate of the Linux TLS handler (design #377b §3.3,
/// invariant I9): is `failing` our LIVE backend origin, over https, on
/// the pinned loopback host? The origin comes from `UiState.nav`, which
/// is maintained by the phase machine (T-B3).
pub fn may_trust_peakbot_cert(failing: &Url, backend_origin: Option<&str>) -> bool {
    // https-only (a TLS failure is an https failure) and the pinned
    // loopback literal (the app webview is always 127.0.0.1; a prefix
    // like `127.0.0.1.evil` is a different host).
    if failing.scheme() != "https" || failing.host_str() != Some("127.0.0.1") {
        return false;
    }
    backend_origin
        .and_then(|o| Url::parse(o).ok())
        .is_some_and(|origin| origin.origin() == failing.origin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        s.parse().unwrap()
    }

    // §9.1 case 25.
    #[test]
    fn t25_classify_navigation() {
        assert_eq!(
            classify_navigation(
                &u("http://127.0.0.1:7824/setup"),
                Some("http://127.0.0.1:7824")
            ),
            NavDecision::Allow,
            "same origin as the allowed backend ⇒ Allow"
        );
        assert_eq!(
            classify_navigation(&u("http://127.0.0.1:7823/"), Some("http://127.0.0.1:7824")),
            NavDecision::OpenExternal,
            "a different local port is not the allowed backend"
        );
        assert_eq!(
            classify_navigation(&u("https://github.com/x"), Some("http://127.0.0.1:7824")),
            NavDecision::OpenExternal
        );
        assert_eq!(
            classify_navigation(&u("mailto:a@b"), None),
            NavDecision::OpenExternal
        );
        assert_eq!(
            classify_navigation(&u("tauri://localhost/index.html"), None),
            NavDecision::Allow,
            "local app origin ⇒ Allow"
        );
        assert_eq!(
            classify_navigation(&u("http://tauri.localhost/"), None),
            NavDecision::Allow
        );
        assert_eq!(
            classify_navigation(&u("file:///etc/passwd"), None),
            NavDecision::Block
        );
        assert_eq!(
            classify_navigation(&u("data:text/html,x"), None),
            NavDecision::Block
        );
        assert_eq!(
            classify_navigation(&u("http://127.0.0.1:7824/"), None),
            NavDecision::OpenExternal,
            "backend URL with no allowed origin opens externally"
        );

        assert_eq!(
            classify_navigation(&u("peakbot-desktop:other"), None),
            NavDecision::Block,
            "other peakbot-desktop: URLs are blocked"
        );
    }

    // 0.24 (spec §8.4, T16): the reserved restart sentinel — restarts
    // only the PeakBot server (child or service), never the desktop app.
    #[test]
    fn classify_restart() {
        assert_eq!(RESTART_URL, "peakbot-desktop:restart");
        assert_eq!(
            classify_navigation(&u(RESTART_URL), None),
            NavDecision::Restart,
            "the restart sentinel ⇒ Restart"
        );
        assert_eq!(
            classify_navigation(&u(RESTART_URL), Some("http://127.0.0.1:7824")),
            NavDecision::Restart,
            "the sentinel wins over the allowed backend origin"
        );
    }

    // Close-to-tray plan (§A.7): the settings page is gone, so the old
    // `peakbot-desktop:settings` sentinel is just another unknown
    // `peakbot-desktop:` URL — blocked, even when it names the backend
    // origin as the allowed one. (The literal is spelled out: the
    // `SETTINGS_URL` constant no longer exists.)
    #[test]
    fn settings_sentinel_is_blocked() {
        assert_eq!(
            classify_navigation(&u("peakbot-desktop:settings"), None),
            NavDecision::Block,
            "the removed settings sentinel falls through to Block"
        );
        assert_eq!(
            classify_navigation(
                &u("peakbot-desktop:settings"),
                Some("http://127.0.0.1:7824")
            ),
            NavDecision::Block,
            "an allowed backend origin does not revive the settings sentinel"
        );
    }

    // 0.24 (spec §8.4, T16): every other `peakbot-desktop:` URL is
    // an unknown sentinel and stays blocked.
    #[test]
    fn other_sentinel_blocked() {
        for other in [
            "peakbot-desktop:other",
            "peakbot-desktop:settings",
            "peakbot-desktop:settings2",
            "peakbot-desktop:restart/extra",
        ] {
            assert_eq!(
                classify_navigation(&u(other), None),
                NavDecision::Block,
                "{other} must be blocked"
            );
        }
    }

    // Design #377b t101: the pure "is this failing URL our live
    // backend origin over https on 127.0.0.1?" decision — the whole
    // trust gate of the Linux TLS handler (invariant I9a).
    #[test]
    fn t101_may_trust_peakbot_cert_only_our_live_https_origin() {
        // The current backend origin over https ⇒ trust candidate.
        assert!(
            may_trust_peakbot_cert(
                &u("https://127.0.0.1:7824/?token=x"),
                Some("https://127.0.0.1:7824")
            ),
            "our live backend origin ⇒ true"
        );
        // http ⇒ never (TLS failures are https-only).
        assert!(
            !may_trust_peakbot_cert(&u("http://127.0.0.1:7824/"), Some("http://127.0.0.1:7824")),
            "http is never trusted"
        );
        // A different port ⇒ not the live backend.
        assert!(
            !may_trust_peakbot_cert(
                &u("https://127.0.0.1:7825/"),
                Some("https://127.0.0.1:7824")
            ),
            "a different local port is not the live backend"
        );
        // No live origin ⇒ never.
        assert!(
            !may_trust_peakbot_cert(&u("https://127.0.0.1:7824/"), None),
            "no live origin ⇒ never"
        );
        // localhost ≠ 127.0.0.1 (the app webview is always 127.0.0.1).
        assert!(
            !may_trust_peakbot_cert(
                &u("https://localhost:7824/"),
                Some("https://127.0.0.1:7824")
            ),
            "localhost is not the pinned loopback host"
        );
        // A foreign host ⇒ never, whatever the origin says.
        assert!(
            !may_trust_peakbot_cert(&u("https://evil.example/"), Some("https://127.0.0.1:7824")),
            "a foreign host is never trusted"
        );
        // A prefix trick on the loopback literal ⇒ a foreign host.
        assert!(
            !may_trust_peakbot_cert(
                &u("https://127.0.0.1.evil:7824/"),
                Some("https://127.0.0.1:7824")
            ),
            "127.0.0.1.evil is a different host"
        );
        // An unparseable origin ⇒ never.
        assert!(
            !may_trust_peakbot_cert(&u("https://127.0.0.1:7824/"), Some("garbage")),
            "a garbage origin is never trusted"
        );
    }
}
