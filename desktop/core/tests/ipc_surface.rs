//! Static surface test for the scoped-remote-IPC design (close-to-tray
//! plan §C: the web UI's desktop pane is gone — the ONLY remote IPC left
//! is reporting the theme): the PeakBot web UI runs on the default
//! loopback backend and must be able to do exactly ONE thing from a
//! remote origin — `theme_set` — and NOTHING ELSE, ever.
//!
//! The enforcement lives in the committed Tauri files, so this test reads
//! them as text/JSON (no Tauri, no GUI, no network, no wall-clock):
//!
//!   `desktop/src-tauri/capabilities/remote-theme.json`
//!     - `local: false` (remote capability), `windows: ["main"]`
//!     - identifier `remote-theme` (renamed from `desktop-pane`)
//!     - remote URLs: exactly the four loopback URLPattern entries on the
//!       desktop's fixed port (`…:7823/*`, never `/**`)
//!     - permissions: exactly `allow-theme-set` — never `allow-act` /
//!       `allow-status` (the status page stays a local page; `act`
//!       carries `quit`), never the deleted `allow-settings-get` /
//!       `allow-toggle-background`
//!
//!   `desktop/src-tauri/capabilities/local-pages.json`
//!     - NO `remote` key (local-only capability)
//!     - permissions: exactly `allow-status`, `allow-act`,
//!       `allow-theme-get`
//!
//!   `desktop/src-tauri/tauri.conf.json`
//!     - `app.security.capabilities == ["local-pages", "remote-theme"]`
//!       (an unlisted capability file is silently ignored — the new
//!       file must be listed to be active)
//!
//!   `desktop/ui/index.html` + `theme.js`
//!     - `theme.js` is loaded as the LAST child of `<head>` (CSP
//!       `script-src 'self'` — no inline scripts)
//!     - no settings page: `settings.html`/`settings.js` are gone, the
//!       index page links no `peakbot-desktop:settings` sentinel, and
//!       the settings permission files are deleted
//!
//!   `desktop/src-tauri/build.rs`
//!     - AppManifest commands: exactly `status`, `act`, `theme_set`,
//!       `theme_get`
//!
//! RED before implementation: `remote-theme.json` does not exist yet
//! (the old `desktop-pane.json` still does), `local-pages.json` and
//! `tauri.conf.json` are unchanged, `index.html` still links the
//! settings page, `settings.{html,js}` still exist, and `build.rs`
//! still lists `settings_get` + `toggle_background` — so the assertions
//! below fail for exactly the "change absent" reasons, file by file.

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

// ── remote-theme.json (the remote capability) ────────────────────────────

#[test]
fn remote_theme_capability_is_remote_main_only() {
    let cap = read_json("src-tauri/capabilities/remote-theme.json");

    assert_eq!(
        cap.get("local").and_then(Value::as_bool),
        Some(false),
        "the remote capability sets \"local\": false"
    );
    assert_eq!(
        cap.get("windows"),
        Some(&Value::from(["main"])),
        "the capability applies to the main window only"
    );
    assert_eq!(
        cap.get("identifier"),
        Some(&Value::from("remote-theme")),
        "identifier names the file (renamed from desktop-pane)"
    );
}

#[test]
fn remote_theme_urls_are_the_four_loopback_entries() {
    let cap = read_json("src-tauri/capabilities/remote-theme.json");
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
        "exactly the four loopback URLPattern entries (`*`, not `/**`; the desktop's fixed port)"
    );
}

#[test]
fn remote_theme_permissions_are_exactly_theme_set() {
    let cap = read_json("src-tauri/capabilities/remote-theme.json");
    let perms = permissions(&cap);

    assert_eq!(
        perms,
        vec!["allow-theme-set".to_string()],
        "the remote web UI gets EXACTLY allow-theme-set — nothing else, ever"
    );

    for forbidden in [
        "allow-act",
        "allow-status",
        "allow-theme-get",
        "allow-settings-get",
        "allow-toggle-background",
    ] {
        assert!(
            !perms.contains(&forbidden.to_string()),
            "the remote capability must not grant {forbidden}"
        );
    }
}

#[test]
fn old_desktop_pane_capability_file_is_gone() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("src-tauri/capabilities/desktop-pane.json");
    assert!(
        !path.exists(),
        "desktop-pane.json must be deleted (renamed to remote-theme.json)"
    );
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
fn local_pages_permissions_are_exactly_the_three() {
    let cap = read_json("src-tauri/capabilities/local-pages.json");
    let perms = permissions(&cap);

    let mut expected = ["allow-status", "allow-act", "allow-theme-get"];
    expected.sort_unstable();
    let mut got = perms.clone();
    got.sort_unstable();
    assert_eq!(
        got, expected,
        "local pages keep status/act + theme_get (theme_get stays local — the web UI only writes its theme); settings_get/toggle_background are gone"
    );
}

// ── tauri.conf.json (capability listing) ─────────────────────────────────

#[test]
fn tauri_conf_lists_local_pages_and_remote_theme() {
    let conf = read_json("src-tauri/tauri.conf.json");
    let caps = conf
        .pointer("/app/security/capabilities")
        .and_then(Value::as_array)
        .expect("app.security.capabilities is an array");
    let mut expected = ["local-pages", "remote-theme"];
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
        "an unlisted capability file is silently ignored — remote-theme must be listed to be active"
    );
}

// ── bundled pages: theme.js wired in, settings page gone ─────────────────

#[test]
fn index_html_loads_theme_js_last_and_has_no_settings_link() {
    let html = read("ui/index.html");
    assert!(
        head_includes_theme_script(&html),
        "index.html must load theme.js in its <head> (CSP script-src 'self' — no inline scripts)"
    );
    assert!(
        theme_script_is_last_head_child(&html),
        "index.html: <script src=\"theme.js\"></script> must be the LAST child of <head> (it must apply after the CSS is parsed)"
    );
    assert!(
        !html.contains("peakbot-desktop:settings"),
        "index.html must not link the settings sentinel (the settings page is gone)"
    );
    assert!(
        !html.contains("settings.html"),
        "index.html must not reference the deleted settings page"
    );
}

#[test]
fn settings_page_files_are_gone() {
    for name in ["ui/settings.html", "ui/settings.js"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name);
        assert!(!path.exists(), "{name} must be deleted");
    }
    for name in [
        "src-tauri/permissions/autogenerated/settings_get.toml",
        "src-tauri/permissions/autogenerated/toggle_background.toml",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name);
        assert!(!path.exists(), "{name} must be deleted");
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

// ── build.rs: the AppManifest command list ───────────────────────────────

#[test]
fn build_rs_manifest_lists_exactly_four_commands() {
    let build = read("src-tauri/build.rs");
    // The `.commands(&[ ... ])` list: extract the slice body.
    let start = build
        .find(".commands(&[")
        .expect("build.rs has an AppManifest .commands(&[ … ]) list");
    let body_start = start + ".commands(&[".len();
    let body_end = build[body_start..]
        .find("])")
        .expect("the commands list is closed")
        + body_start;
    let body = &build[body_start..body_end];
    let mut got: Vec<String> = body
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_string())
        .filter(|c| !c.is_empty())
        .collect();
    got.sort_unstable();
    let mut expected = ["act", "status", "theme_get", "theme_set"];
    expected.sort_unstable();
    assert_eq!(
        got, expected,
        "exactly status/act/theme_set/theme_get — settings_get and toggle_background are deleted"
    );
}
