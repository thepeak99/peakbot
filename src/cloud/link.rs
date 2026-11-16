//! The `CloudLink` actor (p3-design §D): ONE tokio task owns the relay
//! socket, the `VerifiedChain`, the session table and the `Mux`es.
//! Callers talk to it via `mpsc` commands + oneshots; state is published
//! on a `tokio::sync::watch`. No `Mutex` around protocol state.
//!
//! Connect sequence per socket (§D.2): token → `GET /ws` (401 ⇒ delete
//! token ⇒ `LoggedOut{Expired}`) → **pre-Hello `ChainFetch`** → pin /
//! `chain::sync` (Err ⇒ `Trust(..)`, NO Hello) → `Challenge` →
//! `sign_hello` → `Hello`/`HelloPending` → `Welcome` → run loop.

use std::sync::Arc;

use cloud::{DeviceId, LetterId, frames::Op, sas::SasWords};
use tokio::sync::{mpsc, oneshot, watch};
use url::Url;

use super::actor;
use super::config::CloudConfig;
use super::login;
use super::store::{PublicState, Seed, SeedStore, SessionToken};
use super::stream::CloudStream;

/// Why there is no token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoutReason {
    Never,
    Expired,
    SignedOut,
}

/// Terminal trust failures (§D.1); ALL sessions refused, no reconnect
/// until user action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustError {
    GenesisMismatch,
    Fork,
    Revoked,
    ChainInvalid(String),
}

/// The state kind, for `CloudError::NotActive` (a state without detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudStateKind {
    Disabled,
    LoggedOut,
    Connecting,
    Pending,
    Active,
    Trust,
}

/// One device in the account, derived from `chain.active` + the relay's
/// presence info (§D.1: `online` is `None` = unknown until a pipe answers;
/// never stored separately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub platform: String,
    pub online: Option<bool>,
}

/// A verified, unexpired `EnrollRequest` letter (§G, G.4 step 2). `card`
/// is the raw CE card the `Add` entry signs (not re-derivable from the
/// DeviceId — erratum E-F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollRequestInfo {
    pub letter_id: LetterId,
    pub device: DeviceId,
    pub name: String,
    pub platform: String,
    pub card: Vec<u8>,
    pub sas: SasWords,
    pub expires_at: u64,
}

/// The published actor state (§D.1 + §G). Invariant: no sessions unless
/// `Active` (the session table lives inside the actor's private `Active`
/// phase).
#[derive(Debug, Clone, PartialEq)]
pub enum CloudState {
    Disabled,
    LoggedOut {
        reason: LogoutReason,
    },
    Connecting {
        attempt: u32,
        last_error: Option<String>,
    },
    /// `empty_account`: chain fetch returned 0 entries → genesis allowed.
    /// `sas`: `Some` once our `EnrollRequest` is posted (head frozen, §5.5).
    Pending {
        empty_account: bool,
        sas: Option<SasWords>,
        /// An active device refused our EnrollRequest; no auto-retry until
        /// `retry_enroll` or a fresh login (P4 E-F7).
        rejected: bool,
    },
    Active {
        devices: Vec<DeviceInfo>,
        requests: Vec<EnrollRequestInfo>,
    },
    Trust(TrustError),
}

impl CloudState {
    pub fn kind(&self) -> CloudStateKind {
        match self {
            CloudState::Disabled => CloudStateKind::Disabled,
            CloudState::LoggedOut { .. } => CloudStateKind::LoggedOut,
            CloudState::Connecting { .. } => CloudStateKind::Connecting,
            CloudState::Pending { .. } => CloudStateKind::Pending,
            CloudState::Active { .. } => CloudStateKind::Active,
            CloudState::Trust(_) => CloudStateKind::Trust,
        }
    }
}

/// Errors surfaced to callers (§D.6). `Reset` carries the stream reset
/// code; `Offline` = the peer is not in `chain.active` or unreachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    Disabled,
    NotActive(CloudStateKind),
    UnknownDevice,
    Offline,
    Trust(TrustError),
    Reset(u16),
    Io(String),
    /// Flow-specific refusals (§H.1 "specific errors").
    Flow(FlowError),
}

/// The §H.1 route-specific errors (not in the D.6 `CloudError` list — the
/// Architect must confirm this home).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowError {
    AlreadyLoggedIn,
    NotPendingEmpty,
    /// `retry_enroll` while not rejected (P4 E-F7).
    NotRejected,
    GenesisTaken,
    UnknownRequest,
    ChainBusy,
    UseLogout,
    LastDevice,
    /// P5 E-F3: `login` while a login is already pending (never cancels the first).
    LoginInFlight,
    /// P5: `deliver_callback` with no DeepLink login pending.
    NoLoginPending,
}

