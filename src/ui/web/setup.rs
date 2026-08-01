//! `/api/setup` — the wizard's HTTP surface (plan §B, §C, track S tasks S2–S4).
//!
//! **The server is a validating file-writer, nothing more.** The wizard POSTs
//! the exact reviewed YAML; the server parses it with the real
//! `serde_yaml::from_str::<Config>` plus the real validators (`tools.validate`,
//! `timeouts.validate`, `build_model_registry`), and only on success writes
//! those bytes verbatim via [`save_config_at`]. Never rendered, merged, or
//! defaulted server-side. *(principle of least astonishment)*
//!
//! The `SetupState` is built once at boot and injected into the router; the
//! handler never re-derives paths. `install` and `service` are fn-pointer
//! seams (plan §E.9) that track I later populates with the real platform
//! dispatchers — see the `default_for_tests()` no-ops for the test path.
//!
//! The token layer is **not** wired here: tests attach it themselves so the
//! auth contract is exercised independently. Production mounting
//! (plan §E.9 + §B) merges the returned router into `WebUi::run` **before**
//! the token layer, so every `/api/setup/*` route is gated by the same
//! `require_token` as `/ws` and `/commands`.

use crate::config::{Config, save_config_at};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::PathBuf;
use std::sync::Arc;

// ===========================================================================
// SetupInfo — the GET /api/setup response (plan §B).
// ===========================================================================

/// GET /api/setup response. Mirrors the plan §B TS type 1:1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupInfo {
    pub os: String,
    pub arch: String,
    pub exe_path: Option<String>,
    pub config_path: String,
    pub data_dir: Option<String>,
    pub cache_dir: Option<String>,
    pub skills_dir: Option<String>,
    pub lan_bind_hint: String,
    pub needs_setup: bool,
    pub builtin_tools: Vec<String>,
    pub install: InstallInfo,
    pub existing: ExistingConfig,
}

/// Install target surface (plan §B). Track I now drives this from the real
/// [`install_target`](crate::install::install_target) and
/// [`path_state`](crate::install::path_state) over the process PATH — both
/// pure, so the GET /api/setup install block never runs a subprocess.
/// `state` is the canonicalised file comparison (`current` when
/// `target == current_exe` on disk, `absent` when the file is missing,
/// `other` when a *different* file is there). `path` is the PATH-lookup
/// verdict the wizard renders verbatim — same shape §B pins for the
/// install response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallInfo {
    pub target: String,
    pub state: String, // "current" | "absent" | "other"
    /// Same tagged union as §B: `{"status":"on_path"}` |
    /// `{"status":"shadowed","by": <path>}` |
    /// `{"status":"absent","hint": <hint>}`.
    pub path: InstallPath,
}

/// Wire form of [`crate::install::PathState`]. Kept separate so the HTTP
/// module owns its own JSON spelling and the install core can evolve
/// without breaking the contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InstallPath {
    OnPath,
    Shadowed { by: String },
    Absent { hint: String },
}

impl InstallPath {
    /// Pure translation from the install-core verdict to the wire form.
    /// Lives here so the conversion is exercised by the same in-module
    /// tests that cover the rest of the file. `pub` so integration
    /// tests can also round-trip every variant.
    pub fn from_core(s: &crate::install::PathState) -> Self {
        match s {
            crate::install::PathState::OnPath => InstallPath::OnPath,
            crate::install::PathState::Shadowed { by } => InstallPath::Shadowed {
                by: by.display().to_string(),
            },
            crate::install::PathState::NotOnPath { hint } => {
                InstallPath::Absent { hint: hint.clone() }
            }
        }
    }
}

/// State of the existing `config.yaml` (plan §B tagged union).
/// `Absent` for no file, `Ok` for a parsed file (transcoded JSON),
/// `Error` for a malformed file (HTTP 200 regardless — a malformed
/// config can never break the facts fetch).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum ExistingConfig {
    Absent,
    Ok { config: JsonValue },
    Error { message: String },
}

// ===========================================================================
// WriteOutcome — the POST /api/setup/config success response (plan §B).
// ===========================================================================

/// Returns the **exact** shape the wizard's "Restart" step reads. `restart_required`
/// is locked to `true` (plan §A-Q4 ruling A — post-write story is "restart").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteOutcome {
    pub path: String,
    pub backup: Option<String>,
    pub restart_required: bool,
}

