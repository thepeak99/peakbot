//! Remote-profiles T7 — the merged conversation list and kill routing
//! (design §5.6, §5.12 "list: remote down", §6.2, §6.3 rows 11/12, §7, §9-T7).
//! RED until the implementation lands.
//!
//! Written against the interface the design specifies, NOT an existing impl.
//! Everything is driven through the wire, so the implementer is free in the
//! internals:
//!
//! * `request_conversations` (a LOCAL socket and a RELAY socket) is answered
//!   locally with `conversations_list{ items = local rows ∪ every remote
//!   profile's rows, error: None }`, newest `updated_at` first (§5.6).
//! * A remote row is kept only if its `profile` P has a local remote profile
//!   L with (same server, P) (RP4); the row's `profile` is rewritten to L. Rows
//!   with another profile, or none (legacy), are dropped. Ids and every other
//!   field are untouched (`active`/`running` are the REMOTE registry's).
//! * One `GET /api/conversations` per distinct remote SERVER and request,
//!   concurrently, each ≤ 2 s (`LIST_TIMEOUT`). A failing server (refused,
//!   hung, non-2xx, not JSON, 401) just contributes no rows; `error` stays
//!   absent. A broken global config likewise yields local rows only.
//! * `kill_session{convo, profile: L}` with L a REMOTE profile → `Client::kill`
//!   (`POST /api/conversations/{convo}/kill` on that server, remote token
//!   cookie), the remote's `error` is forwarded verbatim as the reply's
//!   `error`, and the reply carries the freshly merged list. Any other
//!   `profile` (none, a local profile, an unknown name) is today's local kill.
//! * `GET /api/conversations` and `POST …/kill` over HTTP stay LOCAL-ONLY
//!   (§6.2, §5.9: that is what stops A→B→C recursion) — green guards below.
//!
//! Harness: `t78_support` — a real local `/ws` + conversations router behind
//! the SAME `require_token`, pointed at plain-HTTP loopback fake remotes
//! (RP3: `http://127.0.0.1` needs no `ca`).

#![allow(clippy::await_holding_lock)] // LocalEnv holds the crate-wide config lock

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::t78_support::*;
use super::test_support::Captured;
use crate::ui::web::KILL_RUNNING_MSG;

const L: &str = "home-dev";
const L2: &str = "vps";
const T_REMOTE_B: &str = "remote-token-b-t78-33cc";
/// Generous wall-clock slack over LIST_TIMEOUT (2 s) for CI.
const BOUND: Duration = Duration::from_millis(3500);

// ═══════════════════════════════════════════════════════════════════════
// Merged list (§5.6)
// ═══════════════════════════════════════════════════════════════════════

/// Two healthy remotes (`dev` on A as `home-dev`, `web` on B as `vps`) and an
/// env listing both.
async fn two_remotes() -> (FakeRemote, FakeRemote, LocalEnv) {
    let a = FakeRemote::start(T_REMOTE).await;
    let b = FakeRemote::start(T_REMOTE_B).await;
    let env = LocalEnv::start(
        &[
            (L, &a.url, T_REMOTE, "dev"),
            (L2, &b.url, T_REMOTE_B, "web"),
        ],
        None,
    )
    .await;
    (a, b, env)
}

// Baseline (green now, protects the sync→spawned refactor): no remote
// profiles ⇒ exactly the local rows, tagged with their own profile.
#[tokio::test]
async fn local_rows_alone_are_returned_when_no_remote_profile_exists() {
    let env = LocalEnv::start(&[], None).await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;
    let row = find(&list, &l1.to_string()).expect("the local row is listed");
    assert_eq!(row["profile"], "dev");
    assert_eq!(row["name"], "local one");
    assert!(list.get("error").is_none(), "no error: {list}");
}

