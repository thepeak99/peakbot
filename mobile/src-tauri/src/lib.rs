//! PeakBot Mobile — the Tauri glue (plan §2.2): plugins, the main window, the
//! nav guard wiring and the Profiles page commands. Decisions live in the pure
//! modules (`link`, `profiles`, `nav`) and the Local core's lifecycle in
//! `local`; this file only wires them to Tauri.
mod link;
mod local;
mod nav;
mod profiles;

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::webview::Color;
use tauri::{
    AppHandle, Manager, RunEvent, State, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tauri_plugin_log::log;
use tauri_plugin_opener::OpenerExt;

use link::ServerLink;
use local::{Local, should_restart};
use nav::{Known, Nav};
use profiles::{Active, ProfileError, ProfileName, Profiles, Remote, RemoteId};

/// The single init script injected on every origin, both OSes (plan §2.3).
/// Declares the mobile shell marker and shims `window.open` as a top-level
/// navigation (I-D8) the nav guard can classify. Locked bytes — no trailing newline.
pub const INIT_SCRIPT: &str = "Object.defineProperty(window,'peakbotShell',{value:'mobile'});\nwindow.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};";

/// The SPA's dark app background (Tailwind zinc-950); the Profiles page uses
/// the same colour so no white band shows behind either.
const BACKGROUND: Color = Color(9, 9, 11, 255);

/// The app's two storage roots (plan §2.6): `data` holds `profiles.json` and
/// the Local core's config/data; `cache` its cache.
struct AppDirs {
    data: PathBuf,
    cache: PathBuf,
}

impl AppDirs {
    fn resolve(app: &AppHandle) -> tauri::Result<AppDirs> {
        let path = app.path();
        // Android: Tauri's app_data_dir() is the app root (`Context.dataDir`);
        // the data root is `Context.filesDir` under it (R14).
        #[cfg(target_os = "android")]
        let data = path.app_data_dir()?.join("files");
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

/// The Profiles store: `profiles.json` is the persistence, the mutex the one
/// live copy (plan §2.3).
struct Store {
    path: PathBuf,
    profiles: Mutex<Profiles>,
}

impl Store {
    fn read<T>(&self, f: impl FnOnce(&Profiles) -> T) -> T {
        f(&self.profiles.lock().expect("profiles lock"))
    }

    /// Apply `f` to a copy, save it, then replace memory — a failed save
    /// leaves both the file and memory unchanged.
    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut Profiles) -> Result<T, ProfileError>,
    ) -> Result<T, String> {
        let mut live = self.profiles.lock().expect("profiles lock");
        let mut next = live.clone();
        let out = f(&mut next).map_err(|e| e.to_string())?;
        next.save(&self.path)
            .map_err(|e| format!("Couldn't save your profiles: {e}"))?;
        *live = next;
        Ok(out)
    }
}

/// What the Profiles page sees. Never a token or a URL that carries one (P4).
#[derive(Serialize)]
struct ProfilesView {
    local: LocalView,
    remotes: Vec<RemoteView>,
    last: Active,
}

#[derive(Serialize)]
struct LocalView {
    consented: bool,
    running: bool,
}

impl ProfilesView {
    fn of(p: &Profiles, local_running: bool) -> ProfilesView {
        ProfilesView {
            local: LocalView {
                consented: p.local_consent(),
                running: local_running,
            },
            remotes: p.remotes().iter().map(RemoteView::of).collect(),
            last: *p.last(),
        }
    }
}

#[derive(Serialize)]
struct RemoteView {
    id: u32,
    name: String,
    host_label: String,
    origin: String,
}

impl RemoteView {
    fn of(r: &Remote) -> RemoteView {
        RemoteView {
            id: r.id.get(),
            name: r.name.as_str().to_string(),
            host_label: r.link.host_label(),
            origin: r.link.origin().ascii_serialization(),
        }
    }
}

/// `profile_open`'s argument: `"local"` or `{"remote": id}`.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Target {
    Local,
    Remote(u32),
}

#[tauri::command]
fn profiles_get(store: State<'_, Store>, local: State<'_, Local>) -> ProfilesView {
    store.read(|p| ProfilesView::of(p, local.is_running()))
}

