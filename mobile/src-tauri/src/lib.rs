//! PeakBot Mobile — the Tauri glue: plugins, the main window and the nav
//! guard. Decisions live in the pure `nav` module; the Local core's
//! lifecycle in `local`. This file only wires them to Tauri.
mod local;
mod nav;
// iOS glue: the edge-swipe back gesture + the data root's backup
// exclusion. Compiled for the iOS target only.
#[cfg(target_os = "ios")]
mod ios;

use std::path::PathBuf;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tauri::webview::Color;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_log::log;
use tauri_plugin_opener::OpenerExt;

use local::{Local, should_restart};
use nav::Nav;

/// The single init script injected on every origin, both OSes.
/// Declares the mobile shell marker and shims `window.open` as a top-level
/// navigation the nav guard can classify. Locked bytes — no trailing newline.
pub const INIT_SCRIPT: &str = "Object.defineProperty(window,'peakbotShell',{value:'mobile'});\nwindow.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};";

/// The SPA's dark app background (Tailwind zinc-950); the bundled error page
/// uses the same colour so no band of a different colour shows.
const BACKGROUND: Color = Color(9, 9, 11, 255);

/// The app's two storage roots: `data` holds the Local core's config/data;
/// `cache` its cache.
struct AppDirs {
    data: PathBuf,
    cache: PathBuf,
}

impl AppDirs {
    fn resolve(app: &AppHandle) -> tauri::Result<AppDirs> {
        let path = app.path();
        // Android: Tauri's app_data_dir() is the app root (`Context.dataDir`);
        // the data root is `Context.filesDir` under it.
        #[cfg(target_os = "android")]
        let data = path.app_data_dir()?.join("files");
        // iOS: the data root is app_data_dir() itself
        // (`…/Library/Application Support/sh.shifu.mobile`) and the cache
        // app_cache_dir() — nothing under `…/peakbot`.
        #[cfg(not(target_os = "android"))]
        let data = path.app_data_dir()?;
        Ok(AppDirs {
            data,
            cache: path.app_cache_dir()?,
        })
    }

    /// The Local core's roots (`peakbot::paths::init_roots`).
    fn core_roots(&self) -> peakbot::paths::Roots {
        peakbot::paths::Roots {
            config: self.data.join("config"),
            data: self.data.join("core"),
            cache: self.cache.join("core"),
        }
    }
}

/// Where the `main` window first loads: the on-device core's URL on success,
/// or the bundled error page (`index.html#<reason>`) on failure. Pure, so the
/// launch decision is unit-tested without a Tauri runtime.
fn launch_url(core: Result<Url, String>) -> WebviewUrl {
    match core {
        Ok(url) => WebviewUrl::External(url),
        Err(reason) => {
            // The page reads it with decodeURIComponent, so encode everything
            // but alphanumerics (form-encoding would turn spaces into `+`).
            let hash = utf8_percent_encode(&reason, NON_ALPHANUMERIC);
            WebviewUrl::App(format!("index.html#{hash}").into())
        }
    }
}

/// The bundled page, as the webview loads it. Android serves the bundle from
/// `http://tauri.localhost`, Apple from `tauri://localhost`.
fn app_index_url() -> Url {
    let base = if cfg!(target_os = "android") {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    Url::parse(base).expect("static url")
}

/// Returns at once; every side effect is spawned, because opener/navigate
/// calls inside Android's `shouldOverrideUrlLoading` deadlock the UI thread (ANR).
fn on_navigation(app: &AppHandle, url: &Url) -> bool {
    let nav = nav::classify(url);
    log::info!("nav {nav:?} {}", without_query(url));
    match nav {
        Nav::Allow => return true,
        Nav::External => {
            let app = app.clone();
            let url = url.to_string();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = app.opener().open_url(url, None::<&str>) {
                    log::warn!("opener: {e}");
                }
            });
        }
        Nav::RestartLocal => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { restart_local(&app).await });
        }
        Nav::Block => {}
    }
    false
}