// ===========================================================================
// FactsBase — platform facts surfaced once at GET /api/setup (plan §B).
//
// Resolved once at boot via [`FactsBase::current`] so the handler never
// re-queries `current_exe()` or `dirs::*` on every request. The fields
/// are nullable because not every platform has a data dir / cache dir.
#[derive(Debug, Clone)]
pub struct FactsBase {
    pub os: String,
    pub arch: String,
    pub exe_path: Option<String>,
    pub data_dir: Option<String>,
    pub cache_dir: Option<String>,
    pub skills_dir: Option<String>,
}

impl FactsBase {
    /// Snapshot the running binary's platform facts. Path turns are
    /// read via `dirs::data_dir()` / `cache_dir()` per the platform
    /// conventions; omit the segment if the host has none.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            exe_path: std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().into_owned()),
            data_dir: dirs::data_dir().map(|p| p.join("peakbot").to_string_lossy().into_owned()),
            cache_dir: dirs::cache_dir().map(|p| p.join("peakbot").to_string_lossy().into_owned()),
            skills_dir: dirs::config_dir()
                .map(|p| p.join("peakbot/skills").to_string_lossy().into_owned()),
        }
    }
}

// ===========================================================================
// InstallFn / ServiceFn — fn-pointer seams (plan §E.9).
//
// Track I replaces the defaults with the real `install::install_binary`
// and `install::service_op` dispatchers. `default_for_tests()` returns a
// no-op so the handler test never copies a binary into $HOME or shells
// out to `systemctl` — the seam is exactly one indirection, no trait,
// no plugin system.
//
// The seam is JSON in / JSON out for both the request (so a request
// body that arrives as `application/json` can flow through unchanged)
// and the success response (so the adapter decides the shape, not the
// handler). The error arm is a structured [`SetupOpError`] so the
// handler can map it to 422 / 500 / 501 without inspecting strings.
// ===========================================================================

/// Failure shape every adapter must produce (plan §B error mapping).
/// 422 carries `problems` (PlanError::TokenRequired); 501 is reserved
/// for `InstallError::Unsupported`; everything else collapses to 500
/// with the underlying message in `error` and any stderr in `problems`.
#[derive(Debug)]
pub struct SetupOpError {
    pub status: StatusCode,
    pub error: String,
    pub problems: Vec<String>,
}

impl SetupOpError {
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            error: msg.into(),
            problems: Vec::new(),
        }
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            error: msg.into(),
            problems: Vec::new(),
        }
    }
    pub fn internal(msg: impl Into<String>, problems: Vec<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: msg.into(),
            problems,
        }
    }
    /// Convert any [`crate::install::InstallError`] to a `SetupOpError`
    /// per the §B table. The `Unsupported` variant is the only one that
    /// maps to 501; everything else lands at 500.
    pub fn from_install_error(e: &crate::install::InstallError) -> Self {
        use crate::install::InstallError;
        match e {
            InstallError::Unsupported(msg) => Self::unsupported(msg.clone()),
            InstallError::CommandFailed { command, stderr } => Self::internal(
                format!("`{command}` failed: {stderr}"),
                vec![stderr.clone()],
            ),
            InstallError::CurrentExe(io) => Self::internal(
                format!("cannot locate the running binary: {io}"),
                Vec::new(),
            ),
            InstallError::Io(io) => Self::internal(format!("install I/O error: {io}"), Vec::new()),
        }
    }
    /// Convert a [`crate::install::PlanError`] to a 422. The plan
    /// invariant is the only reason this exists; PlanError's `Display`
    /// already names the rejected bind (§E.5), so we hand the message
    /// through verbatim.
    pub fn from_plan_error(e: &crate::install::PlanError) -> Self {
        Self::validation(e.to_string())
    }
}

/// Wraps a fn pointer so [`InstallFn::default_for_tests`] and the production
/// adapter share a common surface. `Copy + Clone` because the underlying
/// fn pointer is too.
#[derive(Copy, Clone)]
pub struct InstallFn(pub fn(JsonValue) -> Result<JsonValue, SetupOpError>);

/// Wraps a fn pointer — same reasoning as [`InstallFn`].
#[derive(Copy, Clone)]
pub struct ServiceFn(pub fn(JsonValue) -> Result<JsonValue, SetupOpError>);