/// Directory slug per device (design F.5 / H.1): `slug(name)` =
/// `[a-z0-9-]{1,32}`, de-duplicated by appending `-` + the first 4 hex of
/// the DeviceId when two devices share one. Derived, never stored.
pub fn device_slugs(devices: &[DeviceInfo]) -> Vec<String> {
    fn base(name: &str) -> String {
        let mut s = String::new();
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                s.push(c.to_ascii_lowercase());
            } else if !s.ends_with('-') && !s.is_empty() {
                s.push('-');
            }
        }
        let s = s.trim_end_matches('-');
        let s: String = s.chars().take(32).collect();
        if s.is_empty() {
            "device".to_string()
        } else {
            s
        }
    }
    let bases: Vec<String> = devices.iter().map(|d| base(&d.name)).collect();
    devices
        .iter()
        .zip(&bases)
        .map(|(d, b)| {
            if bases.iter().filter(|x| *x == b).count() > 1 {
                let short: String = d.id.iter().take(2).map(|x| format!("{x:02x}")).collect();
                let keep: String = b.chars().take(27).collect();
                format!("{keep}-{short}")
            } else {
                b.clone()
            }
        })
        .collect()
}

/// `POST /api/cloud/logout` → `{"revoked": bool}` (G.6 step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogoutOutcome {
    pub revoked: bool,
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloudError::Disabled => f.write_str("cloud mode is not configured"),
            CloudError::NotActive(kind) => write!(f, "cloud state is not active: {kind:?}"),
            CloudError::UnknownDevice => f.write_str("unknown device"),
            CloudError::Offline => f.write_str("device offline"),
            CloudError::Trust(e) => write!(f, "trust error: {e:?}"),
            CloudError::Reset(code) => write!(f, "stream reset: {code}"),
            CloudError::Io(msg) => f.write_str(msg),
            CloudError::Flow(e) => write!(f, "{e:?}"),
        }
    }
}

impl std::error::Error for CloudError {}

/// Inbound session hook: host = `CloudServe::accept` (PR-C); Local edition
/// = `None` (the phone answers every inbound `Hs1` with `Close{PROTOCOL}`).
pub type InboundHook = Arc<dyn Fn(DeviceId, Op, CloudStream) + Send + Sync>;

/// The caller's handle to the actor (the seam, §D.6).
#[derive(Clone)]
pub struct CloudHandle {
    cmd: mpsc::Sender<CloudCmd>,
    state: watch::Receiver<CloudState>,
    /// The relay origin (login URLs, logout POST).
    base: Url,
    store: Arc<dyn SeedStore>,
    /// This device's id (derived from the seed; `None` if the identity failed).
    device_id: Option<DeviceId>,
    /// One login listener at a time (G.1 step 5): the only lock, and it's
    /// outside the actor.
    login_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// `last_login_error` of the API state (G.1 step 5).
    last_login_error: Arc<std::sync::Mutex<Option<String>>>,
}

/// Actor commands (private; the handle is the only door in).
pub enum CloudCmd {
    /// A login finished and stored its token: reconnect.
    TokenStored(SessionToken),
    Logout {
        out: oneshot::Sender<Result<LogoutOutcome, CloudError>>,
    },
    Open {
        device: DeviceId,
        op: Op,
        out: oneshot::Sender<Result<CloudStream, CloudError>>,
    },
    Genesis {
        out: oneshot::Sender<Result<DeviceId, CloudError>>,
    },
    Approve {
        letter_id: LetterId,
        out: oneshot::Sender<Result<DeviceId, CloudError>>,
    },
    /// Redial after a rejection with a fresh `EnrollRequest` (P4 E-F7).
    RetryEnroll {
        out: oneshot::Sender<Result<(), CloudError>>,
    },
    Reject {
        letter_id: LetterId,
        out: oneshot::Sender<Result<(), CloudError>>,
    },
    Revoke {
        target: DeviceId,
        out: oneshot::Sender<Result<(), CloudError>>,
    },
    Profiles {
        device: DeviceId,
        out: oneshot::Sender<Result<Vec<crate::cloud::session::ProfileInfo>, CloudError>>,
    },
    /// P5: app backgrounded — close the ws, park reconnect (skeleton: ignored).
    Suspend,
    /// P5: app foregrounded — redial now (skeleton: ignored).
    Resume,
    /// Stop the actor (handle teardown).
    Stop,
}

