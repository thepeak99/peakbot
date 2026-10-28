//! Static surface test for the scoped-remote-IPC design (desktop
//! auto-update plan, section "Glue"): the PeakBot web UI runs on the
//! default loopback backend and may do exactly THREE things from a
//! remote origin — `theme_set`, `update_status`, `update_install` —
//! and NOTHING ELSE, ever.
//!
//! The enforcement lives in the committed Tauri files, so this test
//! reads them as text/JSON (no Tauri, no GUI, no network, no
//! wall-clock):
//!
//!   `desktop/src-tauri/capabilities/remote-web.json`
//!     - renamed from `remote-theme.json` (identifier `remote-web`)
//!     - `local: false` (remote capability), `windows: ["main"]`
//!     - remote URLs: exactly the four loopback URLPattern entries on
//!       the desktop's fixed port (`…:7823/*`, never `/**`)
//!     - permissions: exactly `allow-theme-set`, `allow-update-status`,
//!       `allow-update-install` — never `allow-act` / `allow-status`
//!       (the status page stays a local page; `act` carries `quit`),
//!       never `allow-theme-get`, never the deleted
//!       `allow-settings-get` / `allow-toggle-background`
//!
//!   `desktop/src-tauri/capabilities/local-pages.json`
//!     - unchanged: NO `remote` key (local-only capability),
//!       permissions exactly `allow-status`, `allow-act`,
//!       `allow-theme-get`
//!
//!   no capability file
//!     - grants any `updater:*` plugin permission (the updater runs
//!       inside the shell; it is never exposed over IPC)
//!
//!   `desktop/src-tauri/tauri.conf.json`
//!     - `app.security.capabilities == ["local-pages", "remote-web"]`
//!       (an unlisted capability file is silently ignored — the new
//!       file must be listed to be active)
//!     - `plugins.updater.endpoints` == exactly the public GitHub
//!       mirror's `latest.json`, `plugins.updater.pubkey` non-empty,
//!       and `bundle.createUpdaterArtifacts` absent/false (local
//!       builds must not need the signing key)
//!
//!   `desktop/ui/index.html` + `theme.js` (unchanged surface)
//!     - `theme.js` is loaded as the LAST child of `<head>` (CSP
//!       `script-src 'self'` — no inline scripts)
//!     - no settings page: `settings.html`/`settings.js` are gone, the
//!       index page links no `peakbot-desktop:settings` sentinel, and
//!       the settings permission files are deleted
//!
//!   `desktop/src-tauri/build.rs`
//!     - AppManifest commands: exactly `act`, `status`, `theme_get`,
//!       `theme_set`, `update_install`, `update_status`
//!
//! RED before implementation: `remote-web.json` does not exist yet
//! (the old `remote-theme.json` still does), `tauri.conf.json` still
//! lists `remote-theme` and has no `plugins.updater`, and `build.rs`
//! still lists only the four old commands — so the assertions below
//! fail for exactly the "change absent" reasons, file by file.

use std::path::Path;

use serde_json::Value;

/// Read a committed file under `desktop/` (a sibling of this crate's
/// `core/` dir). Panics with a setup explanation if the file is missing
/// — so a setup problem is never confused with the design's absence.
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

/// `remote.urls` of a capability file, or `None` when there is no
/// `remote` key at all.
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

// ── remote-web.json (the remote capability) ─────────────────────────────

#[test]
fn old_remote_theme_capability_file_is_gone() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("src-tauri/capabilities/remote-theme.json");
    assert!(
        !path.exists(),
        "remote-theme.json must be deleted (renamed to remote-web.json — it now carries the update IPC too)"
    );
}

#[test]
fn old_desktop_pane_capability_file_is_gone() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("src-tauri/capabilities/desktop-pane.json");
    assert!(
        !path.exists(),
        "desktop-pane.json must be deleted (renamed to remote-web.json)"
    );
}