// §5.6: local rows + each remote's rows; remote rows tagged L (ids unchanged).
#[tokio::test]
async fn merged_list_on_a_local_socket_has_local_rows_and_tagged_remote_rows() {
    let (a, b, env) = two_remotes().await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    let local = find(&list, &l1.to_string()).expect("local row kept");
    assert_eq!(local["profile"], "dev", "a local row keeps its own tag");
    let ra = find(&list, &uid(1)).expect("remote A's row is merged in, id unchanged");
    assert_eq!(
        ra["profile"], L,
        "remote A's row is tagged with the LOCAL name"
    );
    assert_eq!(ra["name"], "on A");
    assert_eq!(ra["message_count"], 3);
    assert_eq!(ra["model"], "m");
    let rb = find(&list, &uid(2)).expect("remote B's row is merged in");
    assert_eq!(rb["profile"], L2);
    assert!(list.get("error").is_none(), "error stays absent: {list}");
}

// Same merge on a RELAY socket; and the request is never forwarded (§6.3 #11).
#[tokio::test]
async fn merged_list_on_a_relay_socket_has_local_rows_and_tagged_remote_rows() {
    let (a, b, env) = two_remotes().await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_relay(&env, L).await;
    let list = request_list(&mut ws).await;

    assert!(find(&list, &l1.to_string()).is_some(), "local row: {list}");
    assert_eq!(find(&list, &uid(1)).expect("A's row")["profile"], L);
    assert_eq!(find(&list, &uid(2)).expect("B's row")["profile"], L2);
    assert!(list.get("error").is_none(), "error stays absent: {list}");
    assert!(
        a.ws_frames()
            .iter()
            .all(|f| !f.contains("request_conversations")),
        "never forwarded to the remote: {:?}",
        a.ws_frames()
    );
}

// §5.6: "merge by updated_at desc" — across local rows and both remotes.
#[tokio::test]
async fn merged_list_is_ordered_newest_first_across_local_and_remote_rows() {
    let (a, b, env) = two_remotes().await;
    let l_old = env.seed(None, "local old", Some("dev"), "2026-01-02T12:00:00Z");
    let l_new = env.seed(None, "local new", Some("dev"), "2026-01-06T12:00:00Z");
    a.set_rows(vec![
        row(&uid(1), "A old", "2026-01-01T12:00:00Z", Some("dev")),
        row(&uid(2), "A new", "2026-01-05T12:00:00Z", Some("dev")),
    ]);
    b.set_rows(vec![row(
        &uid(3),
        "B mid",
        "2026-01-04T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    let interest = vec![l_old.to_string(), l_new.to_string(), uid(1), uid(2), uid(3)];
    assert_eq!(
        order(&list, &interest),
        vec![
            l_new.to_string(), // 01-06
            uid(2),            // 01-05  (A)
            uid(3),            // 01-04  (B)
            l_old.to_string(), // 01-02
            uid(1),            // 01-01  (A)
        ],
        "one list, newest first, local and remote interleaved"
    );
}

// §5.6: "keep rows whose profile P has a local remote profile L with (S, P) …
// drop rows with no match (incl. profile-less legacy rows)".
#[tokio::test]
async fn remote_rows_with_an_unmatched_or_missing_profile_are_dropped() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    a.set_rows(vec![
        row(&uid(1), "kept", "2026-01-05T12:00:00Z", Some("dev")),
        row(
            &uid(2),
            "other profile",
            "2026-01-05T11:00:00Z",
            Some("other"),
        ),
        row(&uid(3), "legacy, no profile", "2026-01-05T10:00:00Z", None),
        // The remote happens to have a profile NAMED like our local L: still
        // not its P, so it does not match.
        row(&uid(4), "named like L", "2026-01-05T09:00:00Z", Some(L)),
    ]);

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    assert_eq!(find(&list, &uid(1)).expect("matched row")["profile"], L);
    for dropped in [2, 3, 4] {
        assert!(
            find(&list, &uid(dropped)).is_none(),
            "row {dropped} must be dropped: {list}"
        );
    }
}

// §5.6: `active` and `running` on remote rows are the remote registry's.
#[tokio::test]
async fn remote_rows_keep_the_remote_registry_live_flags() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    a.set_rows(vec![
        row_live(
            &uid(1),
            "busy",
            "2026-01-05T12:00:00Z",
            Some("dev"),
            true,
            true,
        ),
        row_live(
            &uid(2),
            "idle",
            "2026-01-05T11:00:00Z",
            Some("dev"),
            true,
            false,
        ),
        row_live(
            &uid(3),
            "cold",
            "2026-01-05T10:00:00Z",
            Some("dev"),
            false,
            false,
        ),
    ]);

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    let flags = |n: u32| {
        let r = find(&list, &uid(n)).unwrap_or_else(|| panic!("row {n} missing: {list}"));
        (r["active"].as_bool(), r["running"].as_bool())
    };
    assert_eq!(flags(1), (Some(true), Some(true)));
    assert_eq!(flags(2), (Some(true), Some(false)));
    assert_eq!(flags(3), (Some(false), Some(false)));
}

// §5.6 "for each distinct remote server S" + RP4: two local profiles that
// point at DIFFERENT profiles of the SAME server cost ONE fetch per list
// request, and each row is tagged by its own target profile.
#[tokio::test]
async fn two_profiles_on_one_server_share_one_fetch_and_are_tagged_by_target() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(
        &[
            ("p1", &a.url, T_REMOTE, "dev"),
            ("p2", &a.url, T_REMOTE, "ops"),
        ],
        None,
    )
    .await;
    a.set_rows(vec![
        row(&uid(1), "dev row", "2026-01-05T12:00:00Z", Some("dev")),
        row(&uid(2), "ops row", "2026-01-05T11:00:00Z", Some("ops")),
        row(&uid(3), "nobody's", "2026-01-05T10:00:00Z", Some("third")),
    ]);

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    assert_eq!(find(&list, &uid(1)).expect("dev row")["profile"], "p1");
    assert_eq!(find(&list, &uid(2)).expect("ops row")["profile"], "p2");
    assert!(find(&list, &uid(3)).is_none(), "unmatched row dropped");
    assert_eq!(
        a.hits_for("GET", "/api/conversations").len(),
        1,
        "one GET per distinct server per list request"
    );
}