impl InstallFn {
    /// No-op install op. Track I replaces this with the real dispatcher.
    pub fn default_for_tests() -> Self {
        fn noop(_req: JsonValue) -> Result<JsonValue, SetupOpError> {
            Ok(serde_json::json!({
                "status": "not_implemented",
                "message": "installer not configured for this build"
            }))
        }
        Self(noop)
    }
}

impl ServiceFn {
    /// No-op service op. Track I replaces this with the real dispatcher.
    pub fn default_for_tests() -> Self {
        fn noop(_req: JsonValue) -> Result<JsonValue, SetupOpError> {
            Ok(serde_json::json!({
                "status": "not_implemented",
                "message": "service not configured for this build"
            }))
        }
        Self(noop)
    }
}

// ===========================================================================
// Real adapters — production wiring for the §E.9 seams.
//
// `install_op_adapter` and `service_op_adapter` are the *only* places
// `install::install_binary` and `install::service_op` are called from
// the HTTP layer. They live as plain `fn` items so the fn-pointer
// seam keeps working in tests (a fake can replace them; a real
// dispatch needs no trait, no dyn).
//
// `spawn_blocking` is used because the install / service verbs shell
// out to `systemctl` / `launchctl` / `schtasks` and we are on the
// axum runtime. §E.10 specifies blocking subprocesses, not async.
// ===========================================================================

/// Production install adapter. Ignores the request body (plan §B
/// `InstallRequest = Record<string, never>`), calls `install_binary`,
/// and serialises the outcome to the §B `InstallResponse` shape.
pub fn install_op_adapter(_req: JsonValue) -> Result<JsonValue, SetupOpError> {
    let outcome =
        crate::install::install_binary().map_err(|e| SetupOpError::from_install_error(&e))?;
    Ok(install_outcome_to_wire(&outcome))
}

/// Production service adapter. Dispatches on the request's `op` field
/// (one of `{"op":"status"}` | `{"op":"uninstall"}` | `{"op":"install",
/// "bind"?, "token"?}`); builds a `ServicePlan` for the install arm;
/// threads the same §E.5 web-token write + uninstall note side-effects
/// the CLI does so the two surfaces say exactly the same words.
pub fn service_op_adapter(req: JsonValue) -> Result<JsonValue, SetupOpError> {
    use crate::install::{ServiceOp, ServicePlan, write_web_token};
    let op = req
        .get("op")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SetupOpError::validation("expected `op` in {status,install,uninstall}"))?;
    match op {
        "status" => {
            let report = crate::install::service_op(ServiceOp::Status)
                .map_err(|e| SetupOpError::from_install_error(&e))?;
            Ok(service_report_to_wire(&report))
        }
        "uninstall" => {
            let mut report = crate::install::service_op(ServiceOp::Uninstall)
                .map_err(|e| SetupOpError::from_install_error(&e))?;
            // §E.5: uninstall never deletes the token file. Surface the
            // same note the CLI prints so the wizard renders it too.
            if let Some(path) = crate::install::web_token_path() {
                report.notes.push(format!(
                    "the web-token file at {} was NOT deleted; \
                     remove it by hand if you want to drop the secret.",
                    path.display()
                ));
            }
            Ok(service_report_to_wire(&report))
        }
        "install" => {
            let bind = match req.get("bind").and_then(|v| v.as_str()) {
                Some(s) => Some(s.parse::<std::net::SocketAddr>().map_err(|e| {
                    SetupOpError::validation(format!("bind is not a valid SocketAddr: {e}"))
                })?),
                None => None,
            };
            // §A-Q8 default: the loopback-free bind the CLI uses too.
            let bind = bind.unwrap_or_else(|| {
                crate::ui::DEFAULT_WEB_ADDR
                    .parse()
                    .expect("DEFAULT_WEB_ADDR is a valid SocketAddr literal")
            });
            let token_str = req.get("token").and_then(|v| v.as_str());
            let token_trim = token_str.map(str::trim).filter(|t| !t.is_empty());
            // Token precedence mirrors the CLI: explicit `token` first,
            // then the existing `web-token` file. Plan §E.5.
            let owned_token: Option<String> = match token_trim {
                Some(t) => Some(t.to_string()),
                None => crate::install::web_token_path()
                    .as_deref()
                    .and_then(|p| crate::install::read_web_token(p).ok().flatten()),
            };
            // §E.5: same fallback as the CLI's `build_plan_for_install`.
            // Inline here so the wizard hits the same target selection
            // without depending on `main.rs`.
            let target = crate::install::install_target();
            let (exe, fallback_note) = match target.as_ref() {
                Some(t) if t.exists() => (t.clone(), None),
                _ => {
                    // Fallback: prefer the install target's parent
                    // (matches the install verb) — `current_exe()` is
                    // the test runner in CI and would surface a wrong
                    // path. `unwrap_or_default` is fine: an empty
                    // PathBuf is still a valid (if useless) input.
                    let here = target
                        .clone()
                        .unwrap_or_else(|| std::env::current_exe().unwrap_or_default());
                    let note = Some(format!(
                        "the service points at {}, which is where you ran PeakBot from — \
                         run `peakbot install` so it survives a move",
                        here.display()
                    ));
                    (here, note)
                }
            };
            let plan = ServicePlan::new(exe, bind, owned_token)
                .map_err(|e| SetupOpError::from_plan_error(&e))?;
            // Mirror the CLI's web-token side effect: if the wizard
            // sent a non-empty `token`, persist it to the file so a
            // later `peakbot` boot finds it through the chain.
            if let Some(t) = token_trim {
                let path = crate::install::web_token_path().ok_or_else(|| {
                    SetupOpError::internal("no config dir for web-token file", Vec::new())
                })?;
                write_web_token(&path, t).map_err(|e| SetupOpError::from_install_error(&e))?;
            }
            let mut report = crate::install::service_op(ServiceOp::Install { plan })
                .map_err(|e| SetupOpError::from_install_error(&e))?;
            if let Some(note) = fallback_note {
                report.notes.push(note);
            }
            Ok(service_report_to_wire(&report))
        }
        other => Err(SetupOpError::validation(format!(
            "unknown service op: {other:?} (expected status, install, or uninstall)"
        ))),
    }
}

