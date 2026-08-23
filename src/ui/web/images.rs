//! `GET /images/{id}` — serves bytes from the content-addressed image cache
//! (`crate::image_cache`) to the browser. The id IS the sha256 of the body,
//! so a given URL can never return different content: `immutable` caching
//! is safe, and the id's extension set is closed (five values).
//!
//! Presentation-time link rewriting (`ImageLinks`) is stubbed here; the
//! image-link-rewrite follow-up task implements it.

use crate::image_cache::{self, ImageRef};
use crate::ui::app_state::AppState;
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::time::SystemTime;

/// GET /images/{id} — serve bytes from the content-addressed image cache.
pub(crate) async fn image_handler(Path(id): Path<String>) -> Response {
    // `path_for` is the whole security boundary (grammar check before any
    // join); this handler owns zero path logic of its own.
    let path = match image_cache::path_for(&id) {
        Some(path) => path,
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    // One 404 for both "no such id" and "read failed" — no existence oracle.
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type_for(&id)),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response()
}

/// `path_for` already constrained the extension to a closed 5-value set, so
/// this match is total — no `application/octet-stream` fallback is possible
/// (unlike `serve_asset`, which faces an open set via `mime_guess`).
fn content_type_for(id: &str) -> &'static str {
    match id.split_once('.').map(|(_, ext)| ext) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => unreachable!("path_for already rejected any other extension"),
    }
}

/// One connection's memo of "what does this link target resolve to".
/// Lives on the forwarder task; `&mut self` is the whole concurrency story.
#[derive(Default)]
#[allow(dead_code)] // dead until the image-link-rewrite task lands
pub(crate) struct ImageLinks(HashMap<PathBuf, Entry>);

/// What we learned about one path. `Absent` carries no stamp, so
/// "no file but a servable image" is unrepresentable.
#[allow(dead_code)] // dead until the image-link-rewrite task lands
enum Entry {
    Absent,
    Present {
        stamp: (u64, SystemTime),
        image: Option<ImageRef>,
    },
}

impl ImageLinks {
    /// Presentation-time rewrite of one outbound snapshot.
    ///
    /// Takes `AppState` BY VALUE: the caller must own a detached copy, so this
    /// can never be applied to `StateManager`'s shared state (and therefore can
    /// never reach disk via `sync_to_conversation`).
    #[allow(dead_code)] // dead until the image-link-rewrite task lands
    #[allow(unused_variables)] // params kept verbatim for the follow-up task
    pub(crate) fn rewrite(&mut self, state: AppState, cwd: &FsPath) -> AppState {
        unimplemented!("image-link-rewrite: rewrite outbound snapshot image links")
    }

    /// `None` = this target does not resolve to a servable local image.
    #[allow(dead_code)] // only ImageLinks::rewrite (follow-up task) calls this
    fn resolve(&mut self, target: &str, cwd: &FsPath) -> Option<ImageRef> {
        // Policy skips run before any filesystem access: URLs, data URIs, and
        // already-rewritten /images/ ids are not local paths.
        if target.contains("://") || target.starts_with("data:") || target.starts_with("/images/") {
            return None;
        }

        let abs = match target.strip_prefix('~') {
            Some(rest) => dirs::home_dir()?.join(rest.trim_start_matches('/')),
            None if FsPath::new(target).is_absolute() => PathBuf::from(target),
            None => cwd.join(target),
        };

        // One stat per link per frame is deliberate: a regenerated chart.png
        // must update, so the (len, mtime) stamp is re-checked every time.
        let md = std::fs::metadata(&abs).ok();
        let stamp = md
            .as_ref()
            .map(|md| (md.len(), md.modified().unwrap_or(SystemTime::UNIX_EPOCH)));

        // Keyed on the resolved path: ChatMessage has no id and positional
        // indices shift under compaction, so the path is the only stable key
        // (it also dedupes the same file across messages).
        match (self.0.get(&abs), stamp.as_ref()) {
            (Some(Entry::Absent), None) => return None, // cached negative
            (
                Some(Entry::Present {
                    stamp: cached,
                    image,
                }),
                Some(st),
            ) if *cached == *st => {
                return image.clone(); // cached positive
            }
            _ => {}
        }

        // Miss: recompute. load_image_from_path is the single "servable local
        // image" gate (extension allow-list + MAX_IMAGE_BYTES via its own stat).
        let image = stamp.and_then(|_| {
            let att = crate::vision::load_image_from_path(&abs).ok()?;
            let crate::vision::ImageSource::Base64 { bytes, media_type } = att.source else {
                return None;
            };
            crate::image_cache::spill(&bytes, media_type, &att.display_name)
        });

        let entry = match stamp {
            None => Entry::Absent,
            Some(st) => Entry::Present {
                stamp: st,
                image: image.clone(),
            },
        };

        // Bound: clear-all at 1024 — no LRU state, self-healing, normally 0-10.
        if self.0.len() >= 1024 {
            self.0.clear();
        }
        self.0.insert(abs, entry);
        image
    }
}

