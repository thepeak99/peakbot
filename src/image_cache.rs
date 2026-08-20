//! Content-addressed spill cache for images (plan task T2).
//!
//! Large base64 image payloads must stop living in the conversation
//! transcript; instead the bytes are written once to a temp directory and
//! the transcript carries a small [`ImageRef`] (content id + display name).
//!
//! `spill`/`path_for` are thin wrappers around the private `spill_in`/
//! `path_for_in` seam, which takes the directory explicitly so tests can
//! inject an isolated `tempfile::TempDir` instead of sharing the
//! process-wide [`dir`].

use rig_core::completion::message::ImageMediaType;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A displayable image, by reference. Bytes live in the spill cache.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Content address: `<sha256-hex>.<ext>`. Also the spill file name.
    pub id: String,
    /// Source basename, e.g. "shot.png" — alt text / display.
    pub display_name: String,
}

/// Canonical extension for a media type, or `None` if unsupported for spilling
/// (its id could never satisfy `path_for`'s grammar, so minting one would be
/// an unresolvable reference).
fn extension_for(media_type: &ImageMediaType) -> Option<&'static str> {
    match media_type {
        ImageMediaType::PNG => Some("png"),
        ImageMediaType::JPEG => Some("jpg"),
        ImageMediaType::GIF => Some("gif"),
        ImageMediaType::WEBP => Some("webp"),
        ImageMediaType::HEIC | ImageMediaType::HEIF | ImageMediaType::SVG => None,
    }
}

/// `<std::env::temp_dir()>/peakbot/images`. Created on demand.
pub fn dir() -> PathBuf {
    let dir = std::env::temp_dir().join("peakbot").join("images");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!("failed to create image cache dir {}: {e}", dir.display());
    }
    dir
}

/// Write `bytes` to `<dir>/<sha256-hex>.<ext>` (idempotent: skip the write if the file already exists).
/// Returns None and logs at `warn` on ANY I/O failure — must never panic, never propagate an error.
pub fn spill(bytes: &[u8], media_type: ImageMediaType, display_name: &str) -> Option<ImageRef> {
    spill_in(&dir(), bytes, media_type, display_name)
}

fn spill_in(
    dir: &Path,
    bytes: &[u8],
    media_type: ImageMediaType,
    display_name: &str,
) -> Option<ImageRef> {
    let ext = extension_for(&media_type)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let hash: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let id = format!("{hash}.{ext}");
    let path = dir.join(&id);

    if !path.exists()
        && let Err(e) = std::fs::write(&path, bytes)
    {
        tracing::warn!("failed to spill image to {}: {e}", path.display());
        return None;
    }

    Some(ImageRef {
        id,
        display_name: display_name.to_string(),
    })
}

/// Parse-at-the-boundary. Returns `Some(path)` IFF `id` matches
/// `^[0-9a-f]{64}\.(png|jpg|jpeg|gif|webp)$` AND the file exists.
/// MUST NEVER join an unvalidated id onto the directory (path-traversal defence).
pub fn path_for(id: &str) -> Option<PathBuf> {
    path_for_in(&dir(), id)
}

