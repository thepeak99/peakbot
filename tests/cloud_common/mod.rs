//! Shared harness for the p3 PR-B RED tests.
//!
//! The relay (#460) is NOT on master, so these tests run against an
//! in-test FAKE RELAY: a small axum + WebSocket server that speaks the
//! `cloud` relay codec faithfully (Challenge → pre-Hello ChainFetch →
//! Hello verify via the `cloud` crate → Welcome; ChainAppend →
//! ChainChanged; Pipe forwarding between two connected devices;
//! Post/Letter/Ack with an offline mailbox) plus the fake OIDC endpoints
//! (`GET /login` start-redirect, `POST /token` with PKCE verification).
//! Exact routes from relay-design §5.2: `/login`, `/token`, `/ws`,
//! `/logout`.
//!
//! Deliberate test seams (documented, not protocol): `issue_code`
//! (simulates the IdP + `/oidc/callback`), `inject_letter` (relay-side
//! letter with an arbitrary `from` — the design's "tamper the relay DB"
//! case 6), `set_entries`/`append_entry` (fork/genesis-mismatch case 6),
//! `drop_all_sockets` (reconnect case), `hold_ws` (deterministic
//! `Connecting` state).

#![allow(dead_code)]

pub mod devices;
pub mod fake_relay;
pub mod rig;
