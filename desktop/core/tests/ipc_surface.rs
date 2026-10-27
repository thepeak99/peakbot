//! Static surface test for the scoped-remote-IPC design (desktop-pane
//! design §5 item 3, facts F1–F3): the web UI (Settings → Desktop pane)
//! runs on the default loopback backend and must be able to do exactly
//! three things — read the desktop view, toggle the background service,
//! report the theme — and NOTHING ELSE, ever.
//!
//! The enforcement lives in the committed Tauri files, so this test reads
//! them as text/JSON (no Tauri, no GUI, no network, no wall-clock):
//!
//!   `desktop/src-tauri/capabilities/desktop-pane.json`  (new)
//!     - `local: false` (remote capability; F3), `windows: ["main"]`
//!     - remote URLs: exactly the four loopback URLPattern entries on the
//!       desktop's fixed port (F2: `…:7823/*`, never `/**`; F5: the
//!       desktop always opens 127.0.0.1:<port>)
//!     - permissions: exactly `allow-settings-get`, `allow-toggle-background`,
//!       `allow-theme-set` — never `allow-act` / `allow-status` (the
//!       status page stays a local page; `act` carries `quit`)
//!
//!   `desktop/src-tauri/capabilities/local-pages.json`
//!     - NO `remote` key (local-only capability)
//!     - permissions: exactly `allow-status`, `allow-act`,
//!       `allow-settings-get`, `allow-toggle-background`, `allow-theme-get`
//!       (`theme_get` stays local: the bundled pages read it, the remote
//!       web UI only ever WRITES its theme)
//!
//!   `desktop/src-tauri/tauri.conf.json`
//!     - `app.security.capabilities == ["local-pages", "desktop-pane"]`
//!       (F1: an unlisted capability file is silently ignored — the new
//!       file must be listed to be active)
//!
//!   `desktop/ui/{index.html,settings.html}` + `theme.js` + `settings.js`
//!     - `theme.js` is loaded as the LAST child of `<head>` in both pages
//!       (F9: CSP `script-src 'self'` — no inline scripts)
//!     - the CSS stops keying on `prefers-color-scheme` (the theme comes
//!       from the desktop's reported theme now)
//!     - `theme.js` reads the theme via `invoke("theme_get")`
//!     - `settings.js` toggles the background via the dedicated
//!       `invoke("toggle_background")` command — never through `act`
//!       (design D3).
//!
//! RED before implementation: `desktop-pane.json` does not exist, the
//! `local-pages.json` permission set and the `tauri.conf.json` capability
//! list are unchanged, and no `theme.js` is wired in — so the assertions
//! below fail for exactly the "feature absent" reasons, file by file.

use std::path::Path;

use serde_json::Value;

/// Read a committed file under `desktop/` (a sibling of this crate's
/// `core/` dir). Panics with a setup explanation if the file is missing —
/// so a setup problem is never confused with the design's absence.
fn read(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "test setup: cannot read committed file {} ({e}): desktop must be \
             committed in the repo",
            path.display()
        )
    })
}

fn read_json(rel: &str) -> Value {
    let text = read(rel);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("test setup: {rel} is not valid JSON ({e})"))
}

/// `remote.urls` of a capability file, or `None` when there is no `remote`
/// key at all.
fn remote_urls(cap: &Value) -> Option<Vec<String>> {
    let arr = cap.get("remote")?.get("urls")?.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for u in arr {
        out.push(u.as_str()?.to_string());
    }
    Some(out)
}

/// `permissions` of a capability file.
fn permissions(cap: &Value) -> Vec<String> {
    let arr = cap
        .get("permissions")
        .and_then(Value::as_array)
        .expect("the capability has a permissions array");
    let mut out = Vec::with_capacity(arr.len());
    for p in arr {
        out.push(
            p.as_str()
                .expect("permission entries are strings")
                .to_string(),
        );
    }
    out
}

/// True if the HTML declares `<script src="theme.js"></script>` somewhere
/// BEFORE its `</head>` (i.e. in the head, not the body).
fn head_includes_theme_script(html: &str) -> bool {
    let head_end = html.find("</head>").expect("the page has a </head>");
    let head = &html[..head_end];
    head.contains("src=\"theme.js\"") || head.contains("src='theme.js'")
}

/// True if `theme.js` is the LAST child of `<head>`: the LAST
/// `<script` opening tag in the head is the `theme.js` one, and after
/// it the only `</` closing tag before `</head>` is its own
/// `</script>` (later tags in the head must not be closed after it).
fn theme_script_is_last_head_child(html: &str) -> bool {
    let head_end = html.find("</head>").expect("the page has a </head>");
    let head = &html[..head_end];
    let pos = head.rfind("<script").expect("the head has script tags");
    let tag_end = head[pos..].find('>').expect("the script tag is closed");
    let tag = &head[pos..pos + tag_end + 1];
    let after = &head[pos + tag_end + 1..];
    let next_close = after
        .find("</")
        .expect("the script tag is closed in the head");
    let is_theme = tag.contains("src=\"theme.js\"") || tag.contains("src='theme.js'");
    is_theme && after[..next_close].trim().is_empty() && after[next_close..].starts_with("</script")
}

// ── desktop-pane.json (the remote capability) ────────────────────────────

