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

/// Attached-socket count + idle clock for one session — the reaper's input.
/// Split out from the session so the expiry decision is unit-testable without
/// building a full [`Session`] (which needs a live provider).
///
/// A session is *quiescent* when three things are all true: no socket is
/// attached, the agent is not processing a turn, and no `bash_bg` child is
/// alive under it (#158). The idle clock runs from the moment the session
/// becomes quiescent — the reaper samples the live agent/bg signals on each
/// tick and arms `quiescent_since` itself, so no turn-lifecycle events have to
/// be pushed into this web-only registry.
struct AttachState {
    /// Number of sockets currently attached.
    attached: AtomicUsize,
    /// When the session last became fully quiescent — the start of the idle
    /// window the reaper measures against the TTL. `None` whenever the session
    /// is non-quiescent (a socket attached, the agent working, or a bg child
    /// alive). Owned and armed by the reaper tick, not by socket edges.
    quiescent_since: Mutex<Option<Instant>>,
}

impl AttachState {
    fn new() -> Self {
        Self {
            attached: AtomicUsize::new(0),
            quiescent_since: Mutex::new(None),
        }
    }

    /// A socket attached: bump the count. The reaper clears the idle clock on
    /// its next tick (it samples `attached`), so no arming is needed here.
    fn mark_attached(&self) {
        self.attached.fetch_add(1, Ordering::AcqRel);
    }

    /// A socket detached: drop the count. The reaper arms the idle clock on
    /// its next tick iff the session is also agent-idle and bg-idle.
    fn mark_detached(&self) {
        self.attached.fetch_sub(1, Ordering::AcqRel);
    }

    /// Sample the session's liveness and decide whether to reap it, arming or
    /// clearing the idle clock as a side effect. Pure in its inputs
    /// (`agent_running`, `bg_running`, `now`, `ttl`) plus the atomic socket
    /// count, so the reaper tick is fully testable without a live session.
    ///
    /// - non-quiescent → clear the clock, never expire;
    /// - just became quiescent → start the clock, don't expire yet (honours
    ///   "the idle clock starts when the agent finishes");
    /// - quiescent for ≥ `ttl` → expire.
    fn expire_check(
        &self,
        agent_running: bool,
        bg_running: bool,
        now: Instant,
        ttl: Duration,
    ) -> bool {
        let quiescent = self.attached.load(Ordering::Acquire) == 0 && !agent_running && !bg_running;
        let mut since = self.quiescent_since.lock().unwrap();
        if !quiescent {
            *since = None;
            return false;
        }
        match *since {
            None => {
                *since = Some(now);
                false
            }
            Some(t) => now.duration_since(t) >= ttl,
        }
    }
}

