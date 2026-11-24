//! `/api/cloud/*` — the Cloud Mode web surface (p3-design §H). One `Router`,
//! merged into the app INSIDE the token layer (same auth as
//! `/api/conversations`), in BOTH editions. When `general.cloud` is absent
//! every route returns `404 {"error":"cloud_disabled"}` (the router stays
//! mounted so P4's UI can probe one route).
//!
//! All bodies are JSON. Errors: `{"error": "<code>", "message": "…"}`.
//! `CloudError` maps: `NotActive` → 409 `not_active`, `Trust` → 409
//! `trust`, `Offline` → 504 `offline`, `UnknownDevice` → 404
//! `unknown_device`, `Io`/`Reset` → 502 `relay` (a `Reset(PROTOCOL)` — what a
//! peer's `Close{TRUST}` becomes — is 502 `refused`). A `DeviceId` in a path
//! is 64 lowercase hex; bad → 400 `bad_device_id`.
//!
//! §H.3 settings filter: the six mutating routes (login, genesis, approve,
//! reject, revoke, logout) return `403 {"error":"settings_disabled"}` when
//! the settings tab is filtered off; `state`, `devices`, `profiles` and
//! `events` stay readable. One check in the router (a `route_layer` on the
//! mutating sub-router).
//!
//! §H.2: `GET /api/cloud/events` is ONE SSE endpoint — the current
//! `CloudStateDto` as `event: state` on connect, then one full-DTO
//! `event: state` per `watch` change, plus a 15 s keep-alive comment.
//!
//! No route approves implicitly: `enroll/approve` needs an explicit
//! `letter_id` in the body (no "approve the oldest", no GET, no default).

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{Next, from_fn_with_state};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cloud::{DeviceId, LetterId};
use futures::Stream;
use serde_json::json;

use crate::cloud::link::device_slugs;
use crate::cloud::session::ProfileInfo;
use crate::cloud::{
    CloudError, CloudHandle, CloudState, DeviceInfo, EnrollRequestInfo, FlowError, LogoutReason,
    TrustError,
};
use crate::config::file::TabColor;

const KEEPALIVE: Duration = Duration::from_secs(15);

/// The router state. `handle: None` ⇔ no `general.cloud` (every route
/// 404s `cloud_disabled`). `settings_enabled` is the boot-bound
/// "is the settings tab visible for the pinned profile" predicate (H.3).
#[derive(Clone)]
pub struct CloudApiState {
    pub handle: Option<CloudHandle>,
    pub settings_enabled: bool,
}

/// The §H.1 route table (plus the SSE channel).
pub fn router(state: CloudApiState) -> Router {
    let mutating = Router::new()
        .route("/api/cloud/login", post(login))
        .route("/api/cloud/genesis", post(genesis))
        .route("/api/cloud/enroll/approve", post(approve))
        .route("/api/cloud/enroll/reject", post(reject))
        .route("/api/cloud/enroll/retry", post(retry))
        .route("/api/cloud/devices/{id}/revoke", post(revoke))
        .route("/api/cloud/logout", post(logout))
        .route_layer(from_fn_with_state(state.clone(), settings_gate));
    Router::new()
        .route("/api/cloud/state", get(get_state))
        .route("/api/cloud/devices", get(devices))
        .route("/api/cloud/devices/{id}/profiles", get(profiles))
        .route("/api/cloud/events", get(events))
        .merge(mutating)
        // Outermost: "cloud off" answers before anything else.
        .layer(from_fn_with_state(state.clone(), disabled_gate))
        .with_state(state)
}

/// `{"error": code, "message": text}` with a status.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn disabled() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "cloud_disabled",
            "Cloud Mode is not configured (general.cloud)",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.code, "message": self.message })),
        )
            .into_response()
    }
}