#[tauri::command]
fn profile_add(
    store: State<'_, Store>,
    text: String,
    name: Option<String>,
) -> Result<RemoteView, String> {
    store.mutate(|p| {
        let link = ServerLink::parse(&text)?;
        let name = name.as_deref().map(ProfileName::new).transpose()?;
        let id = p.add(link, name)?;
        Ok(RemoteView::of(p.get(id).expect("just added")))
    })
}

#[tauri::command]
fn profile_rename(store: State<'_, Store>, id: u32, name: String) -> Result<(), String> {
    store.mutate(|p| p.rename(RemoteId::new(id), ProfileName::new(&name)?))
}

// Commands that touch the webview are async so they run on the async runtime,
// never on Android's UI thread (the same deadlock class as D2).
#[tauri::command]
async fn profile_remove(
    window: WebviewWindow,
    store: State<'_, Store>,
    id: u32,
) -> Result<(), String> {
    store.mutate(|p| p.remove(RemoteId::new(id)))?;
    // The removed server's token cookie must leave the jar; the other remotes
    // re-authenticate from their stored link on the next open.
    window
        .clear_all_browsing_data()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "android")]
    clear_cookies(&window).map_err(|e| e.to_string())?;
    Ok(())
}

/// wry's Android `clear_all_browsing_data` keeps cookies (and its cookie
/// delete API is a no-op), so wipe them through `CookieManager` directly.
#[cfg(target_os = "android")]
fn clear_cookies(window: &WebviewWindow) -> tauri::Result<()> {
    use tauri::wry::prelude::jni::{JNIEnv, errors::Result, objects::JObject, objects::JValue};

    fn wipe(env: &mut JNIEnv) -> Result<()> {
        let manager = env
            .call_static_method(
                "android/webkit/CookieManager",
                "getInstance",
                "()Landroid/webkit/CookieManager;",
                &[],
            )?
            .l()?;
        let no_callback = JObject::null();
        env.call_method(
            &manager,
            "removeAllCookies",
            "(Landroid/webkit/ValueCallback;)V",
            &[JValue::Object(&no_callback)],
        )?;
        env.call_method(&manager, "flush", "()V", &[])?;
        Ok(())
    }

    window.with_webview(|webview| {
        webview.jni_handle().exec(|env, _activity, _webview| {
            if let Err(e) = wipe(env) {
                let _ = env.exception_clear();
                log::warn!("cookie wipe: {e}");
            }
        })
    })
}

#[tauri::command]
fn local_consent_set(store: State<'_, Store>) -> Result<(), String> {
    store.mutate(|p| {
        p.set_local_consent(true);
        Ok(())
    })
}

/// D3: every profile open is a Rust (browser-initiated) navigation, so
/// Android keeps the Profiles page in history and Back returns to it.
#[tauri::command]
async fn profile_open(
    window: WebviewWindow,
    store: State<'_, Store>,
    local: State<'_, Local>,
    target: Target,
) -> Result<(), String> {
    let url = match target {
        Target::Local => {
            let url = local.ensure().await?;
            store.mutate(|p| {
                p.set_last(Active::Local);
                Ok(())
            })?;
            url
        }
        Target::Remote(id) => {
            let id = RemoteId::new(id);
            store.mutate(|p| {
                let url = p
                    .get(id)
                    .ok_or(ProfileError::NoSuchProfile)?
                    .link
                    .url()
                    .clone();
                p.set_last(Active::Remote(id));
                Ok(url)
            })?
        }
    };
    window.navigate(url).map_err(|e| e.to_string())
}