/// Only a page on the Local origin may restart the phone's core — a page
/// elsewhere sending the sentinel does nothing. A failed restart lands on the
/// bundled error page, which names the reason.
async fn restart_local(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let local = app.state::<Local>();
    let current = match window.url() {
        Ok(url) => url,
        Err(e) => return log::warn!("restart: no current url: {e}"),
    };
    if !should_restart(&current, local.origin().as_ref()) {
        return log::info!(
            "restart ignored: {} is not the Local page",
            without_query(&current)
        );
    }
    let next = match local.restart().await {
        Ok(url) => url,
        Err(e) => {
            log::warn!("{e}");
            app_index_url()
        }
    };
    if let Err(e) = window.navigate(next) {
        log::warn!("navigate after restart: {e}");
    }
}

/// Back in the foreground, re-serve the Local core if its listener
/// was reclaimed; if that moved the port, follow it from the Local page.
async fn resume_local(app: &AppHandle) {
    let Some((old, url)) = app.state::<Local>().resume().await else {
        return;
    };
    if let Some(window) = app.get_webview_window("main")
        && window.url().is_ok_and(|u| u.origin() == old)
        && let Err(e) = window.navigate(url)
    {
        log::warn!("navigate after re-serve: {e}");
    }
}

/// For logs: a core URL's query carries its token.
fn without_query(url: &Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let dirs = AppDirs::resolve(app.handle())?;
    std::fs::create_dir_all(&dirs.data)?;
    // Boot-only and before any core call: the core resolves every path from here.
    peakbot::paths::init_roots(dirs.core_roots());
    app.manage(Local::default());
    log::info!(
        "data root {}, cache root {}, core roots {:?}",
        dirs.data.display(),
        dirs.cache.display(),
        dirs.core_roots()
    );

    // The core is the webview's only origin, so it starts before the window
    // exists. setup() is sync, so block_on is fine — the core serves on the
    // async runtime's worker threads. On failure the window opens the bundled
    // error page with the reason in its fragment.
    let first = launch_url(tauri::async_runtime::block_on(
        app.state::<Local>().ensure(),
    ));

    let handle = app.handle().clone();
    let window = WebviewWindowBuilder::new(app, "main", first)
        .background_color(BACKGROUND)
        .initialization_script(INIT_SCRIPT)
        .on_navigation(move |url| on_navigation(&handle, url))
        .build()?;
    // `D` excludes itself from iCloud backups, and the WKWebView gets
    // the edge-swipe back gesture (off by default; no Tauri option).
    #[cfg(target_os = "ios")]
    {
        ios::exclude_data_root_from_backup(&dirs.data);
        ios::enable_back_gestures(&window);
    }
    #[cfg(not(target_os = "ios"))]
    let _ = window;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // Logcat (Android) / stdout (dev) only — the default LogDir
                // target would also write a log file outside the app's data
                // root. Nothing on this phone is written outside it.
                .targets([tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Stdout,
                )])
                .build(),
        )
        // the default click handler turns `<a target=_blank>` into an
        // opener IPC call the server origin may not make, so the link died.
        // Off, it reaches the nav guard like any navigation (→ External).
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        );
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    builder
        .setup(setup)
        .build(tauri::generate_context!())
        .expect("error while building PeakBot Mobile")
        .run(|app, event| {
            if let RunEvent::Resumed = event {
                let app = app.clone();
                tauri::async_runtime::spawn(async move { resume_local(&app).await });
            }
        });
}

#[cfg(test)]
mod glue_tests {
    use super::*;

    const LINK: &str = "https://nas.lan:7823/?token=s3cr3t-tok";

    #[test]
    fn logged_urls_drop_the_token() {
        let url = Url::parse(LINK).unwrap();
        assert_eq!(without_query(&url), "https://nas.lan:7823/");
    }
}

#[cfg(test)]
mod init_script_tests {
    use super::INIT_SCRIPT;

    /// The locked bytes: two
    /// lines, ONE `\n` between them, no trailing newline — same style as the
    /// desktop `SHELL_MARKER_SCRIPT`
    /// (`Object.defineProperty(window,'peakbotShell',{value:'desktop'});`).
    const EXPECTED: &str = "Object.defineProperty(window,'peakbotShell',{value:'mobile'});\n\
window.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};";