/// Render an [`InstallOutcome`](crate::install::InstallOutcome) as the
/// §B `InstallResponse` JSON. `path` is the PATH verdict the install
/// itself produced (recomputed from the post-install target so the
/// response is self-describing — no need to look at `current_exe()`).
fn install_outcome_to_wire(o: &crate::install::InstallOutcome) -> JsonValue {
    use crate::install::InstallAction;
    let action = match o.action {
        InstallAction::AlreadyCurrent => "already_current",
        InstallAction::Installed => "installed",
        InstallAction::Replaced => "replaced",
    };
    let path_var: std::ffi::OsString = std::env::var_os("PATH").unwrap_or_default();
    let path = InstallPath::from_core(&crate::install::path_state(&path_var, &o.target));
    // Embed the same `path` as a tagged union in the JSON value.
    let path_json = match &path {
        InstallPath::OnPath => serde_json::json!({"status": "on_path"}),
        InstallPath::Shadowed { by } => serde_json::json!({"status": "shadowed", "by": by}),
        InstallPath::Absent { hint } => serde_json::json!({"status": "absent", "hint": hint}),
    };
    serde_json::json!({
        "source": o.source.display().to_string(),
        "target": o.target.display().to_string(),
        "action": action,
        "path": path_json,
        "notes": o.notes,
    })
}

/// Render a [`ServiceReport`](crate::install::ServiceReport) as the
/// §B `ServiceResponse` JSON. Field-for-field match.
fn service_report_to_wire(r: &crate::install::ServiceReport) -> JsonValue {
    serde_json::json!({
        "manager": r.manager.as_wire(),
        "name": r.name,
        "artifact": r.artifact.as_ref().map(|p| p.display().to_string()),
        "installed": r.installed,
        "exe": r.exe.as_ref().map(|p| p.display().to_string()),
        "run_state": r.run_state.as_wire(),
        "survives_logout": r.survives_logout,
        "commands": r.commands,
        "notes": r.notes,
    })
}

// ===========================================================================
// SetupState — built once at boot, cloned into the router (plan §D S2).
// ===========================================================================

/// Per-handler state injected by [`router`]. The handler never re-derives
/// paths or facts; everything is captured here at boot.
#[derive(Clone)]
pub struct SetupState {
    /// Absolute path to the master config file the wizard is configuring.
    /// Injected from production `WebUi::new` so the test path can point
    /// at a tempdir without touching `$HOME`.
    pub config_path: PathBuf,
    /// Snapshot of platform facts at boot.
    pub facts_base: FactsBase,
    /// Whether this server was booted without an existing config file.
    pub needs_setup: bool,
    /// Install op (track I seam).
    pub install: InstallFn,
    /// Service op (track I seam).
    pub service: ServiceFn,
}

