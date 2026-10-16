# PeakBot Desktop — Design (LOCKED)

Status: **LOCKED** for v1 (targets: Linux x86_64 AppImage, macOS universal unsigned `.app` in `.zip`).
Base: peakbot 0.23.0 @ `772aee9`. Owner of this doc: Architect. Changes after lock need a written reason in §8 (amendment #377: settings page, see "Amendment #377" at the end of §8).

---

## 1. SUMMARY

PeakBot Desktop is a Tauri v2 window that shows the **existing** PeakBot web UI by navigating a
webview to `http(s)://127.0.0.1:<port>/[?token=<token>]`, served by a peakbot binary that ships inside the
app (the *sidecar*). By default the desktop app owns the backend's lifetime (**session mode**: spawn
on launch, SIGINT→wait→SIGKILL on exit). Optionally the user ticks "Keep PeakBot running in
background" and the app installs the bundled binary as the user's systemd/launchd service through
peakbot's own `install` / `service …` verbs (**service mode**) and simply connects to
`127.0.0.1:7823`. The web UI is never rebuilt, never bundled, never given IPC; the only local UI is
a small local UI (the status page "starting… / error + Retry / Open logs", and the settings window, #377).

**The one key idea:** *the desktop app is a thin, stateless launcher — every decision is a pure
function in a Tauri-free crate (`peakbot-desktop-core`), every fact comes from peakbot itself.*
The service manager is the single source of truth for the mode (service installed ⇔ service mode;
mode is never stored — a single `settings.json` stores launch parameters only: protocol/token/remote), `peakbot --version` is the single source of truth for versions, and
peakbot's own `install`/`service` verbs are the only code that touches systemd/launchd. The Tauri
crate only paints windows, menus and dialogs.

---

## 2. COMPONENTS

```
desktop/
├── DESIGN.md                      this file
├── README.md                      user doc: install, Gatekeeper, background mode, manual removal
├── Cargo.toml                     Cargo workspace (members: core, src-tauri), shared version
├── Cargo.lock                     committed; all builds use --locked
├── core/                          crate `peakbot-desktop-core` (lib, NO tauri dep)
│   └── src/{lib,consts,token,version,paths,env,nav,service,session,ready,phase,settings,sys,controller}.rs
├── src-tauri/                     crate `peakbot-desktop` (bin) — thin glue
│   ├── build.rs                   tauri_build with app-manifest (command ACL)
│   ├── tauri.conf.json
│   ├── capabilities/{status-page,settings-page}.json
│   ├── icons/                     generated once by `tauri icon`, committed
│   ├── binaries/.gitkeep          sidecar copied here at build time (gitignored)
│   └── src/{main,window,tray,bridge,commands}.rs
├── ui/                            frontendDist — bundled local pages, no build step
│   ├── index.html
│   ├── status.js
│   ├── settings.html              settings window (#377)
│   └── settings.js
├── macos/
│   ├── Info.plist.in              template (@VERSION@)
│   └── assemble-app.sh            hand-assembles + ad-hoc signs + zips the .app
└── scripts/
    ├── build-desktop.sh           single entry point used by Makefile AND CI
    ├── smoke-linux.sh             runs inside Dockerfile.desktop-linux
    ├── smoke-macos.sh             runs inside Dockerfile.desktop-macos
    ├── check-version.sh           desktop version == root Cargo.toml version
    └── attach-release.sh          CI: attach artifacts to the Gitea release for a tag
Dockerfile.desktop-linux           (repo root, next to the existing Dockerfiles)
Dockerfile.desktop-macos
.gitea/workflows/desktop.yml
```

| Component | Single responsibility | Why it exists |
|---|---|---|
| `core` crate | All decisions + std-only side-effect implementation behind the `Sys` trait | Unit-testable with plain `cargo test` in CI's `rust:1.95` container — no webkit, no display, no sidecar file (tauri-build *fails* if `externalBin` is missing, so tests must not live in the Tauri crate). |
| `core::controller` | The startup/mode state machine; owns the session child process | One owner of the child + phase ⇒ no locks, no races. Runs on one dedicated thread. |
| `core::sys` (`RealSys`) | The only place that spawns processes, opens sockets, sends signals, touches files | Injectable seam; tests use a `FakeSys` with a virtual clock. |
| `src-tauri` glue | Window(s), navigation guard, tray, native dialogs, 4 IPC commands for the local pages, exit hook | Tauri-specific; deliberately logic-free. |
| `core::settings` | The settings model (`Settings` sum type), boundary validation, `settings.json` parse/render/load, `settings_view` (#377) | Illegal combos (remote without HTTPS/token) are unrepresentable; validated once at the IPC edge. |
| `ui/settings.*` settings window | Edits protocol/token/remote, toggles background; polled `settings_get`, `settings_set`/`act` | Opened from tray / macOS menu / Ctrl+Shift+S / status-page link; IPC-gated to the bundled origin only (#377). |
| `ui/` status page | Shows `StatusView` (polled), buttons → `act` | The only thing shown when the backend isn't up. Plain HTML/JS, embedded via `frontendDist`. |
| `macos/assemble-app.sh` | Builds the `.app` layout Tauri CLI can't build on Linux | Researcher: Tauri CLI cannot bundle `.app` on Linux. |
| Dockerfiles + `build-desktop.sh` | Reproducible builds, same path locally (podman) and in CI (docker) | Follows the existing `Dockerfile.*` + `--output type=local` pattern. |
| Gitea workflow | Build both artifacts on `dind`, upload, attach on tag | Requirement 5. |

**Deliberately NOT added (and why):**
- **No `tauri-plugin-shell`.** We spawn the sidecar with `std::process::Command` from Rust. `externalBin` is a *bundler* feature and works without the plugin. std gives us exact control over env, stdio, signals; the plugin would add a JS-facing capability surface we must then deny.
- **No mode flag.** Mode is derived from `peakbot service status`. A stored flag could disagree with reality (user ran `peakbot service uninstall` in a terminal). The state files (§3.6) are caches of user *decisions*/process bookkeeping/launch parameters, never of mode. (Amended #377: `settings.json` exists but holds launch parameters only.)
- **No HTTP version endpoint / no backend API change for versions.** `<exe> --version` already exists.
- **No IPC for the remote origin, no `remote` capability, no `@tauri-apps/api` npm package, no frontend build.**
- **No `PR_SET_PDEATHSIG`.** Linux-only, tied to the *spawning thread's* lifetime (subtle), and a second orphan mechanism. The pidfile reaper (§4.6) is portable and sufficient.
- **No auto-updater, no code signing/notarization, no Windows build** (decision 4). Design stays portable: Windows differences are isolated in `RealSys` (`interrupt` = kill) and `paths::sidecar_file_name`.
- **No "Restart backend" / "Open in browser" tray items.** Not requested (YAGNI).
- **No custom app menu on Linux.** Tray only (see risk R6). macOS keeps Tauri's default menu (needed for ⌘C/⌘V/⌘Q) plus our "PeakBot" submenu.
- **No `tauri-plugin-global-shortcut` (#377).** It grabs the key system-wide even when the app is unfocused and does not work on Wayland. The settings shortcut is a macOS menu accelerator plus, on every platform, an init-script `keydown` that navigates to a reserved URL the nav guard intercepts (no IPC).
- **No drift detection (#377).** A CLI `service install` that changes the service is not reflected in `settings.json`; connecting still works (the service protocol comes from the `TLS:` status line), and the next Save re-applies the desktop's values.
- **No process-group for the sidecar.** Not setting it means Ctrl+C on a terminal-launched desktop app also SIGINTs the sidecar (graceful), instead of orphaning it.

---

## 3. DATA MODEL

All types live in `core`. Field privacy enforces invariants; constructors are the only way in.

### 3.1 Constants (`core::consts`)
```rust
pub const SERVICE_PORT: u16 = 7823;            // peakbot default; desktop-installed service always binds 127.0.0.1:7823
pub const SESSION_PREFERRED_PORT: u16 = 7824;  // tried first so the webview origin (localStorage/cookies) is stable across launches
pub const PORT_ATTEMPTS: u32 = 3;              // session spawn retries on "already in use"
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
pub const POLL_INTERVAL: Duration = Duration::from_millis(150);
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
pub const STOP_GRACE: Duration = Duration::from_secs(3);
pub const RUN_TIMEOUT: Duration = Duration::from_secs(60);   // short-lived sidecar commands
pub const TICK: Duration = Duration::from_millis(500);       // controller liveness check
pub const LOG_TAIL_LINES: usize = 20;
```

### 3.2 `Token` (`core::token`)
```rust
pub struct Token(String);                 // private field
impl Token {
    pub fn parse(s: &str) -> Option<Token>;    // trims; None if empty/whitespace or contains any whitespace/control char
    pub fn generate(sys: &impl Sys) -> Token;  // delegates to sys.random_token()
    pub fn as_str(&self) -> &str;
}
impl fmt::Debug for Token  // prints "Token(***)" — never the secret
impl Clone, PartialEq, Eq
```
Invariant: a `Token` is always non-empty and URL-safe after percent-encoding; it is never logged.
Generated tokens = 32 random bytes, lowercase hex (64 chars).

### 3.3 Versions (`core::version`)
```rust
pub fn parse_version_output(stdout: &str) -> Option<semver::Version>; // exactly "peakbot <semver>" (trimmed, first line)
pub enum InstalledVersion { Known(semver::Version), Unknown }        // Unknown = exe missing/failed/unparseable
pub struct DeclinedReplace { pub installed: InstalledVersion, pub bundled: semver::Version }
impl DeclinedReplace {
    pub fn render(&self) -> String;               // "installed=<ver|unknown>\nbundled=<ver>\n"
    pub fn parse(s: &str) -> Option<DeclinedReplace>;
    pub fn matches(&self, installed: &InstalledVersion, bundled: &semver::Version) -> bool; // exact pair equality
}
```

### 3.4 Service (`core::service`)
```rust
pub enum RunState { Running, Stopped, Unknown }
pub struct InstalledService { pub exe: Option<PathBuf>, pub run: RunState, pub tls: bool } // `TLS:       on` ⇒ true; off/missing ⇒ false (#377)
pub enum ServiceState {
    Unavailable { reason: String },   // `service status` exited non-zero (no systemd user bus, unsupported OS, …)
    NotInstalled,
    Installed(InstalledService),
}
pub enum LaunchPlan {
    Session,
    ConnectService { start_first: bool },       // start_first ⇔ run == Stopped
    AskReplace { installed: InstalledVersion }, // versions differ (or unknown) and not previously declined
}
```

### 3.5 Phase, errors, views (`core::phase`)
```rust
pub enum Mode {                            // amended #377: scheme-aware, session token optional
    Session { port: u16, protocol: Protocol, token: Option<Token> },
    Service { protocol: Protocol, token: Option<Token> },   // None ⇔ web-token file absent
}
impl Mode {
    pub fn port(&self) -> u16;             // Service ⇒ SERVICE_PORT
    pub fn url(&self) -> url::Url;         // {scheme}://127.0.0.1:<port>/ [+ ?token=<percent-encoded>]
    pub fn probe_url(&self) -> url::Url;   // {scheme}://127.0.0.1:<port>/commands [+ ?token=…]
    pub fn origin(&self) -> String;        // "{scheme}://127.0.0.1:<port>" — always 127.0.0.1, even when remote
}

pub enum StartStep {
    CheckingBundled, CheckingService, StartingSession, WaitingForBackend,
    InstallingService, RemovingService, ConnectingService, StoppingSession,
}

pub enum StartError {
    SidecarMissing { path: PathBuf },
    SidecarBroken { detail: String },                       // --version failed or unparseable
    PortUnavailable,                                        // PORT_ATTEMPTS exhausted
    SessionExited { code: Option<i32>, log_tail: String },  // died during startup OR while Ready
    NotReady { waited: Duration },                          // session readiness timeout
    TokenRejected { service: bool },                        // probe returned 401/403
    ServiceUnreachable { waited: Duration },
    ServiceCommandFailed { command: String, output: String },
}

pub enum Phase { Starting(StartStep), Ready(Mode), Failed(StartError) }
```
`Cancelled` is not a `StartError`: cancellation (quitting) is a control-flow outcome internal to the
controller and never becomes a visible phase.

```rust
#[derive(Serialize)] #[serde(rename_all = "snake_case")]
pub enum UiAction { Retry, OpenLogs, ReinstallService, Quit, ToggleBackground }   // also Deserialize: the `act` command's input; status_view never emits ToggleBackground (settings window only, #377)

#[derive(Serialize)]
pub struct StatusView {
    pub kind: &'static str,        // "starting" | "ready" | "failed"
    pub title: String,
    pub detail: Option<String>,    // multi-line, shown in <pre>
    pub actions: Vec<UiAction>,    // ordered, rendered as buttons
}
pub fn status_view(phase: &Phase) -> StatusView;

pub struct TrayModel {
    pub status: String,            // disabled first item
    pub background_checked: bool,
    pub background_enabled: bool,
    pub background_label: String,
}
pub fn tray_model(phase: &Phase, service: Option<&ServiceState>, bundled: Option<&semver::Version>) -> TrayModel;

pub struct Dialog { pub title: String, pub body: String, pub ok: String, pub cancel: Option<String> } // cancel None ⇒ info dialog
```

**Locked strings** (tests assert them verbatim; `{v}` = bundled version or "?" when unknown):

| Phase | `tray.status` | `status_view.title` | actions |
|---|---|---|---|
| Starting(step) | `PeakBot {v} · starting…` | step text (below) | `[]` |
| Ready(Session{port}) | `PeakBot {v} · running (port {port})` | `Opening PeakBot…` | `[]` |
| Ready(Service) | `PeakBot {v} · running in background` | `Opening PeakBot…` | `[]` |
| Failed(SidecarMissing) | `PeakBot {v} · error` | `The PeakBot backend is missing from this app` | Quit |
| Failed(SidecarBroken) | 〃 | `The bundled PeakBot backend failed to run` | Retry, OpenLogs, Quit |
| Failed(PortUnavailable) | 〃 | `Couldn't find a free local port` | Retry, Quit |
| Failed(SessionExited) | 〃 | `PeakBot stopped unexpectedly` | Retry, OpenLogs, Quit |
| Failed(NotReady) | 〃 | `PeakBot didn't start within {secs} s` | Retry, OpenLogs, Quit |
| Failed(TokenRejected{service:false}) | 〃 | `Another program answered on PeakBot's port` | Retry, Quit |
| Failed(TokenRejected{service:true}) | 〃 | `The background service rejected the access token` | Retry, ReinstallService, Quit |
| Failed(ServiceUnreachable) | 〃 | `The background PeakBot service isn't responding on 127.0.0.1:7823` | Retry, ReinstallService, Quit |
| Failed(ServiceCommandFailed) | 〃 | `A background-service command failed` | Retry, ReinstallService, OpenLogs, Quit |

Details: `SessionExited` → `"exit code {n}"`/`"killed by a signal"` + `\n\n` + log tail;
`ServiceUnreachable`/`TokenRejected{service:true}` → service log hint:
Linux `journalctl --user -u peakbot.service`, macOS `~/Library/Logs/peakbot.log`;
`ServiceCommandFailed` → `"$ {command}\n{output}"`; `SidecarMissing` → the path.

StartStep texts: CheckingBundled `Checking the bundled PeakBot…`, CheckingService `Looking for a
background service…`, StartingSession `Starting PeakBot…`, WaitingForBackend `Waiting for PeakBot to
be ready…`, InstallingService `Installing the background service…`, RemovingService `Removing the
background service…`, ConnectingService `Connecting to the background service…`, StoppingSession
`Stopping PeakBot…`.

Tray background item: label always `Keep PeakBot running in background`, except
`ServiceState::Unavailable` → `Keep PeakBot running in background (unavailable)`.
`checked = service == Some(Installed(_))`; `enabled = phase is Ready|Failed && service is Some(NotInstalled|Installed)`.

### 3.6 Files the desktop app owns (in `DesktopPaths.data_dir`)

| File | Content | Lifetime |
|---|---|---|
| `sidecar.pid` | `pid=<u32>\nport=<u16>\n` | Written right after session spawn; removed after the child is confirmed exited. A leftover = previous crash. |
| `declined-replace` | `DeclinedReplace::render()` | Written when the user clicks "Keep"; only suppresses the dialog for the *exact* (installed, bundled) pair. |
| `settings.json` | `{"v":1,"protocol":"http\|https","token":bool,"remote":bool}` (§3.9) | Written (0600, atomic) by the controller only, on a confirmed Save. Missing/corrupt/`v≠1` loads as defaults and is never rewritten on load. Reset = delete the file. |

Files peakbot owns that the desktop reads/writes: `<peakbot_config_dir>/web-token` (read; written
0600 / removed by the desktop at every session launch and service (re)install to match `settings.token` — invariant I2), `<peakbot_config_dir>/config.yaml`
(existence check only). Logs (in `DesktopPaths.log_dir`): `desktop.log` (tauri-plugin-log),
`peakbot-session.log` (sidecar stdout+stderr, truncated at each spawn, created 0600 — it contains
the URL with the session token).

### 3.7 Paths (`core::paths`)
```rust
pub struct DesktopPaths {
    pub sidecar: PathBuf,             // sidecar_path(current_exe)
    pub data_dir: PathBuf,            // tauri app_data_dir()  (Linux ~/.local/share/com.peakbot.desktop, macOS ~/Library/Application Support/com.peakbot.desktop)
    pub log_dir: PathBuf,             // tauri app_log_dir()
    pub peakbot_config_dir: PathBuf,  // peakbot_config_dir()
}
pub fn sidecar_path(current_exe: &Path) -> PathBuf;   // current_exe.parent()/sidecar_file_name()
pub fn sidecar_file_name() -> &'static str;           // "peakbot" (unix) | "peakbot.exe" (windows)
pub fn peakbot_config_dir() -> Option<PathBuf>;       // directories_next::ProjectDirs::from("com","peakbot","peakbot").config_dir()
                                                      // — SAME crate + args as peakbot's get_config_dir(), so paths match by construction
impl DesktopPaths {
    pub fn pidfile(&self) -> PathBuf;          // data_dir/sidecar.pid
    pub fn declined_file(&self) -> PathBuf;    // data_dir/declined-replace
    pub fn session_log(&self) -> PathBuf;      // log_dir/peakbot-session.log
    pub fn web_token(&self) -> PathBuf;        // peakbot_config_dir/web-token
    pub fn peakbot_config(&self) -> PathBuf;   // peakbot_config_dir/config.yaml
    pub fn settings_file(&self) -> PathBuf;    // data_dir/settings.json (#377)
}
```
Sidecar location at runtime: AppImage `$APPDIR/usr/bin/peakbot` next to `usr/bin/peakbot-desktop`;
macOS `X.app/Contents/MacOS/peakbot`; dev `desktop/target/debug/peakbot` (tauri-build copies
`externalBin` next to the exe, stripping the triple).

### 3.8 Invariants (summary)
1. (**amended #377 — I1**) `Settings::Remote` ⇒ HTTPS ∧ token, by the type: a `Settings` value cannot exist without passing `TryFrom<SettingsWire>`. (The former "a session is always tokened" is gone: loopback token is optional; token is mandatory only for remote.)
1b. (**I2**) After any desktop-driven session launch or service (re)install: `settings.token()` ⇔ `web-token` holds a parseable token. The session's token *is* the file's token; service mode's token is exactly what's in `web-token`.
2. At most one sidecar child exists, owned by the controller thread (`Option<Session>`).
3. `Phase::Ready(Session)` ⇒ `session.is_some()` and child not yet observed exited.
4. A pidfile exists ⇔ a session child was spawned and not yet confirmed exited (modulo crash).
5. Mode is never stored; it is derived from `ServiceState` on every Start.
6. The remote origin can never invoke IPC (no remote capability; app commands ACL-gated; §5.4).
7. Tokens never appear in argv, desktop logs, or `Debug` output (I7).
8. (**I3**) The service protocol comes from `service status` (`TLS:` line), never from `settings.json`.
9. (**I4**) The session protocol/bind/token come from `settings.json` at launch time.
10. (**I5**) Only the controller thread writes `settings.json` and `web-token`; writes are atomic (`write_private`).

### 3.9 Settings (`core::settings`) — amendment #377
```rust
pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol { Http, Https }
impl Protocol { pub fn scheme(self) -> &'static str; } // "http" | "https"

/// The validated model. Illegal combos are unrepresentable:
/// Remote ⇒ HTTPS ∧ token by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settings {
    Loopback { protocol: Protocol, token: bool },
    Remote,
}
impl Default for Settings { /* Loopback { protocol: Http, token: true } — today's behaviour */ }
impl Settings {
    pub fn protocol(self) -> Protocol;      // Remote ⇒ Https
    pub fn token(self) -> bool;             // Remote ⇒ true
    pub fn remote(self) -> bool;
    pub fn bind_host(self) -> &'static str; // "127.0.0.1" | "0.0.0.0"
}

/// Untrusted flat shape: IPC payload (both directions) and the file body.
/// Unknown fields are IGNORED (serde default), so newer files with extra
/// keys still load. Missing fields are a parse error, so the file loads as defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsWire { pub protocol: Protocol, pub token: bool, pub remote: bool }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsError { RemoteNeedsHttps, RemoteNeedsToken }
impl fmt::Display for SettingsError { /* exact strings below */ }
impl std::error::Error for SettingsError {}

impl TryFrom<SettingsWire> for Settings { type Error = SettingsError; } // THE boundary check
impl From<Settings> for SettingsWire {}

// file = {"v":1, ...SettingsWire}  (private struct SettingsFile { v: u32, #[serde(flatten)] s: SettingsWire })
pub fn render_settings_file(s: Settings) -> String;              // compact JSON + "\n"
pub fn parse_settings_file(text: &str) -> Result<Settings, String>; // json err | "unsupported settings version {v}" | SettingsError text
pub fn load_settings(text: Option<&str>) -> Settings;            // None ⇒ default; Err ⇒ log::warn! + default

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    pub settings: SettingsWire,     // what is saved
    pub background: bool,           // = tray.background_checked
    pub background_enabled: bool,   // = tray.background_enabled
    pub busy: bool,                 // = status.kind == "starting"
    pub web_token_path: String,     // shown under the token row
}
pub fn settings_view(s: Settings, tray: &TrayModel, status: &StatusView, web_token: &Path) -> SettingsView;
```

**Exact strings (locked, asserted by tests):**
- `SettingsError::RemoteNeedsHttps` → `Remote access requires HTTPS.`
- `SettingsError::RemoteNeedsToken` → `Remote access requires the access token.`
- Check order: if HTTPS is missing, that error wins. (`remote ∧ http ∧ ¬token` → the HTTPS message.)
- `render_settings_file(Settings::default())` == `{"v":1,"protocol":"http","token":true,"remote":false}\n`
- `render_settings_file(Settings::Remote)` == `{"v":1,"protocol":"https","token":true,"remote":true}\n`

**Load policy (criterion 9):** any of these makes `load_settings` return the default and log
one warning: missing file, non-JSON, `null`, a missing field, `v != 1`, an illegal combination.
Unknown extra keys are ignored. Load never writes.

---

## 4. DATA FLOW / CONTROL FLOW

### 4.1 Threads
- **Main thread** (Tauri event loop): window, tray, menu events, IPC commands. Never blocks on backend work.
- **Controller thread** (one, spawned in `setup`): owns `Controller`; loop =
  `rx.recv_timeout(TICK)` → `handle(action)` or `tick()`. All long work (spawn, readiness wait,
  service commands, blocking dialogs) happens here, serialized. Dialogs use
  `tauri-plugin-dialog`'s `blocking_show` (legal off the main thread).
- A shared `Arc<AtomicBool> quitting` lets the main thread cancel waits within one `POLL_INTERVAL`.

### 4.2 Actions
```rust
pub enum Action { Start, ToggleBackground, ReinstallService, SaveSettings(Settings), Shutdown(std::sync::mpsc::Sender<()>) }
```
Mapping: app launch → `Start`; status page `Retry` → `Start`; `ReinstallService` → `ReinstallService`;
tray checkbox click → `ToggleBackground` (controller decides enable vs disable from its own
`ServiceState`); settings window background row → `act(toggle_background)` → `ToggleBackground`; settings window Save → `settings_set` → `SaveSettings` (§4.11); `RunEvent::Exit` → `Shutdown(ack)`. `OpenLogs`, `Quit`, "Open PeakBot" are handled
in glue without the controller.
While `Phase::Starting`, `ToggleBackground`, `ReinstallService` and `SaveSettings` are ignored (tray item is disabled anyway).

### 4.3 Startup state machine (`Action::Start`)

```
        ┌──────────────────────────────────────────────────────────────────────┐
Start ──► stop existing session (if any) ─► reap stale pidfile (§4.6)          │
        ► Starting(CheckingBundled): run `<sidecar> --version`                 │
             not found ─────────────────────────────► Failed(SidecarMissing)   │
             error / unparseable ───────────────────► Failed(SidecarBroken)    │
        ► Starting(CheckingService): run `<sidecar> service status` → ServiceState
        ► plan_launch(state, version_of, bundled, declined) ─┬─ Session ───────► LAUNCH_SESSION
                                                             ├─ ConnectService{start_first} ─► CONNECT
                                                             └─ AskReplace ─► dialog(replace_dialog)
                                                                   Replace ─► REPLACE ─► CONNECT{false}
                                                                   Keep ────► write declined-replace ─► CONNECT{start_first}
                                                                                                        │
LAUNCH_SESSION (attempt 1..=PORT_ATTEMPTS):                                                             │
   Starting(StartingSession): port = 7824 if free else sys.free_port()   (err ► Failed(PortUnavailable))│
     (amended #377) s = settings(); token = sync_token(s) once per launch (§4.11)                      │
     spawn `<sidecar> --bind {127.0.0.1|0.0.0.0}:<port> [--tls]`                                       │
       env: sanitized parent env + [PEAKBOT_WEB_TOKEN=<token> iff token on] + PEAKBOT_NO_OPEN=1        │
       stdin null, stdout+stderr → peakbot-session.log (truncate, 0600)                                 │
       spawn io error ─► Failed(SidecarBroken)                                                          │
     write pidfile                                                                                      │
   Starting(WaitingForBackend): wait_ready(probe_url, READY_TIMEOUT, Some(child))                       │
     Ready ─► Phase::Ready(Session{port,protocol,token})                                                          │
     ChildExited(code) & classify_early_exit == PortTaken & attempts left ─► next attempt               │
     ChildExited(code) ─► Failed(SessionExited{code, log_tail})                                         │
     Timeout ─► stop child ─► Failed(NotReady)                                                          │
     TokenRejected ─► stop child ─► Failed(TokenRejected{service:false})                                │
   attempts exhausted ─► Failed(PortUnavailable)                                                        │
                                                                                                        │
CONNECT{start_first}:  ◄────────────────────────────────────────────────────────────────────────────────┘
   Starting(ConnectingService)
   token = Token::parse(read(web-token))   (absent ⇒ None; NEVER generated here — would not match the running service)
   protocol = Https iff service is Installed{tls:true} (`TLS:` status line, I3), else Http   (amended #377)
   if start_first: run `<sidecar> service install --bind 127.0.0.1:7823` (+ `--tls` per settings; idempotent; starts it)
       non-zero ─► Failed(ServiceCommandFailed)
   wait_ready(Mode::Service{token}.probe_url(), READY_TIMEOUT, None)
     Ready ─► Phase::Ready(Service{protocol,token})
     Timeout ─► Failed(ServiceUnreachable)      TokenRejected ─► Failed(TokenRejected{service:true})

REPLACE (also = Action::ReinstallService):
   Starting(RemovingService):   `<sidecar> service uninstall`  (non-zero tolerated: may not be loaded)
   Starting(InstallingService): sync_token(settings())   (amended #377; was ensure_token)
                                `<sidecar> install`                     non-zero ─► Failed(ServiceCommandFailed)
                                `<sidecar> service install --bind {host}:7823 [--tls]`   non-zero ─► Failed(ServiceCommandFailed)
   then CONNECT{start_first:false}; refresh ServiceState via `service status`; delete declined-replace
```
Why uninstall-then-install for replace: `systemctl --user enable --now` does **not** restart a
running unit, so a plain re-install would leave the old binary running. Uninstall (`disable --now`
/ `bootout`) + install is the portable "restart with new binary" using only peakbot verbs.

### 4.4 `plan_launch` truth table (locked)

| ServiceState | installed version vs bundled | declined pair matches | → LaunchPlan |
|---|---|---|---|
| Unavailable | – | – | Session |
| NotInstalled | – | – | Session |
| Installed(run) | equal | – | ConnectService{start_first: run==Stopped} |
| Installed | different `Known` | no | AskReplace |
| Installed | different `Known` | yes | ConnectService{start_first: run==Stopped} |
| Installed | `Unknown` | no | AskReplace |
| Installed | `Unknown` | yes | ConnectService{…} |

`version_of(installed)` = `exe` is `None` → `Unknown`; else run `<exe> --version` → `parse_version_output` → `Known`/`Unknown`.
`RunState::Unknown` counts as running (`start_first = false`).

**Decision (requirement 3): the service's version = `<Exec path from service status> --version`.**
Rejected: `/api/setup` `exe_path` — needs the service to be *running* and a valid token, and is one
more HTTP contract. `service status` works when the service is stopped, broken, or tokenless, and
`--version` is the binary's own truth. Known gap: if someone overwrote the binary without
restarting the service, the on-disk version may differ from the running process — harmless,
because Replace restarts it anyway.

Port-7823 probing is **not** used for detection: a peakbot answering on 7823 without an installed
service is a user-run foreground instance; session mode uses its own port and ignores it.

### 4.5 Mode toggle (`Action::ToggleBackground`)

**Enable** (ServiceState = NotInstalled):
1. `!sys.exists(paths.peakbot_config())` → info dialog `Finish setup first` (§5.6) → return (no state change).
   Reason: a service started without config + without DISPLAY refuses to start and crash-loops.
2. `!sys.port_is_free(7823)` → info dialog `Port 7823 is busy` → return.
3. confirm dialog `Keep PeakBot running in background?` → Cancel → return.
4. stop session (Starting(StoppingSession)) — **before** installing, so two backends never share the config/DB.
5. REPLACE steps minus the uninstall (sync_token(settings()) → `install` → `service install`) → CONNECT{false}.
6. On any failure in 5: best-effort `service uninstall`, refresh ServiceState, error dialog with the
   command output, then LAUNCH_SESSION (the user is back where they were).

**Disable** (ServiceState = Installed):
1. confirm dialog `Remove background service?` → Cancel → return.
2. Starting(RemovingService): `service uninstall`; non-zero → error dialog, refresh state, CONNECT{false} again (unchanged mode).
3. refresh ServiceState → LAUNCH_SESSION.
Leaves `~/.local/bin/peakbot` and `web-token` in place (stated in the dialog).

**This unchecking is the explicit "Remove background service" action** (requirement 2). Because
AppImage/zip have no uninstall hooks, honoring "uninstall removes service" = (a) this item, (b) the
README's manual-removal section (§5.8), (c) by design the service runs from the *stable copy*
`~/.local/bin/peakbot`, so deleting the app never leaves a crash-looping service pointing at a
vanished `/tmp/.mount_*` or `.app` path.

### 4.6 Shutdown and orphans

`stop_session()` (used by Shutdown, Start-restart, Enable):
`child.interrupt()` (SIGINT to the pid) → poll `try_wait` every `POLL_INTERVAL` up to `STOP_GRACE`
→ if still alive `child.kill()` (SIGKILL) + `try_wait` → remove pidfile → `session = None`.

`Action::Shutdown(ack)`: `stop_session()` if any, send `ack`, return from the loop. Service mode: nothing to stop.

Glue `RunEvent::Exit`: set `quitting = true`; send `Shutdown(ack)`; `ack_rx.recv_timeout(6 s)`; then let
the process exit. The timeout bounds the case where the controller is stuck in a modal dialog.

Unexpected session death: `tick()` → if `Ready(Session)` and `child.try_wait()` is `Some(code)` →
remove pidfile → `Failed(SessionExited{code, log_tail})` (glue navigates back to the status page).

Stale reaper (start of every `Start`): read pidfile → parse `PidRecord` → `sys.cmdline_of(pid)`;
if `Some(cmd)` and `is_our_sidecar(&cmd, &rec)` → `signal(Interrupt)`, poll `cmdline_of` up to
`STOP_GRACE`, then `signal(Kill)`. Always remove the pidfile afterward. Identity check (guards PID
reuse) = cmdline contains `peakbot` AND (`--bind 127.0.0.1:<port>` OR `--bind 0.0.0.0:<port>` — remote sessions, #377) (basename-only on purpose: an
AppImage mount path is gone after a crash).

### 4.7 Readiness (`core::ready`)
```
loop:
  if quitting                         → Cancelled
  if let Some(child) and child exited → ChildExited(code)
  match sys.probe(url):  Status(200) → Ready;  Listening → Ready (https only, #377);  Status(401|403) → TokenRejected;  other/Unreachable → continue
  if sys.now() - start >= timeout     → Timeout
  sys.sleep(POLL_INTERVAL)
```
`Probe::Listening` (a bare TCP connect, https URLs only) counts as Ready because core's `ureq` has no TLS — until #374 swaps it for a CA-verified GET. Cost: no 401 detection over https meanwhile (R15).
`classify_early_exit(code, log_tail)`: log tail contains `already in use` → `PortTaken`, else `Crashed`.
(The TOCTOU window between `free_port()` and the child's bind is closed by this retry.)

### 4.8 Webview navigation
- Window `main` starts on the local page (`WebviewUrl::App("index.html")`).
- Glue `phase_changed`: `Ready(mode)` → set allowed backend origin = `mode.origin()`, `navigate(mode.url())`.
  `Starting`/`Failed` → clear allowed origin; if current URL is not the local page, `navigate(local_url)`.
- `on_navigation(url)` → `classify_navigation(url, allowed_origin)`:
  local app origin (`tauri://localhost`, `http://tauri.localhost`, `https://tauri.localhost`) → Allow;
  same origin as allowed backend → Allow; other `http(s)`/`mailto` → OpenExternal (opener) + block;
  anything else (`file:`, `data:`, `javascript:` …) → Block.
- `on_new_window(url, _)` → same classifier, but Allow is treated as OpenExternal; always
  `NewWindowResponse::Deny` (no popups inside the app).
- (amended #377) Reserved URL `peakbot-desktop:settings` → `NavDecision::OpenSettings` (wins over the allowed backend origin): the navigation/new-window is blocked and `window::request_settings` opens the single `settings` window (title `PeakBot Settings`, 520×600, min 420×480, `WebviewUrl::App("settings.html")`; a second trigger focuses it). Entry points: tray / macOS PeakBot submenu item `settings` (macOS accelerator `CmdOrCtrl+Shift+S`), the main window's init-script `keydown` (Ctrl/Cmd+Shift+S → `location.href = sentinel`, works on every origin incl. the remote UI, no IPC), and a status-page footer link. `request_settings` spawns on the async runtime — never build a window inside a nav/menu callback. The settings window's own nav guard allows the local origin only (I6).

### 4.9 Environment for every sidecar invocation
`sanitize_env(parent)` (pure): if `APPDIR` is set, drop `APPDIR, APPIMAGE, ARGV0, OWD`; for
`PATH, LD_LIBRARY_PATH, XDG_DATA_DIRS, GST_PLUGIN_SYSTEM_PATH, GST_PLUGIN_PATH, GIO_EXTRA_MODULES`
remove `:`-entries under `$APPDIR` (drop the var if nothing remains); drop any *other* var whose
value contains `$APPDIR`. No `APPDIR` → env unchanged. Rationale: the agent spawns user shells;
AppImage plumbing must not leak into them. Session spawn adds `PEAKBOT_WEB_TOKEN`, `PEAKBOT_NO_OPEN=1`;
short-lived commands add `PEAKBOT_NO_OPEN=1` only.

### 4.10 Linux webkit default
First statement of `main()` (before any thread): if Linux and `WEBKIT_DISABLE_DMABUF_RENDERER` is
unset → set it to `1` (`unsafe { std::env::set_var }`, edition 2024). Decision fn
`core::env::webkit_dmabuf_default(existing: Option<&OsStr>) -> Option<(&'static str, &'static str)>`.

### 4.11 Saving settings (`Action::SaveSettings`) — amendment #377

**`SaveSettings(new)`** (controller thread; ignored while `Starting`, like its siblings):
```
current = load_settings(read(settings_file))
new == current                       → return (nothing to apply; no dialog)
service_mode = self.service is Some(Installed(_))
confirm(service_mode ? settings_reinstall_dialog() : settings_relaunch_dialog())
    Cancel                           → return (nothing written, nothing stopped)
write_private(settings_file, render_settings_file(new))
    Err(e)                           → message(save_failed_dialog(path, e)); return
service_mode ? reconfigure_service() : handle_start()
```
- **Session = `handle_start()`.** The existing, fully tested "stop child → reap → version →
  status → plan → launch" path; the plan is `Session` (no service installed) →
  LAUNCH_SESSION with the new settings.
- **Write-then-apply.** The file is the desired state. If the apply fails, the app ends in
  `Failed(…)` with Retry/Reinstall, and both of those re-apply the new file. No rollback
  machinery.

**LAUNCH_SESSION (changes only):**
```
s = settings(); token = sync_token(s)           // once, before the port loop
spawn args = session_args(port, s)              // ["--bind","{bind_host}:{port}"] + ["--tls"]?
      env  = session_env(token.as_ref())        // [("PEAKBOT_WEB_TOKEN",t)]? + ("PEAKBOT_NO_OPEN","1")
Session { child, port, protocol: s.protocol(), token }  →  Mode::Session{port, protocol, token}
wait_ready(mode.probe_url())  // https ⇒ Listening counts as Ready
```
`sync_token(s) -> Option<Token>` (private; replaces `ensure_token`):
- `s.token()`: if `Token::parse(read(web-token))` is `Some(t)` → `Some(t)`; else generate →
  `write_private` (0600) → `Some(new)`. A write error logs a warning and still returns
  `Some(new)` — the session uses the env token.
- `!s.token()`: `remove(web-token)`; an error logs a warning → `None`.

**Service flows** (all use `service_install_args(settings())`):
- **`reconfigure_service()`** (SaveSettings in service mode). The binary is NOT replaced, so
  a kept older/newer installed version stays as it is:
  ```
  Starting(RemovingService):   run service uninstall            (non-zero tolerated)
  Starting(InstallingService): sync_token(s)
                               run service install --bind {host}:7823 [--tls]   non-zero → Failed(ServiceCommandFailed)
  refresh_service(); connect_service(false)
  ```
  Ordering matters: `sync_token` runs before `service install` — a remote plan needs the
  file (the backend's `build_plan_for_install` reads it); token off removes the file before
  the restarted service reads it.
- **`replace_service()`**: unchanged except `ensure_token(None)` → `sync_token(settings())`
  and the new args.
- **`enable_background()`**: unchanged except `ensure_token(session_token)` →
  `sync_token(settings())` and the new args (the `preferred` parameter is deleted); the
  session already uses the file token, so the webview token stays stable across the switch
  with no extra code.
- **`connect_service(start_first)`**: `protocol = Https` iff
  `self.service == Some(Installed{tls: true, ..})`, else `Http` (the `TLS:` status line,
  invariant I3 — never `settings.json`); the token is still read-only from the file (never
  generated); `start_first` uses `service_install_args(settings())`.

---

## 5. INTERFACES / CONTRACTS

### 5.1 `core::sys`
```rust
pub struct RunOutput { pub code: Option<i32>, pub stdout: String, pub stderr: String }
impl RunOutput { pub fn ok(&self) -> bool; /* code == Some(0) */ pub fn combined(&self) -> String; }

pub enum Probe { Status(u16), Listening, Unreachable }   // Listening (#377): TCP accepted, no HTTP spoken — https URLs only, until #374
pub enum Signal { Interrupt, Kill }

pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub extra_env: Vec<(String, String)>,   // added on top of the sanitized base env
    pub log: PathBuf,                       // truncated, 0600, receives stdout+stderr
}

pub trait Child: Send {
    fn pid(&self) -> u32;
    fn try_wait(&mut self) -> std::io::Result<Option<Option<i32>>>; // Some(exit) once exited; exit None = killed by signal
    fn interrupt(&mut self);   // unix: kill(pid, SIGINT); windows: kill()
    fn kill(&mut self);        // SIGKILL / TerminateProcess; errors ignored (already dead)
}

pub trait Sys: Send {
    fn run(&self, program: &Path, args: &[&str]) -> std::io::Result<RunOutput>; // RUN_TIMEOUT; NotFound if program missing
    fn spawn(&self, spec: SpawnSpec) -> std::io::Result<Box<dyn Child>>;
    fn probe(&self, url: &url::Url) -> Probe;                 // GET, PROBE_TIMEOUT, no redirects followed
    fn free_port(&self) -> std::io::Result<u16>;              // bind 127.0.0.1:0, read port, drop
    fn port_is_free(&self, port: u16) -> bool;                // bind 127.0.0.1:port succeeds
    fn random_token(&self) -> Token;
    fn read(&self, path: &Path) -> std::io::Result<Option<String>>;  // Ok(None) if NotFound
    fn write_private(&self, path: &Path, contents: &str) -> std::io::Result<()>; // mkdir -p, tmp+rename, 0600
    fn remove(&self, path: &Path) -> std::io::Result<()>;     // NotFound = Ok
    fn exists(&self, path: &Path) -> bool;
    fn cmdline_of(&self, pid: u32) -> Option<String>;         // `ps -o command= -p <pid>`; None if not alive
    fn signal(&self, pid: u32, sig: Signal);
    fn sleep(&self, d: Duration);
    fn now(&self) -> std::time::Instant;
}

pub struct RealSys { base_env: Vec<(OsString, OsString)> }   // = sanitize_env(std::env::vars_os()) at construction
impl RealSys { pub fn new() -> RealSys; }
```
`RealSys::run`: `env_clear().envs(base_env).env("PEAKBOT_NO_OPEN","1")`, stdin null, stdout/stderr
piped and drained on two reader threads, `try_wait` poll until `RUN_TIMEOUT` then kill →
`Err(io::ErrorKind::TimedOut)`. `probe` uses `ureq` (no TLS features) for `http` URLs; for `https` URLs it is `TcpStream::connect_timeout(127.0.0.1:port, PROBE_TIMEOUT)` → `Listening` / `Unreachable` (amended #377).

### 5.2 Pure functions (each unit-tested)
```rust
// version
pub fn parse_version_output(stdout: &str) -> Option<semver::Version>;
// service
pub fn status_args() -> [&'static str; 2];            // ["service","status"]
pub fn service_install_args(s: Settings) -> Vec<String>;   // ["service","install","--bind","{host}:7823"] + ["--tls"]? (amended #377)
pub fn service_uninstall_args() -> [&'static str; 2]; // ["service","uninstall"]
pub fn install_args() -> [&'static str; 1];           // ["install"]
pub fn parse_service_status(out: &RunOutput) -> ServiceState;
pub fn plan_launch(state: &ServiceState, version_of: impl FnOnce(&InstalledService) -> InstalledVersion,
                   bundled: &semver::Version, declined: Option<&DeclinedReplace>) -> LaunchPlan;
pub fn replace_dialog(installed: &InstalledVersion, bundled: &semver::Version) -> Dialog;
pub fn enable_dialog() -> Dialog; pub fn disable_dialog() -> Dialog;
pub fn finish_setup_dialog() -> Dialog; pub fn port_busy_dialog() -> Dialog;
pub fn command_failed_dialog(command: &str, output: &str) -> Dialog;
pub fn service_log_hint() -> &'static str;            // per target_os
pub fn settings_relaunch_dialog() -> Dialog; pub fn settings_reinstall_dialog() -> Dialog;   // #377
pub fn save_failed_dialog(path: &Path, err: &str) -> Dialog;
// session
pub fn session_args(port: u16, s: Settings) -> Vec<String>;        // ["--bind", "{bind_host}:<port>"] + ["--tls"]? (amended #377)
pub fn session_env(token: Option<&Token>) -> Vec<(String,String)>; // [("PEAKBOT_WEB_TOKEN",tok)]? + ("PEAKBOT_NO_OPEN","1")
pub struct PidRecord { pub pid: u32, pub port: u16 }
impl PidRecord { pub fn render(&self) -> String; pub fn parse(s: &str) -> Option<PidRecord>; }
pub fn is_our_sidecar(cmdline: &str, rec: &PidRecord) -> bool;
pub enum EarlyExit { PortTaken, Crashed }
pub fn classify_early_exit(code: Option<i32>, log_tail: &str) -> EarlyExit;
pub fn tail_lines(text: &str, n: usize) -> String;
// ready
pub enum ReadyOutcome { Ready, ChildExited(Option<i32>), Timeout, TokenRejected, Cancelled }
pub fn wait_ready(sys: &impl Sys, url: &url::Url, timeout: Duration,
                  child: Option<&mut dyn Child>, quitting: &AtomicBool) -> ReadyOutcome;
// env / nav / paths
pub fn sanitize_env(parent: impl IntoIterator<Item=(OsString,OsString)>) -> Vec<(OsString,OsString)>;
pub fn webkit_dmabuf_default(existing: Option<&OsStr>) -> Option<(&'static str, &'static str)>;
pub enum NavDecision { Allow, OpenExternal, OpenSettings, Block }   // OpenSettings: #377
pub fn classify_navigation(url: &url::Url, backend_origin: Option<&str>) -> NavDecision;
// phase
pub fn status_view(phase: &Phase) -> StatusView;
pub fn tray_model(phase: &Phase, service: Option<&ServiceState>, bundled: Option<&semver::Version>) -> TrayModel;
```

**`parse_service_status` contract** (input = stdout of peakbot's `print_service_report`, pinned by backend task B5):
non-zero exit → `Unavailable{reason: first non-empty line of stderr, else stdout, else "service status failed"}`;
line `Installed: true` → Installed, `Installed: false` → NotInstalled, missing → `Unavailable{"unrecognised service status output"}`;
`Exec:   <path>` (value trimmed) → `exe`; `State: running|stopped|unknown` → RunState (missing/other → Unknown); `TLS:       on` → `tls = true`, `off`/missing → false (#375/#377).

**Dialog texts** (locked; `{b}` bundled, `{i}` installed or `an unknown version`):
- replace: title `Replace PeakBot background service?`; body `A PeakBot background service ({i}) is already installed. This app includes PeakBot {b}.\n\nReplace the service with {b}? This also replaces ~/.local/bin/peakbot (your "peakbot" command).` + if Known(i) > b: `\n\nWarning: {b} is OLDER than the installed {i}.`; ok `Replace`, cancel `Keep {i}` (Unknown → `Keep current`).
- enable: `Keep PeakBot running in background?` / `PeakBot will be installed to ~/.local/bin/peakbot and start automatically when you log in. It keeps running after you close this window.` / `Enable` / `Cancel`.
- disable: `Remove background service?` / `PeakBot will stop running in the background and will only run while this window is open. ~/.local/bin/peakbot and your settings are kept.` / `Remove` / `Cancel`.
- finish setup: `Finish setup first` / `Complete the PeakBot setup in this window, then enable background mode.` / `OK`.
- port busy: `Port 7823 is busy` / `Another program (maybe a PeakBot started from a terminal) is using 127.0.0.1:7823. Stop it and try again.` / `OK`.
- command failed: `Background service error` / `$ {command}\n{output}` / `OK`.
- settings relaunch (#377): `Apply settings?` / `PeakBot will restart with the new settings. Conversations that are running now will be interrupted.` / `Restart` / `Cancel`.
- settings reinstall (#377): `Apply settings?` / `The background service will be reinstalled with the new settings and restarted. Conversations that are running now will be interrupted.` / `Reinstall` / `Cancel`.
- save failed (#377): `Couldn't save settings` / `{path}\n{err}` / `OK`.

### 5.3 Controller
```rust
pub trait Ui: Send {
    fn phase_changed(&self, phase: &Phase, tray: &TrayModel);
    fn confirm(&self, d: &Dialog) -> bool;  // ok → true
    fn message(&self, d: &Dialog);
}
pub struct Controller<S: Sys, U: Ui> { /* sys, ui, paths, quitting, phase, bundled: Option<Version>,
                                          service: Option<ServiceState>, session: Option<Session{child,port,token}> */ }
impl<S: Sys, U: Ui> Controller<S, U> {
    pub fn new(sys: S, ui: U, paths: DesktopPaths, quitting: Arc<AtomicBool>) -> Self; // phase = Starting(CheckingBundled)
    pub fn handle(&mut self, action: Action) -> std::ops::ControlFlow<()>;            // Break after Shutdown
    pub fn tick(&mut self);
    pub fn phase(&self) -> &Phase;
}
pub fn run_loop<S: Sys, U: Ui>(ctrl: Controller<S, U>, rx: std::sync::mpsc::Receiver<Action>);
```
Every phase assignment goes through one private `set_phase(p)` which calls `ui.phase_changed(&p, &tray_model(..))`.
Same for every `service` refresh (so the tray checkbox always mirrors the last `service status`).

### 5.4 Tauri glue (`src-tauri`)

`main.rs`
```rust
fn main() {
    // 1. webkit_dmabuf_default → set_var (Linux), before anything else
    // 2. tauri::Builder::default()
    //      .plugin(tauri_plugin_single_instance::init(|app,_,_| window::focus_main(app)))  // FIRST
    //      .plugin(tauri_plugin_log::Builder::new().target(LogDir{file_name:Some("desktop")}).target(Stdout).level(Info).build())
    //      .plugin(tauri_plugin_window_state::Builder::default().build())
    //      .plugin(tauri_plugin_opener::init())
    //      .plugin(tauri_plugin_dialog::init())
    //      .invoke_handler(tauri::generate_handler![commands::status, commands::act, commands::settings_get, commands::settings_set])
    //      .setup(|app| { paths; window::create_main; tray::create; spawn controller thread; send Action::Start })
    //      .build(ctx)?.run(|app, ev| match ev { RunEvent::Exit => shutdown(app), RunEvent::Reopen{..} => focus_main, _ => {} })
}
```
- `window.rs`: `create_main(app, nav: Arc<RwLock<Option<String>>>)` — label `main`, title `PeakBot`, 1200×800, min 800×500, `WebviewUrl::App("index.html")`, `.on_navigation(...)`, `.on_new_window(...)`; `CloseRequested` → `app.exit(0)` (closing the window quits the app on every OS). `focus_main(app)` = show + unminimize + set_focus.
- `tray.rs`: `create(app)` builds menu; `apply(app, &TrayModel)` updates text/checked/enabled (always re-applied after a click because muda auto-toggles check items). On macOS the same submenu `PeakBot` is appended to `Menu::default(app)` as the app menu.
- `menu.rs` (macOS only, issue #373): `app_menu(app)` is the Tauri default menu **minus "Close Window"** (File submenu dropped, Window = minimize/maximize only), with the `PeakBot` app submenu (ids as above) first; set via `builder.menu(menu::app_menu)` in `main.rs` so Cmd+W reaches the page and the in-app tab bar closes a conversation tab instead of quitting. Never on Linux (no GTK menubar).
- `bridge.rs`: `struct TauriUi { app: AppHandle, snapshot: Arc<Mutex<StatusView>>, nav: Arc<RwLock<Option<String>>> }` implements `core::Ui` — dialogs via `app.dialog().message(..).title(..).buttons(MessageDialogButtons::OkCancelCustom(ok,cancel) | OkCustom(ok)).blocking_show()`; `phase_changed` updates snapshot, nav origin, navigates, `tray::apply`.
- `commands.rs`:
  ```rust
  #[tauri::command] fn status(state: State<Snapshot>) -> StatusView;
  #[tauri::command] fn act(action: UiAction, app: AppHandle, tx: State<ActionTx>) -> Result<(), String>;
  // Retry→Action::Start, ReinstallService→Action::ReinstallService, OpenLogs→opener.open_path(log_dir), Quit→app.exit(0),
  // ToggleBackground→Action::ToggleBackground (#377)
  #[tauri::command] fn settings_get(state: State<UiState>, paths: State<DesktopPaths>) -> SettingsView;   // read-only file + tray/status snapshot (#377)
  #[tauri::command] fn settings_set(settings: SettingsWire, tx: State<ActionTx>) -> Result<(), String>;   // Settings::try_from → Action::SaveSettings; error text returned verbatim
  ```

**Tray / macOS submenu items (ids locked):**

| id | kind | label | behaviour |
|---|---|---|---|
| `status` | text, disabled | `TrayModel.status` | – |
| `open` | text | `Open PeakBot` | `focus_main` |
| `settings` | text | `Settings…` (macOS submenu: accelerator `CmdOrCtrl+Shift+S`) | `window::request_settings` (#377) |
| — | separator | | |
| `background` | check | `TrayModel.background_label` | `Action::ToggleBackground` |
| `logs` | text | `Open logs folder` | opener `open_path(log_dir)` |
| — | separator | | |
| `quit` | text | `Quit PeakBot Desktop` | `app.exit(0)` |

Tray icon = `app.default_window_icon()`; tooltip `PeakBot`. Left-click on Linux shows the menu (appindicator); on macOS shows the menu.

**`build.rs`** (command ACL — makes app commands require explicit permission):
```rust
tauri_build::try_build(tauri_build::Attributes::new()
    .app_manifest(tauri_build::AppManifest::new().commands(&["status", "act", "settings_get", "settings_set"])))
```
**`capabilities/status-page.json`**:
```json
{ "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "status-page",
  "description": "Bundled local status page only. No remote origins, ever.",
  "windows": ["main"],
  "permissions": ["allow-status", "allow-act"] }
```
**`capabilities/settings-page.json`** (#377):
```json
{ "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "settings-page",
  "description": "Bundled local settings window only. No remote origins, ever.",
  "windows": ["settings"],
  "permissions": ["allow-settings-get", "allow-settings-set", "allow-act"] }
```
No `remote` key ⇒ the capabilities apply to the bundled origin only; the PeakBot web UI origin
(`http(s)://127.0.0.1:*`) has **zero** permissions (no core, no plugins, no app commands).

| Command | Granted to | Capability file |
|---|---|---|
| `status`, `act` | window `main`, local origin | `status-page.json` |
| `settings_get`, `settings_set`, `act` | window `settings`, local origin | `settings-page.json` |
| anything | remote PeakBot origin | none |

**`tauri.conf.json`** (locked keys):
```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "PeakBot Desktop",
  "mainBinaryName": "peakbot-desktop",
  "identifier": "com.peakbot.desktop",
  "build": { "frontendDist": "../ui" },
  "app": {
    "windows": [],
    "withGlobalTauri": true,
    "security": {
      "csp": "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:",
      "capabilities": ["status-page", "settings-page"]
    }
  },
  "bundle": {
    "active": true,
    "targets": ["appimage"],
    "externalBin": ["binaries/peakbot"],
    "category": "DeveloperTool",
    "shortDescription": "PeakBot desktop app",
    "icon": ["icons/32x32.png","icons/128x128.png","icons/128x128@2x.png","icons/icon.icns","icons/icon.ico"],
    "linux": { "appimage": { "bundleMediaFramework": false } },
    "macOS": { "minimumSystemVersion": "11.0" }
  }
}
```
- No `version` key → Tauri uses the crate version (workspace version) — single source in `desktop/Cargo.toml`, kept equal to root by `release-bump` + `check-version.sh`.
- No `devUrl`, no `beforeDevCommand`/`beforeBuildCommand` (nothing to build for the UI).
- `windows: []` because the window must be built in Rust to attach `on_navigation`/`on_new_window`.
- `capabilities` lists every capability file explicitly — **a capability file missing from this list is silently inactive** (#377).
- `withGlobalTauri: true` so `status.js` / `settings.js` can call `window.__TAURI__.core.invoke` with no build step. The object is also visible on the remote page but inert (ACL denies everything) — verified in the manual checklist.

**Custom-protocol (devUrl vs embedded) — addressed:** tauri's build script sets `cfg(dev)` iff the
`tauri/custom-protocol` feature is OFF. `tauri build` (Linux) enables it automatically. The macOS
plain-cargo build MUST pass `--features tauri/custom-protocol` (in `Dockerfile.desktop-macos`,
locked). Since there is no `devUrl`, even a dev build embeds `ui/`, so a mistake degrades to a
dev-flavoured binary, not a blank window.

### 5.5 Cargo manifests (pins; `Cargo.lock` committed, all builds `--locked`)

`desktop/Cargo.toml`
```toml
[workspace]
members = ["core", "src-tauri"]
resolver = "3"
[workspace.package]
version = "0.23.0"          # == root Cargo.toml version (release-bump + check-version.sh)
edition = "2024"
[profile.test]
debug = false
```
`desktop/core/Cargo.toml`
```toml
[package]  name = "peakbot-desktop-core"  version.workspace = true  edition.workspace = true  publish = false
[dependencies]
semver = "1.0.28"
url = "2.5"
ureq = { version = "3.4.2", default-features = false }
getrandom = "0.3.4"
directories-next = "2"      # same crate as peakbot's get_config_dir
serde = { version = "1", features = ["derive"] }
serde_json = "1"            # settings.json (#377; was dev-only)
log = "0.4"
[target.'cfg(unix)'.dependencies]
libc = "0.2.190"
[dev-dependencies]
tempfile = "3.27"
```
`desktop/src-tauri/Cargo.toml`
```toml
[package]  name = "peakbot-desktop"  version.workspace = true  edition.workspace = true  publish = false
[build-dependencies]
tauri-build = { version = "2.7.3", features = [] }
[dependencies]
peakbot-desktop-core = { path = "../core" }
tauri = { version = "2.12.4", features = ["tray-icon"] }
tauri-plugin-single-instance = "2.5.4"
tauri-plugin-window-state = "2.5.0"
tauri-plugin-opener = "2.7.2"
tauri-plugin-dialog = "2.8.3"
tauri-plugin-log = "2.10.1"
serde = { version = "1", features = ["derive"] }
log = "0.4"
```
Tauri CLI: `npx --yes @tauri-apps/cli@2.12.3` (no `package.json`; pinned on the command line in `build-desktop.sh`/Dockerfile/Makefile).

### 5.6 Status page (`ui/`)
`index.html`: `<!-- peakbot-desktop-status-page -->`, inline `<style>`, elements
`#title`, `#detail` (`<pre>`, hidden when null), `#spinner` (shown iff kind ≠ failed), `#actions`
(container), `<script src="status.js">`. Dark/light via `prefers-color-scheme`. No external resources.
`status.js` (≈40 lines): every 500 ms `invoke('status')` → render; buttons from `actions` with
labels `Retry`, `Open logs`, `Reinstall background service`, `Quit`; click →
`invoke('act', { action: 'retry' | 'open_logs' | 'reinstall_service' | 'quit' })`. If
`window.__TAURI__` is missing, render "Internal error: no Tauri API". (#377) A footer link
`<a href="peakbot-desktop:settings">Settings (Ctrl+Shift+S)</a>` opens the settings window through the nav guard (no IPC).

**Settings page (#377)** — `settings.html` (copies the `:root` theme block of `index.html`; two static pages, no shared CSS) + `settings.js` (no build step):
```
Protocol      (•) HTTP  ( ) HTTPS        #protocol-note (HTTPS only): "PeakBot uses its own certificate. The app window may
                                          show a certificate warning until that certificate is trusted; browsers and phones
                                          work after you accept it once."
Access token  [x] Require access token   #token-note: on → "Stored in {web_token_path}"
                                                       off → "Without a token, any program or web page on this computer can control PeakBot."
Remote access [ ] Allow other devices    "Listens on all network interfaces. Requires HTTPS and the access token."
<p id="error" role="alert">  <p id="state">  [Save]
---
Background    [ ] Keep PeakBot running in background   "Applies immediately."
```
Lifecycle: `settings_get` on start, then polled every 1000 ms (refreshes `saved`, the background row and `busy`; **never overwrites a dirty form**). Remote on ⇒ force HTTPS + token on and disable both controls (UX only — core validates); off ⇒ re-enable, keeping values. Save is enabled iff `dirty && !busy` → `settings_set`; `Err(msg)` is shown verbatim in `#error`, `Ok` shows `Applying…` until the file reflects the values. The background checkbox is enabled iff `background_enabled && !dirty && !busy` → `act(toggle_background)`; the existing enable/disable dialogs and flows run unchanged.

### 5.7 Build contracts
- **Sidecar into `binaries/`:** `build-desktop.sh` first builds peakbot with the *existing*
  `Dockerfile.linux` / `Dockerfile.macos` (unversioned → `output/peakbot-linux-amd64`,
  `output/peakbot-macos-universal2`), then the desktop Dockerfiles `COPY` them into
  `desktop/src-tauri/binaries/peakbot-<triple>`: Linux → `peakbot-x86_64-unknown-linux-gnu`;
  macOS → the universal binary copied to BOTH `peakbot-aarch64-apple-darwin` and
  `peakbot-x86_64-apple-darwin` (tauri-build checks per real target triple; zigbuild universal2
  compiles each arch separately). One peakbot build recipe, reused — no duplicated stages.
  Gitignore: `desktop/src-tauri/binaries/peakbot-*`, `desktop/target/`, `desktop/src-tauri/gen/`.
- **Dev loop:** `make desktop-dev` = `cargo build` (root) → copy `target/debug/peakbot` to
  `binaries/peakbot-$(rustc -vV | sed -n 's/host: //p')` → `npx @tauri-apps/cli@2.12.3 dev` in `desktop/src-tauri`.
- **`Dockerfile.desktop-linux`:** `FROM ubuntu:22.04`; apt: `build-essential pkg-config
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
  xdg-utils file xz-utils curl ca-certificates binutils`; Node 22 (nodesource); rustup
  `--default-toolchain 1.91.0 --profile minimal`. `COPY desktop/ /src/desktop`; `ARG SIDECAR=output/peakbot-linux-amd64`;
  `COPY ${SIDECAR} …/binaries/peakbot-x86_64-unknown-linux-gnu`; `chmod 755`; `sh scripts/check-version.sh`;
  `npx --yes @tauri-apps/cli@2.12.3 build --ci --bundles appimage` (in `src-tauri`, cargo gets `--locked` via
  `-- --locked`); copy to `/out/PeakBot-Desktop-${VERSION}-linux-x86_64.AppImage`;
  `RUN sh scripts/smoke-linux.sh /out/*.AppImage ${VERSION}`; `FROM scratch` + `COPY --from=build /out/ /`.
- **`Dockerfile.desktop-macos`:** `FROM ghcr.io/rust-cross/cargo-zigbuild:0.23.3`; `rustup target add
  aarch64-apple-darwin x86_64-apple-darwin`; apt: `zip unzip file`; rcodesign:
  `https://github.com/indygreg/apple-platform-rs/releases/download/apple-codesign/0.29.0/apple-codesign-0.29.0-x86_64-unknown-linux-musl.tar.gz`,
  sha256 **`dbe85cedd8ee4217b64e9a0e4c2aef92ab8bcaaa41f20bde99781ff02e600002`** (verified 2026-10-16) →
  `/usr/local/bin/rcodesign`. `ENV RUSTFLAGS="-C link-arg=-Wl,-headerpad,0x4000" MACOSX_DEPLOYMENT_TARGET=11.0`;
  `cargo zigbuild --release --locked -p peakbot-desktop --features tauri/custom-protocol --target universal2-apple-darwin`;
  `sh macos/assemble-app.sh ${VERSION} target/universal2-apple-darwin/release/peakbot-desktop src-tauri/binaries/peakbot-aarch64-apple-darwin /out`;
  `RUN sh scripts/smoke-macos.sh /out/PeakBot-Desktop-${VERSION}-macos-universal.zip ${VERSION}`; scratch export.
  (Fallback if zigbuild `universal2` fails with Tauri: the spike recipe — two single-arch builds + `rcodesign macho-universal-create`.)
- **`macos/assemble-app.sh <version> <desktop-bin> <sidecar-bin> <outdir>`:**
  ```
  PeakBot Desktop.app/Contents/Info.plist          (Info.plist.in with @VERSION@ substituted)
  PeakBot Desktop.app/Contents/MacOS/peakbot-desktop   (755)
  PeakBot Desktop.app/Contents/MacOS/peakbot           (755)
  PeakBot Desktop.app/Contents/Resources/icon.icns
  ```
  then `rcodesign sign "<app>"` (ad-hoc: no cert args; signs nested Mach-O in `Contents/MacOS` and the bundle),
  then `(cd outdir && zip -qry "PeakBot-Desktop-<v>-macos-universal.zip" "PeakBot Desktop.app")` (`-y` keeps symlinks; Info-ZIP stores unix modes).
- **`Info.plist.in` keys:** `CFBundleDevelopmentRegion=en`, `CFBundleExecutable=peakbot-desktop`,
  `CFBundleIdentifier=com.peakbot.desktop`, `CFBundleName=PeakBot Desktop`, `CFBundleDisplayName=PeakBot`,
  `CFBundlePackageType=APPL`, `CFBundleShortVersionString=@VERSION@`, `CFBundleVersion=@VERSION@`,
  `CFBundleIconFile=icon`, `CFBundleInfoDictionaryVersion=6.0`, `LSMinimumSystemVersion=11.0`,
  `NSHighResolutionCapable=true`, `LSApplicationCategoryType=public.app-category.developer-tools`,
  `NSAppTransportSecurity → NSAllowsLocalNetworking=true` (http://127.0.0.1 in WKWebView).
- **Artifacts:** `output/desktop/PeakBot-Desktop-<v>-linux-x86_64.AppImage`, `output/desktop/PeakBot-Desktop-<v>-macos-universal.zip`.
- **`scripts/build-desktop.sh <linux|macos|all>`**: reads version from root `Cargo.toml` (same awk as `release-bump`);
  `CONTAINER_BUILDER` (default podman, else docker); builds sidecar Dockerfile then desktop Dockerfile with
  `--build-arg VERSION=<v> --output type=local,dest=output/desktop`; if `PRUNE=1` runs `$CONTAINER_BUILDER builder prune -af` between builds (CI disk).
- **Makefile:** `desktop-linux`, `desktop-macos`, `desktop` (both) → `sh desktop/scripts/build-desktop.sh …`; `desktop-dev`; `desktop-test` → `cargo test --locked --manifest-path desktop/Cargo.toml -p peakbot-desktop-core`. Add to `.PHONY` and `## help` lines.
- **`.gitea/workflows/desktop.yml`:** `on: workflow_dispatch` + `push: tags: ['*']`; one job `runs-on: dind`,
  no `container:` (needs host docker), `timeout-minutes: 120`; steps: pure-git checkout (same as `ci.yml`) →
  `PRUNE=1 CONTAINER_BUILDER=docker sh desktop/scripts/build-desktop.sh all` → `actions/upload-artifact@v3`
  (`output/desktop/*`, Gitea doesn't support v4) → if `startsWith(github.ref, 'refs/tags/')`:
  `docker run --rm -e GITEA_URL -e REPO -e TAG -e TOKEN -v "$PWD":/w -w /w alpine:3.20 sh -c 'apk add --no-cache curl jq >/dev/null && sh desktop/scripts/attach-release.sh'`
  with `TOKEN: ${{ secrets.RELEASE_TOKEN }}`.
- **`attach-release.sh`:** GET `/api/v1/repos/{owner}/{repo}/releases/tags/{tag}`; on 404 retry every 30 s up to 20 min
  (`make release` creates the release after pushing the tag); delete existing assets with the same name (idempotent re-runs);
  POST each file to `/releases/{id}/assets?name=…`.
- **`ci.yml` (PR):** add step `Desktop core` → `cargo fmt --manifest-path desktop/Cargo.toml -p peakbot-desktop-core -- --check`,
  `cargo clippy --locked --manifest-path desktop/Cargo.toml -p peakbot-desktop-core --all-targets -- -D warnings`,
  `cargo test --locked --manifest-path desktop/Cargo.toml -p peakbot-desktop-core`. (Glue is compiled only in desktop.yml — needs webkit.)

### 5.8 README (manual removal section, locked content)
Linux: `peakbot service uninstall` (or `systemctl --user disable --now peakbot.service; rm ~/.config/systemd/user/peakbot.service; systemctl --user daemon-reload`);
optional `rm ~/.local/bin/peakbot`, `rm -r ~/.local/share/com.peakbot.desktop`; settings `~/.config/peakbot` are user data — delete only deliberately.
macOS: `~/.local/bin/peakbot service uninstall` (or `launchctl bootout gui/$(id -u)/com.peakbot.agent; rm ~/Library/LaunchAgents/com.peakbot.agent.plist`);
optional `rm ~/.local/bin/peakbot`, `rm -r ~/Library/Application\ Support/com.peakbot.desktop ~/Library/Logs/com.peakbot.desktop`.
First launch on macOS: `xattr -dr com.apple.quarantine "/Applications/PeakBot Desktop.app"` (unsigned build).
Note: "Before deleting the app, untick *Keep PeakBot running in background* if you don't want PeakBot to keep running."

---

## 6. KEY TRADEOFFS

| Decision | Chosen | Rejected | Why |
|---|---|---|---|
| UI delivery | navigate webview to `http://127.0.0.1:<port>/?token=` | bundle web/ into Tauri | Researcher: UI is strictly same-origin; zero frontend/CORS changes. |
| Mode storage | derived from `service status` | settings file | Single source of truth; CLI changes are reflected automatically. (#377: `settings.json` stores launch *parameters* only, never mode.) |
| Sidecar spawning | `std::process::Command` | tauri-plugin-shell | Need SIGINT, env control, no JS surface. `externalBin` bundling works without the plugin. |
| Token delivery | `PEAKBOT_WEB_TOKEN` env | `--token` argv | Human decision 1; argv is visible in `ps`. |
| Service token | desktop writes `web-token` 0600 itself | `service install --token` | Same file peakbot reads; keeps the secret out of argv. |
| Installed-service version | `<Exec> --version` | `/api/setup` exe_path | Works when service is stopped/broken/tokenless; no extra HTTP contract (§4.4). |
| Replace | `service uninstall` → `install` → `service install` | re-run `service install` | `enable --now` doesn't restart a running unit (old binary would keep running). |
| Session port | 7824 if free, else random | always random | Researcher: `--bind :0` unusable; stable origin keeps webview localStorage/cookies across launches. |
| Port race | retry ≤3 on "already in use" | lock/handshake | Backend exits 1 with that message (Researcher) — cheap, deterministic. |
| Orphan handling | pidfile reaper on next launch | PR_SET_PDEATHSIG | Portable, one mechanism; PDEATHSIG is Linux-only and thread-lifetime-subtle. |
| Logic placement | separate `core` crate | `src-tauri/src/core` module | tauri-build fails without the sidecar file and needs webkit; `core` tests run in the existing `rust:1.95` CI container. |
| Concurrency | one controller thread + channel | async tasks + Mutex | Child process + phase have one owner; no locks; blocking dialogs are legal there. |
| Readiness | `GET /commands?token=` 200 | parse stdout URL | Researcher-verified; 401 distinguishes "wrong server/token". |
| macOS arch | universal zip | per-arch zips | One download; mirrors existing `peakbot-macos-universal2`; zigbuild `universal2` already proven in `Dockerfile.macos`. |
| macOS min OS | 11.0 | 13.0 (spike) | Matches the SDK 11.3 in the zigbuild image and widens reach; Tauri needs ≥10.15. Untested either way (R1). |
| macOS bundling | hand-assembled `.app` + rcodesign ad-hoc | Tauri bundler | Tauri CLI cannot bundle `.app` on Linux (Researcher); no mac runner. |
| Window close | quits the app (all OSes) | hide to tray | Session lifetime = window lifetime is the least surprising; service mode keeps running by design. |
| Linux menu | tray only | in-window menubar | Menubar costs UI space on every launch for a GNOME-without-appindicator edge case (R6). |
| Remote page IPC | none (ACL) + `withGlobalTauri` | `__TAURI_INTERNALS__` hack | Stable API, no build step; remote calls are denied by capabilities. |
| Token on/off (#377, T1) | "token on" ⇔ `web-token` file exists; the session uses the file's token | fresh env token every launch, env unset for "off" | The backend token chain is flag > env > **file**, so "off" silently does nothing while the file exists (always true after background mode was used once). A rotating, never-shown remote token is useless for a phone. Cost: tokens no longer rotate per launch (R12 amended); the first launch creates `web-token`. |
| Settings model (#377, T2) | `Settings` sum type + `TryFrom<SettingsWire>` at the IPC edge | 3 bools + `validate()` in the controller | Illegal states are unrepresentable; one check, at the edge. |
| https readiness (#377, T3) | `Probe::Listening` (TCP connect) | real TLS GET now | Core `ureq` has no TLS, so every https probe would be `Unreachable` and every HTTPS launch would die after 30 s. A TCP connect needs no deps and sends no token to an unverified peer; #374 replaces it with a CA-verified GET. |
| Service protocol (#377, T4) | from the `TLS:` status line | from `settings.json` | Facts come from peakbot (§1); a CLI-modified service still connects. |
| Settings shortcut (#377, T8) | macOS accelerator + init-script keydown → reserved URL | global-shortcut plugin; Linux menubar; IPC from the remote origin | Global grabs surprise users and break on Wayland; a menubar was rejected (R6); remote IPC would break I6. A reserved URL needs zero permissions. |
| Build entry | `build-desktop.sh` called by Make AND CI | Make in CI | Runner image tooling unknown; script needs only `sh` + docker. Smoke runs inside build images (all tools present). |

---

## 7. IMPLEMENTATION PLAN

Flags: **[J]** junior-safe (mechanical), **[M]** mid, **[S]** senior judgment. Each task = one PR-sized commit.
Order matters: T-tasks (tests) precede the C-tasks they cover (RED → GREEN).

### Phase 0 — scaffolding
1. **[J] P0.1 gitignore + skeleton.** Root `.gitignore`: add `!desktop/**/*.md`, `desktop/target/`, `desktop/src-tauri/binaries/peakbot-*`, `desktop/src-tauri/gen/`. Create `desktop/Cargo.toml` workspace, empty `core` lib, `src-tauri/binaries/.gitkeep`. (Note: root `.gitignore` ignores `*.md` — without this, this DESIGN.md is untracked.)
2. **[J] P0.2 icons.** `npx --yes @tauri-apps/cli@2.12.3 icon web/public/icon-512.png -o desktop/src-tauri/icons`; commit.
3. **[J] P0.3 version sync.** `scripts/check-version.sh` (exit 1 if `[workspace.package].version` ≠ root `[package].version`); extend `make release-bump` to sed `desktop/Cargo.toml` and run `cargo update --workspace --manifest-path desktop/Cargo.toml`, add both files to the bump commit.

### Phase 1 — backend changes (independent of desktop; separate PRs)
4. **[J] B1 headerpad.** `Dockerfile.macos` builder: `ENV RUSTFLAGS="-C link-arg=-Wl,-headerpad,0x4000"` (so the sidecar can be re-signed by rcodesign).
5. **[M] B2 services never open a browser.** `install::linux::render_unit` adds `Environment=PEAKBOT_NO_OPEN=1` under `[Service]`; `install::macos::render_plist` adds `<key>EnvironmentVariables</key><dict><key>PEAKBOT_NO_OPEN</key><string>1</string></dict>`. Update `tests/install_render_tests.rs`: the "no `Environment=`" assertion becomes "the only `Environment=` line is exactly `Environment=PEAKBOT_NO_OPEN=1`" (secret-free invariant §E.5 preserved); update snapshots. Not a CLI flag (YAGNI): a background service must never open a browser. Windows renderer untouched.
6. **[M] B3 wizard service fix** (`src/ui/web/setup.rs:358-375`): when `install_target()` doesn't exist, run `install_binary()` first and point the service at the target (instead of a non-existent path). Test with the existing setup API test harness. Matters for desktop because the setup wizard runs inside the desktop window.
7. **[J] B4 pin the status contract.** Move `print_service_report` body to `pub fn install::format_service_report(&ServiceReport) -> String` (main prints it); add a unit test asserting line prefixes `Installed: `, `Exec:      `, `State:     `. Comment: "parsed by desktop/core::service::parse_service_status".

### Phase 2 — core (tester first)
8. **[M] T1 tester: RED tests + fakes** for all of §9.1 in `core/src/*` `#[cfg(test)]` modules, plus `core/src/testing.rs` (`#[cfg(test)]`): `FakeSys` (scripted `run` responses keyed by `(program, args)`, per-URL probe script, virtual clock where `sleep` advances `now`, in-memory files, recorded signals/spawns), `FakeChild` (exit after N `try_wait`s or on `interrupt`/never), `FakeUi` (scripted confirm answers, recorded phases/dialogs).
9. **[J] C1** `consts`, `token`, `version`.
10. **[J] C2** `paths`, `env` (`sanitize_env`, `webkit_dmabuf_default`), `nav`.
11. **[M] C3** `service` (args, `parse_service_status`, `plan_launch`, dialogs, log hint).
12. **[J] C4** `session` (args/env, `PidRecord`, `is_our_sidecar`, `classify_early_exit`, `tail_lines`).
13. **[M] C5** `ready::wait_ready`.
14. **[J] C6** `phase` (`Mode` url/origin, `status_view`, `tray_model`, `UiAction` serde).
15. **[S] C7** `controller` + `run_loop` — the flows in §4.3–4.6 exactly; all T1 controller tests green.
16. **[S] C8** `RealSys` (process run with timeout + reader threads, spawn with env_clear/log 0600, SIGINT/SIGKILL via libc, `ps` cmdline, ureq probe, atomic 0600 writes). Plus `#[ignore]`-free integration tests in `core/tests/real_sys.rs` using a shell-script fake sidecar written to a tempdir (unix only): `run` captures stdout/exit; `run` timeout kills; `spawn` + `interrupt` → script's `trap INT` exits 0 within STOP_GRACE; `write_private` mode 0600; `free_port` returns bindable port.
17. **[J] P2.1** CI: add the `Desktop core` step to `.gitea/workflows/ci.yml` (§5.7).

### Phase 3 — Tauri glue
18. **[M] G1** `src-tauri` skeleton: Cargo.toml, `build.rs` (app manifest), `tauri.conf.json`, capability, `main.rs` with plugins, `make desktop-dev` target. App opens a blank local page.
19. **[J] G2** `ui/index.html` + `ui/status.js` (§5.6).
20. **[M] G3** `window.rs` (create_main, nav guard via `classify_navigation`, on_new_window, close → exit, focus_main), `commands.rs` (`status`, `act`).
21. **[M] G4** `tray.rs` (+ macOS app submenu) and `bridge.rs` (`TauriUi`: dialogs, snapshot, navigate, tray apply).
22. **[S] G5** wiring: controller thread, `Action` channel, `quitting` flag, `RunEvent::Exit` shutdown with 6 s ack timeout, `Reopen`, webkit env in `main`. Manual Linux run-through of §9.3 items L1–L12.

### Phase 4 — packaging & CI
23. **[M] P4.1** `Dockerfile.desktop-linux` + `scripts/smoke-linux.sh`.
24. **[S] P4.2** `Dockerfile.desktop-macos` + `macos/Info.plist.in` + `macos/assemble-app.sh` + `scripts/smoke-macos.sh` (zigbuild universal2 with Tauri, rcodesign, fallback recipe if needed).
25. **[J] P4.3** `scripts/build-desktop.sh` + Makefile targets + help lines.
26. **[S] P4.4** `.gitea/workflows/desktop.yml` + `scripts/attach-release.sh`; first verify on the `dind` runner (a throwaway dispatch) that the job has `docker`, `git`, `sh`, node (for upload-artifact) and ≥ 15 GB free; create the `RELEASE_TOKEN` secret (human).
27. **[J] D1** `desktop/README.md` (install, Gatekeeper, background mode, logs locations, manual removal §5.8).
28. **[S] M1 (human, on a Mac)** run §9.4 checklist; file issues.

---

## 8. RISKS & OPEN QUESTIONS

**Risks**
- **R1 macOS runtime is untested** (no Mac, no mac runner). Cross-built Tauri + hand-assembled bundle + ad-hoc signature + Gatekeeper/quarantine + WKWebView ATS on http loopback + tray. Mitigation: smoke checks for layout/arch/signature; human checklist §9.4 before announcing.
- **R2 Gatekeeper/quarantine.** Unsigned zip → "app is damaged". README documents `xattr -dr`. `peakbot install` copies the sidecar to `~/.local/bin` — if the quarantine xattr travels with it, launchd may refuse to run the service. Checklist item MAC9 verifies; mitigation if it fails: backend `install` strips `com.apple.quarantine` on macOS (new backend task, not in v1).
- **R3 macOS GUI PATH.** Apps launched from Finder get a minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`); the agent's shell tools (Homebrew) may be missing in session mode. The launchd service has the same pre-existing limitation. Checklist MAC10 verifies; if confirmed, add login-shell PATH resolution before spawn (e.g. `fix-path-env`) as a follow-up.
- **R4 Tauri on zigbuild universal2** might fail (objc/framework linking per arch). Fallback recipe is specified (two single-arch builds + `rcodesign macho-universal-create`).
- **R5 CI disk (~25 GB)** for 4 container builds. Mitigated by `PRUNE=1`; if insufficient, split into two jobs (linux, macos) with artifacts passed between them.
- **R6 GNOME without AppIndicator** shows no tray → no way to toggle background mode. Accepted for v1; follow-up: Linux in-window menu or a status-page link.
- **R7 Backend-text contract.** `parse_service_status` parses CLI text; B4 pins the labels on the backend side.
- **R8 Two backends on one config.** Session sidecar + a foreground CLI peakbot (or a declined, different-version service) can run simultaneously on the same config/DB. Pre-existing peakbot behaviour; the desktop avoids it only during its own mode switches (stop session before install).
- **R9 Custom-bind CLI services.** A pre-existing service bound elsewhere than 127.0.0.1:7823 → `ServiceUnreachable`; "Reinstall background service" rebinds it to 7823 (desktop owns the service config). Documented in README.
- **R10 `withGlobalTauri` on the remote page.** The API object exists there but has no permissions; checklist L11 verifies denial in a debug build.
- **R11 Desktop killed by SIGTERM/crash** leaves the session sidecar running until the next launch (pidfile reaper). Accepted.
- **R12 Token in `PEAKBOT_WEB_TOKEN` is inherited by agent shells** spawned by peakbot. Same trust domain (the agent can read `web-token` anyway). (Amended #377: the session now uses the persistent `web-token` file's token, T1 — tokens no longer rotate per launch.) Accepted.
- **R13 HTTPS before #376 (#377).** The in-window view shows a certificate error until the webview trusts PeakBot's CA. Accepted; recovery: tray / macOS menu / Ctrl+Shift+S (manual M5 records whether the shortcut survives the WebKit error page) / delete `settings.json` (README).
- **R14 `config.yaml` `web.tls: true` (#377).** The backend ORs it with `--tls`; if set while the desktop says HTTP, the session/service serves https and the desktop probes http → 30 s timeout. Pre-existing (#374 territory); the desktop cannot force http. Documented in the README.
- **R15 No 401 detection over https (#377)** until #374 (`Listening` ≠ authenticated). The window for another TLS process answering on the port is narrow (the child-exit check runs first on each poll).
- **R16 Origin change (#377).** Switching http↔https changes the webview origin, so the web UI's localStorage (tabs, theme) starts fresh. Accepted; conversations live server-side.
- **R17 Behaviour change (#377, T1).** The first launch now creates `web-token`, so a terminal-launched `peakbot` starts requiring the token (it prints/opens a tokened URL, so mostly transparent). Called out in the release notes.
- **R4b Drift (#377).** A CLI `service install` changes the service without touching `settings.json`; connecting still works (I3), the page shows the desktop's desired values, and the next Save re-applies them.
- **R18 Token-off on loopback (#377, #378).** Without a token any local program or web page can drive the agent over `ws://127.0.0.1:<port>/ws` until Origin validation (#378) lands. The settings page says so on the token row.

**Open questions (none block Phase 0–3)**
- **Q1 (human, before P4.4):** does the `dind` runner job (no `container:`) have `docker`, `git`, `sh`, and node for `actions/upload-artifact@v3`? If not, P4.4 adds a `container: docker:27-cli` with the docker socket — requires runner config confirmation.
- **Q2 (human):** create repo secret `RELEASE_TOKEN` (Gitea PAT with `write:repository`), or confirm the built-in `${{ github.token }}` may upload release assets.
- **Q3 (human, optional):** a ≥1024 px square source icon; `web/public/icon-512.png` is used until then.

### Amendment #377 — desktop settings page (2026-10-16)

**Reason.** Users could not change how their backend runs (protocol, token, background, remote) without a terminal (#377). That requires (a) a stored file, contradicting "no settings file", and (b) a loopback session without a token, contradicting "a session is always tokened". User decision: token is mandatory **only** for remote access; remote ⇒ HTTPS ∧ token.

**What changed.** §1/§2 (settings file wording, tree, component rows, "NOT added"), §3.4 (`InstalledService.tls`), §3.5 (`Mode` shape, `UiAction::ToggleBackground`), §3.6 (`settings.json` row; `web-token` is now written/removed to match `settings.token`), §3.8 (invariant 1 replaced by I1 + I2; I3–I5 added), new §3.9 (settings model) and §4.11 (save/apply flows), §4.2/§4.3/§4.5/§4.6/§4.7/§4.8 (actions, launch args/env, reaper accepts `0.0.0.0`, `Probe::Listening`, settings sentinel + window), §5.1/§5.2/§5.4/§5.6 (signatures, dialogs, IPC, capability + `tauri.conf.json` list, settings page), §6 (T1–T4, T8), this section's R12 amendment and R13–R18, §9.1/§9.3/§9.4 (tests, manual checks). Backend: no change beyond #375 (`service install --tls`, `TLS:` status line).

**Out of scope (separate tickets).** #374 (real HTTPS readiness probe), #376 (webview trust of PeakBot's CA), #378 (Origin validation on `/ws`).

---

## 9. TEST PLAN

### 9.1 Unit tests — `peakbot-desktop-core` (written first, RED)

**token**
1. `parse(" abc ")` → `"abc"`; `parse("")`, `parse("   ")`, `parse("a b")`, `parse("a\nb")` → None.
2. `format!("{:?}", token)` doesn't contain the secret.
3. `RealSys::random_token()` (via C8 integration) → 64 lowercase hex chars; two calls differ.

**version**
4. `"peakbot 0.23.0\n"` → 0.23.0; `"peakbot 0.24.0-rc.1"` → pre-release parsed; `"peakbot 0.23.0\nextra"` → 0.23.0.
5. `""`, `"peakbot"`, `"peakbot x.y"`, `"shifu 0.23.0"`, `"0.23.0"` → None.
6. `DeclinedReplace` render→parse roundtrip for Known and Unknown; garbage → None; `matches` true only for exact pair (different bundled → false; Known vs Unknown → false).

**service**
7. `parse_service_status`: exit 0 sample with `Installed: true`, `Exec:      /home/u/.local/bin/peakbot`, `State:     running` → Installed{exe:Some(path), Running}.
8. `Exec` path containing spaces preserved; missing `Exec:` → exe None.
9. `State: stopped` → Stopped; `State: unknown` / missing → Unknown.
10. `Installed: false` → NotInstalled.
11. exit 1 with stderr `service status failed: systemctl --user not available` → Unavailable with that first line; exit 1 no output → `"service status failed"`.
12. exit 0 without `Installed:` → Unavailable(`unrecognised service status output`).
13. `plan_launch` — every row of §4.4 (7 cases) + `RunState::Unknown` → `start_first: false`.
14. `plan_launch` with NotInstalled/Unavailable never calls `version_of` (closure panics if called).
15. `replace_dialog`: Known older-installed → no warning; Known newer-installed → contains `OLDER`; Unknown → `an unknown version` and cancel `Keep current`; ok label `Replace`.
16. Arg builders return the exact arrays of §5.2.

**session**
17. `session_args(7824)` = `["--bind","127.0.0.1:7824"]`; `session_env` contains exactly the two vars.
18. `PidRecord` roundtrip; parse rejects missing port, non-numeric pid, empty.
19. `is_our_sidecar`: `/tmp/.mount_x/usr/bin/peakbot --bind 127.0.0.1:7824` + port 7824 → true; port 7825 → false; `/usr/bin/vim --bind 127.0.0.1:7824` → false; `peakbot service status` → false.
20. `classify_early_exit(Some(1), "...127.0.0.1:7824 is already in use — ...")` → PortTaken; other text → Crashed; `None` code + no text → Crashed.
21. `tail_lines` returns last n lines; fewer lines → all; empty → empty.

**env / nav / paths**
22. `sanitize_env` without APPDIR → identical (order preserved).
23. With `APPDIR=/tmp/.mount_ab`: APPDIR/APPIMAGE/ARGV0/OWD removed; `PATH=/tmp/.mount_ab/usr/bin:/usr/bin` → `/usr/bin`; `LD_LIBRARY_PATH=/tmp/.mount_ab/usr/lib` → removed; `GTK_PATH=/tmp/.mount_ab/usr/lib/gtk-3.0` → removed; `HOME`, `DISPLAY` untouched.
24. `webkit_dmabuf_default(None)` → `Some(("WEBKIT_DISABLE_DMABUF_RENDERER","1"))`; `Some("0")` → None.
25. `classify_navigation`: `http://127.0.0.1:7824/setup` with origin `http://127.0.0.1:7824` → Allow; `http://127.0.0.1:7823/` with origin 7824 → OpenExternal; `https://github.com/x` → OpenExternal; `mailto:a@b` → OpenExternal; `tauri://localhost/index.html` and `http://tauri.localhost/` → Allow; `file:///etc/passwd`, `data:text/html,x` → Block; backend url with origin None → OpenExternal.
26. `sidecar_path("/tmp/.mount_x/usr/bin/peakbot-desktop")` → `/tmp/.mount_x/usr/bin/peakbot`; `DesktopPaths` helper file names exact.
27. `peakbot_config_dir()` ends with `peakbot` (Linux) — `#[cfg(target_os="linux")]`.

**phase**
28. `Mode::Session{7824, tok}.url()` = `http://127.0.0.1:7824/?token=<tok>`; token with `+`/`&` is percent-encoded; `probe_url` path `/commands`.
29. `Mode::Service{None}.url()` = `http://127.0.0.1:7823/` (no query); `origin()` = `http://127.0.0.1:7823`.
30. `status_view` — one case per row of the §3.5 table (title + actions exact); Starting(step) titles exact; `SessionExited` detail includes code and log tail.
31. `tray_model`: Starting → background disabled; Ready(Session)+NotInstalled → unchecked enabled; Ready(Service)+Installed → checked enabled; Failed+Installed → checked enabled; Unavailable → `(unavailable)` label, disabled; service None → disabled; status string uses `?` when bundled None.
32. `UiAction` deserializes from `"retry"`, `"open_logs"`, `"reinstall_service"`, `"quit"`; `"rm_rf"` → error.

**ready (FakeSys, virtual clock)**
33. Probe Unreachable ×3 then 200 → Ready; elapsed virtual time = 3×POLL_INTERVAL.
34. Always Unreachable → Timeout at ≥ READY_TIMEOUT (no real sleeping).
35. 401 → TokenRejected immediately; 503 then 200 → Ready.
36. Child exits (code 1) while unreachable → ChildExited(Some(1)) before timeout.
37. `quitting` set → Cancelled on next iteration.

**controller (FakeSys + FakeUi)**
38. Sidecar missing (`run` → NotFound) → Failed(SidecarMissing{path}); no spawn.
39. `--version` prints garbage → Failed(SidecarBroken).
40. Happy session: NotInstalled; 7824 free → spawn args `--bind 127.0.0.1:7824`, extra_env has token + NO_OPEN, pidfile written `pid/port`, probe 200 → Ready(Session{7824}); phases emitted in order CheckingBundled, CheckingService, StartingSession, WaitingForBackend, Ready.
41. 7824 busy → uses `free_port()` result.
42. First child exits with "already in use" in log → second attempt on a new port → Ready; 3 failures → Failed(PortUnavailable).
43. Child exits with other output → Failed(SessionExited) carrying log tail; pidfile removed.
44. Session readiness timeout → child interrupted (then killed if FakeChild ignores interrupt after STOP_GRACE) → Failed(NotReady).
45. Service Unavailable → session mode (no prompt).
46. Installed same version, Running → no spawn, no `service install`; Ready(Service{token from file}).
47. Installed same version, Stopped → runs `service install --bind 127.0.0.1:7823` then Ready.
48. Installed, web-token absent → Ready(Service{None}); token file NOT created.
49. Installed different version, user Replace → commands in order: `service uninstall`, (token file written 0600 if absent), `install`, `service install --bind 127.0.0.1:7823`; Ready(Service); declined file removed.
50. Installed different version, user Keep → declined file written with exact pair; next `Start` → no dialog.
51. Declined pair differs (new bundled version) → dialog shown again.
52. Service probe 401 → Failed(TokenRejected{service:true}); timeout → Failed(ServiceUnreachable).
53. `service install` non-zero during Replace → Failed(ServiceCommandFailed{command, output}).
54. Enable: config.yaml missing → `finish_setup_dialog`, no commands run, phase unchanged.
55. Enable: 7823 busy → `port_busy_dialog`, session untouched.
56. Enable: confirm Cancel → nothing.
57. Enable happy: session child interrupted BEFORE `install` is run (assert ordering via recorded log); token generated+written if absent; ends Ready(Service); pidfile removed; tray checked.
58. Enable failure (`service install` fails) → `service uninstall` attempted, error dialog shown, new session launched → Ready(Session).
59. Disable: confirm → `service uninstall` → session launched → Ready(Session); tray unchecked. Cancel → nothing.
60. Disable: uninstall fails → error dialog; reconnect → Ready(Service).
61. `ToggleBackground`/`ReinstallService` while Starting → ignored (no commands).
62. `tick()` when Ready(Session) and child exited → Failed(SessionExited), pidfile removed; `tick()` in service mode → no-op.
63. `Shutdown` in session mode → interrupt; child exits within grace → no kill; ack sent; `ControlFlow::Break`. Child ignores interrupt → kill after STOP_GRACE.
64. Stale reaper: pidfile + `cmdline_of` matches → Interrupt (then Kill if still alive after grace), pidfile removed; cmdline mismatch → no signal, pidfile removed; pidfile garbage → removed, no signal.
65. `Retry` (Start) from Ready(Session) stops the old child before spawning a new one.

**settings (#377; t66–t89, in `core::settings` / `core::controller` / `core::sys`; plus updated t16/t17/t19/t25/t28/t29/t32/t40/t57)**
66. Default = http loopback, token on; `render_settings_file` byte-exact for default and `Remote`.
67. `TryFrom<SettingsWire>`: 4 loopback combos OK; remote+https+token → `Remote`; remote+http → `Remote access requires HTTPS.`; remote+¬token → `Remote access requires the access token.`; remote+http+¬token → the HTTPS message.
68. render→parse roundtrip for all 5 valid values.
69. `load_settings`: None / `""` / `"{"` / `null` / missing field / `v:0` / `v:2` / illegal combo → default; unknown extra key → parsed.
70. Accessors (`bind_host`, Remote ⇒ https + token).
71. `TLS:       on` → `tls = true`; `off`/missing → false.
72. Dialog strings exact (relaunch, reinstall, save-failed).
73. https session/service URLs + origin; session `token: None` → no query.
74. `settings_view` maps tray/status/path; `busy` ⇔ kind == "starting".
75. `Probe::Listening` → Ready (FakeSys); real_sys t88 (unix): https URL to a bound listener → `Listening`; closed port → `Unreachable`.
76. SaveSettings (session) `Loopback{Https,true}` → file exact, old child interrupted, spawn args end `--tls`, `Ready(Session{Https})`, url `https://127.0.0.1:7824/?token=…`.
77. SaveSettings `Loopback{Http,false}` with a seeded web-token → `Remove(web-token)` before Spawn, no `PEAKBOT_WEB_TOKEN` in the spawn env, url without query.
78. confirm = Cancel → no write/spawn/interrupt, phase unchanged.
79. Same settings → no dialog, no effects.
80. Service mode, save `Remote` → `service uninstall`, `service install --bind 0.0.0.0:7823 --tls` (NO `install`); web-token written before the install; `Ready(Service{Https, Some})`.
81. From a Remote file, save `Loopback{Http,false}` → `Remove(web-token)` before `service install --bind 127.0.0.1:7823` (no `--tls`); `Ready(Service{Http, None})`.
82. Reconfigure install fails → `Failed(ServiceCommandFailed)`; the file holds the new settings.
83. settings.json write fails → `save_failed_dialog`, no Run/Spawn.
84. Start with an installed `TLS: on` service → probe hits `https://127.0.0.1:7823/commands…` → `Ready(Service{Https})`.
85. Enable background with a Remote file → `service install --bind 0.0.0.0:7823 --tls`; the session itself ran with the remote settings.
86. Replace with an https file → `--tls`; `install` still runs.
87. Corrupt settings.json at Start → default session, no write to the settings path.
88. (see 75) real_sys https probe.
89. SaveSettings while Starting → ignored.

### 9.2 Artifact smoke checks (run inside the build containers; build fails on any failure)

`smoke-linux.sh <AppImage> <version>`:
- S-L1 file exists, mode has `x`; `file` reports `ELF 64-bit LSB executable, x86-64`.
- S-L2 `--appimage-extract` into a temp dir succeeds (no FUSE).
- S-L3 `squashfs-root/usr/bin/peakbot-desktop` and `squashfs-root/usr/bin/peakbot` exist and are executable.
- S-L4 `squashfs-root/usr/bin/peakbot --version` prints exactly `peakbot <version>`.
- S-L5 a `.desktop` file exists at the squashfs root with `Exec=peakbot-desktop` and `Name=PeakBot Desktop`.
- S-L6 `objdump -T usr/bin/peakbot | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1` ≤ `GLIBC_2.34`.
- S-L7 `usr/bin/peakbot-desktop` links `libwebkit2gtk-4.1` (`ldd`/`readelf -d` NEEDED) — proves the right webkit ABI.

`smoke-macos.sh <zip> <version>`:
- S-M1 `unzip -Z1` lists exactly: `PeakBot Desktop.app/`, `…/Contents/`, `…/Contents/Info.plist`, `…/Contents/MacOS/`, `…/Contents/MacOS/peakbot-desktop`, `…/Contents/MacOS/peakbot`, `…/Contents/Resources/`, `…/Contents/Resources/icon.icns`, `…/Contents/_CodeSignature/`, `…/Contents/_CodeSignature/CodeResources` (nothing else).
- S-M2 after unzip, both MacOS binaries have mode `755`.
- S-M3 `file` on both binaries: `Mach-O universal binary with 2 architectures: [x86_64…] [arm64…]`.
- S-M4 Info.plist contains `CFBundleExecutable`=`peakbot-desktop`, `CFBundleIdentifier`=`com.peakbot.desktop`, `CFBundleShortVersionString`=`<version>`, `LSMinimumSystemVersion`=`11.0`, `NSAllowsLocalNetworking` `<true/>`.
- S-M5 `rcodesign print-signature-info` on the `.app` and on `Contents/MacOS/peakbot` exits 0 and shows a code directory for both arm64 and x86_64 slices; `rcodesign verify` on each Mach-O exits 0.
- S-M6 zip filename = `PeakBot-Desktop-<version>-macos-universal.zip`.

### 9.3 Manual Linux checklist (dev, before P4)
L1 first launch with no `~/.config/peakbot` → setup wizard inside the window, no browser opens.
L2 normal launch → UI loads; `ps` shows `peakbot --bind 127.0.0.1:7824` and no token in argv.
L3 close window → sidecar gone within 3 s; pidfile removed.
L4 `kill -9` the desktop app → relaunch reaps the old sidecar.
L5 `kill` the sidecar while UI open → status page "PeakBot stopped unexpectedly" → Retry works.
L6 external link in chat opens the system browser; `window.open` doesn't create an in-app window.
L7 tick background → service installed (`systemctl --user status peakbot`), UI reconnects on 7823, unit has `Environment=PEAKBOT_NO_OPEN=1`.
L8 relaunch app → connects to the service, no sidecar spawned.
L9 install a different peakbot version as service via CLI → relaunch → replace dialog; Keep → no dialog next launch.
L10 untick → service removed, session restarts.
L11 debug build: in the remote page devtools, `__TAURI__.core.invoke('status')` rejects (not allowed).
L12 second launch focuses the existing window; window size/position restored after restart; tray shows correct status in each mode; Hyprland/NVIDIA window is not blank.
M1 (#377) Shortcut (Ctrl+Shift+S), tray item and status-page link each open one settings window; a second trigger focuses it.
M2 First launch shows `HTTP / token on / remote off`.
M3 Save HTTPS → confirm → `ps` shows `--tls`, the main window goes to `https://…`. A certificate error is expected before #376.
M4 Token off → `~/.config/peakbot/web-token` gone and the URL has no `?token`. Remote → `ss -ltn` shows `0.0.0.0:<port>`; a phone reaches it with the file token.
M5 On the WebKit TLS error page, does Ctrl+Shift+S still work? Record the result; if not, the README reset path stands.
M6 Background toggle from the settings page behaves like the tray item.
M7 Devtools on the remote page: `__TAURI__.core.invoke('settings_get')` and `invoke('settings_set', …)` both reject (criterion 8).

### 9.4 Manual macOS checklist (human — no Mac in CI)
MAC1 Unzip, move to /Applications, `xattr -dr com.apple.quarantine` → app opens (Apple Silicon and, if available, Intel).
MAC2 Without the xattr step: document exactly what Gatekeeper shows (README accuracy).
MAC3 First run → setup wizard renders in the window; http://127.0.0.1 loads (ATS ok); no browser tab opens.
MAC4 ⌘C/⌘V/⌘A work in the chat input; ⌘Q quits and the sidecar exits (Activity Monitor).
MAC5 Menu-bar tray icon visible; menu items behave as on Linux; "PeakBot" submenu present in the app menu.
MAC6 Dock-icon click after window close relaunches cleanly; second launch focuses existing window.
MAC7 Enable background → `~/Library/LaunchAgents/com.peakbot.agent.plist` exists with `EnvironmentVariables/PEAKBOT_NO_OPEN`; app reconnects to 7823.
MAC8 Log out/in → service running, **no browser tab opened at login**; app connects.
MAC9 `~/.local/bin/peakbot --version` runs from Terminal without a Gatekeeper prompt; `xattr ~/.local/bin/peakbot` shows no `com.apple.quarantine` (R2).
MAC10 Ask the agent to run `which brew git node` in session mode and in service mode; record PATH (R3).
MAC11 Disable background → plist removed; session restarts.
MAC12 `codesign -dv --verbose=4 "/Applications/PeakBot Desktop.app"` reports `Signature=adhoc`; `codesign --verify --deep` passes.
M8 (#377) Cmd+Shift+S and the PeakBot-menu `Settings…` item both open the settings window; the item shows the shortcut.
