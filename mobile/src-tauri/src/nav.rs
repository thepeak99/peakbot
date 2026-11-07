//! `classify(url, known) -> Nav` — the webview's security boundary (plan §2.3); implemented in M4.
//!
//! TESTER'S LOCKS — Phase 2, locks 3–4 (RED until M4 lands): written BEFORE
//! the implementation, against the architect's §2.3 interface:
//!
//! ```rust
//! pub enum Nav { Allow, External, Profiles, RestartLocal, Block }
//! pub struct Known<'a> { pub local: Option<&'a url::Origin>, pub remotes: &'a [url::Origin] }
//! pub const PROFILES_SENTINEL: &str = "peakbot-mobile:profiles";
//! pub const RESTART_SENTINEL:  &str = "peakbot-mobile:restart";
//! pub fn classify(url: &url::Url, known: &Known) -> Nav;
//! ```
//!
//! Locked rules, first match wins: (1) `peakbot-mobile:profiles` →
//! `Profiles`; (2) `peakbot-mobile:restart` → `RestartLocal` (the restart
//! GATE lives in the spawned handler — §2.4 — not in `classify`); (3) app
//! origin (`tauri://localhost`, `http(s)://tauri.localhost`) → `Allow`; (4)
//! origin == `known.local` or ∈ `known.remotes` → `Allow` (the only way
//! `http:` is ever allowed: the Local loopback origin); (5)
//! `http/https/mailto/tel` → `External`; (6) else (`intent:`, `file:`,
//! `data:`, `javascript:`, other `peakbot-*:`) → `Block`.
//!
//! The sentinel consts are cross-pinned against the web tests, which read the
//! SPA test files at compile time (deterministic, no fs at test time):
//! `web/src/settings/mobileGating.test.tsx` pins the href
//! `peakbot-mobile:profiles` (F2) and `web/src/desktop.test.ts` pins the
//! `peakbotShell` marker values `"desktop"` / `"mobile"` (F1).

use url::Url;

/// What a webview navigation may do (plan §2.3). First match wins.
#[derive(Debug, PartialEq)]
pub enum Nav {
    /// The page may display (app or known profile origin).
    Allow,
    /// Open in the system browser.
    External,
    /// Navigate to the mobile-only profiles screen.
    Profiles,
    /// Ask the app to restart the Local core (gate is in the handler, §2.4).
    RestartLocal,
    /// Refuse.
    Block,
}

pub const PROFILES_SENTINEL: &str = "peakbot-mobile:profiles";
pub const RESTART_SENTINEL: &str = "peakbot-mobile:restart";

/// Origins the app may display: the Local loopback origin + paired remotes.
/// Built per navigation so back/forward re-asks the guard (N1).
#[derive(Debug, PartialEq, Eq)]
pub struct Known<'a> {
    pub local: Option<&'a url::Origin>,
    pub remotes: &'a [url::Origin],
}

