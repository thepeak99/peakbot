//! `core::extract` — the Windows sidecar extractor (DESIGN amendment
//! (windows-portable)).
//!
//! Written FIRST (RED): the `peakbot_desktop_core::extract` module does not
//! exist yet. These tests are the executable contract the developer builds to;
//! they compile against the public API exactly as locked in §4.2:
//!
//! ```rust
//! pub fn payload_path(bin_root: &Path, id: &str) -> PathBuf;
//! pub fn ensure_extracted(target: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()>;
//! pub fn prune_siblings(target: &Path);
//! ```
//!
//! The module is platform-neutral: on this (Linux) host the sidecar file name
//! is `"peakbot"` via the existing `paths::sidecar_file_name()` helper (on a
//! Windows host it would be `"peakbot.exe"`), so every assertion below is
//! written against that helper rather than a hardcoded `.exe`.
//!
//! Invariants under test (DESIGN amendment (windows-portable)):
//! - I1 identity = content: the folder is named `<id>`; the payload lives at
//!   `bin_root/<id>/<sidecar_file_name()>`.
//! - I2 complete or absent: the target appears only after write→sync→rename;
//!   a crash leaves at most `<target>.tmp`, which the next run overwrites.
//! - I4 never overwritten: an existing target is never rewritten.
//! - prune: every sibling *directory* in `bin_root` other than the current
//!   one is removed best-effort; files and the current dir survive.

use std::ffi::OsStr;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

use peakbot_desktop_core::extract;
use peakbot_desktop_core::paths::sidecar_file_name;

/// The crash-leftover temp file for `target`: `<target>.tmp` (WINDOWS-PLAN §2:
/// `bin_root/<ID>/peakbot[.exe].tmp`). Derived from the target so the test and
/// the implementation share one source of truth for the name.
fn tmp_sibling(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(OsStr::new(".tmp"));
    PathBuf::from(s)
}

/// A `<bin_root>/<id>` payload path for the id, using the real helper for the
/// file name (so the test is correct on both unix and windows hosts).
fn payload(bin_root: &Path, id: &str) -> PathBuf {
    bin_root.join(id).join(sidecar_file_name())
}

// X1 — the target-path layout: `bin_root/<id>/<sidecar_file_name()>`.
#[test]
fn x1_payload_path_is_bin_root_id_sidecar_file_name() {
    let root = Path::new("/home/u/AppData/Local/com.peakbot.desktop/bin");
    for id in ["0.23.0-3fa1c09be27d", "0.22.0-aaa", "1.0.0-deadbeef0000"] {
        let p = extract::payload_path(root, id);
        assert_eq!(
            p,
            root.join(id).join(sidecar_file_name()),
            "payload_path({root:?}, {id:?}) must be bin_root/<id>/<sidecar_file_name()>"
        );
        assert_eq!(
            p.parent().and_then(|d| d.file_name()),
            Some(OsStr::new(id)),
            "the parent dir must be named exactly <id>"
        );
        assert_eq!(
            p.file_name(),
            Some(OsStr::new(sidecar_file_name())),
            "the file name must come from sidecar_file_name()"
        );
    }
}

// X2 — happy path: target absent → write is called exactly once, the target
// carries the written bytes, the parent dirs are created, and no .tmp remains.
#[test]
fn x2_ensure_extracted_writes_once_creates_dirs_leaves_no_tmp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir
        .path()
        .join("bin")
        .join("0.23.0-abc")
        .join(sidecar_file_name());
    assert!(!target.exists(), "precondition: no target yet");

    let mut calls = 0;
    let res = extract::ensure_extracted(&target, |f| {
        calls += 1;
        f.write_all(b"the-sidecar-bytes")
    });
    res.expect("extraction of a missing target must succeed");

    assert_eq!(calls, 1, "write must be called exactly once");
    assert_eq!(
        std::fs::read(&target).expect("target exists"),
        b"the-sidecar-bytes",
        "the target must carry exactly the written bytes"
    );
    assert!(
        !tmp_sibling(&target).exists(),
        "no .tmp sibling may remain after a successful extraction"
    );
}

// X3 — I4 never overwritten: target exists → write is NOT called, content is
// untouched (a running sidecar locks its image, so rewriting is forbidden).
#[test]
fn x3_ensure_extracted_existing_target_is_never_rewritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("0.23.0-abc").join(sidecar_file_name());
    std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
    std::fs::write(&target, b"original-bytes").expect("seed target");

    let res = extract::ensure_extracted(&target, |_f| -> io::Result<()> {
        panic!("write must NOT be called when the target already exists")
    });
    res.expect("an existing target is a no-op success");

    assert_eq!(
        std::fs::read(&target).expect("target still exists"),
        b"original-bytes",
        "the existing target must be byte-for-byte unchanged"
    );
}

// X4 — error path: write returns Err → the error propagates, no partial target
// is left, and the .tmp is cleaned up.
#[test]
fn x4_ensure_extracted_write_error_propagates_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("0.23.0-abc").join(sidecar_file_name());

    let res = extract::ensure_extracted(&target, |_f| Err(io::Error::other("disk full")));
    assert!(res.is_err(), "a write error must propagate as Err");
    assert!(!target.exists(), "no (partial) target may be left behind");
    assert!(
        !tmp_sibling(&target).exists(),
        "the .tmp must be removed on the error path"
    );
}

// X5 — I2 crash recovery: a stale <target>.tmp from a crashed run is
// overwritten; the result is the new bytes and no .tmp remains.
#[test]
fn x5_ensure_extracted_overwrites_a_stale_tmp_left_by_a_crash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("0.23.0-abc").join(sidecar_file_name());
    std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");
    std::fs::write(tmp_sibling(&target), b"stale-junk-from-crash").expect("seed stale .tmp");

    let res = extract::ensure_extracted(&target, |f| f.write_all(b"fresh-bytes"));
    res.expect("extraction over a stale .tmp must succeed");

    assert_eq!(
        std::fs::read(&target).expect("target exists"),
        b"fresh-bytes",
        "the target must be the NEW bytes, not the stale .tmp content"
    );
    assert!(
        !tmp_sibling(&target).exists(),
        "the stale .tmp must be gone (consumed by the rename)"
    );
}

// X6 — prune: bin_root has an older version dir, the current version dir, and
// a stray FILE. The older dir is removed; the current dir and the file survive.
#[test]
fn x6_prune_siblings_removes_other_version_dirs_keeps_current_and_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin_root = dir.path().join("bin");
    let current = payload(&bin_root, "0.23.0-bbb");
    let older = payload(&bin_root, "0.22.0-aaa");
    let stray = bin_root.join("stray.txt");

    for p in [&current, &older] {
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, b"x").expect("seed");
    }
    std::fs::write(&stray, b"keep me").expect("seed stray file");

    extract::prune_siblings(&current);

    assert!(
        !bin_root.join("0.22.0-aaa").exists(),
        "the older version DIRECTORY must be removed"
    );
    assert!(!older.exists(), "the older version's payload must be gone");
    assert!(current.exists(), "the CURRENT version dir must be kept");
    assert!(stray.exists(), "a non-directory sibling must be kept");
}

// X7 — prune robustness: bin_root does not exist at all → no panic (best-effort
// removal of a missing dir is a no-op).
#[test]
fn x7_prune_siblings_with_missing_bin_root_does_not_panic() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Points into a bin_root that was never created.
    let target = dir
        .path()
        .join("does-not-exist")
        .join("bin")
        .join("0.23.0-bbb")
        .join(sidecar_file_name());
    extract::prune_siblings(&target); // must not panic
}