// ===========================================================================
// JSON / error envelope helper (plan §B).
// ===========================================================================

/// One error envelope everywhere (plan §B). 422 carries the `problems`
/// array; the others carry `error` only.
#[derive(Debug, Serialize)]
struct ApiError {
    error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    problems: Option<Vec<String>>,
}

impl ApiError {
    fn simple(status: StatusCode, message: impl Into<String>) -> Response {
        json_response(
            status,
            &Self {
                error: message.into(),
                problems: None,
            },
        )
    }

    fn validation(problems: Vec<String>) -> Response {
        json_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            &Self {
                error: "config is not valid".into(),
                problems: Some(problems),
            },
        )
    }
}

/// Serialize `value` as JSON and return a 200/4xx response with the
/// `Content-Type: application/json` header. Hand-rolled because axum in
/// this build is configured without the `json` feature (the plan keeps
/// it off to avoid pulling serde_json's derived-tree walk into four
/// platform builds).
fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => (
            status,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialise response: {e}"),
        )
            .into_response(),
    }
}

/// Require `Content-Type: application/json` and return 415 otherwise.
/// Hand-rolled (not `axum::extract::Json`) so the missing-or-wrong
/// content type produces an `ApiError` envelope the rest of the API uses.
///
/// Returns `Box<Response>` for the error arm — `axum::Response` is large
/// (~128 bytes) and clippy's `result_large_err` would otherwise warn every
/// call site. The box is one indirection on the slow path; the success
/// path is `()`.
fn require_json_content_type(req: &Request<Body>) -> Result<(), Box<Response>> {
    let reject = |msg: &str| -> Box<Response> {
        Box::new(ApiError::simple(StatusCode::UNSUPPORTED_MEDIA_TYPE, msg))
    };
    let Some(ct) = req.headers().get(header::CONTENT_TYPE) else {
        return Err(reject("Content-Type must be application/json"));
    };
    let Ok(ct) = ct.to_str() else {
        return Err(reject("Content-Type must be application/json"));
    };
    // Split off any `; charset=…` parameter — the spec only requires the
    // media type to match.
    let media_type = ct.split(';').next().unwrap_or("").trim();
    if !media_type.eq_ignore_ascii_case("application/json") {
        return Err(reject("Content-Type must be application/json"));
    }
    Ok(())
}

/// Read the JSON body as `serde_json::Value`, returning a 400 ApiError
/// envelope on parse failure. Boxed Response for the same
/// `result_large_err` reason as `require_json_content_type`.
async fn read_json_body(req: Request<Body>) -> Result<JsonValue, Box<Response>> {
    let bytes = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            return Err(Box::new(ApiError::simple(
                StatusCode::BAD_REQUEST,
                format!("failed to read body: {e}"),
            )));
        }
    };
    match serde_json::from_slice::<JsonValue>(&bytes) {
        Ok(v) => Ok(v),
        Err(e) => Err(Box::new(ApiError::simple(
            StatusCode::BAD_REQUEST,
            format!("body is not valid JSON: {e}"),
        ))),
    }
}

// ===========================================================================
// Handlers
// ===========================================================================

/// GET /api/setup — platform facts + the existing config (plan §A-Q3).
/// Always 200; a malformed file becomes `existing: {status: "error"}`.
async fn get_setup(State(state): State<Arc<SetupState>>) -> Response {
    let info = build_setup_info(&state);
    json_response(StatusCode::OK, &info)
}

fn build_setup_info(state: &SetupState) -> SetupInfo {
    let existing = read_existing(&state.config_path);
    let install = build_install_info(&state.facts_base);
    SetupInfo {
        os: state.facts_base.os.clone(),
        arch: state.facts_base.arch.clone(),
        exe_path: state.facts_base.exe_path.clone(),
        config_path: state.config_path.to_string_lossy().into_owned(),
        data_dir: state.facts_base.data_dir.clone(),
        cache_dir: state.facts_base.cache_dir.clone(),
        skills_dir: state.facts_base.skills_dir.clone(),
        // Plan §B fixed at the loopback-free bind address — the same
        // value the `--bind 0.0.0.0` flag defaults to in the legacy
        // surface. The wizard displays this verbatim in the launch line.
        lan_bind_hint: "0.0.0.0:7823".to_string(),
        // Computed once by the boot gate and threaded through WebUi.
        needs_setup: state.needs_setup,
        builtin_tools: crate::config::BUILTIN_TOOL_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        install,
        existing,
    }
}