/// Rewrite every markdown image whose target `f` maps to `Some(id)`.
///
/// Pure markdown mechanics — no I/O, no policy: every target found is passed
/// to `f` (remote-URL skipping lives in `resolve`). A `None` answer leaves
/// that span byte-identical, so we splice selectively into the original
/// string instead of rebuilding the document from events.
#[allow(dead_code)] // caller lands in the ImageLinks::rewrite follow-up task
fn rewrite_markdown_images(
    src: &str,
    mut f: impl FnMut(&str) -> Option<String>,
) -> std::borrow::Cow<'_, str> {
    // Fast path: 0 of 345 sampled assistant messages contain `![`, so the
    // common case must not parse or allocate.
    if !src.contains("![") {
        return std::borrow::Cow::Borrowed(src);
    }

    // Collect (span, replacement) pairs first — splicing during iteration
    // would invalidate the later offsets.
    let mut replacements: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut image: Option<(std::ops::Range<usize>, String, String)> = None; // span, dest, alt

    for (event, range) in pulldown_cmark::Parser::new(src).into_offset_iter() {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { dest_url, .. }) => {
                // Image tags cannot nest; the range spans the whole node.
                image = Some((range, dest_url.to_string(), String::new()));
            }
            pulldown_cmark::Event::Text(t) => {
                if let Some((_, _, alt)) = image.as_mut() {
                    alt.push_str(&t);
                }
            }
            pulldown_cmark::Event::Code(t) => {
                if let Some((_, _, alt)) = image.as_mut() {
                    alt.push_str(&t);
                }
            }
            pulldown_cmark::Event::SoftBreak => {
                if let Some((_, _, alt)) = image.as_mut() {
                    alt.push('\n');
                }
            }

            pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Image) => {
                if let Some((span, dest, alt)) = image.take()
                    && let Some(new_target) = f(&dest)
                {
                    let mut replacement = String::with_capacity(alt.len() + new_target.len() + 6);
                    replacement.push_str("![");
                    replacement.push_str(&escape_alt(&alt));
                    replacement.push_str("](");
                    replacement.push_str(&new_target);
                    replacement.push(')');
                    replacements.push((span, replacement));
                }
            }
            _ => {}
        }
    }

    if replacements.is_empty() {
        return std::borrow::Cow::Borrowed(src);
    }
    let mut out = String::with_capacity(src.len() + replacements.len() * 8);
    let mut last = 0;
    for (span, replacement) in replacements {
        out.push_str(&src[last..span.start]);
        out.push_str(&replacement);
        last = span.end;
    }
    out.push_str(&src[last..]);
    std::borrow::Cow::Owned(out)
}

