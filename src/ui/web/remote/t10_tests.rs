//! Unpinned `/ws` attach on a relay-only config (remotes through the
//! on-device core).
//!
//! RED until relay-only configs parse and an attach without a profile
//! takes the named-remote relay path.
//!
//! Harness: the shared t78 fixture — a real local `/ws` + conversations
//! router behind `require_token`, and a `FakeRemote` the home profile
//! points at (plain loopback HTTP: RP3 allows it without a `ca`).

use std::time::Duration;

use serde_json::{Value, json};

use super::t78_support::*;
use futures::StreamExt;
use tokio::time::error::Elapsed;
use tokio_tungstenite::tungstenite::Message as TMsg;

const L: &str = "dev on nas";

/// A relay-only global: the ONE remote profile, no local profile, no
/// providers — the phone's shape.
fn relay_only_yaml(url: &str) -> String {
    format!(
        "profiles:\n  \"{L}\":\n    remote:\n      url: {url}\n      token: {T_REMOTE}\n      profile: dev\n"
    )
}

/// A relay-only env over the given remote URL.
async fn relay_only_env(url: &str) -> LocalEnv {
    LocalEnv::start_yaml(relay_only_yaml(url), None).await
}

/// An attach with NO profile on a relay-only config takes the
/// named-remote path to the home remote — the client gets the remote's
/// ready handshake and the remote's conversations, and no local session
/// is minted.
#[tokio::test]
async fn unpinned_attach_relays_to_the_home_remote() {
    let remote = FakeRemote::start(T_REMOTE).await;
    let id = uid(9);
    remote.set_rows(vec![row(
        &id,
        "Remote convo",
        "2026-11-08T10:00:00Z",
        Some("dev"),
    )]);
    let env = relay_only_env(&remote.url).await;

    // `connect_local` attaches with no profile and waits for `ready` —
    // on a relay-only config that ready must be the REMOTE's, forwarded.
    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    let r = find(&list, &id).expect("the home remote's row is in the merged list");
    assert_eq!(
        r["profile"], L,
        "the remote row is tagged with the local name"
    );
    assert!(
        env.registry.active_ids().is_empty(),
        "an unpinned relay attach mints no local session"
    );
}

/// All frames until the socket closes (bounded); `Err` on timeout.
async fn frames_until_close(ws: &mut Ws, secs: u64) -> Result<Vec<Value>, Elapsed> {
    let mut frames = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Err(elapsed) => return Err(elapsed),
            Ok(None) | Ok(Some(Ok(TMsg::Close(_)))) => return Ok(frames),
            Ok(Some(Ok(TMsg::Text(t)))) => {
                frames.push(serde_json::from_str(&t).expect("server frames are JSON"));
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) => return Ok(frames),
        }
    }
}

/// The home remote is down at dial → the UNPINNED attach gets
/// exactly ONE locked E-DOWN error frame, then close — no local session,
/// no local fallback.
#[tokio::test]
async fn unpinned_attach_to_a_down_home_remote_answers_one_edown_error() {
    with_dead_port(|port| async move {
        // Plain loopback HTTP: RP3 allows it without a `ca`, so the ONLY
        // failure in this test is the refused dial (E-DOWN).
        let env = relay_only_env(&format!("http://127.0.0.1:{port}")).await;

        let mut ws = ws_connect(env.addr).await;
        ws_send(&mut ws, &json!({ "type": "attach", "convo": null })).await;
        let frames = frames_until_close(&mut ws, 10)
            .await
            .expect("the down-remote attach is bounded");

        let errs: Vec<&Value> = frames.iter().filter(|f| f["type"] == "error").collect();
        let msg = errs
            .first()
            .and_then(|f| f["message"].as_str())
            .unwrap_or("");
        if !msg.starts_with(&format!(
            "Can't reach remote profile '{L}' at 127.0.0.1:{port}: "
        )) {
            // Not E-DOWN: the probed port was taken by a foreign listener
            // (a sibling test's server) between probe and dial.
            return Err(msg.to_string());
        }
        assert_eq!(errs.len(), 1, "one locked E-DOWN error: {frames:?}");
        assert!(
            msg.ends_with('.'),
            "locked string ends with a period: {msg}"
        );
        assert!(env.registry.active_ids().is_empty(), "no local session");
        Ok(())
    })
    .await
    .expect("the down-remote attach answers one locked E-DOWN error");
}