/// Build the `install` block of GET /api/setup from §E.4 pure functions
/// only — `install_target()` + canonicalised `current_exe()` + `path_state`
/// over the process PATH. No subprocess, no I/O beyond `current_exe()`
/// resolution. Called once per GET, not per request, so even a chatty
/// wizard costs nothing.
fn build_install_info(facts_base: &FactsBase) -> InstallInfo {
    let target = crate::install::install_target();
    let target_display = target
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    // §B `state`: "current" when the running binary *is* the install
    // target (the second-run case), "absent" when the file does not
    // exist yet, "other" when a different file is in the target slot.
    let install_state = match (&target, &facts_base.exe_path) {
        (Some(t), Some(exe)) if target.as_deref() == Some(std::path::Path::new(exe)) => {
            // Cheap path: the wizard's `exe_path` already equals target.
            // Means `current_exe()` IS the install target (the common
            // case once the user has run `peakbot install`).
            "current".to_string()
        }
        (Some(t), _) if t.exists() => {
            // Compare canonical paths when the cheap path disagrees —
            // avoids false "other" verdicts caused by symlinks or
            // relative-path differences in `current_exe()`.
            match (t.canonicalize(), std::fs::canonicalize(t)) {
                (Ok(a), Ok(b)) if a == b => "current".to_string(),
                _ => "other".to_string(),
            }
        }
        (Some(_), _) => "absent".to_string(),
        (None, _) => "absent".to_string(),
    };
    // PATH verdict: only meaningful when we have a target to look up.
    // `path_state` is a pure walk over the var we hand it; the process
    // PATH is the only env read on this path.
    let path = match target.as_deref() {
        Some(t) => {
            let path_var: std::ffi::OsString = std::env::var_os("PATH").unwrap_or_default();
            InstallPath::from_core(&crate::install::path_state(&path_var, t))
        }
        None => InstallPath::Absent {
            hint: "no install target on this platform".to_string(),
        },
    };
    InstallInfo {
        target: target_display,
        state: install_state,
        path,
    }
}

/// Read the existing config file and classify it into the tagged union.
/// Parse failures stay at 200 with `status: "error"` — the facts fetch
/// must never break because the user's YAML is malformed.
fn read_existing(path: &std::path::Path) -> ExistingConfig {
    if !path.exists() {
        return ExistingConfig::Absent;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return ExistingConfig::Error {
                message: format!("read {}: {e}", path.display()),
            };
        }
    };
    let yaml = match serde_yaml::from_str::<serde_yaml::Value>(&content) {
        Ok(v) => v,
        Err(e) => {
            return ExistingConfig::Error {
                message: format!("parse {}: {e}", path.display()),
            };
        }
    };
    // Transcode to JSON for the wire. `serde_json::to_value` on a
    // `serde_yaml::Value` walks the YAML node graph and emits a JSON
    // representation — no defaults pollution (the user only sees the
    // keys they actually wrote). This is the only reason we don't need
    // `Serialize` on `Config`: the wizard gets the user's file, not the
    // Rust struct.
    let config = match serde_json::to_value(&yaml) {
        Ok(v) => v,
        Err(e) => {
            return ExistingConfig::Error {
                message: format!("transcode yaml→json: {e}"),
            };
        }
    };
    ExistingConfig::Ok { config }
}