impl CloudHandle {
    /// The published state (§D.1).
    pub fn state(&self) -> watch::Receiver<CloudState> {
        self.state.clone()
    }

    /// PKCE login (§G.1): runs in the caller's task — the loopback
    /// listener continues in the background, so the actor never blocks on
    /// a browser. A second login cancels the first (G.1 step 5).
    pub async fn login(&self, open_browser: impl FnOnce(Url)) -> Result<(), CloudError> {
        let mut slot = self.login_task.lock().await;
        // Any state past `LoggedOut` means a token already exists (H.1).
        if self.state.borrow().kind() != CloudStateKind::LoggedOut {
            return Err(CloudError::Flow(FlowError::AlreadyLoggedIn));
        }
        if let Some(t) = slot.take() {
            t.abort();
        }
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| CloudError::Io(e.to_string()))?;
        let port = tcp
            .local_addr()
            .map(|a| a.port())
            .map_err(|e| CloudError::Io(e.to_string()))?;
        let redirect_uri = Url::parse(&format!("http://127.0.0.1:{port}/cb"))
            .map_err(|e| CloudError::Io(e.to_string()))?;
        let mut rng = actor::Os;
        let plan = login::plan_login(&self.base, &redirect_uri, &mut rng)?;
        let auth_url = plan.auth_url.clone();
        let cmd = self.cmd.clone();
        let store = self.store.clone();
        let last_err = self.last_login_error.clone();
        let base = self.base.clone();
        *slot = Some(tokio::spawn(async move {
            login::listener(base, tcp, plan, cmd, store, last_err).await;
        }));
        open_browser(auth_url);
        Ok(())
    }

    /// P5 T4: hand an inbound deep-link URL to the pending DeepLink login
    /// (`NoLoginPending` / state mismatch ⇒ Err, slot kept). SKELETON.
    pub async fn deliver_callback(&self, _url: &str) -> Result<(), CloudError> {
        todo!("P5 T4: deliver_callback")
    }

    /// P5 T5: app foreground signal; `false` suspends after a 3 s debounce,
    /// `true` resumes now. SKELETON.
    pub fn set_foreground(&self, _fg: bool) {
        todo!("P5 T5: set_foreground")
    }

    /// A login listener is running right now (G.1 step 5). The handle's
    /// `login` cancels it (design G.1); the API instead refuses with
    /// `already_logged_in` (H.1) so a double click cannot kill a flow the
    /// user is completing in the browser (erratum E-F3).
    pub async fn login_in_flight(&self) -> bool {
        self.login_task
            .lock()
            .await
            .as_ref()
            .is_some_and(|t| !t.is_finished())
    }

    /// Logout = `POST /logout` (best effort) + WIPE (seed, token, chain,
    /// pin) → `LoggedOut{SignedOut}` (§G.6).
    pub async fn logout(&self) -> Result<LogoutOutcome, CloudError> {
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Logout { out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// This device's id (derived from the seed).
    pub fn device_id(&self) -> Option<DeviceId> {
        self.device_id
    }

    /// `last_login_error` of the API state (G.1 step 5): set by a failed
    /// login, cleared by the next successful attempt.
    pub fn last_login_error(&self) -> Option<String> {
        self.last_login_error.lock().ok()?.clone()
    }

    /// Open a stream to `device` for `op` (§D.5): Active + the peer in
    /// `chain.active` only; `Unreachable{to}` ⇒ `CloudError::Offline`.
    pub async fn open(&self, device: DeviceId, op: Op) -> Result<CloudStream, CloudError> {
        let kind = self.state.borrow().kind();
        if kind != CloudStateKind::Active {
            return Err(CloudError::NotActive(kind));
        }
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Open { device, op, out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// Stop the actor (handle teardown): the relay socket closes, so the
    /// device drops offline for its peers. Test seam for the "offline
    /// computer" e2e case (a core can't otherwise be stopped in-process).
    pub async fn shutdown(&self) {
        let _ = self.cmd.send(CloudCmd::Stop).await;
    }

    /// Genesis for an empty account (§G.2).
    pub async fn genesis(&self) -> Result<DeviceId, CloudError> {
        let kind = self.state.borrow().kind();
        if kind != CloudStateKind::Pending {
            return Err(CloudError::NotActive(kind));
        }
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Genesis { out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// Approve an `EnrollRequest` (§G.4): `Add` at head+1; the reply rides
    /// the `ChainChanged` sync (one rebuild on a relay conflict).
    pub async fn approve(&self, letter_id: LetterId) -> Result<DeviceId, CloudError> {
        let kind = self.state.borrow().kind();
        if kind != CloudStateKind::Active {
            return Err(CloudError::NotActive(kind));
        }
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Approve { letter_id, out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// Redial after a rejection with a fresh `EnrollRequest` (P4 E-F7).
    pub async fn retry_enroll(&self) -> Result<(), CloudError> {
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::RetryEnroll { out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// Reject an `EnrollRequest` (§G.4 step 4): sealed `EnrollReject`.
    pub async fn reject(&self, letter_id: LetterId) -> Result<(), CloudError> {
        let kind = self.state.borrow().kind();
        if kind != CloudStateKind::Active {
            return Err(CloudError::NotActive(kind));
        }
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Reject { letter_id, out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// Revoke a peer (§G.5): `Revoke` entry; self ⇒ `use_logout`, an
    /// unknown id ⇒ `unknown_device`.
    pub async fn revoke(&self, target: DeviceId) -> Result<(), CloudError> {
        let kind = self.state.borrow().kind();
        if kind != CloudStateKind::Active {
            return Err(CloudError::NotActive(kind));
        }
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Revoke { target, out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }

    /// The `Profiles` op on a peer (§E.3: local profiles only).
    pub async fn profiles(
        &self,
        device: DeviceId,
    ) -> Result<Vec<crate::cloud::session::ProfileInfo>, CloudError> {
        let kind = self.state.borrow().kind();
        if kind != CloudStateKind::Active {
            return Err(CloudError::NotActive(kind));
        }
        let (out, rx) = oneshot::channel();
        self.cmd
            .send(CloudCmd::Profiles { device, out })
            .await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?;
        rx.await
            .map_err(|_| CloudError::Io("the cloud actor stopped".into()))?
    }
}

/// P5: like [`spawn`] but with the OAuth callback source (desktop = `Loopback`).
/// SKELETON: `callback` is ignored until T4.
pub fn spawn_with_callback(
    cfg: CloudConfig,
    store: Arc<dyn SeedStore>,
    inbound: Option<InboundHook>,
    callback: login::CallbackSource,
) -> CloudHandle {
    let _ = callback;
    spawn(cfg, store, inbound)
}

/// Spawn the process-wide actor (p3-design §D.6 / B8). Both editions;
/// Local passes `inbound = None`. No `general.cloud` ⇒ the caller does not
/// spawn (state `Disabled` is the config-level absence).
pub fn spawn(
    cfg: CloudConfig,
    store: Arc<dyn SeedStore>,
    inbound: Option<InboundHook>,
) -> CloudHandle {
    let origin = cfg.relay.origin().clone();
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let handle = |state_rx, device_id| CloudHandle {
        cmd: cmd_tx.clone(),
        state: state_rx,
        base: origin.clone(),
        store: store.clone(),
        device_id,
        login_task: Arc::new(tokio::sync::Mutex::new(None)),
        last_login_error: Arc::new(std::sync::Mutex::new(None)),
    };
    let id = match load_identity(&cfg, &*store) {
        Ok(id) => id,
        Err(e) => {
            // spawn() is infallible by the seam: an identity failure is a
            // terminal state and no actor runs (open() then reports Io).
            tracing::error!(error = %e, "cloud identity unavailable");
            let (_tx, state_rx) = watch::channel(CloudState::Trust(TrustError::ChainInvalid(
                format!("identity: {e}"),
            )));
            return handle(state_rx, None);
        }
    };
    let public = PublicState::new(&cfg.state_dir, &origin);
    // A stored token means we connect at once.
    let initial = if store.load_token().ok().flatten().is_some() {
        CloudState::Connecting {
            attempt: 0,
            last_error: None,
        }
    } else {
        CloudState::LoggedOut {
            reason: LogoutReason::Never,
        }
    };
    let (state_tx, state_rx) = watch::channel(initial);
    let h = handle(state_rx, Some(id.id));
    let (actor, out_rx) = actor::Actor::new(cfg, store, public, inbound, id);
    tokio::spawn(actor::run(actor, cmd_rx, out_rx, state_tx));
    h
}

/// The seed is the single source of identity (§C): create one if absent.
/// A failing OS RNG is an error, never a weak fallback seed.
fn load_identity(cfg: &CloudConfig, store: &dyn SeedStore) -> Result<actor::Identity, String> {
    let seed = match store.load_seed().map_err(|e| e.to_string())? {
        Some(s) => s,
        None => {
            let mut bytes = [0u8; 64];
            getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
            let s = Seed::new(bytes);
            store.store_seed(&s).map_err(|e| e.to_string())?;
            s
        }
    };
    actor::Identity::new(&seed, &cfg.device_name, cfg.platform)
}