impl From<CloudError> for ApiError {
    fn from(e: CloudError) -> Self {
        let message = e.to_string();
        let (status, code) = match &e {
            CloudError::Disabled => (StatusCode::NOT_FOUND, "cloud_disabled"),
            CloudError::NotActive(_) => (StatusCode::CONFLICT, "not_active"),
            CloudError::Trust(_) => (StatusCode::CONFLICT, "trust"),
            CloudError::Offline => (StatusCode::GATEWAY_TIMEOUT, "offline"),
            CloudError::UnknownDevice => (StatusCode::NOT_FOUND, "unknown_device"),
            CloudError::SelfTarget => (StatusCode::BAD_REQUEST, "self_target"),
            CloudError::Reset(c) if *c == cloud::constants::reset::PROTOCOL => {
                (StatusCode::BAD_GATEWAY, "refused")
            }
            CloudError::Io(_) | CloudError::Reset(_) => (StatusCode::BAD_GATEWAY, "relay"),
            CloudError::Flow(f) => match f {
                FlowError::AlreadyLoggedIn => (StatusCode::CONFLICT, "already_logged_in"),
                FlowError::NotPendingEmpty => (StatusCode::CONFLICT, "not_pending_empty"),
                FlowError::NotRejected => (StatusCode::CONFLICT, "not_rejected"),
                FlowError::GenesisTaken => (StatusCode::CONFLICT, "genesis_taken"),
                FlowError::UnknownRequest => (StatusCode::NOT_FOUND, "unknown_request"),
                FlowError::ChainBusy => (StatusCode::CONFLICT, "chain_busy"),
                FlowError::UseLogout => (StatusCode::BAD_REQUEST, "use_logout"),
                FlowError::LastDevice => (StatusCode::CONFLICT, "last_device"),
                FlowError::LoginInFlight => (StatusCode::CONFLICT, "login_in_flight"),
                FlowError::NoLoginPending => (StatusCode::BAD_REQUEST, "no_login_pending"),
            },
        };
        Self::new(status, code, message)
    }
}

type ApiResult = Result<Json<serde_json::Value>, ApiError>;

async fn disabled_gate(State(s): State<CloudApiState>, req: Request, next: Next) -> Response {
    if s.handle.is_none() {
        return ApiError::disabled().into_response();
    }
    next.run(req).await
}

async fn settings_gate(State(s): State<CloudApiState>, req: Request, next: Next) -> Response {
    if !s.settings_enabled {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "settings_disabled",
            "the settings tab is disabled for this profile",
        )
        .into_response();
    }
    next.run(req).await
}

fn handle(s: &CloudApiState) -> Result<&CloudHandle, ApiError> {
    s.handle.as_ref().ok_or_else(ApiError::disabled)
}

// ── DTOs ───────────────────────────────────────────────────────────────

/// A 1:1 render of `CloudState` (p3-design §D.1 + §G), tagged by `state`
/// (H.1 `CloudStateDto`).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CloudStateDto {
    Disabled,
    LoggedOut {
        reason: String,
    },
    Connecting {
        attempt: u32,
        last_error: Option<String>,
    },
    Pending {
        empty_account: bool,
        sas: Option<Vec<String>>,
        rejected: bool,
    },
    Active {
        devices: Vec<DeviceDto>,
        requests: Vec<RequestDto>,
    },
    Trust {
        trust_error: String,
    },
}

/// `{ "id": hex, "name": str, "platform": str, "online": true|false|null,
/// "slug": str, "self": bool }`. `online` is always present: `null` =
/// unknown (no relay presence, D.1).
#[derive(Debug, serde::Serialize)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub online: Option<bool>,
    pub slug: String,
    #[serde(rename = "self")]
    pub self_: bool,
}

/// `{ "letter_id": hex, "device_id": hex, "name": str, "platform": str,
/// "sas": [6 words], "expires_at": unix_s }`.
#[derive(Debug, serde::Serialize)]
pub struct RequestDto {
    pub letter_id: String,
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub sas: Vec<String>,
    pub expires_at: u64,
}

/// The state body and SSE payload: the DTO plus the any-state
/// `last_login_error` (G.1 step 5).
#[derive(Debug, serde::Serialize)]
struct StateBody {
    #[serde(flatten)]
    state: CloudStateDto,
    last_login_error: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Exactly `2 * N` lowercase hex chars → `N` bytes.
fn parse_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// 64 lowercase hex → `DeviceId` (parsed once in the extractor; bad →
/// 400 `bad_device_id`).
pub fn parse_device_id(hex: &str) -> Option<DeviceId> {
    parse_hex::<32>(hex)
}

fn device_id_or_400(raw: &str) -> Result<DeviceId, ApiError> {
    parse_device_id(raw).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_device_id",
            "a device id is 64 lowercase hex characters",
        )
    })
}

