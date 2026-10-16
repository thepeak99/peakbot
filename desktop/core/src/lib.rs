//! `peakbot-desktop-core`: the logic crate for the PeakBot Desktop shell
//! (DESIGN.md §2). All decisions are pure functions plus std-only side
//! effects behind the `Sys` trait; the Tauri glue stays logic-free.
//!
//! The modules carry the public signatures locked in DESIGN §3/§5, and
//! each file's `#[cfg(test)]` module carries its share of the §9.1 test
//! list (`t01`–`t65`). `testing` holds the fakes (`FakeSys` /
//! `FakeChild` / `FakeUi`); `tests/real_sys.rs` carries the C8
//! integration cases (unix only).

pub mod consts;
pub mod controller;
pub mod env;
pub mod nav;
pub mod paths;
pub mod phase;
pub mod ready;
pub mod service;
pub mod session;
pub mod sys;
pub mod token;
pub mod version;

#[cfg(test)]
pub(crate) mod testing;
