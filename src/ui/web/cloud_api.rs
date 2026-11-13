//! `/api/cloud/*` — the Cloud Mode web surface (p3-design §H). One `Router`,
//! merged into the app INSIDE the token layer (same auth as
//! `/api/conversations`), in BOTH editions. When `general.cloud` is absent
//! every route returns `404 {"error":"cloud_disabled"}` (the router stays
//! mounted so P4's UI can probe one route).
//!
//! All bodies are JSON. Errors: `{"error": "<code>", "message": "…"}`.
//! `CloudError` maps: `NotActive` → 409 `not_active`, `Trust` → 409
//! `trust`, `Offline` → 504 `offline`, `UnknownDevice` → 404
//! `unknown_device`, `Io`/`Reset` → 502 `relay`. A `DeviceId` in a path is
//! 64 lowercase hex; bad → 400 `bad_device_id`.
//!
//! §H.3 settings filter: the six mutating routes (login, genesis, approve,
//! reject, revoke, logout) return `403 {"error":"settings_disabled"}` when
//! the settings tab is filtered off; `state`, `devices`, `profiles` and
//! `events` stay readable. One check in the router (a `route_layer` on the
//! mutating sub-router), bound to the same predicate the settings routes
//! use (K.6).
//!
//! §H.2: `GET /api/cloud/events` is ONE SSE endpoint — the current
//! `CloudStateDto` as `event: state` on connect, then one full-DTO
//! `event: state` per `watch` change, plus a 15 s keep-alive comment.

#![allow(unused_variables)]

use crate::cloud::CloudHandle;
use axum::Router;
use cloud::DeviceId;

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
    todo!("p3 PR-B: /api/cloud/* routes + DTOs + SSE + settings gate")
}

/// A 1:1 render of `CloudState` (p3-design §D.1 + §G), tagged by `state`
/// (H.1 `CloudStateDto`).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
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
/// "slug": str, "self": bool }`.
#[derive(Debug, serde::Serialize)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
    pub slug: String,
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

/// 64 lowercase hex → `DeviceId` (parsed once in the extractor; bad →
/// 400 `bad_device_id`).
pub fn parse_device_id(hex: &str) -> Option<DeviceId> {
    todo!("p3 PR-B: parse_device_id")
}