/// POST /api/setup/config — validate the YAML, then write the verbatim bytes.
/// Plan §A-Q4 pipeline: parse → tools validate → timeouts validate → registry
/// build → write. Every failure becomes a 422 envelope; the file is never
/// touched on failure.
async fn post_config(State(state): State<Arc<SetupState>>, req: Request<Body>) -> Response {
    if let Err(resp) = require_json_content_type(&req) {
        return *resp;
    }
    let body = match read_json_body(req).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    let Some(yaml) = body.get("yaml").and_then(|v| v.as_str()) else {
        return ApiError::simple(
            StatusCode::BAD_REQUEST,
            "expected JSON body with a `yaml` string field",
        );
    };

    let mut problems: Vec<String> = Vec::new();

    // Step 1 — parse the bytes as `Config`. `deny_unknown_fields` (and
    // the per-struct tags) means anything that survives this is, by
    // construction, a file the binary will boot from.
    let cfg: Config = match serde_yaml::from_str::<Config>(yaml) {
        Ok(c) => c,
        Err(e) => {
            problems.push(format!("yaml: {e}"));
            return ApiError::validation(problems);
        }
    };

    // Step 2 — tools filter XOR (blocklist vs allowlist) and the
    // known-tool-name check.
    if let Err(e) = cfg.tools.validate() {
        problems.push(e);
    }

    // Step 3 — outbound timeouts (delegation / tool-call budgets).
    if let Err(e) = cfg.timeouts.validate() {
        problems.push(e);
    }

    // Step 4 — model registry build. Alias charset, reserved `unknown`,
    // uniqueness, default_model resolution.
    if let Err(e) = cfg.build_model_registry() {
        problems.push(e.to_string());
    }

    if !problems.is_empty() {
        return ApiError::validation(problems);
    }

    // Step 5 — write. This is the only mutating call. The CFG is fully
    // validated at this point; the bytes written are the reviewed YAML
    // verbatim (plan §A-Q4 ruling: "what you review is byte-for-byte
    // what lands").
    let outcome = match save_config_at(
        state
            .config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
        yaml,
    ) {
        Ok(o) => o,
        Err(e) => {
            return ApiError::simple(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to write config: {e}"),
            );
        }
    };

    let body = WriteOutcome {
        path: outcome.path.to_string_lossy().into_owned(),
        backup: outcome
            .backup
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        // Post-write story is "restart" (plan §A-Q4 ruling A). The
        // agent loop's `Arc<ModelRegistry>` and `WsState.models` are
        // built once at boot and can't live-adopt a new config.
        restart_required: true,
    };

    json_response(StatusCode::OK, &body)
}

// ===========================================================================
// Router (plan §D S3).
// ===========================================================================

/// POST /api/setup/install — track I, plan §B. Body `{}` (ignored by
/// the adapter). Success carries the §B `InstallResponse`; errors map
/// per the §B error table (Unsupported→501, anything else→500).
async fn post_install(State(state): State<Arc<SetupState>>, req: Request<Body>) -> Response {
    if let Err(resp) = require_json_content_type(&req) {
        return *resp;
    }
    let body = match read_json_body(req).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    // Plan §E.10: blocking subprocesses belong in spawn_blocking, not
    // on the runtime. install_binary itself is short but the future
    // path runs `systemctl` (in the service adapter) — keep the
    // dispatch async-correct.
    let install = state.install;
    let result = tokio::task::spawn_blocking(move || (install.0)(body))
        .await
        .unwrap_or_else(|e| {
            Err(SetupOpError::internal(
                format!("install task panicked: {e}"),
                Vec::new(),
            ))
        });
    finish_op(result)
}

/// GET /api/setup/service — track I, plan §B. Body is empty (the
/// handler synthesises `{"op":"status"}` so the adapter stays
/// uniform across the three verbs).
async fn get_service(State(state): State<Arc<SetupState>>) -> Response {
    let service = state.service;
    let result =
        tokio::task::spawn_blocking(move || (service.0)(serde_json::json!({"op":"status"})))
            .await
            .unwrap_or_else(|e| {
                Err(SetupOpError::internal(
                    format!("service task panicked: {e}"),
                    Vec::new(),
                ))
            });
    finish_op(result)
}

/// POST /api/setup/service — track I, plan §B. Body `{op:"install",
/// bind?, token?}`. 422 on PlanError::TokenRequired; same mapping
/// table for the rest.
async fn post_service(State(state): State<Arc<SetupState>>, req: Request<Body>) -> Response {
    if let Err(resp) = require_json_content_type(&req) {
        return *resp;
    }
    let body = match read_json_body(req).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    // Validate `op` *before* dispatching — the wizard should never be
    // able to trigger a 500 by sending an unknown verb. The §B
    // verb set is `install` (POST) / `status` (GET) / `uninstall`
    // (DELETE); POST also accepts a bare `{bind, token}` and we
    // normalise that to `op:"install"`.
    let mut body = body;
    if let Some(op) = body.get("op").and_then(|v| v.as_str()) {
        if !matches!(op, "install" | "status" | "uninstall") {
            return ApiError::simple(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("unknown service op: {op:?} (expected install, status, or uninstall)"),
            );
        }
    } else if body.as_object().is_some()
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("op".to_string(), serde_json::json!("install"));
    }
    let service = state.service;
    let result = tokio::task::spawn_blocking(move || (service.0)(body))
        .await
        .unwrap_or_else(|e| {
            Err(SetupOpError::internal(
                format!("service task panicked: {e}"),
                Vec::new(),
            ))
        });
    finish_op(result)
}

