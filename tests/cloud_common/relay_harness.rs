//! The REAL relay's test harness (`crates/relay/tests/common/mod.rs`),
//! included verbatim: `FakeIdp` (in-test OIDC issuer) + `TestRelay`
//! (in-process `peakbot_relay::serve`, temp DB, no-browser login dance).
//!
//! Sharing approach: a `#[path]` include — the file is self-contained
//! (no submodules), so this reuses the relay's exact harness with no
//! feature flag on `peakbot-relay` and no ~700-line copy. Host-gated:
//! it drags `libc`/IdP crypto into the test build.

#![allow(dead_code)]

#[path = "../../crates/relay/tests/common/mod.rs"]
pub mod real;