fn device_dtos(devices: &[DeviceInfo], own: Option<DeviceId>) -> Vec<DeviceDto> {
    let slugs = device_slugs(devices);
    devices
        .iter()
        .zip(slugs)
        .map(|(d, slug)| DeviceDto {
            id: hex(&d.id),
            name: d.name.clone(),
            platform: d.platform.clone(),
            online: d.online,
            slug,
            self_: own == Some(d.id),
        })
        .collect()
}

fn request_dto(r: &EnrollRequestInfo) -> RequestDto {
    RequestDto {
        letter_id: hex(&r.letter_id),
        device_id: hex(&r.device),
        name: r.name.clone(),
        platform: r.platform.clone(),
        sas: r.sas.words().iter().map(|w| w.to_string()).collect(),
        expires_at: r.expires_at,
    }
}

/// `/profiles` rows: the peer's own colour never crosses; instead every row
/// carries the requesting host's per-host accent. An unknown slug omits
/// `color` entirely (never `null`).
pub fn profile_dtos(profiles: &[ProfileInfo], slug: Option<&str>) -> Vec<serde_json::Value> {
    profiles
        .iter()
        .map(|p| {
            let mut row = json!({ "name": p.name.clone() });
            if let Some(slug) = slug {
                row["color"] = json!(TabColor::derived(slug));
            }
            row
        })
        .collect()
}

impl CloudStateDto {
    fn from_state(st: &CloudState, own: Option<DeviceId>) -> Self {
        match st {
            CloudState::Disabled => Self::Disabled,
            CloudState::LoggedOut { reason } => Self::LoggedOut {
                reason: match reason {
                    LogoutReason::Never => "never",
                    LogoutReason::Expired => "expired",
                    LogoutReason::SignedOut => "signed_out",
                }
                .to_string(),
            },
            CloudState::Connecting {
                attempt,
                last_error,
            } => Self::Connecting {
                attempt: *attempt,
                last_error: last_error.clone(),
            },
            CloudState::Pending {
                empty_account,
                sas,
                rejected,
            } => Self::Pending {
                empty_account: *empty_account,
                rejected: *rejected,
                sas: sas
                    .as_ref()
                    .map(|s| s.words().iter().map(|w| w.to_string()).collect()),
            },
            CloudState::Active { devices, requests } => Self::Active {
                devices: device_dtos(devices, own),
                requests: requests.iter().map(request_dto).collect(),
            },
            CloudState::Trust(e) => Self::Trust {
                trust_error: match e {
                    TrustError::GenesisMismatch => "genesis_mismatch",
                    TrustError::Fork => "fork",
                    TrustError::Revoked => "revoked",
                    TrustError::ChainInvalid(_) => "chain_invalid",
                    TrustError::SeedStoreError(_) => "seed_store_error",
                }
                .to_string(),
            },
        }
    }
}

fn state_body(h: &CloudHandle) -> StateBody {
    // Clone out of the watch so no `Ref` outlives this call.
    let st = h.state().borrow().clone();
    StateBody {
        state: CloudStateDto::from_state(&st, h.device_id()),
        last_login_error: h.last_login_error(),
    }
}

// ── Handlers ───────────────────────────────────────────────────────────

async fn get_state(State(s): State<CloudApiState>) -> Result<Json<StateBody>, ApiError> {
    Ok(Json(state_body(handle(&s)?)))
}

async fn login(State(s): State<CloudApiState>) -> ApiResult {
    let h = handle(&s)?;
    let mut auth_url = None;
    h.login(|url| auth_url = Some(url)).await?;
    let url = auth_url.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "relay",
            "login produced no url",
        )
    })?;
    Ok(Json(json!({ "auth_url": url.as_str() })))
}

async fn genesis(State(s): State<CloudApiState>) -> ApiResult {
    let h = handle(&s)?;
    let first_device = matches!(
        &*h.state().borrow(),
        CloudState::Pending {
            empty_account: true,
            ..
        }
    );
    if !first_device {
        return Err(CloudError::Flow(FlowError::NotPendingEmpty).into());
    }
    let id: DeviceId = h.genesis().await?;
    Ok(Json(json!({ "device_id": hex(&id) })))
}

