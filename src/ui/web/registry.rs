//! Sticky web sessions — a registry of live agent sessions keyed by
//! conversation id (issue #118).
//!
//! The default web UI is Option C: one WebSocket == one session, torn down on
//! disconnect. The registry decouples **session lifetime** from **socket
//! lifetime**: a conversation is "active" while a live [`Session`]
//! (StateManager + controller loop) is bound to it, regardless of how many
//! sockets are watching. Sockets *attach* and *detach*; the session survives
//! reconnects and can be shared by multiple tabs.
//!
//! ## Teardown is graceful, never `abort`
//!
//! `run_loop` only runs `clear_bg()` (kills PTY children) when its
//! `action_sender` channel closes — **not** on `JoinHandle::abort`. So
//! [`SessionRegistry::kill`] signals a graceful `/exit` (which sets
//! `exit_requested` → broadcast → attached sockets close → their `Arc`s
//! drop → last `action_sender` drops → `clear_bg` runs) and removes the entry
//! so no new attach finds it. Aborting would leak PTYs.
//!
//! ## Concurrency
//!
//! One `std::sync::Mutex` guards the map. `create_session` is synchronous, so
//! it runs under the lock without an `.await` — attaches are serialized (rare,
//! per new connection) and the check-then-create is race-free.

use crate::session::SessionDeps;
use crate::{Session, UiAction, create_session};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Attached-socket count + idle window for one session — the reaper's input.
/// Split out from the session so the expiry decision is unit-testable without
/// building a full [`Session`] (which needs a live provider).
struct AttachState {
    /// Number of sockets currently attached.
    attached: AtomicUsize,
    /// When `attached` last fell to zero — the start of the idle window the
    /// reaper measures against the TTL. `None` while a socket is attached.
    idle_since: Mutex<Option<Instant>>,
}

impl AttachState {
    fn new() -> Self {
        Self {
            attached: AtomicUsize::new(0),
            idle_since: Mutex::new(None),
        }
    }

    /// A socket attached: bump the count and clear any idle window.
    fn mark_attached(&self) {
        self.attached.fetch_add(1, Ordering::AcqRel);
        *self.idle_since.lock().unwrap() = None;
    }

    /// A socket detached: drop the count; when it hits zero, start the idle
    /// window so the reaper can expire the session after the TTL.
    fn mark_detached(&self) {
        if self.attached.fetch_sub(1, Ordering::AcqRel) == 1 {
            *self.idle_since.lock().unwrap() = Some(Instant::now());
        }
    }

    /// `true` when no socket is attached and the idle window exceeds `ttl`.
    fn is_expired(&self, ttl: Duration) -> bool {
        self.attached.load(Ordering::Acquire) == 0
            && matches!(*self.idle_since.lock().unwrap(), Some(t) if t.elapsed() >= ttl)
    }
}

/// A live session plus the registry bookkeeping (attached-socket count and
/// idle timestamp) that decides when it expires.
pub(crate) struct RegisteredSession {
    /// Stable internal handle for the session's whole life. The session's
    /// *conversation* id can change under it (`/model`, `/load`, `/new` mint a
    /// new one), so the map is keyed by this, not by the mutable convo id.
    key: Uuid,
    pub session: Session,
    attach: AttachState,
}

impl RegisteredSession {
    fn new(session: Session) -> Self {
        Self {
            key: session.conversation_id,
            session,
            attach: AttachState::new(),
        }
    }

    /// The conversation this session is *currently* on — the live source of
    /// truth, which `/model` / `/load` / `/new` move under us.
    pub fn live_convo(&self) -> Option<Uuid> {
        self.session.state_manager.get_current_conversation_id()
    }

    fn mark_attached(&self) {
        self.attach.mark_attached();
    }

    fn mark_detached(&self) {
        self.attach.mark_detached();
    }

    fn is_expired(&self, ttl: Duration) -> bool {
        self.attach.is_expired(ttl)
    }

    /// Signal a graceful exit — the teardown path that runs `clear_bg`.
    fn signal_exit(&self) {
        let _ = self
            .session
            .action_sender
            .send(UiAction::SendMessage("/exit".to_string()));
    }
}