/// Escape `\` and `]` (backslash first) so a spliced alt re-parses to the
/// same text.
fn escape_alt(alt: &str) -> String {
    let mut out = String::with_capacity(alt.len());
    for c in alt.chars() {
        if c == '\\' || c == ']' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::ImageLinks;
    use super::rewrite_markdown_images;
    use crate::image_cache::ImageRef;
    use crate::ui::app_state::{AppState, ChatMessage, MessageRole, MessageSource};
    use rig_core::completion::message::ImageMediaType;
    use std::borrow::Cow;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    /// Alt texts of every image node, in document order — reparse-based
    /// assertions avoid pinning the implementation's escape spelling.
    fn image_alts(src: &str) -> Vec<String> {
        let mut alts = Vec::new();
        let mut current = None;
        for event in pulldown_cmark::Parser::new(src) {
            match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { .. }) => {
                    current = Some(String::new())
                }
                pulldown_cmark::Event::End(pulldown_cmark::TagEnd::Image) => {
                    if let Some(alt) = current.take() {
                        alts.push(alt);
                    }
                }
                pulldown_cmark::Event::Text(text) => {
                    if let Some(alt) = current.as_mut() {
                        alt.push_str(&text);
                    }
                }
                _ => {}
            }
        }
        alts
    }

    /// Destination URLs of every image node, in document order.
    fn image_dests(src: &str) -> Vec<String> {
        pulldown_cmark::Parser::new(src)
            .filter_map(|event| match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Image { dest_url, .. }) => {
                    Some(dest_url.to_string())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn input_without_an_image_returns_borrowed() {
        let src = "hello [link](https://example.com) world\n\nsecond line `code` and **bold**\n";
        let out = rewrite_markdown_images(src, |_| Some("/images/x.png".to_string()));
        // the no-op fast path the whole feature's performance rests on
        assert!(
            matches!(out, Cow::Borrowed(_)),
            "no image nodes must borrow the input"
        );
        assert_eq!(out, src);
    }

    #[test]
    fn single_image_target_is_rewritten_to_the_resolved_id() {
        let out = rewrite_markdown_images("![alt](a.png)", |_| Some("/images/abc.png".to_string()));
        assert_eq!(out, "![alt](/images/abc.png)");
    }

    #[test]
    fn every_image_is_rewritten_when_all_targets_resolve() {
        let src = "![a](one.png) mid ![b](two.png) end ![c](three.png)";
        let out = rewrite_markdown_images(src, |target| match target {
            "one.png" => Some("/images/one.png".to_string()),
            "two.png" => Some("/images/two.png".to_string()),
            _ => Some("/images/three.png".to_string()),
        });
        assert_eq!(
            out,
            "![a](/images/one.png) mid ![b](/images/two.png) end ![c](/images/three.png)"
        );
    }

    #[test]
    fn unresolved_image_span_stays_byte_identical() {
        // the span carries its title verbatim when `f` declines it
        let src = "before ![a](x.png \"t\") after\n";
        let out = rewrite_markdown_images(src, |_| None);
        assert_eq!(out, src);
    }

    #[test]
    fn only_the_resolved_image_is_rewritten_the_other_stays_byte_identical() {
        let src = "![a](one.png) and ![b](two.png \"t\")";
        let out = rewrite_markdown_images(src, |target| {
            if target == "one.png" {
                Some("/images/one.png".to_string())
            } else {
                None
            }
        });
        assert_eq!(out, "![a](/images/one.png) and ![b](two.png \"t\")");
    }

    #[test]
    fn image_in_fenced_code_block_is_left_alone() {
        // pulldown-cmark emits code-block contents as Text, so we get this for free
        let calls = RefCell::new(Vec::<String>::new());
        let src = "before\n```\n![a](x.png)\n```\nafter\n";
        let out = rewrite_markdown_images(src, |target| {
            calls.borrow_mut().push(target.to_string());
            Some("/images/x.png".to_string())
        });
        assert!(
            calls.borrow().is_empty(),
            "code-block contents are not image nodes"
        );
        assert_eq!(out, src);
    }

    #[test]
    fn image_in_inline_code_is_left_alone() {
        let calls = RefCell::new(Vec::<String>::new());
        let src = "see `![a](x.png)` here\n";
        let out = rewrite_markdown_images(src, |target| {
            calls.borrow_mut().push(target.to_string());
            Some("/images/x.png".to_string())
        });
        assert!(
            calls.borrow().is_empty(),
            "inline code is not an image node"
        );
        assert_eq!(out, src);
    }

    #[test]
    fn alt_text_with_bracket_and_backslash_reparses_to_the_same_alt() {
        let src = "![a\\]b\\\\c](x.png)";
        // sanity: pin the input's own parse so a test-setup typo can't hide
        assert_eq!(image_alts(src), vec!["a]b\\c"]);
        let out = rewrite_markdown_images(src, |_| Some("/images/x.png".to_string()));
        assert_eq!(
            image_alts(&out),
            image_alts(src),
            "rewritten alt must reparse identically"
        );
    }

    #[test]
    fn empty_alt_stays_empty_when_resolved() {
        let out = rewrite_markdown_images("![](x.png)", |_| Some("/images/x.png".to_string()));
        assert_eq!(image_alts(&out), vec![""]);
        assert_eq!(image_dests(&out), vec!["/images/x.png"]);
    }

    #[test]
    fn f_is_called_once_per_image_with_the_raw_target_in_document_order() {
        let calls = RefCell::new(Vec::<String>::new());
        let src = "![a](one.png) mid ![b](two.png) end ![c](three.png)";
        let out = rewrite_markdown_images(src, |target| {
            calls.borrow_mut().push(target.to_string());
            Some("/images/x.png".to_string())
        });
        assert_eq!(*calls.borrow(), vec!["one.png", "two.png", "three.png"]);
        assert_eq!(
            out,
            "![a](/images/x.png) mid ![b](/images/x.png) end ![c](/images/x.png)"
        );
    }

    #[test]
    fn reference_style_image_is_rewritten_to_inline_form() {
        // honest v1 behaviour: pulldown-cmark emits reference images as Image
        // nodes too, so the span is spliced and the definition line dangles
        let src = "![alt][ref]\n\n[ref]: path.png\n";
        let out = rewrite_markdown_images(src, |_| Some("/images/abc.png".to_string()));
        assert_eq!(out, "![alt](/images/abc.png)\n\n[ref]: path.png\n");
    }

    #[test]
    fn titled_image_loses_its_title_on_rewrite() {
        // the splice is `![{alt}]({new_target})`, so the title is dropped
        let out = rewrite_markdown_images("![a](p.png \"title\")", |_| {
            Some("/images/abc.png".to_string())
        });
        assert_eq!(out, "![a](/images/abc.png)");
    }

    // -- resolve(): policy + filesystem + caching ------------------------

    /// Unique bytes per test: the spill dir is process-wide, so distinct
    /// content keeps parallel tests' content addresses from colliding.
    fn unique_bytes(seed: &str) -> Vec<u8> {
        format!("peakbot-image-link-red-{seed}-{}", uuid::Uuid::new_v4()).into_bytes()
    }

    /// Independent sha256-hex, so id assertions don't mirror the implementation.
    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// Best-effort removal of a file this suite spilled into the process-wide
    /// cache dir; that dir is shared state and is never asserted on.
    fn cleanup_spilled(id: &str) {
        if let Some(path) = crate::image_cache::path_for(id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Removes its file on drop (including unwind) so a RED panic cannot
    /// litter the real home dir.
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn resolve_returns_none_for_a_remote_target() {
        // The file exists locally under the same name: the scheme check must
        // run before any path resolution, or this target would resolve.
        let cwd = TempDir::new().expect("tempdir");
        std::fs::write(cwd.path().join("img.png"), unique_bytes("remote")).expect("write");
        let mut links = ImageLinks::default();
        assert_eq!(
            links.resolve("https://example.com/img.png", cwd.path()),
            None
        );
    }

    #[test]
    fn resolve_returns_none_for_a_data_uri_target() {
        let cwd = TempDir::new().expect("tempdir");
        let bytes = unique_bytes("data-uri");
        std::fs::write(cwd.path().join("img.png"), &bytes).expect("write");
        let mut links = ImageLinks::default();
        use base64::Engine;
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );

        assert_eq!(links.resolve(&uri, cwd.path()), None);
    }

    #[test]
    fn resolve_returns_none_for_an_already_rewritten_images_target() {
        // The id is real and its file exists in the cache: `/images/` is the
        // URL namespace, not a path, so this must be skipped without I/O.
        let bytes = unique_bytes("already-rewritten");
        let ref_ = crate::image_cache::spill(&bytes, ImageMediaType::PNG, "x.png").expect("spill");
        let mut links = ImageLinks::default();
        let cwd = TempDir::new().expect("tempdir");
        assert_eq!(
            links.resolve(&format!("/images/{}", ref_.id), cwd.path()),
            None
        );
        cleanup_spilled(&ref_.id);
    }

    #[test]
    fn resolve_resolves_a_relative_target_against_the_supplied_cwd() {
        let cwd = TempDir::new().expect("tempdir");
        let bytes = unique_bytes("relative");
        std::fs::write(cwd.path().join("rel.png"), &bytes).expect("write");

        let mut links = ImageLinks::default();
        let r = links
            .resolve("rel.png", cwd.path())
            .expect("relative target must resolve");

        assert_eq!(r.id, format!("{}.png", sha256_hex(&bytes)));
        assert_eq!(r.display_name, "rel.png");
        let path = crate::image_cache::path_for(&r.id)
            .expect("the spilled file must be servable under the returned id");
        assert_eq!(std::fs::read(&path).expect("read"), bytes);
        cleanup_spilled(&r.id);
    }

    #[test]
    fn resolve_resolves_an_absolute_target_regardless_of_cwd() {
        let file_dir = TempDir::new().expect("tempdir");
        let cwd = TempDir::new().expect("tempdir"); // unrelated and empty
        let bytes = unique_bytes("absolute");
        let file = file_dir.path().join("abs.png");
        std::fs::write(&file, &bytes).expect("write");

        let mut links = ImageLinks::default();
        let r = links
            .resolve(file.to_str().expect("utf8 path"), cwd.path())
            .expect("absolute target must resolve against itself, not cwd");
        assert_eq!(r.id, format!("{}.png", sha256_hex(&bytes)));
        cleanup_spilled(&r.id);
    }

    #[test]
    fn resolve_expands_a_tilde_target_to_the_home_dir() {
        // Writes one uniquely-named dotfile into the real home dir (removed
        // on drop); the expectation is built from home_dir(), nothing pinned.
        let home = std::env::home_dir().expect("HOME is set in the test environment");
        let name = format!(".peakbot-red-tilde-{}.png", uuid::Uuid::new_v4());
        let file = home.join(&name);
        let _guard = RemoveOnDrop(file.clone());
        let bytes = unique_bytes("tilde");
        std::fs::write(&file, &bytes).expect("write");

        let cwd = TempDir::new().expect("tempdir"); // unrelated: ~ must not use cwd
        let mut links = ImageLinks::default();
        let r = links
            .resolve(format!("~/{name}").as_str(), cwd.path())
            .expect("~ target must expand to the home dir");
        assert_eq!(r.id, format!("{}.png", sha256_hex(&bytes)));
        cleanup_spilled(&r.id);
    }

    #[test]
    fn resolve_returns_none_for_a_missing_file() {
        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        assert_eq!(links.resolve("no-such-file.png", cwd.path()), None);
    }

    #[test]
    fn resolve_returns_none_for_a_non_allowlisted_extension() {
        let cwd = TempDir::new().expect("tempdir");
        std::fs::write(cwd.path().join("notes.txt"), b"not an image").expect("write");
        let mut links = ImageLinks::default();
        assert_eq!(links.resolve("notes.txt", cwd.path()), None);
    }

    #[test]
    fn resolve_returns_none_for_a_file_over_max_image_bytes() {
        // Sparse via set_len: no blocks are allocated, so the 10MB+1 file is instant.
        let cwd = TempDir::new().expect("tempdir");
        let file = cwd.path().join("big.png");
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&file)
            .expect("create")
            .set_len((crate::vision::MAX_IMAGE_BYTES + 1) as u64)
            .expect("set_len");

        let mut links = ImageLinks::default();
        assert_eq!(links.resolve("big.png", cwd.path()), None);
    }

    #[test]
    fn resolve_caches_a_positive_hit_and_does_not_reread_the_body() {
        // Proof: after the first resolve the body is made unreadable (chmod
        // 000) while metadata stays intact — a cache hit still returns the
        // same id, a re-read would die with EACCES. If ever run as root the
        // chmod is a no-op and the proof degrades to id stability.
        let cwd = TempDir::new().expect("tempdir");
        let file = cwd.path().join("cached.png");
        let bytes = unique_bytes("positive-cache");
        std::fs::write(&file, &bytes).expect("write");

        let mut links = ImageLinks::default();
        let first = links
            .resolve("cached.png", cwd.path())
            .expect("first resolve");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        }
        let second = links
            .resolve("cached.png", cwd.path())
            .expect("second resolve must hit the cache, not re-read the body");
        assert_eq!(first.id, second.id);
        cleanup_spilled(&first.id);
    }

    #[test]
    fn resolve_caches_a_negative_result_for_a_missing_file() {
        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        assert_eq!(links.resolve("ghost.png", cwd.path()), None);
        assert_eq!(links.resolve("ghost.png", cwd.path()), None);
    }

    #[test]
    fn resolve_picks_up_a_file_that_appears_after_a_negative_result() {
        // A chart written after the first snapshot must show up on the next
        // rewrite — a memoized Absent would hide it forever (same bug class
        // as the staleness test below).
        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        assert_eq!(links.resolve("late.png", cwd.path()), None);

        let bytes = unique_bytes("late");
        std::fs::write(cwd.path().join("late.png"), &bytes).expect("write");
        let r = links
            .resolve("late.png", cwd.path())
            .expect("file now exists");
        assert_eq!(r.id, format!("{}.png", sha256_hex(&bytes)));
        cleanup_spilled(&r.id);
    }

    #[test]
    fn resolve_returns_a_new_id_when_the_file_content_changes() {
        // A regenerated chart that silently never updates is the bug this
        // test exists to prevent: same path, new bytes ⇒ new content address.
        let cwd = TempDir::new().expect("tempdir");
        let file = cwd.path().join("chart.png");
        let old = unique_bytes("stale-old");
        std::fs::write(&file, &old).expect("write");

        let mut links = ImageLinks::default();
        let first = links
            .resolve("chart.png", cwd.path())
            .expect("first resolve");

        // Distinct lengths plus an explicit future mtime force the (len,
        // mtime) stamp to change regardless of filesystem mtime granularity.
        let new = unique_bytes("stale-new-different-length");
        std::fs::write(&file, &new).expect("overwrite");
        let times =
            std::fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(3600));
        std::fs::File::open(&file)
            .expect("open")
            .set_times(times)
            .expect("set_times");

        let second = links
            .resolve("chart.png", cwd.path())
            .expect("second resolve");
        assert_ne!(first.id, second.id, "changed content must mint a new id");
        assert_eq!(second.id, format!("{}.png", sha256_hex(&new)));
        cleanup_spilled(&first.id);
        cleanup_spilled(&second.id);
    }

    #[test]
    fn resolve_dedupes_two_targets_with_identical_content_to_one_id() {
        let cwd = TempDir::new().expect("tempdir");
        let bytes = unique_bytes("dedupe");
        std::fs::write(cwd.path().join("a.png"), &bytes).expect("write a");
        std::fs::write(cwd.path().join("b.png"), &bytes).expect("write b");

        let mut links = ImageLinks::default();
        let ra = links.resolve("a.png", cwd.path()).expect("a resolves");
        let rb = links.resolve("b.png", cwd.path()).expect("b resolves");
        assert_eq!(
            ra.id, rb.id,
            "identical bytes must share one content address"
        );
        assert_eq!(ra.display_name, "a.png");
        assert_eq!(rb.display_name, "b.png");
        cleanup_spilled(&ra.id);
    }

    #[test]
    fn resolve_keeps_its_cache_bounded_across_many_distinct_targets() {
        // Pins the property, not the eviction spelling: the map never holds
        // more than 1024 entries and stays correct across the clear (an
        // early target re-resolves to the same id from disk).
        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        let first_bytes = unique_bytes("bound-first");
        std::fs::write(cwd.path().join("first.png"), &first_bytes).expect("write");
        let first = links
            .resolve("first.png", cwd.path())
            .expect("first resolves");

        let mut spilled = vec![first.id.clone()];
        for i in 0..1024 {
            let name = format!("f{i:04}.png");
            std::fs::write(cwd.path().join(&name), unique_bytes("bound")).expect("write");
            let r = links.resolve(&name, cwd.path()).expect("must resolve");
            spilled.push(r.id);
        }

        assert!(
            links.0.len() <= 1024,
            "cache grew to {} entries",
            links.0.len()
        );
        let again = links
            .resolve("first.png", cwd.path())
            .expect("re-resolves after the clear");
        assert_eq!(again.id, first.id);
        for id in &spilled {
            cleanup_spilled(id);
        }
    }

    // -- rewrite(): frame-level, takes AppState by value -----------------

    fn state_with(messages: Vec<ChatMessage>) -> AppState {
        let mut state = AppState::new();
        state.chat.messages = messages;
        state
    }

    /// A tool-result row with hand-set content/images — the
    /// `ChatMessage::tool_result` constructor would recompute content from
    /// the result string, which these tests need to control exactly.
    fn tool_result_row(tool_name: &str, content: &str, images: Vec<ImageRef>) -> ChatMessage {
        ChatMessage {
            role: MessageRole::ToolResult,
            content: content.to_string(),
            attachments: Vec::new(),
            images,
            timestamp: chrono::Local::now(),
            tool_name: Some(tool_name.to_string()),
            tool_args: None,
            tool_result: None,
            call_id: None,
            compacted: false,
            source: MessageSource::Human,
            thinking: Vec::new(),
            response_id: None,
        }
    }

    /// Field-wise comparison — ChatMessage does not derive PartialEq.
    fn assert_messages_equal(a: &[ChatMessage], b: &[ChatMessage]) {
        assert_eq!(a.len(), b.len(), "message count");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x.role, y.role, "msg {i} role");
            assert_eq!(x.content, y.content, "msg {i} content");
            assert_eq!(x.images, y.images, "msg {i} images");
            assert_eq!(x.attachments, y.attachments, "msg {i} attachments");
            assert_eq!(x.tool_name, y.tool_name, "msg {i} tool_name");
            assert_eq!(x.tool_args, y.tool_args, "msg {i} tool_args");
            assert_eq!(x.tool_result, y.tool_result, "msg {i} tool_result");
            assert_eq!(x.call_id, y.call_id, "msg {i} call_id");
            assert_eq!(x.compacted, y.compacted, "msg {i} compacted");
            assert_eq!(x.source, y.source, "msg {i} source");
            assert_eq!(x.thinking, y.thinking, "msg {i} thinking");
            assert_eq!(x.response_id, y.response_id, "msg {i} response_id");
            assert_eq!(x.timestamp, y.timestamp, "msg {i} timestamp");
        }
    }

    #[test]
    fn rewrite_replaces_content_of_a_message_with_images_instead_of_appending() {
        // The SPA re-shows the 🖼 text only as an image-load-error fallback,
        // so the markdown link must replace it, not follow it.
        let bytes = unique_bytes("replace");
        let ref_ =
            crate::image_cache::spill(&bytes, ImageMediaType::PNG, "shot.png").expect("spill");
        let msg = ChatMessage::tool_result(
            "view_image",
            r#"{"path":"shot.png"}"#,
            &format!(
                r#"{{"image_ref":{{"id":"{}","display_name":"shot.png"}}}}"#,
                ref_.id
            ),
            None,
        );
        assert_eq!(msg.content, "🖼 shot.png", "test-setup sanity");

        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());

        let got = &out.chat.messages[0];
        assert_eq!(got.content, format!("![shot.png](/images/{})", ref_.id));
        assert!(
            !got.content.contains("🖼"),
            "original marker text must be gone"
        );
        cleanup_spilled(&ref_.id);
    }

    #[test]
    fn rewrite_joins_multiple_image_refs_with_newlines() {
        let b1 = unique_bytes("multi-1");
        let b2 = unique_bytes("multi-2");
        let r1 = crate::image_cache::spill(&b1, ImageMediaType::PNG, "one.png").expect("spill 1");
        let r2 = crate::image_cache::spill(&b2, ImageMediaType::PNG, "two.png").expect("spill 2");

        let msg = tool_result_row("view_image", "🖼 one.png", vec![r1.clone(), r2.clone()]);
        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());

        let expected = format!(
            "![one.png](/images/{})\n![two.png](/images/{})",
            r1.id, r2.id
        );
        assert_eq!(out.chat.messages[0].content, expected);
        cleanup_spilled(&r1.id);
        cleanup_spilled(&r2.id);
    }

    #[test]
    fn rewrite_needs_no_filesystem_for_already_spilled_refs() {
        // The ref is already a content address of spilled bytes: cwd may
        // point at a directory that does not exist at all.
        let bytes = unique_bytes("no-fs");
        let ref_ =
            crate::image_cache::spill(&bytes, ImageMediaType::PNG, "shot.png").expect("spill");
        let msg = tool_result_row("view_image", "🖼 shot.png", vec![ref_.clone()]);

        let missing_cwd = PathBuf::from("/nonexistent-peakbot-red-suite-dir");
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), &missing_cwd);
        assert_eq!(
            out.chat.messages[0].content,
            format!("![shot.png](/images/{})", ref_.id)
        );
        cleanup_spilled(&ref_.id);
    }

    #[test]
    fn rewrite_escapes_a_display_name_containing_a_closing_bracket() {
        // Re-parse to assert: the alt must survive the round trip; the escape
        // spelling itself is the implementation's choice.
        let bytes = unique_bytes("escape");
        let ref_ =
            crate::image_cache::spill(&bytes, ImageMediaType::PNG, "we]ird.png").expect("spill");
        let msg = tool_result_row("view_image", "🖼 we]ird.png", vec![ref_.clone()]);

        let cwd = TempDir::new().expect("tempdir");
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());

        let content = &out.chat.messages[0].content;
        assert_eq!(image_alts(content), vec!["we]ird.png".to_string()]);
        assert_eq!(image_dests(content), vec![format!("/images/{}", ref_.id)]);
        cleanup_spilled(&ref_.id);
    }

    #[test]
    fn rewrite_rewrites_an_agent_row_image_link_to_the_spilled_id() {
        let cwd = TempDir::new().expect("tempdir");
        let bytes = unique_bytes("agent-link");
        std::fs::write(cwd.path().join("chart.png"), &bytes).expect("write");

        let msg = ChatMessage::agent("see ![chart](chart.png) below".to_string());
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());

        let id = format!("{}.png", sha256_hex(&bytes));
        assert_eq!(
            out.chat.messages[0].content,
            format!("see ![chart](/images/{id}) below")
        );
        cleanup_spilled(&id);
    }

    #[test]
    fn rewrite_does_not_path_rewrite_a_tool_result_row() {
        // A relative path inside tool output means "relative to that file",
        // not the session cwd — resolving it would show the wrong image.
        let cwd = TempDir::new().expect("tempdir");
        std::fs::write(cwd.path().join("x.png"), unique_bytes("tool-scope")).expect("write");

        let msg = tool_result_row("bash", "wrote ![x](./x.png) to disk", Vec::new());
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());
        assert_eq!(out.chat.messages[0].content, "wrote ![x](./x.png) to disk");
    }

    #[test]
    fn rewrite_does_not_path_rewrite_a_user_row() {
        let cwd = TempDir::new().expect("tempdir");
        std::fs::write(cwd.path().join("x.png"), unique_bytes("user-scope")).expect("write");

        let msg = ChatMessage::user("look at ![x](./x.png)".to_string());
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());
        assert_eq!(out.chat.messages[0].content, "look at ![x](./x.png)");
    }

    #[test]
    fn rewrite_applies_replacement_not_path_rewrite_to_a_tool_result_with_images() {
        // A row that is BOTH a tool result and carries images gets rule 1
        // (replacement): the markdown link in its content is left alone.
        let cwd = TempDir::new().expect("tempdir");
        std::fs::write(cwd.path().join("x.png"), unique_bytes("both-link")).expect("write");
        let img_bytes = unique_bytes("both-ref");
        let ref_ =
            crate::image_cache::spill(&img_bytes, ImageMediaType::PNG, "shot.png").expect("spill");

        let msg = tool_result_row(
            "view_image",
            "![x](./x.png) and 🖼 shot.png",
            vec![ref_.clone()],
        );
        let mut links = ImageLinks::default();
        let out = links.rewrite(state_with(vec![msg]), cwd.path());
        assert_eq!(
            out.chat.messages[0].content,
            format!("![shot.png](/images/{})", ref_.id)
        );
        cleanup_spilled(&ref_.id);
    }

    #[test]
    fn rewrite_returns_the_state_unchanged_when_no_images_are_present() {
        let messages = vec![
            ChatMessage::user("hello".to_string()),
            ChatMessage::agent("hi there".to_string()),
            ChatMessage::tool_call("bash", r#"{"command":"ls"}"#, Some("c1".into())),
            ChatMessage::tool_result(
                "bash",
                r#"{"command":"ls"}"#,
                "Exit code: 0\nSTDOUT:\nfile.txt",
                Some("c1".into()),
            ),
        ];
        let before = state_with(messages.clone());
        let after = ImageLinks::default().rewrite(state_with(messages), &PathBuf::from("/tmp"));
        assert_messages_equal(&before.chat.messages, &after.chat.messages);
    }

    // Invariant (type-system-enforced, not runtime-testable): `rewrite` takes
    // `AppState` BY VALUE, so it can never be applied to `StateManager`'s
    // shared state and can never leak rewritten links to disk via
    // `sync_to_conversation`. A runtime test here would be fake — none written.
}
