//! `GET /api/conversations` and `POST /api/conversations/{convo}/kill` — the
//! HTTP face of the picker's list/kill, for a relaying server (remote-profiles
//! design §6.2). Local rows only; neither route creates or attaches a session.
//!
//! Merged before the token layer in `WebUi::run`, so both are token-gated.

use super::{KILL_RUNNING_MSG, WsState, registry::KillOutcome, registry::SessionRegistry};
use crate::ui::wire::{ConversationSummaryWire, summaries_to_wire};
use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use uuid::Uuid;

pub(crate) fn router(state: WsState) -> Router {
    Router::new()
        .route("/api/conversations", get(list))
        .route("/api/conversations/{convo}/kill", post(kill))
        .with_state(state)
}

/// `?limit=N` bounds the reply; absent keeps the legacy unlimited response.
/// Axum's `Query` makes a malformed value a 400 and `limit=0` an empty list.
#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

async fn list(State(state): State<WsState>, Query(query): Query<ListQuery>) -> Response {
    match state.registry {
        Some(registry) => items_response(&registry, None, query.limit),
        None => super::ws_setup_guard().await,
    }
}

async fn kill(State(state): State<WsState>, Path(convo): Path<String>) -> Response {
    let Some(registry) = state.registry else {
        return super::ws_setup_guard().await;
    };
    let Ok(id) = Uuid::parse_str(&convo) else {
        return (StatusCode::BAD_REQUEST, "invalid conversation id").into_response();
    };
    // Same outcome mapping as the `kill_session` WS frame: only a refused
    // running kill is an error; `NotLive` is a plain list.
    let refused = registry.kill(id) == KillOutcome::Running;
    items_response(&registry, refused.then_some(KILL_RUNNING_MSG), None)
}

/// `{"items":[…]}` (+ `error`), built AFTER any kill so the rows reflect it.
fn items_response(
    registry: &SessionRegistry,
    error: Option<&str>,
    limit: Option<usize>,
) -> Response {
    let mut body = serde_json::json!({ "items": local_rows(registry, limit) });
    if let Some(e) = error {
        body["error"] = e.into();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(body.to_string()),
    )
        .into_response()
}

/// This server's saved conversations, newest first, with the registry's
/// live flags. Needs no session, so relay sockets can use it too.
pub(crate) fn local_rows(
    registry: &SessionRegistry,
    limit: Option<usize>,
) -> Vec<ConversationSummaryWire> {
    let mut rows = registry.list_conversations();
    // Storage order is unspecified (the in-memory store is a HashMap).
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
    // The bound applies to the sorted list, so a limit keeps the newest rows.
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    summaries_to_wire(rows, &registry.active_ids(), &registry.running_ids())
}
