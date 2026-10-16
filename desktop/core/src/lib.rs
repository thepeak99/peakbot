//! `peakbot-desktop-core`: the logic crate for the PeakBot Desktop shell
//! (DESIGN.md §2). All decisions are pure functions plus std-only side
//! effects behind the `Sys` trait; the Tauri glue stays logic-free.
//!
//! Phase 0 skeleton — modules per §3/§9.1 land in phases 2–3.
