//! Remote-profiles T7 — the merged conversation list and kill routing
//! (design §5.6). Both socket kinds are answered here, locally, never
//! forwarded: the local rows plus one `GET /api/conversations` per
//! DISTINCT remote server (concurrent, each ≤ LIST_TIMEOUT), and one
//! `{origin, online}` status per server (the mobile redesign's `machines`).
//! A remote row is kept only when its `profile` P matches a local remote
//! profile L on that server, and it is re-tagged with L (RP4 makes the
//! match unique); everything else — other profiles, profile-less legacy
//! rows — is dropped. A failing server or a broken global config
//! contributes no rows and is reported offline: no error marker, and the
//! reply never takes longer than 2 s.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use super::{Client, DIRECTORY};
use crate::ui::web::KILL_RUNNING_MSG;
use crate::ui::web::conversations::local_rows;
use crate::ui::web::registry::{KillOutcome, SessionRegistry};
use crate::ui::wire::{ConversationSummaryWire, MachineStatusWire, OutboundMessage};
use uuid::Uuid;

/// §5.6 / §5.12 "list: remote down": the reply never waits past this per
/// remote server.
const LIST_TIMEOUT: Duration = Duration::from_secs(2);

/// The merged list answer: local rows + every reachable remote's rows,
/// plus one status per distinct origin. This device is implicit — the
/// SPA's `null` machine — so no "local" entry rides the wire.
pub(crate) struct Listing {
    pub(crate) items: Vec<ConversationSummaryWire>,
    pub(crate) machines: Vec<MachineStatusWire>,
}

impl Listing {
    /// The `conversations_list` reply frame (empty `machines` is omitted,
    /// keeping a no-remote frame byte-identical to the pre-redesign one).
    pub(crate) fn into_frame(self, error: Option<String>) -> OutboundMessage {
        OutboundMessage::ConversationsList {
            items: self.items,
            machines: self.machines,
            error,
        }
    }
}

/// One summary fetch: (display name, rows, offline) in a join_all.
pub(crate) type FetchFuture =
    Pin<Box<dyn Future<Output = (String, Vec<ConversationSummaryWire>, bool)> + Send>>;

/// Local rows + every reachable remote's rows, merged by `updated_at`
/// desc, plus one status per distinct origin in BTreeMap order (the SPA
/// re-orders by catalog). `online` is true iff the list answered within
/// LIST_TIMEOUT — a dial error, a 401, any list failure or the timeout
/// all mean offline.
pub(crate) async fn merged_listing(registry: &SessionRegistry) -> Listing {
    let mut items = local_rows(registry);

    // §5.11: a fresh global-config parse on every request; a broken file
    // means no remote rows and no known machines.
    let Ok(refs) = DIRECTORY.remotes() else {
        return Listing {
            items,
            machines: Vec::new(),
        };
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

    // One fetch per target (a distinct remote server, or a cloud device),
    // all in a single `join_all` (F.6: no new cap).
    let mut fetches: Vec<FetchFuture> = Vec::new();
    for (origin, targets) in servers {
        // Any profile's client reaches that server (the token is the
        // server's), so one of its names picks the client.
        let client_name = targets
            .values()
            .next()
            .cloned()
            .expect("a server has ≥1 profile");
        fetches.push(Box::pin(async move {
            // The bound covers the client build too (a first-use CA
            // bootstrap must not push the reply past LIST_TIMEOUT); a
            // failure at any step is a `None`, which reports offline.
            let fetched: Option<Vec<ConversationSummaryWire>> =
                tokio::time::timeout(LIST_TIMEOUT, async {
                    let Ok(client) = DIRECTORY.client(&client_name).await else {
                        return None;
                    };
                    client.list().await.ok()
                })
                .await
                .ok()
                .flatten();
            let online = fetched.is_some();
            let rows = fetched
                .into_iter()
                .flatten()
                .filter_map(|mut row| {
                    targets.get(row.profile.as_deref()?).map(|local_name| {
                        row.profile = Some(local_name.clone());
                        row
                    })
                })
                .collect();
            (origin, rows, online)
        }));
    }

    // F.6: cloud devices are more fan-out targets — one `list()` per
    // Active device (self excluded), the same bound, rows re-tagged
    // `profile@slug`; a dead device contributes no rows and an offline
    // status, in the same shape remotes use for "down".
    if let Some(handle) = DIRECTORY.cloud() {
        let state = handle.state().borrow().clone();
        if let crate::cloud::CloudState::Active { devices, .. } = state {
            let self_id = handle.device_id();
            for (device, slug) in devices
                .iter()
                .zip(crate::cloud::link::device_slugs(&devices))
            {
                if Some(device.id) == self_id {
                    continue; // self is never listed (F.5)
                }
                let handle = handle.clone();
                let device = device.id;
                fetches.push(Box::pin(async move {
                    // `list()` ignores the profile field; the rows carry
                    // their own local profiles, which get re-tagged.
                    let client = Client::Cloud {
                        handle,
                        device,
                        profile: String::new(),
                    };
                    let fetched = tokio::time::timeout(LIST_TIMEOUT, client.list())
                        .await
                        .ok()
                        .and_then(Result::ok);
                    let online = fetched.is_some();
                    let rows = fetched
                        .into_iter()
                        .flatten()
                        .filter_map(|mut row| {
                            row.profile.take().map(|p| {
                                row.profile = Some(format!("{p}@{slug}"));
                                row
                            })
                        })
                        .collect();
                    (slug, rows, online)
                }));
            }
        }
    }

    let mut machines = Vec::new();
    for (id, rows, online) in futures::future::join_all(fetches).await {
        machines.push(MachineStatusWire { id, online });
        items.extend(rows);
    }
    items.sort_by_key(|row| std::cmp::Reverse(parse_updated_at(&row.updated_at)));
    Listing { items, machines }
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
) -> Option<(Listing, Option<String>)> {
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
        let listing = merged_listing(registry).await;
        return Some((listing, refused.then(|| KILL_RUNNING_MSG.to_string())));
    };

    let error = match DIRECTORY.client(&remote_name).await {
        Ok(client) => match client.kill(&id).await {
            Ok(refusal) => refusal,
            Err(e) => Some(e.to_string()),
        },
        Err(e) => Some(e.to_string()),
    };
    let listing = merged_listing(registry).await;
    Some((listing, error))
}

/// RFC 3339 → instant for the merge sort; an unparseable stamp sorts first.
fn parse_updated_at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
}
