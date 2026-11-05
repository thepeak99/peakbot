//! Remote-profiles T7 — the merged conversation list and kill routing
//! (design §5.6). Both socket kinds are answered here, locally, never
//! forwarded: the local rows plus one `GET /api/conversations` per
//! DISTINCT remote server (concurrent, each ≤ LIST_TIMEOUT). A remote row
//! is kept only when its `profile` P matches a local remote profile L on
//! that server, and it is re-tagged with L (RP4 makes the match unique);
//! everything else — other profiles, profile-less legacy rows — is
//! dropped. A failing server or a broken global config contributes no
//! rows: no error marker, and the reply never takes longer than 2 s.

use std::collections::BTreeMap;
use std::time::Duration;

use super::DIRECTORY;
use crate::ui::web::KILL_RUNNING_MSG;
use crate::ui::web::conversations::local_rows;
use crate::ui::web::registry::{KillOutcome, SessionRegistry};
use crate::ui::wire::ConversationSummaryWire;
use uuid::Uuid;

/// §5.6 / §5.12 "list: remote down": the reply never waits past this per
/// remote server.
const LIST_TIMEOUT: Duration = Duration::from_secs(2);

/// Local rows + every reachable remote's rows, merged by `updated_at` desc.
pub(crate) async fn merged_items(registry: &SessionRegistry) -> Vec<ConversationSummaryWire> {
    let mut items = local_rows(registry);

    // §5.11: a fresh global-config parse on every request; a broken file
    // means no remote rows.
    let Ok(refs) = DIRECTORY.remotes() else {
        return items;
    };

    // One fetch per distinct server (RP4): origin → (target profile P →
    // local name L).
    let mut servers: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for r in &refs {
        servers
            .entry(r.origin.clone())
            .or_default()
            .insert(r.target.clone(), r.name.clone());
    }

    let fetches = servers.into_values().map(|targets| {
        // Any profile's client reaches that server (the token is the
        // server's), so one of its names picks the client.
        let client_name = targets
            .values()
            .next()
            .cloned()
            .expect("a server has ≥1 profile");
        async move {
            // The bound covers the client build too (a first-use CA
            // bootstrap must not push the reply past LIST_TIMEOUT).
            let rows = tokio::time::timeout(LIST_TIMEOUT, async {
                let Ok(client) = DIRECTORY.client(&client_name).await else {
                    return Vec::new();
                };
                client.list().await.unwrap_or_default()
            })
            .await
            .unwrap_or_default();
            rows.into_iter()
                .filter_map(|mut row| {
                    targets.get(row.profile.as_deref()?).map(|local_name| {
                        row.profile = Some(local_name.clone());
                        row
                    })
                })
                .collect::<Vec<_>>()
        }
    });

    items.extend(
        futures::future::join_all(fetches)
            .await
            .into_iter()
            .flatten(),
    );
    items.sort_by_key(|row| std::cmp::Reverse(parse_updated_at(&row.updated_at)));
    items
}

/// `kill_session{convo, profile}` (design §5.6), routed by `profile`: a
/// REMOTE profile L goes to L's server — the remote's `error` is forwarded
/// verbatim, a dial/401 failure the locked `RemoteError` string — and the
/// reply carries a freshly merged list. Anything else (none, a local name,
/// an unknown name) is today's local kill. `None` = a bad UUID: no frame,
/// exactly like today's local kill.
pub(crate) async fn answer_kill(
    registry: &SessionRegistry,
    convo: &str,
    profile: Option<&str>,
) -> Option<(Vec<ConversationSummaryWire>, Option<String>)> {
    let Ok(id) = Uuid::parse_str(convo) else {
        return None;
    };

    let Some(remote_name) = profile.and_then(|p| {
        DIRECTORY
            .remotes()
            .ok()
            .into_iter()
            .flatten()
            .find(|r| r.name == p)
            .map(|r| r.name)
    }) else {
        // The local path, unchanged: KILL_RUNNING_MSG on a refused kill.
        let refused = registry.kill(id) == KillOutcome::Running;
        let items = merged_items(registry).await;
        return Some((items, refused.then(|| KILL_RUNNING_MSG.to_string())));
    };

    let error = match DIRECTORY.client(&remote_name).await {
        Ok(client) => match client.kill(&id).await {
            Ok(refusal) => refusal,
            Err(e) => Some(e.to_string()),
        },
        Err(e) => Some(e.to_string()),
    };
    let items = merged_items(registry).await;
    Some((items, error))
}

/// RFC 3339 → instant for the merge sort; an unparseable stamp sorts first.
fn parse_updated_at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
}
