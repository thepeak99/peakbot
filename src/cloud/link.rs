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
use tokio::sync::{mpsc, watch};
use url::Url;

use super::config::CloudConfig;
use super::store::{SeedStore, SessionToken};
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

/// A verified, unexpired `EnrollRequest` letter (§G, G.4 step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollRequestInfo {
    pub letter_id: LetterId,
    pub device: DeviceId,
    pub name: String,
    pub platform: String,
    pub sas: SasWords,
    pub expires_at: u64,
}

/// The published actor state (§D.1 + §G). Invariant: no sessions unless
/// `Active` (the session table lives inside the actor's private `Active`
/// phase).
#[derive(Debug, Clone, PartialEq)]
pub enum CloudState {
    Disabled,
    LoggedOut { reason: LogoutReason },
    Connecting { attempt: u32, last_error: Option<String> },
    /// `empty_account`: chain fetch returned 0 entries → genesis allowed.
    /// `sas`: `Some` once our `EnrollRequest` is posted (head frozen, §5.5).
    Pending { empty_account: bool, sas: Option<SasWords> },
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
}

/// Actor commands (private; the handle is the only door in).
pub enum CloudCmd {
    Login {
        base: Url,
        out: tokio::sync::oneshot::Sender<Result<(), CloudError>>,
    },
    TokenStored(SessionToken),
    Logout { out: tokio::sync::oneshot::Sender<Result<(), CloudError>> },
    Open {
        device: DeviceId,
        op: Op,
        out: tokio::sync::oneshot::Sender<Result<CloudStream, CloudError>>,
    },
    Genesis { out: tokio::sync::oneshot::Sender<Result<DeviceId, CloudError>> },
    Approve {
        letter_id: LetterId,
        out: tokio::sync::oneshot::Sender<Result<DeviceId, CloudError>>,
    },
    Reject { letter_id: LetterId, out: tokio::sync::oneshot::Sender<Result<(), CloudError>> },
    Revoke { target: DeviceId, out: tokio::sync::oneshot::Sender<Result<(), CloudError>> },
    Profiles {
        device: DeviceId,
        out: tokio::sync::oneshot::Sender<Vec<crate::cloud::session::ProfileInfo>>,
    },
}

impl CloudHandle {
    /// The published state (§D.1).
    pub fn state(&self) -> watch::Receiver<CloudState> {
        self.state.clone()
    }

    /// PKCE login (§G.1): runs in the caller's task; on success the actor
    /// reconnects with the stored token.
    pub async fn login(&self, open_browser: impl FnOnce(Url)) -> Result<(), CloudError> {
        todo!("p3 PR-B: CloudHandle::login")
    }

    /// Logout = self-revoke (best effort) + `POST /logout` + WIPE (seed,
    /// token, chain, pin) → `LoggedOut{SignedOut}` (§G.6).
    pub async fn logout(&self) -> Result<(), CloudError> {
        todo!("p3 PR-B: CloudHandle::logout")
    }

    /// Open a stream to `device` for `op` (§D.5): Active + the peer in
    /// `chain.active` only; `Unreachable{to}` ⇒ `CloudError::Offline`.
    pub async fn open(&self, device: DeviceId, op: Op) -> Result<CloudStream, CloudError> {
        todo!("p3 PR-B: CloudHandle::open")
    }

    /// Genesis for an empty account (§G.2).
    pub(crate) async fn genesis(&self) -> Result<DeviceId, CloudError> {
        todo!("p3 PR-B: CloudHandle::genesis")
    }

    /// Approve an `EnrollRequest` (§G.4): `Add` at head+1.
    pub(crate) async fn approve(&self, letter_id: LetterId) -> Result<DeviceId, CloudError> {
        todo!("p3 PR-B: CloudHandle::approve")
    }

    /// Reject an `EnrollRequest` (§G.4 step 4): sealed `EnrollReject`.
    pub(crate) async fn reject(&self, letter_id: LetterId) -> Result<(), CloudError> {
        todo!("p3 PR-B: CloudHandle::reject")
    }

    /// Revoke a peer (§G.5): `Revoke` entry; self ⇒ `use_logout`, the last
    /// active device ⇒ `last_device`.
    pub(crate) async fn revoke(&self, target: DeviceId) -> Result<(), CloudError> {
        todo!("p3 PR-B: CloudHandle::revoke")
    }

    /// The `Profiles` op on a peer (§E.3: local profiles only).
    pub(crate) async fn profiles(&self, device: DeviceId) -> Result<Vec<crate::cloud::session::ProfileInfo>, CloudError> {
        todo!("p3 PR-B: CloudHandle::profiles")
    }
}

/// Spawn the process-wide actor (p3-design §D.6 / B8). Both editions;
/// Local passes `inbound = None`. No `general.cloud` ⇒ the caller does not
/// spawn (state `Disabled` is the config-level absence).
pub fn spawn(cfg: CloudConfig, store: Arc<dyn SeedStore>, inbound: Option<InboundHook>) -> CloudHandle {
    let _ = (&cfg, &store, inbound);
    todo!("p3 PR-B: CloudLink actor (socket, chain sync, sessions, reconnect)")
}