/// DELETE /api/setup/service — track I, plan §B. Body is empty; the
/// handler synthesises `{"op":"uninstall"}`.
async fn delete_service(State(state): State<Arc<SetupState>>) -> Response {
    let service = state.service;
    let result =
        tokio::task::spawn_blocking(move || (service.0)(serde_json::json!({"op":"uninstall"})))
            .await
            .unwrap_or_else(|e| {
                Err(SetupOpError::internal(
                    format!("service task panicked: {e}"),
                    Vec::new(),
                ))
            });
    finish_op(result)
}

/// Render the seam's result as the final HTTP response. The success
/// path is the adapter's JSON verbatim (already shaped per §B); the
/// error path is the §B envelope (one `error` + optional `problems`).
fn finish_op(result: Result<JsonValue, SetupOpError>) -> Response {
    match result {
        Ok(v) => json_response(StatusCode::OK, &v),
        Err(e) => {
            let problems = if e.problems.is_empty() {
                None
            } else {
                Some(e.problems)
            };
            json_response(
                e.status,
                &ApiError {
                    error: e.error,
                    problems,
                },
            )
        }
    }
}

/// Build the setup router. Mount this in `WebUi::run` **before** the token
/// layer so every `/api/setup/*` route is gated by the same `require_token`
/// as `/ws` and `/commands`. The returned router carries no auth — the
/// caller adds it.
pub fn router(state: SetupState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/api/setup", get(get_setup))
        .route("/api/setup/config", post(post_config))
        .route("/api/setup/install", post(post_install))
        .route(
            "/api/setup/service",
            get(get_service).post(post_service).delete(delete_service),
        )
        .with_state(state)
}

// ===========================================================================
// Pure tests for the small helpers — no axum, no I/O.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_json_content_type_accepts_application_json() {
        let req = Request::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .unwrap();
        assert!(require_json_content_type(&req).is_ok());
    }

    #[test]
    fn require_json_content_type_accepts_charset_suffix() {
        let req = Request::builder()
            .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
            .body(Body::empty())
            .unwrap();
        assert!(require_json_content_type(&req).is_ok());
    }

    #[test]
    fn require_json_content_type_rejects_text_plain() {
        let req = Request::builder()
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::empty())
            .unwrap();
        let resp = require_json_content_type(&req).unwrap_err();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn require_json_content_type_rejects_missing_header() {
        let req = Request::builder().body(Body::empty()).unwrap();
        let resp = require_json_content_type(&req).unwrap_err();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[test]
    fn facts_base_current_populates_os_and_arch() {
        let fb = FactsBase::current();
        assert!(!fb.os.is_empty());
        assert!(!fb.arch.is_empty());
    }

    #[test]
    fn install_fn_default_for_tests_is_a_no_op() {
        let f = InstallFn::default_for_tests();
        let v = (f.0)(serde_json::json!({})).unwrap();
        assert_eq!(v["status"], "not_implemented");
    }

    #[test]
    fn service_fn_default_for_tests_is_a_no_op() {
        let f = ServiceFn::default_for_tests();
        let v = (f.0)(serde_json::json!({})).unwrap();
        assert_eq!(v["status"], "not_implemented");
    }

    #[test]
    fn read_existing_returns_absent_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        assert!(matches!(read_existing(&path), ExistingConfig::Absent));
    }

    #[test]
    fn read_existing_returns_ok_with_keys_for_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            "provider:\n  type: openrouter\n  config:\n    model: x\n",
        )
        .unwrap();
        match read_existing(&path) {
            ExistingConfig::Ok { config } => {
                assert!(config.get("provider").is_some());
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn read_existing_returns_error_with_message_for_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "this: is: not: valid: yaml: at: all:\n").unwrap();
        match read_existing(&path) {
            ExistingConfig::Error { message } => assert!(!message.is_empty()),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
