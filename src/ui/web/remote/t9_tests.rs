//! Remote-profiles T9 — server side of "make it reachable" (design §5.11,
//! §9 T9). RED until T9: the attach routing in `handle_socket` decides
//! "is this a remote profile" — and resolves the target profile P and the
//! agent pin — from the BOOT catalogue (`registry.boot_catalog()`), while
//! the `Directory` re-reads the global config on every call. So a remote
//! profile added or edited in the global config AFTER boot is not routable
//! until a restart.
//!
//! §5.11 is the contract these tests are written against: "Adding, editing
//! or removing a remote profile in Settings takes effect on the **next
//! attach**." The harness reuses `t78_support` (the real local server
//! pieces behind the same `require_token` layer, a plain-HTTP loopback fake
//! remote, the WS helpers).

use super::t78_support::{
    FakeRemote, LocalEnv, RELAY_CONVO, T_REMOTE, Ws, next_frame, wait_type, ws_connect, ws_send,
    yaml,
};
use crate::ui::wire::PROTOCOL_VERSION;
use serde_json::{Value, json};

/// Attach a browser socket to remote profile `l`; expect the relay to have
/// dialed the fake remote and forwarded its handshake (`attached` with the
/// REMOTE id + `ready{v}`).
async fn attach_remote(env: &LocalEnv, l: &str) -> Ws {
    let mut ws = ws_connect(env.addr).await;
    ws_send(
        &mut ws,
        &json!({ "type": "attach", "convo": null, "profile": l }),
    )
    .await;
    let attached = wait_type(&mut ws, "attached").await;
    assert_eq!(
        attached["convo"], RELAY_CONVO,
        "a local attach would bind a local id, not the remote's: {attached}"
    );
    let ready = wait_type(&mut ws, "ready").await;
    assert_eq!(
        ready["v"],
        json!(PROTOCOL_VERSION),
        "the relay forwards the remote's ready"
    );
    ws
}

/// The target profile the relay's Nth attach named on the remote (`attach`
/// frames it records).
fn relay_target(remote: &FakeRemote, nth: usize) -> Value {
    let frames = remote.ws_frames();
    let frame = frames.get(nth).unwrap_or_else(|| {
        panic!(
            "the remote saw {} attach frames, wanted #{}",
            frames.len(),
            nth
        )
    });
    let v: Value = serde_json::from_str(frame).expect("an attach frame");
    assert_eq!(v["type"], "attach");
    v["profile"].clone()
}

/// §5.11: a remote profile added to the global config after boot routes on
/// the NEXT attach — no restart. The relay must dial the remote (the tab
/// binds to the REMOTE conversation) and mint no local session (G7).
#[tokio::test]
async fn attach_routes_a_remote_profile_added_after_boot() {
    let remote = FakeRemote::start(T_REMOTE).await;
    // Boot with NO remote profiles at all.
    let env = LocalEnv::start(&[], None).await;

    std::fs::write(
        env.config_path(),
        yaml(&[("home-dev", &remote.url, T_REMOTE, "dev")]),
    )
    .expect("add the remote profile after boot");

    let ws = attach_remote(&env, "home-dev").await;
    drop(ws);

    // The relay attached the remote with the configured target profile.
    assert_eq!(relay_target(&remote, 0), "dev");
    assert!(
        env.registry.active_ids().is_empty(),
        "the relay returns before `registry.attach`: no local session"
    );
}

/// §5.11: EDITING a remote profile after boot (here: its target profile P)
/// is live on the next attach — the relay re-resolves from a fresh parse,
/// not the boot catalogue.
#[tokio::test]
async fn attach_routes_a_remote_profile_edited_after_boot() {
    let remote = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[], None).await;
    std::fs::write(
        env.config_path(),
        yaml(&[("home-dev", &remote.url, T_REMOTE, "dev")]),
    )
    .expect("add the remote profile after boot");

    let ws = attach_remote(&env, "home-dev").await;
    drop(ws);
    assert_eq!(relay_target(&remote, 0), "dev");

    std::fs::write(
        env.config_path(),
        yaml(&[("home-dev", &remote.url, T_REMOTE, "ops")]),
    )
    .expect("edit the target profile");

    let ws = attach_remote(&env, "home-dev").await;
    drop(ws);
    assert_eq!(
        relay_target(&remote, 1),
        "ops",
        "the edited target must be live on the next attach"
    );
}

/// §5.11 E-CONFIG: a hand-broken global config makes remote attaches fail
/// with the locked E-CONFIG string — and never falls back to a local
/// session (G7).
#[tokio::test]
async fn broken_global_config_refuses_remote_attach_with_econfig_and_no_local_session() {
    let remote = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[], None).await;
    std::fs::write(
        env.config_path(),
        yaml(&[("home-dev", &remote.url, T_REMOTE, "dev")]),
    )
    .expect("add the remote profile after boot");
    std::fs::write(env.config_path(), "providers: [this is: not valid")
        .expect("break the global config");

    let mut ws = ws_connect(env.addr).await;
    ws_send(
        &mut ws,
        &json!({ "type": "attach", "convo": null, "profile": "home-dev" }),
    )
    .await;

    let mut saw_econfig = false;
    for _ in 0..200 {
        match next_frame(&mut ws, 15).await {
            Ok(f) if f["type"] == "error" => {
                let msg = f["message"].as_str().unwrap_or_default();
                assert!(
                    msg.starts_with(
                        "config.yaml has an error, so remote profiles are unavailable:"
                    ),
                    "the locked E-CONFIG string: {f}"
                );
                saw_econfig = true;
                break;
            }
            Ok(f) if f["type"] == "attached" => {
                panic!("a broken config must never fall back to a local session: {f}")
            }
            Ok(_) => continue,
            Err(_closed) => break,
        }
    }
    assert!(saw_econfig, "the E-CONFIG error must reach the browser");
    assert!(
        env.registry.active_ids().is_empty(),
        "no local session may be minted"
    );
}