/// A live session plus the registry bookkeeping (attached-socket count and
/// idle clock) that decides when it expires.
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

    /// Reaper hook: sample this session's live agent/bg activity and decide
    /// whether it has been quiescent long enough to reap. Reads `is_running`
    /// and `bg_running_count` off the state manager here (derive, don't
    /// duplicate) so the registry never has to be told when a turn or bg child
    /// starts or ends.
    fn expire_check(&self, now: Instant, ttl: Duration) -> bool {
        let agent_running = self.session.state_manager.is_running();
        let bg_running = self.session.state_manager.bg_running_count() > 0;
        self.attach
            .expire_check(agent_running, bg_running, now, ttl)
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

    /// Kill every session that has been fully quiescent (no sockets, agent
    /// idle, no live bg children) longer than `ttl`.
    pub fn reap(&self, ttl: Duration) {
        let now = Instant::now();
        let mut map = self.inner.map.lock().unwrap();
        let expired: Vec<Uuid> = map
            .iter()
            .filter(|(_, s)| s.expire_check(now, ttl))
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

    // `expire_check` is the reaper's per-tick decision. These tests drive it
    // directly with explicit (agent_running, bg_running, now, ttl) inputs — no
    // live Session needed. `now` is threaded so we can advance time without
    // sleeping. Quiescent = no sockets AND agent idle AND no bg children.

    fn idle(s: &AttachState, now: Instant, ttl: Duration) -> bool {
        s.expire_check(false, false, now, ttl)
    }

    #[test]
    fn fresh_state_becomes_quiescent_then_expires_after_ttl() {
        let s = AttachState::new();
        let t0 = Instant::now();
        let ttl = Duration::from_secs(600);
        // No socket ever attached, agent idle, no bg → quiescent. First tick
        // arms the clock but must NOT reap (clock starts now).
        assert!(!idle(&s, t0, ttl), "first quiescent tick arms, never reaps");
        // Not yet past ttl.
        assert!(!idle(&s, t0 + Duration::from_secs(599), ttl));
        // Past ttl → reap.
        assert!(idle(&s, t0 + Duration::from_secs(600), ttl));
    }

    #[test]
    fn attached_session_never_expires() {
        let s = AttachState::new();
        s.mark_attached();
        let t0 = Instant::now();
        assert!(
            !idle(&s, t0 + Duration::from_secs(10_000), t0.elapsed()),
            "an attached session is live regardless of ttl"
        );
    }

    #[test]
    fn running_agent_never_expires_even_with_no_sockets() {
        let s = AttachState::new();
        let t0 = Instant::now();
        let ttl = Duration::ZERO;
        // No sockets, but the agent is working → not quiescent, never reaped.
        assert!(!s.expire_check(true, false, t0, ttl));
        assert!(!s.expire_check(true, false, t0 + Duration::from_secs(10_000), ttl));
    }

    #[test]
    fn live_bg_child_never_expires_even_with_no_sockets() {
        let s = AttachState::new();
        let t0 = Instant::now();
        let ttl = Duration::ZERO;
        // No sockets, agent idle, but a bg child is alive → not quiescent.
        assert!(!s.expire_check(false, true, t0, ttl));
        assert!(!s.expire_check(false, true, t0 + Duration::from_secs(10_000), ttl));
    }

    #[test]
    fn clock_starts_when_agent_finishes_not_when_socket_detached() {
        // The load-bearing correctness case: socket drops at t0 while the
        // agent keeps working for 20 minutes, then finishes. The idle clock
        // must start at t_finish, not t0 — so the session gets a full ttl of
        // grace after the agent goes idle.
        let s = AttachState::new();
        s.mark_attached();
        let t0 = Instant::now();
        let ttl = Duration::from_secs(600);

        s.mark_detached(); // socket gone at t0
        // Agent still running for 20 min → never quiescent, clock stays unset.
        assert!(!s.expire_check(true, false, t0 + Duration::from_secs(1200), ttl));
        // Agent finishes at t0+1200 → first quiescent tick arms, doesn't reap.
        let t_finish = t0 + Duration::from_secs(1200);
        assert!(
            !idle(&s, t_finish, ttl),
            "clock arms at finish, no instant reap"
        );
        // Only ttl AFTER the finish does it expire.
        assert!(!idle(&s, t_finish + Duration::from_secs(599), ttl));
        assert!(idle(&s, t_finish + Duration::from_secs(600), ttl));
    }

    #[test]
    fn reattach_before_ttl_resets_the_clock() {
        let s = AttachState::new();
        let t0 = Instant::now();
        let ttl = Duration::from_secs(600);
        // Quiescent, clock armed at t0.
        assert!(!idle(&s, t0, ttl));
        // Someone reconnects → next tick sees attached>0, clears the clock.
        s.mark_attached();
        assert!(!s.expire_check(false, false, t0 + Duration::from_secs(1200), ttl));
        // They leave again → clock restarts from this tick, not t0.
        s.mark_detached();
        let t_reidle = t0 + Duration::from_secs(1200);
        assert!(
            !idle(&s, t_reidle, ttl),
            "reidle arms fresh, no instant reap"
        );
        assert!(idle(&s, t_reidle + Duration::from_secs(600), ttl));
    }

    #[test]
    fn agent_restart_mid_window_resets_the_clock() {
        // Quiescent clock armed, then a bg drain re-engages the agent before
        // ttl elapses → the clock must reset, not carry over.
        let s = AttachState::new();
        let t0 = Instant::now();
        let ttl = Duration::from_secs(600);
        assert!(!idle(&s, t0, ttl)); // armed
        // Agent picks up work at t0+300 → clears the clock.
        assert!(!s.expire_check(true, false, t0 + Duration::from_secs(300), ttl));
        // Back to idle at t0+400 → fresh arm, so t0+400+600 is the deadline.
        let t_reidle = t0 + Duration::from_secs(400);
        assert!(!idle(&s, t_reidle, ttl));
        assert!(!idle(&s, t_reidle + Duration::from_secs(599), ttl));
        assert!(idle(&s, t_reidle + Duration::from_secs(600), ttl));
    }
}
