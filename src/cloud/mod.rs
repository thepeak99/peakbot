//! Cloud Mode (p3-design): one process-wide [`CloudHandle`] actor owns the
//! relay socket, the verified chain, and every E2E peer session.
//!
//! PR-B is RED: the public seams below are locked (p3-design §B, §C, §D.6,
//! §H) and the bodies are `todo!()` — the tests in `tests/cloud_*` are
//! written against them. Protocol logic lives only in `peakbot-cloud`;
//! this module is IO, state and glue.
//!
//! Edition gating (p3-design §A): only [`keyring`] is host-only. The Local
//! edition builds the actor with `inbound = None` (controller-only).

// RED skeleton (todo!() bodies): drop with the green phase.
#![allow(unused_variables, dead_code)]

pub mod config;
pub mod enroll;
pub mod link;
pub mod login;
pub mod session;
pub mod store;
pub mod stream;

#[cfg(feature = "host")]
pub mod keyring;

pub use config::{CloudConfig, CloudDef};
pub use link::{
    CloudError, CloudHandle, CloudState, CloudStateKind, DeviceInfo, EnrollRequestInfo,
    InboundHook, LogoutReason, TrustError, spawn,
};
pub use store::{Seed, SeedStore, SessionToken};