// §5.12 "list: remote down": a refused remote is absent; local rows and the
// healthy remote's rows still arrive; no error marker.
#[tokio::test]
async fn unreachable_remote_is_omitted_and_everything_else_is_still_listed() {
    let b = FakeRemote::start(T_REMOTE_B).await;
    let dead = dead_url();
    let env = LocalEnv::start(
        &[(L, &dead, T_REMOTE, "dev"), (L2, &b.url, T_REMOTE_B, "web")],
        None,
    )
    .await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_local(&env).await;
    let started = Instant::now();
    let list = request_list(&mut ws).await;

    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert!(find(&list, &l1.to_string()).is_some(), "local rows remain");
    assert!(
        find(&list, &uid(2)).is_some(),
        "the healthy remote's rows remain"
    );
    assert!(
        list["items"]
            .as_array()
            .expect("items")
            .iter()
            .all(|r| r["profile"] != L),
        "nothing tagged with the dead profile: {list}"
    );
    assert!(
        list.get("error").is_none(),
        "no per-remote error marker: {list}"
    );
}

// §5.12: a remote that accepts and never answers is cut off at LIST_TIMEOUT
// (2 s) — the list "never waits > 2 s".
#[tokio::test]
async fn hanging_remote_is_cut_off_within_the_list_timeout() {
    let (a, b, env) = two_remotes().await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    a.set_list_mode(ListMode::Hang);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_local(&env).await;
    let started = Instant::now();
    let list = request_list(&mut ws).await;
    let elapsed = started.elapsed();

    assert!(elapsed < BOUND, "must not wait past ~2 s, took {elapsed:?}");
    assert!(find(&list, &l1.to_string()).is_some(), "local rows remain");
    assert!(
        find(&list, &uid(2)).is_some(),
        "the healthy remote is listed"
    );
    assert!(
        list.get("error").is_none(),
        "no per-remote error marker: {list}"
    );
}