async fn devices(State(s): State<CloudApiState>) -> ApiResult {
    let h = handle(&s)?;
    let st = h.state().borrow().clone();
    match st {
        CloudState::Active { devices, .. } => {
            let own = h.device_id();
            Ok(Json(json!({
                "self": own.map(|id: DeviceId| hex(&id)),
                "devices": device_dtos(&devices, own),
            })))
        }
        CloudState::Trust(e) => Err(CloudError::Trust(e).into()),
        other => Err(CloudError::NotActive(other.kind()).into()),
    }
}

/// The only accepted approve/reject body: an explicit `letter_id`.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LetterReq {
    letter_id: String,
}

/// Parse the body by hand so a bad one is the JSON error envelope, not
/// axum's plain-text rejection. No body / no id = 400: nothing is ever
/// approved or rejected by default.
fn letter_id_from(body: &Bytes) -> Result<LetterId, ApiError> {
    let req: LetterReq = serde_json::from_slice(body).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body must be {\"letter_id\": <32 hex>}",
        )
    })?;
    parse_hex::<16>(&req.letter_id).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_letter_id",
            "a letter id is 32 lowercase hex characters",
        )
    })
}

async fn approve(State(s): State<CloudApiState>, body: Bytes) -> ApiResult {
    let h = handle(&s)?;
    let id: DeviceId = h.approve(letter_id_from(&body)?).await?;
    Ok(Json(json!({ "device_id": hex(&id) })))
}

async fn reject(State(s): State<CloudApiState>, body: Bytes) -> ApiResult {
    let h = handle(&s)?;
    h.reject(letter_id_from(&body)?).await?;
    Ok(Json(json!({})))
}

async fn retry(State(s): State<CloudApiState>) -> ApiResult {
    let h = handle(&s)?;
    h.retry_enroll().await?;
    Ok(Json(json!({})))
}

async fn revoke(State(s): State<CloudApiState>, Path(id): Path<String>) -> ApiResult {
    let h = handle(&s)?;
    let target = device_id_or_400(&id)?;
    h.revoke(target).await?;
    Ok(Json(json!({})))
}

async fn logout(State(s): State<CloudApiState>) -> ApiResult {
    let h = handle(&s)?;
    let out = h.logout().await?;
    Ok(Json(json!({ "revoked": out.revoked })))
}

async fn profiles(State(s): State<CloudApiState>, Path(id): Path<String>) -> ApiResult {
    let h = handle(&s)?;
    let device = device_id_or_400(&id)?;
    // Resolve the host slug from the active device list — the same
    // `device_slugs` dedupe routing uses. The snapshot ends before the
    // actor round-trip below.
    let st = h.state().borrow().clone();
    let slug = match &st {
        CloudState::Active { devices, .. } => {
            let slugs = device_slugs(devices);
            devices
                .iter()
                .position(|d| d.id == device)
                .map(|i| slugs[i].clone())
        }
        // Not active: `profiles` below returns the existing error.
        _ => None,
    };
    let list = h.profiles(device).await?;
    Ok(Json(
        json!({ "profiles": profile_dtos(&list, slug.as_deref()) }),
    ))
}

/// `GET /api/cloud/events` — the full state on connect, then on every
/// `watch` change (H.2). `borrow_and_update` first, so a change racing the
/// connect is never both missed and replayed. An identical payload is not
/// re-sent (the actor republishes equal states, e.g. on re-entering Idle).
async fn events(
    State(s): State<CloudApiState>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let h = handle(&s)?.clone();
    let rx = h.state();
    let stream = futures::stream::unfold((h, rx, None::<String>), |(h, mut rx, last)| async move {
        let mut first = last.is_none();
        loop {
            if !first && rx.changed().await.is_err() {
                return None;
            }
            first = false;
            rx.borrow_and_update();
            // Compact JSON has no raw newlines, so `data` never splits.
            let json = serde_json::to_string(&state_body(&h)).unwrap_or_else(|_| "{}".to_string());
            if last.as_deref() != Some(json.as_str()) {
                let ev = Event::default().event("state").data(json.clone());
                return Some((Ok(ev), (h, rx, Some(json))));
            }
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE)))
}
