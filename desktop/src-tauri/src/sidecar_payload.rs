//! The embedded Windows sidecar payload (DESIGN amendment
//! (windows-portable)): the zstd blob + its identity, owned by the glue.
//! Compiled on Windows only (gated at the `mod` line in main.rs) — a
//! `peakbot.exe` sitting next to the desktop exe is intentionally
//! ignored: the payload is the single source.

use std::io;

/// `"<pkg-version>-<sha12>"` — computed once by build.rs from the staged
/// sidecar bytes (I1: identity = content).
pub const ID: &str = env!("PEAKBOT_SIDECAR_ID");

/// The zstd frame of the sidecar (level 19, single frame).
static BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/peakbot.exe.zst"));

/// Decode the payload into `f` (the extractor opens the tmp file).
pub fn write_to(f: &mut std::fs::File) -> io::Result<()> {
    let mut d = ruzstd::decoding::StreamingDecoder::new(BLOB).map_err(io::Error::other)?;
    io::copy(&mut d, f).map(drop)
}