// §5.6 "concurrently, each ≤ 2 s": two hung servers cost ~2 s, not ~4 s.
#[tokio::test]
async fn two_hanging_remotes_are_waited_for_concurrently_not_serially() {
    let (a, b, env) = two_remotes().await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    a.set_list_mode(ListMode::Hang);
    b.set_list_mode(ListMode::Hang);

    let mut ws = connect_local(&env).await;
    let started = Instant::now();
    let list = request_list(&mut ws).await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < BOUND,
        "serial waits would take ≥ 4 s; took {elapsed:?}"
    );
    assert!(find(&list, &l1.to_string()).is_some(), "local rows remain");
}

// A relay socket gets the same bounded behaviour.
#[tokio::test]
async fn hanging_remote_is_cut_off_on_a_relay_socket_too() {
    let (a, b, env) = two_remotes().await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    // The relay attaches to A (healthy at dial), then A's list hangs.
    a.set_list_mode(ListMode::Hang);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_relay(&env, L).await;
    let started = Instant::now();
    let list = request_list(&mut ws).await;

    assert!(started.elapsed() < BOUND, "took {:?}", started.elapsed());
    assert!(find(&list, &l1.to_string()).is_some());
    assert!(find(&list, &uid(2)).is_some());
}

// A remote that answers non-2xx, garbage, or 401 (stale token) is just absent.
#[tokio::test]
async fn failing_remote_answers_are_omitted_without_an_error_marker() {
    let r500 = FakeRemote::start(T_REMOTE).await;
    r500.set_list_mode(ListMode::Status(500));
    let garbage = FakeRemote::start(T_REMOTE_B).await;
    garbage.set_list_mode(ListMode::Garbage);
    let stale = FakeRemote::start("the-real-token-xyz").await;
    stale.set_rows(vec![row(
        &uid(9),
        "unseen",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    let env = LocalEnv::start(
        &[
            ("r-500", &r500.url, T_REMOTE, "dev"),
            ("r-garbage", &garbage.url, T_REMOTE_B, "dev"),
            ("r-stale", &stale.url, "stale-token-in-config", "dev"),
        ],
        None,
    )
    .await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    assert!(find(&list, &l1.to_string()).is_some(), "local rows remain");
    assert!(
        find(&list, &uid(9)).is_none(),
        "401 remote contributes nothing"
    );
    assert!(list.get("error").is_none(), "no error marker: {list}");
}

// §5.11: the remote set is read from the global config on EVERY list request.
#[tokio::test]
async fn remote_profiles_added_or_removed_in_the_config_show_on_the_next_list() {
    let a = FakeRemote::start(T_REMOTE).await;
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    let env = LocalEnv::start(&[], None).await;
    let mut ws = connect_local(&env).await;

    let before = request_list(&mut ws).await;
    assert!(find(&before, &uid(1)).is_none(), "no remote configured yet");

    std::fs::write(env.config_path(), yaml(&[(L, &a.url, T_REMOTE, "dev")]))
        .expect("rewrite config");
    let added = request_list(&mut ws).await;
    assert_eq!(
        find(&added, &uid(1)).expect("added remote is listed")["profile"],
        L
    );

    std::fs::write(env.config_path(), yaml(&[])).expect("rewrite config");
    let removed = request_list(&mut ws).await;
    assert!(find(&removed, &uid(1)).is_none(), "removed remote is gone");
}

// §5.11 E-CONFIG: a hand-broken global config must not break the list —
// local rows only, no error frame.
#[tokio::test]
async fn broken_global_config_yields_local_rows_only() {
    let a = FakeRemote::start(T_REMOTE).await;
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    std::fs::write(env.config_path(), "providers: [this is: not valid").expect("break config");

    let mut ws = connect_local(&env).await;
    let list = request_list(&mut ws).await;

    assert!(find(&list, &l1.to_string()).is_some(), "local rows remain");
    assert!(
        find(&list, &uid(1)).is_none(),
        "no remote rows without a config"
    );
    assert!(list.get("error").is_none(), "no error: {list}");
}

// §7 / RP6: only each remote's OWN token reaches it, only as a cookie, and
// the local token reaches nobody — through list AND kill.
#[tokio::test]
async fn each_remote_sees_only_its_own_token_and_never_the_local_one() {
    let (a, b, env) = two_remotes().await;
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let mut ws = connect_local(&env).await;
    let _ = request_list(&mut ws).await;
    let _ = kill(&mut ws, &uid(1), Some(L)).await;
    let _ = kill(&mut ws, &uid(2), Some(L2)).await;

    for (fake, mine, other) in [(&a, T_REMOTE, T_REMOTE_B), (&b, T_REMOTE_B, T_REMOTE)] {
        let hits = fake.rec.hits();
        assert!(hits.len() >= 2, "list + kill reached the remote: {hits:?}");
        for hit in &hits {
            assert_clean(hit, mine, other);
        }
    }
    assert!(
        !a.kills().is_empty() && !b.kills().is_empty(),
        "both kills routed"
    );
}

fn assert_clean(hit: &Captured, mine: &str, other: &str) {
    for needle in [T_LOCAL, other] {
        assert!(!hit.uri.contains(needle), "token in URL: {}", hit.uri);
        for (name, value) in &hit.headers {
            assert!(!value.contains(needle), "token in header {name}: {value}");
        }
    }
    assert_eq!(
        hit_cookie(hit),
        Some(format!("peakbot_token={mine}")),
        "the cookie is exactly the profile's own token: {hit:?}"
    );
    assert!(!hit.uri.contains(mine), "remote token in URL: {}", hit.uri);
}

// ═══════════════════════════════════════════════════════════════════════
// Kill routing (§5.6)
// ═══════════════════════════════════════════════════════════════════════

enum Kind {
    Local,
    Relay,
}

async fn socket(env: &LocalEnv, kind: &Kind) -> Ws {
    match kind {
        Kind::Local => connect_local(env).await,
        Kind::Relay => connect_relay(env, L).await,
    }
}

// kill{convo, profile: L} → the remote's kill route (right server, remote
// cookie), reply = freshly merged list reflecting the kill, no error; local
// sessions untouched.
async fn kill_on_remote_profile_hits_the_remote(kind: Kind) {
    let (a, b, env) = two_remotes().await;
    a.set_rows(vec![row_live(
        &uid(1),
        "live on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
        true,
        false,
    )]);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);
    let mut ws = socket(&env, &kind).await;
    let local_before = env.registry.active_ids();

    let reply = kill(&mut ws, &uid(1), Some(L)).await;

    assert_eq!(a.kills(), vec![uid(1)], "A's kill route was called once");
    assert!(b.kills().is_empty(), "B was not asked to kill anything");
    let hit = a.hits_for("POST", &format!("/api/conversations/{}/kill", uid(1)));
    assert_eq!(
        hit.len(),
        1,
        "exactly one upstream kill: {:?}",
        a.rec.hits()
    );
    assert_eq!(
        hit_cookie(&hit[0]),
        Some(format!("peakbot_token={T_REMOTE}")),
        "the remote token, as a cookie"
    );
    assert!(
        reply.get("error").is_none(),
        "clean kill, no error: {reply}"
    );
    let r = find(&reply, &uid(1)).expect("the killed row stays listed, tagged L");
    assert_eq!(r["profile"], L);
    assert_eq!(
        r["active"], false,
        "the reply reflects the remote kill (fresh list)"
    );
    assert!(
        find(&reply, &uid(2)).is_some(),
        "other remotes' rows are merged too"
    );
    assert_eq!(
        env.registry.active_ids(),
        local_before,
        "no local session touched"
    );
}

#[tokio::test]
async fn kill_with_a_remote_profile_hits_the_remote_on_a_local_socket() {
    kill_on_remote_profile_hits_the_remote(Kind::Local).await;
}

#[tokio::test]
async fn kill_with_a_remote_profile_hits_the_remote_on_a_relay_socket() {
    kill_on_remote_profile_hits_the_remote(Kind::Relay).await;
}

// The remote's refusal (mid-turn) reaches the client verbatim as `error`,
// and the list still comes back (row still active).
async fn remote_refusal_is_forwarded_verbatim(kind: Kind) {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    a.set_rows(vec![row_live(
        &uid(1),
        "running on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
        true,
        true,
    )]);
    let refusal = "remote says: busy right now — press Stop there first";
    a.refuse_kill_with(refusal);
    let mut ws = socket(&env, &kind).await;

    let reply = kill(&mut ws, &uid(1), Some(L)).await;

    assert_eq!(a.kills(), vec![uid(1)], "the remote was asked");
    assert_eq!(
        reply["error"], refusal,
        "mapped verbatim, not re-worded: {reply}"
    );
    let r = find(&reply, &uid(1)).expect("row still listed");
    assert_eq!(r["active"], true, "the refused kill changed nothing");
    assert_eq!(r["profile"], L);
}

#[tokio::test]
async fn remote_kill_refusal_is_forwarded_verbatim_on_a_local_socket() {
    remote_refusal_is_forwarded_verbatim(Kind::Local).await;
}

#[tokio::test]
async fn remote_kill_refusal_is_forwarded_verbatim_on_a_relay_socket() {
    remote_refusal_is_forwarded_verbatim(Kind::Relay).await;
}

// Routing is by `profile`, never by id: a remote-profile kill whose id equals
// a LOCAL live session must not kill the local one.
#[tokio::test]
async fn remote_profile_kill_never_kills_a_local_session_with_the_same_id() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    let live = env.registry.attach(None).expect("local session");
    let id = live.live_convo().expect("live id").to_string();
    a.set_rows(vec![row(
        &id,
        "same id on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    let mut ws = connect_local(&env).await;

    let _ = kill(&mut ws, &id, Some(L)).await;

    assert_eq!(a.kills(), vec![id.clone()], "went to the remote");
    assert!(
        env.registry
            .active_ids()
            .iter()
            .any(|u| u.to_string() == id),
        "the local session survives"
    );
}

// Remote down at kill time: the locked E-DOWN string as the reply's `error`
// (ASSUMPTION — the doc is silent), local rows still returned.
#[tokio::test]
async fn kill_on_an_unreachable_remote_replies_with_the_edown_string() {
    let dead = dead_url();
    let port = dead.rsplit(':').next().expect("port").to_string();
    let env = LocalEnv::start(&[(L, &dead, T_REMOTE, "dev")], None).await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    let mut ws = connect_local(&env).await;

    let reply = kill(&mut ws, &uid(1), Some(L)).await;

    let msg = reply["error"].as_str().expect("an error string");
    assert!(
        msg.starts_with(&format!(
            "Can't reach remote profile '{L}' at 127.0.0.1:{port}: "
        )) && msg.ends_with('.'),
        "locked E-DOWN string, got {msg:?}"
    );
    assert!(
        find(&reply, &l1.to_string()).is_some(),
        "local rows still listed"
    );
}

// A stale remote token: the locked E-AUTH string (RemoteError::Auth).
#[tokio::test]
async fn kill_on_a_remote_that_rejects_the_token_replies_with_the_eauth_string() {
    let a = FakeRemote::start("the-real-token-xyz").await;
    let env = LocalEnv::start(&[(L, &a.url, "stale-token-in-config", "dev")], None).await;
    let mut ws = connect_local(&env).await;

    let reply = kill(&mut ws, &uid(1), Some(L)).await;

    assert_eq!(
        reply["error"],
        json!(
            "Remote profile 'home-dev' rejected its token. Update the token in Settings → Profiles."
        )
    );
    assert!(a.kills().is_empty(), "the gate refused it");
}

// `Client::kill` takes a Uuid: an unparsable id with a remote profile never
// reaches the remote (and, like today's local kill, is not an error frame).
#[tokio::test]
async fn kill_with_a_remote_profile_and_an_invalid_uuid_does_not_contact_the_remote() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    let mut ws = connect_local(&env).await;

    ws_send(
        &mut ws,
        &json!({ "type": "kill_session", "convo": "not-a-uuid", "profile": L }),
    )
    .await;
    // Sync point: a later request_conversations is answered after the kill frame.
    let _ = request_list(&mut ws).await;

    assert!(a.kills().is_empty(), "no kill call for a bad id");
}

// ── Local kill is unchanged ────────────────────────────────────────────

async fn local_kill_with(profile: Option<&str>) {
    let a = FakeRemote::start(T_REMOTE).await;
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    let mut ws = connect_local(&env).await;
    let ids = env.registry.active_ids();
    assert_eq!(ids.len(), 1, "the socket's own session");
    let id = ids.into_iter().next().expect("live id").to_string();

    let reply = kill(&mut ws, &id, profile).await;

    assert!(
        env.registry.active_ids().is_empty(),
        "the local session was killed"
    );
    assert!(reply.get("error").is_none(), "idle kill is clean: {reply}");
    assert!(a.kills().is_empty(), "the remote was never asked to kill");
    assert!(
        a.hits_for("POST", &format!("/api/conversations/{id}/kill"))
            .is_empty(),
        "no kill POST reached the remote"
    );
}

#[tokio::test]
async fn kill_without_a_profile_stays_local() {
    local_kill_with(None).await;
}

#[tokio::test]
async fn kill_naming_a_local_profile_stays_local() {
    local_kill_with(Some("dev")).await;
}

#[tokio::test]
async fn kill_naming_an_unknown_profile_stays_local() {
    local_kill_with(Some("no-such-profile")).await;
}

// Today's mid-turn refusal text on the local path is unchanged.
#[tokio::test]
async fn local_kill_of_a_running_session_is_still_refused_with_the_local_message() {
    let a = FakeRemote::start(T_REMOTE).await;
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    let busy = env.registry.attach(None).expect("local session");
    let id = busy.live_convo().expect("id");
    busy.session.state_manager.set_running(true);
    let mut ws = connect_local(&env).await;

    let reply = kill(&mut ws, &id.to_string(), None).await;

    assert_eq!(reply["error"], KILL_RUNNING_MSG);
    assert!(env.registry.active_ids().contains(&id), "session survives");
    assert!(a.kills().is_empty());
}

// ═══════════════════════════════════════════════════════════════════════
// HTTP routes stay LOCAL-ONLY (§6.2, §5.9) — GUARDS, green before and after.
// NOTE: the task brief asked for merged HTTP lists / remote HTTP kill; the
// design says the opposite (relays call these; merging would recurse A→B→C).
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn http_get_conversations_is_local_only_and_never_contacts_a_remote() {
    let (a, b, env) = two_remotes().await;
    let l1 = env.seed(None, "local one", Some("dev"), "2026-01-02T12:00:00Z");
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    b.set_rows(vec![row(
        &uid(2),
        "on B",
        "2026-01-03T12:00:00Z",
        Some("web"),
    )]);

    let resp = http()
        .get(format!("{}/api/conversations", env.base()))
        .header("cookie", local_cookie())
        .send()
        .await
        .expect("GET");
    assert_eq!(resp.status(), 200);
    let body: Value = serde_json::from_str(&resp.text().await.expect("body")).expect("json");

    assert!(find(&body, &l1.to_string()).is_some(), "local row listed");
    assert!(find(&body, &uid(1)).is_none() && find(&body, &uid(2)).is_none());
    assert_eq!(a.rec.count() + b.rec.count(), 0, "no remote was contacted");
}

#[tokio::test]
async fn http_kill_is_local_only_and_never_contacts_a_remote() {
    let a = FakeRemote::start(T_REMOTE).await;
    a.set_rows(vec![row(
        &uid(1),
        "on A",
        "2026-01-05T12:00:00Z",
        Some("dev"),
    )]);
    let env = LocalEnv::start(&[(L, &a.url, T_REMOTE, "dev")], None).await;
    let live = env.registry.attach(None).expect("local session");
    let id = live.live_convo().expect("id");

    let resp = http()
        .post(format!("{}/api/conversations/{id}/kill", env.base()))
        .header("cookie", local_cookie())
        .send()
        .await
        .expect("POST");
    assert_eq!(resp.status(), 200);
    assert!(
        !env.registry.active_ids().contains(&id),
        "local kill happened"
    );
    assert_eq!(a.rec.count(), 0, "no remote was contacted");
}