#[test]
fn desktop_pane_capability_is_remote_main_only() {
    let cap = read_json("src-tauri/capabilities/desktop-pane.json");

    assert_eq!(
        cap.get("local").and_then(Value::as_bool),
        Some(false),
        "F3: the remote capability sets \"local\": false"
    );
    assert_eq!(
        cap.get("windows"),
        Some(&Value::from(["main"])),
        "the capability applies to the main window only"
    );
    assert_eq!(
        cap.get("identifier"),
        Some(&Value::from("desktop-pane")),
        "identifier names the file"
    );
}

#[test]
fn desktop_pane_remote_urls_are_exactly_the_four_loopback_entries() {
    let cap = read_json("src-tauri/capabilities/desktop-pane.json");
    let urls = remote_urls(&cap).expect("the remote capability declares remote.urls");

    let mut expected = [
        "http://127.0.0.1:7823/*",
        "https://127.0.0.1:7823/*",
        "http://localhost:7823/*",
        "https://localhost:7823/*",
    ];
    expected.sort_unstable();
    let mut got = urls.clone();
    got.sort_unstable();
    assert_eq!(
        got, expected,
        "exactly the four loopback URLPattern entries (F2: `*`, not `/**`; F5: the desktop's fixed port)"
    );
}

#[test]
fn desktop_pane_permissions_are_exactly_the_three() {
    let cap = read_json("src-tauri/capabilities/desktop-pane.json");
    let perms = permissions(&cap);

    let mut expected = [
        "allow-settings-get",
        "allow-toggle-background",
        "allow-theme-set",
    ];
    expected.sort_unstable();
    let mut got = perms.clone();
    got.sort_unstable();
    assert_eq!(
        got, expected,
        "exactly the three: allow-settings-get, allow-toggle-background, allow-theme-set — nothing else, ever"
    );

    for forbidden in ["allow-act", "allow-status"] {
        assert!(
            !perms.contains(&forbidden.to_string()),
            "the remote web UI must never get {forbidden} (`act` carries quit; `status` is a local-page read)"
        );
    }
}

// ── local-pages.json (the local capability) ──────────────────────────────

#[test]
fn local_pages_capability_stays_local_only() {
    let cap = read_json("src-tauri/capabilities/local-pages.json");

    assert!(
        cap.get("remote").is_none(),
        "local-pages must have no `remote` key (the bundled pages are local-only)"
    );
    assert_eq!(
        cap.get("windows"),
        Some(&Value::from(["main"])),
        "the local capability keeps the main window"
    );
}

#[test]
fn local_pages_permissions_are_exactly_the_five() {
    let cap = read_json("src-tauri/capabilities/local-pages.json");
    let perms = permissions(&cap);

    let mut expected = [
        "allow-status",
        "allow-act",
        "allow-settings-get",
        "allow-toggle-background",
        "allow-theme-get",
    ];
    expected.sort_unstable();
    let mut got = perms.clone();
    got.sort_unstable();
    assert_eq!(
        got, expected,
        "local pages keep status/act/settings_get and gain toggle_background + theme_get (theme_get stays local — the web UI only writes its theme)"
    );
}

// ── tauri.conf.json (capability listing, F1) ─────────────────────────────

#[test]
fn tauri_conf_lists_both_capabilities() {
    let conf = read_json("src-tauri/tauri.conf.json");
    let caps = conf
        .pointer("/app/security/capabilities")
        .and_then(Value::as_array)
        .expect("app.security.capabilities is an array");
    let mut expected = ["local-pages", "desktop-pane"];
    expected.sort_unstable();
    let mut got: Vec<String> = caps
        .iter()
        .map(|c| {
            c.as_str()
                .expect("capability entries are strings")
                .to_string()
        })
        .collect();
    got.sort_unstable();
    assert_eq!(
        got, expected,
        "F1: an unlisted capability file is silently ignored — desktop-pane must be listed to be active"
    );
}

// ── bundled pages: theme.js wired in, no prefers-color-scheme ────────────

#[test]
fn bundled_pages_load_theme_js_as_last_head_child() {
    for name in ["index.html", "settings.html"] {
        let html = read(&format!("ui/{name}"));
        assert!(
            head_includes_theme_script(&html),
            "{name} must load theme.js in its <head> (CSP script-src 'self' — no inline scripts)"
        );
        assert!(
            theme_script_is_last_head_child(&html),
            "{name}: <script src=\"theme.js\"></script> must be the LAST child of <head> (it must apply after the CSS is parsed)"
        );
        assert!(
            !html.contains("prefers-color-scheme"),
            "{name} must stop keying the CSS on prefers-color-scheme (the theme comes from the desktop's reported theme)"
        );
    }
}

#[test]
fn theme_js_reads_the_reported_theme() {
    let js = read("ui/theme.js");
    assert!(
        js.contains("invoke(\"theme_get\")") || js.contains("invoke('theme_get')"),
        "theme.js must read the desktop's reported theme via invoke(\"theme_get\")"
    );
}

// ── settings.js: the dedicated toggle command (D3) ───────────────────────

#[test]
fn settings_js_uses_the_dedicated_toggle_background_command() {
    let js = read("ui/settings.js");
    assert!(
        js.contains("invoke(\"toggle_background\")") || js.contains("invoke('toggle_background')"),
        "settings.js must toggle the background via the dedicated toggle_background command"
    );
    assert!(
        !js.contains("action: \"toggle_background\"")
            && !js.contains("action: 'toggle_background'"),
        "settings.js must no longer route the toggle through `act` (design D3)"
    );
}