    /// `web/src/desktop.test.ts` sets `window.peakbotShell` to exactly
    /// `"desktop"` / `"mobile"` — read at compile time so the cross-side pin
    /// is deterministic.
    const WEB_DESKTOP_TEST: &str = include_str!("../../../web/src/desktop.test.ts");

    #[test]
    fn init_script_is_the_locked_string() {
        assert_eq!(
            INIT_SCRIPT, EXPECTED,
            "INIT_SCRIPT must be exactly the two locked lines (marker + window.open shim)"
        );
    }

    #[test]
    fn init_script_declares_the_mobile_shell_marker() {
        // Same shape as the desktop marker, value 'mobile'.
        assert!(
            INIT_SCRIPT.contains("Object.defineProperty(window,'peakbotShell',{value:'mobile'});")
        );
    }

    #[test]
    fn init_script_shims_window_open_as_top_level_navigation() {
        // every window.open becomes a top-level navigation the guard
        // classifies (foreign → External → system browser).
        assert!(INIT_SCRIPT.contains(
            "window.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};"
        ));
    }

    #[test]
    fn shell_marker_value_matches_the_web_tests() {
        // Cross-side pin: the marker our init script declares is one of the
        // exactly two values the web `shell()` seam accepts.
        assert!(WEB_DESKTOP_TEST.contains(r#"peakbotShell = "mobile""#));
        assert!(WEB_DESKTOP_TEST.contains(r#"peakbotShell = "desktop""#));
        assert!(INIT_SCRIPT.contains("{value:'mobile'}"));
        assert!(!INIT_SCRIPT.contains("{value:'desktop'}"));
    }
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use percent_encoding::percent_decode_str;

    /// The URL `local.ensure()` hands back: loopback, token in the query.
    fn core_url(port: u16, token: &str) -> Url {
        let mut u = Url::parse(&format!("http://127.0.0.1:{port}/")).expect("static url");
        u.query_pairs_mut().append_pair("token", token);
        u
    }

    /// The reason a `WebviewUrl::App` page decodes back to — the same
    /// percent-decoding the page does with `decodeURIComponent(location.hash)`.
    fn decoded_reason(page: &std::path::Path) -> Option<String> {
        let s = page.to_string_lossy();
        let hash = s.strip_prefix("index.html#")?;
        percent_decode_str(hash)
            .decode_utf8()
            .ok()
            .map(|reason| reason.into_owned())
    }

    #[test]
    fn launch_url_ok_is_the_core_url_with_its_token() {
        // Success: the window loads the core's URL, token and all — the
        // webview's one origin.
        let u = core_url(47823, "s3cr3t-token");
        assert_eq!(launch_url(Ok(u.clone())), WebviewUrl::External(u));
    }

    #[test]
    fn launch_url_ok_works_on_an_ephemeral_port() {
        // A busy preferred port falls back to an ephemeral one; the launch
        // URL just carries whatever port the core bound.
        let u = core_url(53211, "ab12");
        assert_eq!(launch_url(Ok(u.clone())), WebviewUrl::External(u));
    }

    #[test]
    fn launch_url_err_is_the_bundled_error_page() {
        assert_eq!(
            launch_url(Err("boom".to_string())),
            WebviewUrl::App("index.html#boom".into())
        );
    }

    #[test]
    fn launch_url_err_urlencodes_the_reason() {
        // Spaces and apostrophes must not reach the URL raw.
        assert_eq!(
            launch_url(Err("it didn't answer within 5 seconds".to_string())),
            WebviewUrl::App("index.html#it%20didn%27t%20answer%20within%205%20seconds".into())
        );
    }

    #[test]
    fn launch_url_err_percent_encodes_utf8() {
        // The hash is pure-ASCII percent-encoding (the page decodes it as
        // UTF-8), never raw non-ASCII bytes.
        let page = launch_url(Err("café".to_string()));
        match page {
            WebviewUrl::App(p) => {
                let page = p.to_string_lossy();
                let hash = page
                    .strip_prefix("index.html#")
                    .expect("the bundled page with a hash");
                assert_eq!(hash, "caf%C3%A9");
                assert!(hash.bytes().all(|b| b.is_ascii()), "{hash}");
            }
            other => panic!("expected the bundled error page, got {other:?}"),
        }
    }

    #[test]
    fn launch_url_err_reason_round_trips_through_the_hash() {
        // The page shows `Couldn't start PeakBot on this phone: <reason>.
        // Close the app and open it again.` — `<reason>` is exactly what
        // the hash decodes back to, byte for byte, for any reason the core
        // can produce.
        for reason in [
            "",
            "boom",
            "it didn't answer within 5 seconds",
            "bind: Address in use (os error 98): http://127.0.0.1:47823/?token=ab12",
            "café + naïve",
        ] {
            let page = launch_url(Err(reason.to_string()));
            match page {
                WebviewUrl::App(p) => {
                    let s = p.to_string_lossy().into_owned();
                    let hash = s
                        .strip_prefix("index.html#")
                        .expect("the bundled page with a hash");
                    assert!(
                        hash.bytes().all(|b| b.is_ascii() && b != b' ' && b != b'#'),
                        "hash must be percent-encoded: {s:?}"
                    );
                    let decoded =
                        decoded_reason(&p).unwrap_or_else(|| panic!("no decodable hash in {s:?}"));
                    assert_eq!(decoded, reason, "round trip for {reason:?}");
                }
                other => panic!("expected the bundled error page, got {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    /// The ACL files the shell ships. Exactly one of them may grant IPC to
    /// the loopback core origin, and only the barcode-scanner permissions.
    fn caps_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capabilities")
    }

    fn cap(name: &str) -> serde_json::Value {
        let path = caps_dir().join(format!("{name}.json"));
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("capabilities/{name}.json must exist: {e}"));
        serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("capabilities/{name}.json must be valid JSON: {e}"))
    }

    /// A string array, sorted so the contract does not depend on order.
    fn sorted_str_array(v: &serde_json::Value, what: &str) -> Vec<String> {
        let arr = v
            .as_array()
            .unwrap_or_else(|| panic!("capabilities: {what} must be a string array"));
        let mut out = arr
            .iter()
            .map(|x| {
                x.as_str()
                    .unwrap_or_else(|| panic!("capabilities: {what} entries must be strings"))
                    .to_string()
            })
            .collect::<Vec<_>>();
        out.sort();
        out
    }

    #[test]
    fn loopback_capability_identifier_and_windows() {
        let cap = cap("loopback");
        assert_eq!(cap["identifier"], serde_json::json!("loopback"));
        assert_eq!(sorted_str_array(&cap["windows"], "windows"), ["main"]);
    }

    #[test]
    fn loopback_capability_is_mobile_platforms_only() {
        // The barcode plugin exists only on the phone targets.
        assert_eq!(
            sorted_str_array(&cap("loopback")["platforms"], "platforms"),
            ["android", "iOS"]
        );
    }

    #[test]
    fn loopback_capability_permissions_are_exactly_the_four_barcode_grants() {
        // Nothing else — no core:default, no app commands. The bundled
        // error page gets no IPC at all.
        assert_eq!(
            sorted_str_array(&cap("loopback")["permissions"], "permissions"),
            [
                "barcode-scanner:allow-cancel",
                "barcode-scanner:allow-check-permissions",
                "barcode-scanner:allow-request-permissions",
                "barcode-scanner:allow-scan",
            ]
        );
    }

    #[test]
    fn loopback_capability_grants_ipc_to_exactly_the_loopback_origin() {
        // The one origin, on any port — and no other remote pattern.
        assert_eq!(
            sorted_str_array(&cap("loopback")["remote"]["urls"], "remote.urls"),
            ["http://127.0.0.1:*"]
        );
    }

    #[test]
    fn profiles_page_capability_is_gone() {
        assert!(!caps_dir().join("profiles-page.json").exists());
    }

    #[test]
    fn no_other_capability_grants_remote_urls() {
        let entries =
            std::fs::read_dir(caps_dir()).unwrap_or_else(|e| panic!("capabilities/: {e}"));
        for entry in entries {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "loopback.json" {
                continue;
            }
            let body =
                std::fs::read_to_string(entry.path()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let cap: serde_json::Value =
                serde_json::from_str(&body).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                cap.get("remote").is_none(),
                "capabilities/{name} must not grant remote urls"
            );
        }
    }
}
