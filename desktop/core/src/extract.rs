//! The Windows sidecar extractor (DESIGN amendment (windows-portable)):
//! pure filesystem work — target path layout, atomic write-once, pruning
//! of older version folders. Knows nothing about zstd or Tauri.
//!
//! Invariants (DESIGN amendment (windows-portable)):
//! - I1 identity = content: the folder is named `<id>`; the payload
//!   lives at `bin_root/<id>/<sidecar_file_name()>`.
//! - I2 complete or absent: the target appears only after
//!   write → sync → rename; a crash leaves at most `<target>.tmp`.
//! - I4 never overwritten: an existing target is never rewritten (a
//!   running sidecar locks its image).

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::paths::sidecar_file_name;

/// Rename retries: Defender briefly holds freshly written executables,
/// so the rename can hit a sharing violation for a moment.
const RENAME_RETRIES: u32 = 10;
const RENAME_DELAY: Duration = Duration::from_millis(100);

/// `bin_root/<id>/peakbot[.exe]` — the extraction target for the payload
/// with identity `<id>`.
pub fn payload_path(bin_root: &Path, id: &str) -> PathBuf {
    bin_root.join(id).join(sidecar_file_name())
}

/// The crash-leftover temp file for `target`: `<target>.tmp`.
fn tmp_name(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(std::ffi::OsStr::new(".tmp"));
    PathBuf::from(s)
}

/// Write-once extraction (I2/I4): if `target` exists as a file, `Ok(())`
/// WITHOUT calling `write`. Otherwise: create the parent dirs, write to
/// `<target>.tmp` (truncating any crash leftover), `sync_all`, then
/// rename `<target>.tmp` → `target` — retried up to `RENAME_RETRIES`
/// times at `RENAME_DELAY`. On any error: the tmp is removed
/// best-effort and the error returned; the target is never left partial.
pub fn ensure_extracted(
    target: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    if target.is_file() {
        return Ok(());
    }
    let tmp = tmp_name(target);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        write(&mut file)?;
        file.sync_all()?;
        rename_with_retry(&tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn rename_with_retry(tmp: &Path, target: &Path) -> io::Result<()> {
    let mut retries = 0;
    loop {
        match fs::rename(tmp, target) {
            Ok(()) => return Ok(()),
            Err(e) => {
                retries += 1;
                if retries >= RENAME_RETRIES {
                    return Err(e);
                }
                std::thread::sleep(RENAME_DELAY);
            }
        }
    }
}

/// Best-effort: remove every *directory* in `target.parent().parent()`
/// (the bin root) other than `target.parent()` (the current version
/// folder). Errors are ignored — an in-use older image can't be deleted
/// on Windows and is retried on the next launch.
pub fn prune_siblings(target: &Path) {
    let Some(keep) = target.parent() else {
        return;
    };
    let Some(bin_root) = keep.parent() else {
        return;
    };
    let Ok(entries) = fs::read_dir(bin_root) else {
        return; // bin root missing: nothing to prune
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && path != keep {
            let _ = fs::remove_dir_all(&path);
        }
    }
}