/// Registry of active sessions. Cloned (cheaply, via `Arc` internally) into
/// every connection handler and the reaper task.
#[derive(Clone)]
pub(crate) struct SessionRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    deps: Arc<SessionDeps>,
    /// Keyed by the stable session handle (see [`RegisteredSession::key`]).
    map: Mutex<HashMap<Uuid, Arc<RegisteredSession>>>,
}

impl SessionRegistry {
    pub fn new(deps: Arc<SessionDeps>) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                map: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Attach a socket to a session. `want` is the `?convo=` id from the URL:
    /// - a live session is currently on that conversation → share it;
    /// - persisted but inactive → resume it (new session loads the id);
    /// - unknown / malformed / `None` → mint a fresh session.
    ///
    /// Matching is against each session's *live* conversation id, so a session
    /// that moved to a new convo (`/model`, `/load`) is found under its
    /// current id — never a stale one.
    pub fn attach(&self, want: Option<Uuid>) -> Result<Arc<RegisteredSession>> {
        let mut map = self.inner.map.lock().unwrap();

        if let Some(id) = want
            && let Some(existing) = map.values().find(|r| r.live_convo() == Some(id))
        {
            existing.mark_attached();
            return Ok(existing.clone());
        }

        // Not active. `create_session` is total: it resumes `want` if it
        // exists on disk, else mints fresh. We key on the stable handle.
        let session = create_session(&self.inner.deps, want)?;
        let registered = Arc::new(RegisteredSession::new(session));
        registered.mark_attached();
        map.insert(registered.key, registered.clone());
        Ok(registered)
    }

    /// A socket detached. `key` is [`RegisteredSession::key`] (stable for the
    /// socket's lifetime even if the session's convo id changed).
    pub fn detach(&self, key: Uuid) {
        if let Some(entry) = self.inner.map.lock().unwrap().get(&key) {
            entry.mark_detached();
        }
    }

    /// End the session currently on conversation `convo` for everyone: remove
    /// it so no new attach finds it, then signal a graceful exit (which
    /// unwinds the controller loop and kills its bg PTY children once the last
    /// socket drops).
    pub fn kill(&self, convo: Uuid) {
        let mut map = self.inner.map.lock().unwrap();
        let key = map
            .values()
            .find(|r| r.live_convo() == Some(convo))
            .map(|r| r.key);
        if let Some(key) = key
            && let Some(entry) = map.remove(&key)
        {
            entry.signal_exit();
        }
    }

    /// Live conversation ids with a session bound — feeds the `active` flag on
    /// the conversations list.
    pub fn active_ids(&self) -> HashSet<Uuid> {
        self.inner
            .map
            .lock()
            .unwrap()
            .values()
            .filter_map(|r| r.live_convo())
            .collect()
    }

    /// Kill every session idle (zero sockets) longer than `ttl`.
    pub fn reap(&self, ttl: Duration) {
        let mut map = self.inner.map.lock().unwrap();
        let expired: Vec<Uuid> = map
            .iter()
            .filter(|(_, s)| s.is_expired(ttl))
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            if let Some(entry) = map.remove(&key) {
                entry.signal_exit();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_attach_state_is_not_expired() {
        let s = AttachState::new();
        // No socket ever attached, no idle window → never expires.
        assert!(!s.is_expired(Duration::ZERO));
    }

    #[test]
    fn attached_session_never_expires() {
        let s = AttachState::new();
        s.mark_attached();
        assert!(!s.is_expired(Duration::ZERO), "an attached session is live");
    }

    #[test]
    fn last_detach_arms_idle_window() {
        let s = AttachState::new();
        s.mark_attached();
        s.mark_attached(); // two sockets
        s.mark_detached(); // one left → still live
        assert!(!s.is_expired(Duration::ZERO));
        s.mark_detached(); // last one gone → idle window armed
        assert!(
            s.is_expired(Duration::ZERO),
            "zero-ttl idle session is immediately expired"
        );
        assert!(
            !s.is_expired(Duration::from_secs(3600)),
            "a fresh idle window is not yet past a long ttl"
        );
    }

    #[test]
    fn reattach_clears_idle_window() {
        let s = AttachState::new();
        s.mark_attached();
        s.mark_detached(); // idle armed
        assert!(s.is_expired(Duration::ZERO));
        s.mark_attached(); // someone reconnected
        assert!(
            !s.is_expired(Duration::ZERO),
            "reattach revives the session"
        );
    }
}
