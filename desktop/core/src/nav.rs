//! Webview navigation guard (DESIGN §4.8).

use url::Url;

/// Reserved URL the settings window is opened from (design #377 §4.1):
/// any origin navigating to it gets the settings window, and the
/// navigation itself is blocked.
pub const SETTINGS_URL: &str = "peakbot-desktop:settings";

/// What to do about a webview navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDecision {
    /// In-app: the local status page origin, or the allowed backend origin.
    Allow,
    /// Open in the external browser and block in-app (other http(s)/mailto).
    OpenExternal,
    /// The reserved settings sentinel: open the settings window, block
    /// the navigation.
    OpenSettings,
    /// `file:`, `data:`, `javascript:` …
    Block,
}

pub fn classify_navigation(url: &Url, backend_origin: Option<&str>) -> NavDecision {
    // The sentinel wins over everything, incl. the allowed backend origin.
    if url.as_str() == SETTINGS_URL {
        return NavDecision::OpenSettings;
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

        // The reserved settings sentinel (design #377 §4.1).
        assert_eq!(SETTINGS_URL, "peakbot-desktop:settings");
        assert_eq!(
            classify_navigation(&u(SETTINGS_URL), None),
            NavDecision::OpenSettings,
            "the sentinel opens the settings window"
        );
        assert_eq!(
            classify_navigation(&u("peakbot-desktop:other"), None),
            NavDecision::Block,
            "other peakbot-desktop: URLs are blocked"
        );
        assert_eq!(
            classify_navigation(&u(SETTINGS_URL), Some("http://127.0.0.1:7824")),
            NavDecision::OpenSettings,
            "the sentinel wins over the allowed backend origin"
        );
    }
}
