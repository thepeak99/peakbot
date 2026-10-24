//! `/api/system/*` — install and service management (0.24 cutover).
//!
//! The setup wizard is gone (`/api/setup*` → 404, see [`setup_removed`]).
//! Its install/service surface moved here **verbatim** under
//! `/api/system/*` (spec §8.2): `GET/POST/DELETE /api/system/service` and
//! `POST /api/system/install`. The `install` and `service` ops stay
//! fn-pointer seams (`InstallFn` / `ServiceFn`) so tests can inject fakes
//! without copying a binary or shelling out to `systemctl`.
//!
//! The token layer is **not** wired here: tests attach it themselves so
//! the auth contract is exercised independently. Production mounting
//! merges the returned router into `WebUi::run` **before** the token
//! layer, so every `/api/system/*` route is gated by the same
//! `require_token` as `/ws` and `/commands`.

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
use std::sync::Arc;

// ===========================================================================
// InstallPath — the wire form of the PATH verdict (spec §B tagged union).
// ===========================================================================

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

// ===========================================================================
// InstallFn / ServiceFn — fn-pointer seams (plan §E.9).
//
// The production adapters (below) are the real `install::install_binary`
// and `install::service_op` dispatchers; `default_for_tests()` returns a
// no-op so a handler test never copies a binary into $HOME or shells
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
    /// No-op install op. The production adapter is the real dispatcher.
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
    /// No-op service op. The production adapter is the real dispatcher.
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
/// "bind"?, "token"?}`). The token, when given, rides the
/// `ServicePlan` into the service argv-free world: the service reads
/// `general.web.token` from config.yaml at start (spec §8.4), so this
/// adapter never touches a token *file* — the `<config_dir>/web-token`
/// file was retired with the 0.24 layout.
pub fn service_op_adapter(req: JsonValue) -> Result<JsonValue, SetupOpError> {
    use crate::install::{ServiceOp, ServicePlan};
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
            let report = crate::install::service_op(ServiceOp::Uninstall)
                .map_err(|e| SetupOpError::from_install_error(&e))?;
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
            // The token, when the client sends one, travels in the
            // `ServicePlan`; the service picks it up from
            // `general.web.token` at start (spec §8.4).
            let token_str = req.get("token").and_then(|v| v.as_str());
            let token_trim = token_str.map(str::trim).filter(|t| !t.is_empty());
            let owned_token: Option<String> = token_trim.map(str::to_string);
            // B3 (desktop design §7): when the install target does not
            // exist, run the binary install FIRST — the same copy the
            // `peakbot install` CLI verb performs — and point the service
            // at the target. The client must never point a service at a
            // path that does not exist (it matters for the desktop, where
            // the settings screen runs inside the window).
            let exe = match crate::install::install_target().as_ref() {
                Some(t) if t.exists() => t.clone(),
                _ => {
                    let outcome = crate::install::install_binary()
                        .map_err(|e| SetupOpError::from_install_error(&e))?;
                    outcome.target
                }
            };
            let plan = ServicePlan::new(exe, bind, owned_token)
                .map_err(|e| SetupOpError::from_plan_error(&e))?;
            let report = crate::install::service_op(ServiceOp::Install { plan })
                .map_err(|e| SetupOpError::from_install_error(&e))?;
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
        "tls": r.tls,
        "survives_logout": r.survives_logout,
        "commands": r.commands,
        "notes": r.notes,
    })
}

// ===========================================================================
// SystemState — built once at boot, cloned into the router.
// ===========================================================================

/// Per-handler state injected by [`router`]. The handler never re-derives
/// paths or facts; everything is captured here at boot.
#[derive(Clone)]
pub struct SystemState {
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

/// POST /api/system/install — track I, plan §B. Body `{}` (ignored by
/// the adapter). Success carries the §B `InstallResponse`; errors map
/// per the §B error table (Unsupported→501, anything else→500).
async fn post_install(State(state): State<Arc<SystemState>>, req: Request<Body>) -> Response {
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

/// GET /api/system/service — track I, plan §B. Body is empty (the
/// handler synthesises `{"op":"status"}` so the adapter stays
/// uniform across the three verbs).
async fn get_service(State(state): State<Arc<SystemState>>) -> Response {
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

/// POST /api/system/service — track I, plan §B. Body `{op:"install",
/// bind?, token?}`. 422 on PlanError::TokenRequired; same mapping
/// table for the rest.
async fn post_service(State(state): State<Arc<SystemState>>, req: Request<Body>) -> Response {
    if let Err(resp) = require_json_content_type(&req) {
        return *resp;
    }
    let body = match read_json_body(req).await {
        Ok(v) => v,
        Err(resp) => return *resp,
    };
    // Validate `op` *before* dispatching — the client should never be
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

/// DELETE /api/system/service — track I, plan §B. Body is empty; the
/// handler synthesises `{"op":"uninstall"}`.
async fn delete_service(State(state): State<Arc<SystemState>>) -> Response {
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

// ===========================================================================
// Router (spec §8.2).
// ===========================================================================

/// Build the system router. Mount this in `WebUi::run` **before** the
/// token layer so every `/api/system/*` route is gated by the same
/// `require_token` as `/ws` and `/commands`. The returned router
/// carries no auth — the caller adds it.
pub fn router(state: SystemState) -> Router {
    let state = Arc::new(state);
    Router::new()
        .route("/api/system/install", post(post_install))
        .route(
            "/api/system/service",
            get(get_service).post(post_service).delete(delete_service),
        )
        .with_state(state)
}

/// The old `/api/setup*` surface, answered with 404 (spec B12). The SPA
/// fallback would otherwise serve `index.html` (200) for these paths,
/// which hides the removal. Only the routes that existed in 0.23 are
/// tombstoned; unknown sub-paths keep the SPA fallback like any other
/// unknown route.
pub fn setup_removed() -> Router {
    Router::new()
        .route("/api/setup", get(gone))
        .route("/api/setup/config", post(gone))
        .route("/api/setup/install", post(gone))
        .route("/api/setup/service", get(gone).post(gone).delete(gone))
}

/// 404 tombstone handler for the removed setup surface.
async fn gone() -> Response {
    ApiError::simple(
        StatusCode::NOT_FOUND,
        "the /api/setup routes were removed in 0.24 — use /api/settings and /api/system/*",
    )
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
}
