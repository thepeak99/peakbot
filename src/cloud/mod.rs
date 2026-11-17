//! Cloud Mode (p3-design): one process-wide [`CloudHandle`] actor owns the
//! relay socket, the verified chain, and every E2E peer session.
//!
//! The public seams are locked (p3-design §B, §C, §D.6, §H); protocol
//! logic lives only in `peakbot-cloud` — this module is IO, state and
//! glue.
//!
//! Edition gating (p3-design §A): only [`keyring`] is host-only. The Local
//! edition builds the actor with `inbound = None` (controller-only).

// Re-export the protocol core from peakbot-cloud so the rest of the crate
// imports from `cloud::` uniformly.
pub use peakbot_cloud::{
    DeviceId, LetterId, SessionId, Head,
    Error, Result,
    h, mac, mac_verify, sign_tagged, verify_tagged,
    card, chain, codec, constants, frames, handshake, letter, sas,
};

mod actor;
pub mod config;
pub mod enroll;
pub mod link;
pub mod login;
pub mod session;
pub mod store;
pub mod stream;

pub mod keyring;

pub use config::{CloudConfig, CloudDef};
pub use link::{
    CloudError, CloudHandle, CloudState, CloudStateKind, DeviceInfo, EnrollRequestInfo, FlowError,
    InboundHook, LogoutOutcome, LogoutReason, TrustError, spawn,
};
pub use store::{Seed, SeedStore, SessionToken};

// ── PR-C boot wiring: the process-wide handle the remote `Directory`
// reads to resolve `profile@device` names (§F.5). Set once at boot
// (like `paths::init_roots`): later calls are ignored, never mid-run.

static BOOT_HANDLE: std::sync::OnceLock<CloudHandle> = std::sync::OnceLock::new();

/// Boot wiring: publish the spawned handle (second call ignored).
pub fn set_boot_handle(h: CloudHandle) {
    let _ = BOOT_HANDLE.set(h);
}

/// The boot-published handle, if `general.cloud` was configured.
pub fn boot_handle() -> Option<CloudHandle> {
    BOOT_HANDLE.get().cloned()
}