fn path_for_in(dir: &Path, id: &str) -> Option<PathBuf> {
    // Validate the id's grammar before ever touching the filesystem — an
    // unvalidated id must never be joined onto `dir` (path-traversal).
    let (hash, ext) = id.split_once('.')?;
    let is_hex64 = hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    let is_known_ext = matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp");
    if !is_hex64 || !is_known_ext {
        return None;
    }

    let path = dir.join(id);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::ImageMediaType;
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use tempfile::TempDir;

    // -- helpers -------------------------------------------------------

    /// Independent sha256-hex computation, so tests don't just mirror
    /// whatever hash the implementation happens to pick.
    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// 64 lowercase hex characters — a well-formed content-address prefix.
    fn hex64() -> String {
        "ab".repeat(32)
    }

    fn count_entries(dir: &Path) -> usize {
        std::fs::read_dir(dir).expect("read_dir").count()
    }

    #[cfg(unix)]
    fn running_as_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
            .unwrap_or(false)
    }

    // -- 1. dedupe: identical bytes -> one file, same id ----------------

    #[test]
    fn spill_in_dedupes_identical_bytes_into_one_file() {
        let tmp = TempDir::new().expect("tempdir");
        let bytes = b"identical payload for dedupe test";

        let first = spill_in(tmp.path(), bytes, ImageMediaType::PNG, "a.png").expect("first spill");
        let second =
            spill_in(tmp.path(), bytes, ImageMediaType::PNG, "a-again.png").expect("second spill");

        assert_eq!(
            first.id, second.id,
            "identical bytes + same media type must yield the same content address"
        );
        assert_eq!(
            count_entries(tmp.path()),
            1,
            "spilling identical bytes twice must not create a second file"
        );
        // display_name is per-call, not merged/deduped.
        assert_eq!(first.display_name, "a.png");
        assert_eq!(second.display_name, "a-again.png");
    }

    // -- 2. different bytes -> different ids -----------------------------

    #[test]
    fn spill_in_yields_different_ids_for_different_bytes() {
        let tmp = TempDir::new().expect("tempdir");
        let a = spill_in(tmp.path(), b"payload A", ImageMediaType::PNG, "a.png").expect("spill a");
        let b = spill_in(tmp.path(), b"payload B", ImageMediaType::PNG, "b.png").expect("spill b");

        assert_ne!(a.id, b.id);
        assert_eq!(count_entries(tmp.path()), 2);
    }

    // -- 3. id format: sha256-hex + supported extension ------------------

    #[test]
    fn spill_in_id_is_sha256_hex_dot_extension_for_every_supported_media_type() {
        let tmp = TempDir::new().expect("tempdir");
        let re = regex::Regex::new(r"^[0-9a-f]{64}\.(png|jpg|jpeg|gif|webp)$").unwrap();
        let cases: [(ImageMediaType, &[u8]); 4] = [
            (ImageMediaType::PNG, b"png-bytes-1"),
            (ImageMediaType::JPEG, b"jpeg-bytes-2"),
            (ImageMediaType::GIF, b"gif-bytes-3"),
            (ImageMediaType::WEBP, b"webp-bytes-4"),
        ];
        for (media_type, bytes) in cases {
            let r = spill_in(tmp.path(), bytes, media_type, "irrelevant.bin").expect("spill");
            assert!(
                re.is_match(&r.id),
                "id `{}` does not match ^[0-9a-f]{{64}}\\.(png|jpg|jpeg|gif|webp)$",
                r.id
            );
            let (hash_part, _ext) = r.id.split_once('.').expect("id must have an extension");
            assert_eq!(
                hash_part,
                sha256_hex(bytes),
                "hash component must be sha256(bytes) in lowercase hex"
            );
        }
    }

    // -- 4. spilled file bytes are byte-identical to input ---------------

    #[test]
    fn spill_in_file_contents_are_byte_identical_to_input() {
        let tmp = TempDir::new().expect("tempdir");
        // All 256 byte values, not just ASCII — catches accidental text-mode
        // writes or encoding round-trips.
        let bytes: Vec<u8> = (0u8..=255).cycle().take(5000).collect();

        let r = spill_in(tmp.path(), &bytes, ImageMediaType::PNG, "big.png").expect("spill");
        let on_disk = std::fs::read(tmp.path().join(&r.id)).expect("read spilled file");

        assert_eq!(on_disk, bytes);
    }

    #[test]
    fn spill_in_accepts_empty_byte_payload() {
        // Spec doesn't forbid a zero-length payload; content-addressing is
        // still well-defined (sha256 of the empty string). If the real
        // implementation is meant to reject empty input instead, this test
        // must be revisited together with the Architect (flagged in report).
        let tmp = TempDir::new().expect("tempdir");
        let r = spill_in(tmp.path(), b"", ImageMediaType::PNG, "empty.png")
            .expect("spill of empty payload should succeed");

        let (hash_part, _ext) = r.id.split_once('.').expect("id must have an extension");
        assert_eq!(hash_part, sha256_hex(b""));

        let on_disk = std::fs::read(tmp.path().join(&r.id)).expect("read spilled file");
        assert!(on_disk.is_empty());
    }

    // -- 5. display_name carried through unchanged ------------------------

    #[test]
    fn spill_in_carries_display_name_through_unchanged() {
        let tmp = TempDir::new().expect("tempdir");
        let cases: [(&str, &[u8]); 4] = [
            ("shot.png", b"payload-1"),
            (
                "\u{30b9}\u{30af}\u{30ea}\u{30fc}\u{30f3}\u{30b7}\u{30e7}\u{30c3}\u{30c8} 001.png",
                b"payload-2",
            ),
            ("weird name with spaces & (parens).png", b"payload-3"),
            (
                "../not/a/real/path/but/should/pass/through.png",
                b"payload-4",
            ),
        ];
        for (name, bytes) in cases {
            let r = spill_in(tmp.path(), bytes, ImageMediaType::PNG, name).expect("spill");
            assert_eq!(
                r.display_name, name,
                "display_name must pass through unmodified for `{name}`"
            );
        }
    }

    // -- 6. path_for_in rejection matrix ----------------------------------

    #[test]
    fn path_for_in_rejects_path_traversal_dotdot_slash() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(path_for_in(tmp.path(), "../etc/passwd").is_none());
    }

    #[test]
    fn path_for_in_rejects_nested_path_traversal_with_valid_extension() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(path_for_in(tmp.path(), "../../etc/passwd.png").is_none());
    }

    #[test]
    fn path_for_in_never_escapes_directory_even_when_the_naive_join_target_exists() {
        // Prove the defence is structural (format validation before any
        // join), not merely "the file happens not to exist": plant a real
        // file exactly where a naive `dir.join(id)` would land, and confirm
        // path_for_in still refuses because the id itself is malformed.
        let root = TempDir::new().expect("tempdir");
        let cache_dir = root.path().join("cache");
        std::fs::create_dir_all(&cache_dir).expect("mkdir cache");
        let secret = root.path().join("secret.png");
        std::fs::write(&secret, b"top secret").expect("write secret");

        assert!(path_for_in(&cache_dir, "../secret.png").is_none());
    }

    #[test]
    fn path_for_in_rejects_non_hex_id() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(path_for_in(tmp.path(), "foo.png").is_none());
    }

    #[test]
    fn path_for_in_rejects_63_char_hex_id_with_valid_extension() {
        let tmp = TempDir::new().expect("tempdir");
        let short = format!("{}c.png", "ab".repeat(31)); // 63 hex chars total
        assert_eq!(short.split('.').next().unwrap().len(), 63);
        assert!(path_for_in(tmp.path(), &short).is_none());
    }

    #[test]
    fn path_for_in_rejects_well_formed_hash_with_unknown_extension() {
        let tmp = TempDir::new().expect("tempdir");
        let id = format!("{}.exe", hex64());
        assert!(path_for_in(tmp.path(), &id).is_none());
    }

    #[test]
    fn path_for_in_rejects_well_formed_hash_with_no_extension() {
        let tmp = TempDir::new().expect("tempdir");
        let id = hex64();
        assert!(path_for_in(tmp.path(), &id).is_none());
    }

    #[test]
    fn path_for_in_rejects_uppercase_hex_id() {
        let tmp = TempDir::new().expect("tempdir");
        let id = format!("{}.png", "AB".repeat(32));
        assert!(path_for_in(tmp.path(), &id).is_none());
    }

    #[test]
    fn path_for_in_rejects_well_formed_id_whose_file_does_not_exist() {
        let tmp = TempDir::new().expect("tempdir");
        let id = format!("{}.png", hex64());
        assert!(path_for_in(tmp.path(), &id).is_none());
    }

    #[test]
    fn path_for_in_rejects_empty_id() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(path_for_in(tmp.path(), "").is_none());
    }

    #[test]
    fn path_for_in_rejects_further_malformed_ids() {
        // Extra hardening beyond the spec's enumerated cases — not each
        // individually named because they all share the same "must fail
        // the format regex" reasoning as the cases above.
        let tmp = TempDir::new().expect("tempdir");
        let bad_ids = [
            "/etc/passwd".to_string(),
            "/etc/passwd.png".to_string(),
            format!("{}.png\0", hex64()),
            format!("{}..png", hex64()),
            format!("{}.png/", hex64()),
            format!(" {}.png", hex64()),
            format!("{}.png ", hex64()),
            format!("{}.PNG", hex64()),
            format!("{}.png.png", hex64()),
        ];
        for id in bad_ids {
            assert!(
                path_for_in(tmp.path(), &id).is_none(),
                "expected None for malformed id `{id:?}`"
            );
        }
    }

    // -- 7. path_for_in resolves a real, just-spilled id -------------------

    #[test]
    fn path_for_in_returns_some_for_a_well_formed_id_that_was_just_spilled() {
        let tmp = TempDir::new().expect("tempdir");
        let r = spill_in(
            tmp.path(),
            b"round trip bytes",
            ImageMediaType::PNG,
            "rt.png",
        )
        .expect("spill");

        let path = path_for_in(tmp.path(), &r.id)
            .expect("path_for_in should resolve an id it just spilled");

        assert_eq!(path, tmp.path().join(&r.id));
        assert!(path.is_file());
        assert_eq!(std::fs::read(&path).expect("read"), b"round trip bytes");
    }

    // -- 8. spill_in never panics / never propagates an error on I/O failure

    #[cfg(unix)]
    #[test]
    fn spill_in_returns_none_on_unwritable_directory_without_panicking() {
        use std::os::unix::fs::PermissionsExt;

        if running_as_root() {
            eprintln!(
                "skipping spill_in_returns_none_on_unwritable_directory_without_panicking: \
                 running as root, permission bits are ignored"
            );
            return;
        }

        let tmp = TempDir::new().expect("tempdir");
        let ro_dir = tmp.path().join("readonly-spill-target");
        std::fs::create_dir_all(&ro_dir).expect("mkdir");

        let mut perms = std::fs::metadata(&ro_dir).expect("metadata").permissions();
        perms.set_mode(0o555); // read + execute, no write
        std::fs::set_permissions(&ro_dir, perms).expect("chmod ro");

        let result = std::panic::catch_unwind(|| {
            spill_in(
                &ro_dir,
                b"unwritable-target-bytes",
                ImageMediaType::PNG,
                "x.png",
            )
        });

        // Restore write perms unconditionally so TempDir's Drop can clean up.
        let mut restore = std::fs::metadata(&ro_dir).expect("metadata").permissions();
        restore.set_mode(0o755);
        let _ = std::fs::set_permissions(&ro_dir, restore);

        match result {
            Ok(opt) => assert!(
                opt.is_none(),
                "expected None when the target directory is unwritable, got Some"
            ),
            Err(_) => panic!(
                "spill_in must never panic on an I/O failure (unwritable directory); it panicked instead"
            ),
        }
    }

    // -- 9. ImageRef serde round-trip -------------------------------------

    #[test]
    fn image_ref_serde_round_trips() {
        let original = ImageRef {
            id: format!("{}.png", sha256_hex(b"roundtrip")),
            display_name: "roundtrip.png".to_string(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: ImageRef = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn image_ref_serializes_with_id_and_display_name_keys() {
        let original = ImageRef {
            id: "x".into(),
            display_name: "y".into(),
        };
        let value: serde_json::Value = serde_json::to_value(&original).expect("to_value");
        assert_eq!(value.get("id").and_then(|v| v.as_str()), Some("x"));
        assert_eq!(
            value.get("display_name").and_then(|v| v.as_str()),
            Some("y")
        );
    }

    #[test]
    fn image_ref_deserialize_rejects_missing_required_field() {
        let bad = r#"{"id":"only-id-present"}"#;
        let result: Result<ImageRef, _> = serde_json::from_str(bad);
        assert!(
            result.is_err(),
            "an ImageRef JSON object missing display_name must fail to deserialize"
        );
    }

    #[test]
    fn image_ref_clone_and_partial_eq_work_as_declared() {
        let a = ImageRef {
            id: "same".into(),
            display_name: "same-name".into(),
        };
        let b = a.clone();
        assert_eq!(a, b);

        let c = ImageRef {
            id: "different".into(),
            display_name: "same-name".into(),
        };
        assert_ne!(a, c);
    }

    // -- public wrapper smoke tests -----------------------------------
    //
    // These exercise the real, process-wide `dir()`/`spill()`/`path_for()`
    // (no injected seam). Kept deliberately few, and made collision-safe
    // under parallel test execution by using unique-per-call content so a
    // dedupe hit from another test can never change what this test observes.

    #[test]
    fn dir_points_at_peakbot_images_under_the_system_temp_dir() {
        let expected = std::env::temp_dir().join("peakbot").join("images");
        assert_eq!(dir(), expected);
    }

    #[test]
    fn dir_creates_the_directory_if_it_does_not_already_exist() {
        let d = dir();
        assert!(
            d.exists() && d.is_dir(),
            "dir() must create `{}` on demand",
            d.display()
        );
    }

    #[test]
    fn path_for_public_wrapper_returns_none_for_a_malformed_id() {
        assert!(path_for("not-a-valid-id").is_none());
    }

    #[test]
    fn spill_and_path_for_public_wrappers_round_trip_smoke() {
        let unique = format!(
            "peakbot-image-cache-smoke-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let bytes = unique.as_bytes();

        let r = spill(bytes, ImageMediaType::PNG, "smoke.png").expect("spill via public wrapper");
        assert_eq!(r.display_name, "smoke.png");

        let resolved = path_for(&r.id)
            .expect("path_for via public wrapper should resolve what spill just wrote");
        assert_eq!(std::fs::read(&resolved).expect("read"), bytes);

        // Best-effort cleanup only — the real temp dir is shared process-wide
        // state, so we don't assert on it beyond this point.
        let _ = std::fs::remove_file(&resolved);
    }
}