/// Classify a navigation (first match wins, §2.3 rule order).
pub fn classify(url: Url, known: &Known<'_>) -> Nav {
    // (1)+(2): peakbot-mobile:* — only the two sentinels are recognised; any
    // other peakbot-* name is Block (it is not an open namespace).
    if url.scheme() == "peakbot-mobile" {
        return match url.path() {
            "profiles" => Nav::Profiles,
            "restart" => Nav::RestartLocal,
            _ => Nav::Block,
        };
    }
    // (3): the app's own origin (local assets + dev server).
    if is_app_origin(&url) {
        return Nav::Allow;
    }
    // (4): any known origin. The only way http: is allowed (the Local loopback).
    let o = url.origin();
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

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap_or_else(|e| panic!("test URL {s:?} must parse: {e}"))
    }

    fn origin(s: &str) -> url::Origin {
        u(s).origin()
    }

    fn known<'a>(local: Option<&'a url::Origin>, remotes: &'a [url::Origin]) -> Known<'a> {
        Known { local, remotes }
    }

    fn none() -> Known<'static> {
        known(None, &[])
    }

    // ── the locked sentinel consts (cross-side pin, lock 4) ──────────────

    /// `web/src/settings/mobileGating.test.tsx` (F2) asserts the exact
    /// `a[href="peakbot-mobile:profiles"]` — read at compile time so this pin
    /// is deterministic.
    const WEB_MOBILE_GATING: &str = include_str!("../../../web/src/settings/mobileGating.test.tsx");

    #[test]
    fn sentinel_consts_are_locked() {
        assert_eq!(PROFILES_SENTINEL, "peakbot-mobile:profiles");
        assert_eq!(RESTART_SENTINEL, "peakbot-mobile:restart");
    }

    #[test]
    fn profiles_sentinel_matches_the_web_test_href() {
        assert!(
            WEB_MOBILE_GATING.contains(format!(r#"a[href="{PROFILES_SENTINEL}"]"#).as_str()),
            "web/src/settings/mobileGating.test.tsx must pin href={PROFILES_SENTINEL:?}"
        );
    }

    // ── rule 1+2: the sentinels ──────────────────────────────────────────

    #[test]
    fn profiles_sentinel_is_profiles() {
        assert_eq!(classify(u(PROFILES_SENTINEL), &none()), Nav::Profiles);
    }

    #[test]
    fn restart_sentinel_is_restart_local() {
        // v3: `classify` is unconditional — the Local-only gate is in the
        // spawned handler (§2.4 "Restart gate"), not here.
        assert_eq!(classify(u(RESTART_SENTINEL), &none()), Nav::RestartLocal);
    }

    // ── rule 3: app origins ──────────────────────────────────────────────

    #[test]
    fn app_origin_tauri_localhost_is_allow() {
        assert_eq!(classify(u("tauri://localhost"), &none()), Nav::Allow);
    }

    #[test]
    fn app_origin_http_tauri_localhost_is_allow() {
        assert_eq!(classify(u("http://tauri.localhost/"), &none()), Nav::Allow);
    }

    #[test]
    fn app_origin_https_tauri_localhost_is_allow() {
        assert_eq!(
            classify(u("https://tauri.localhost/settings"), &none()),
            Nav::Allow
        );
    }

    // ── rule 4: known origins ────────────────────────────────────────────

    #[test]
    fn known_remote_origin_is_allow() {
        let remotes = [origin("https://nas.lan:7823")];
        assert_eq!(
            classify(u("https://nas.lan:7823/"), &known(None, &remotes)),
            Nav::Allow
        );
    }

    #[test]
    fn known_remote_subpath_is_allow() {
        let remotes = [origin("https://nas.lan:7823")];
        assert_eq!(
            classify(
                u("https://nas.lan:7823/conversations"),
                &known(None, &remotes)
            ),
            Nav::Allow
        );
    }

    #[test]
    fn other_port_same_host_is_external() {
        let remotes = [origin("https://nas.lan:7823")];
        assert_eq!(
            classify(u("https://nas.lan:9999/"), &known(None, &remotes)),
            Nav::External
        );
    }

    #[test]
    fn http_same_host_is_external() {
        // scheme differs → origin differs → never allowed
        let remotes = [origin("https://nas.lan:7823")];
        assert_eq!(
            classify(u("http://nas.lan:7823/"), &known(None, &remotes)),
            Nav::External
        );
    }

    #[test]
    fn unknown_https_origin_is_external() {
        assert_eq!(classify(u("https://evil.example/"), &none()), Nav::External);
    }

    #[test]
    fn local_loopback_is_allow_when_known_local() {
        // The ONLY way http: is ever allowed: the Local loopback origin.
        let local = origin("http://127.0.0.1:47823");
        assert_eq!(
            classify(u("http://127.0.0.1:47823/"), &known(Some(&local), &[])),
            Nav::Allow
        );
    }

    #[test]
    fn local_loopback_is_external_when_not_known() {
        assert_eq!(
            classify(u("http://127.0.0.1:47823/"), &none()),
            Nav::External
        );
    }

    #[test]
    fn local_loopback_other_port_is_external() {
        let local = origin("http://127.0.0.1:47823");
        assert_eq!(
            classify(u("http://127.0.0.1:9999/"), &known(Some(&local), &[])),
            Nav::External
        );
    }

    // ── rule 5: plain external schemes ───────────────────────────────────

    #[test]
    fn mailto_is_external() {
        assert_eq!(
            classify(u("mailto:alice@example.com"), &none()),
            Nav::External
        );
    }

    #[test]
    fn tel_is_external() {
        assert_eq!(classify(u("tel:+15551234567"), &none()), Nav::External);
    }

    // ── rule 6: everything else is blocked ───────────────────────────────

    #[test]
    fn intent_is_block() {
        assert_eq!(
            classify(u("intent:https://x.example#Intent;end"), &none()),
            Nav::Block
        );
    }

    #[test]
    fn file_is_block() {
        assert_eq!(classify(u("file:///etc/passwd"), &none()), Nav::Block);
    }

    #[test]
    fn data_is_block() {
        assert_eq!(classify(u("data:text/html,<script>"), &none()), Nav::Block);
    }

    #[test]
    fn javascript_is_block() {
        assert_eq!(classify(u("javascript:alert(1)"), &none()), Nav::Block);
    }

    #[test]
    fn other_mobile_sentinel_is_block() {
        // `peakbot-mobile:*` is NOT an open namespace: only the two locked
        // sentinels are recognised.
        assert_eq!(classify(u("peakbot-mobile:report"), &none()), Nav::Block);
    }

    #[test]
    fn desktop_sentinel_is_block() {
        assert_eq!(classify(u("peakbot-desktop:restart"), &none()), Nav::Block);
    }
}