/// The bundled Profiles page, as the webview first loads it. Android serves the
/// bundle from `http://tauri.localhost`, Apple from `tauri://localhost`.
fn app_index_url() -> Url {
    let base = if cfg!(target_os = "android") {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    Url::parse(base).expect("static url")
}

/// D2: returns at once; every side effect is spawned, because opener/navigate
/// calls inside Android's `shouldOverrideUrlLoading` deadlock the UI thread (ANR).
fn on_navigation(app: &AppHandle, url: &Url) -> bool {
    let remotes: Vec<url::Origin> = app
        .state::<Store>()
        .read(|p| p.remotes().iter().map(|r| r.link.origin()).collect());
    let local = app.state::<Local>().origin();
    let known = Known {
        local: local.as_ref(),
        remotes: &remotes,
    };
    let nav = nav::classify(url, &known);
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
        Nav::Profiles => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(w) = app.get_webview_window("main")
                    && let Err(e) = w.navigate(app_index_url())
                {
                    log::warn!("navigate to profiles: {e}");
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

/// §2.4: only the Local page may restart the phone's core — a Remote page
/// sending the sentinel does nothing. A failed restart lands on the Profiles
/// page, where opening Local again shows the error.
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

/// §2.9: back in the foreground, re-serve the Local core if its listener
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

/// For logs: a profile URL's query carries its token.
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
    let path = dirs.data.join("profiles.json");
    let profiles = Mutex::new(Profiles::load(&path));
    app.manage(Store { path, profiles });

    let handle = app.handle().clone();
    WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .background_color(BACKGROUND)
        .initialization_script(INIT_SCRIPT)
        .on_navigation(move |url| on_navigation(&handle, url))
        .build()?;
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
        // D1: the default click handler turns `<a target=_blank>` into an
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
        .invoke_handler(tauri::generate_handler![
            profiles_get,
            profile_add,
            profile_rename,
            profile_remove,
            local_consent_set,
            profile_open
        ])
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

    fn store_at(path: PathBuf) -> Store {
        Store {
            path,
            profiles: Mutex::new(Profiles::default()),
        }
    }

    #[test]
    fn profiles_view_never_carries_the_token() {
        let mut p = Profiles::default();
        let id = p.add(ServerLink::parse(LINK).unwrap(), None).unwrap();
        p.set_last(Active::Remote(id));
        let json = serde_json::to_string(&ProfilesView::of(&p, false)).unwrap();
        assert_eq!(
            json,
            r#"{"local":{"consented":false,"running":false},"remotes":[{"id":1,"name":"nas.lan:7823","host_label":"nas.lan:7823","origin":"https://nas.lan:7823"}],"last":{"remote":1}}"#
        );
        assert!(!json.contains("s3cr3t"));
    }

    #[test]
    fn target_accepts_exactly_the_two_shapes() {
        let t = |s: &str| serde_json::from_str::<Target>(s).ok();
        assert!(matches!(t(r#""local""#), Some(Target::Local)));
        assert!(matches!(t(r#"{"remote":3}"#), Some(Target::Remote(3))));
        assert!(t(r#""remote""#).is_none());
        assert!(t(r#""Local""#).is_none());
    }

    #[test]
    fn mutate_saves_then_replaces_memory() {
        let dir = std::env::temp_dir().join(format!("peakbot-mobile-glue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = store_at(dir.join("profiles.json"));
        store
            .mutate(|p| p.add(ServerLink::parse(LINK)?, None))
            .unwrap();
        let on_disk = Profiles::load(&store.path);
        store.read(|live| assert_eq!(*live, on_disk));
        assert_eq!(on_disk.remotes().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failed_save_leaves_memory_unchanged() {
        let store = store_at(PathBuf::from("/nonexistent-peakbot-dir/profiles.json"));
        let err = store
            .mutate(|p| p.add(ServerLink::parse(LINK)?, None))
            .unwrap_err();
        assert!(err.starts_with("Couldn't save your profiles: "), "{err}");
        store.read(|live| assert_eq!(*live, Profiles::default()));
    }

    #[test]
    fn rejected_link_surfaces_the_locked_string() {
        let store = store_at(PathBuf::from("/nonexistent-peakbot-dir/profiles.json"));
        let err = store
            .mutate(|p| p.add(ServerLink::parse("http://nas.lan/?token=x")?, None))
            .unwrap_err();
        assert_eq!(err, link::LinkError::NotHttps.to_string());
    }

    #[test]
    fn logged_urls_drop_the_token() {
        let url = Url::parse(LINK).unwrap();
        assert_eq!(without_query(&url), "https://nas.lan:7823/");
    }
}

#[cfg(test)]
mod init_script_tests {
    use super::INIT_SCRIPT;

    /// The locked bytes (plan §2.3 + the I-D8 `window.open` shim): two
    /// lines, ONE `\n` between them, no trailing newline — same style as the
    /// desktop `SHELL_MARKER_SCRIPT`
    /// (`Object.defineProperty(window,'peakbotShell',{value:'desktop'});`).
    const EXPECTED: &str = "Object.defineProperty(window,'peakbotShell',{value:'mobile'});\n\
window.open=function(u){if(u){location.href=new URL(String(u),location.href).href}return null};";

    /// `web/src/desktop.test.ts` (F1) sets `window.peakbotShell` to exactly
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
        // I-D8: every window.open becomes a top-level navigation the guard
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