#[test]
fn remote_web_capability_is_remote_main_only() {
    let cap = read_json("src-tauri/capabilities/remote-web.json");

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
        Some(&Value::from("remote-web")),
        "identifier names the file (renamed from remote-theme)"
    );
}

#[test]
fn remote_web_urls_are_the_four_loopback_entries() {
    let cap = read_json("src-tauri/capabilities/remote-web.json");
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
fn remote_web_permissions_are_theme_set_and_the_update_commands() {
    let cap = read_json("src-tauri/capabilities/remote-web.json");
    let perms = permissions(&cap);

    let mut expected = [
        "allow-theme-set",
        "allow-update-status",
        "allow-update-install",
    ];
    expected.sort_unstable();
    let mut got = perms.clone();
    got.sort_unstable();
    assert_eq!(
        got, expected,
        "the remote web UI gets EXACTLY theme_set + update_status + update_install — nothing else, ever"
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

// ── every capability file: no updater plugin surface ────────────────────

#[test]
fn no_capability_grants_updater_plugin_permissions() {
    // The updater runs inside the shell (its own thread, its own menu
    // item) — no `updater:*` permission may ever cross the IPC boundary,
    // from any capability file.
    let rel = "src-tauri/capabilities";
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(rel);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .expect("test setup: the capabilities dir exists")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|n| n.ends_with(".json"))
        .collect();
    assert!(!names.is_empty(), "there is at least one capability file");
    for name in &names {
        let cap = read_json(&format!("{rel}/{name}"));
        for p in permissions(&cap) {
            assert!(
                !p.starts_with("updater:"),
                "{name} must not grant the updater plugin's `{p}` permission"
            );
        }
    }
}

// ── local-pages.json (the local capability) ─────────────────────────────

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

// ── tauri.conf.json (capability listing + updater plugin) ───────────────

#[test]
fn tauri_conf_lists_local_pages_and_remote_web() {
    let conf = read_json("src-tauri/tauri.conf.json");
    let caps = conf
        .pointer("/app/security/capabilities")
        .and_then(Value::as_array)
        .expect("app.security.capabilities is an array");
    let mut expected = ["local-pages", "remote-web"];
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
        "an unlisted capability file is silently ignored — remote-web must be listed to be active"
    );
}

#[test]
fn tauri_conf_updater_plugin_points_at_the_github_mirror() {
    let conf = read_json("src-tauri/tauri.conf.json");
    let updater = conf
        .pointer("/plugins/updater")
        .and_then(Value::as_object)
        .expect("plugins.updater must be configured (the plugin rejects a missing pubkey)");

    let endpoints = updater
        .get("endpoints")
        .and_then(Value::as_array)
        .expect("plugins.updater.endpoints is an array");
    let got: Vec<&str> = endpoints.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        got,
        ["https://github.com/thepeak99/peakbot/releases/latest/download/latest.json"],
        "exactly the public GitHub mirror's latest.json (the Gitea push-mirror has no release assets; no token in the app)"
    );

    let pubkey = updater.get("pubkey").and_then(Value::as_str).unwrap_or("");
    assert!(
        !pubkey.is_empty(),
        "plugins.updater.pubkey must be non-empty"
    );

    let artifacts = conf.pointer("/bundle/createUpdaterArtifacts");
    assert!(
        artifacts.is_none() || artifacts == Some(&Value::from(false)),
        "createUpdaterArtifacts must stay off — local builds must not need the signing key (signing is a post-build step)"
    );
}

// ── bundled pages: theme.js wired in, settings page gone ────────────────

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

// ── build.rs: the AppManifest command list ──────────────────────────────

#[test]
fn build_rs_manifest_lists_exactly_six_commands() {
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
    let mut expected = [
        "act",
        "status",
        "theme_get",
        "theme_set",
        "update_install",
        "update_status",
    ];
    expected.sort_unstable();
    assert_eq!(
        got, expected,
        "exactly act/status/theme_get/theme_set/update_install/update_status — the two update commands are new, settings_get/toggle_background stay deleted"
    );
}
