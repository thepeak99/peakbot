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
    #[allow(dead_code)] // dead until the image-link-rewrite task lands
    #[allow(unused_variables)] // params kept verbatim for the follow-up task
    fn resolve(&mut self, target: &str, cwd: &FsPath) -> Option<ImageRef> {
        unimplemented!("image-link-rewrite: resolve a link target to an image ref")
    }
}

/// Rewrite every markdown image whose target `f` maps to `Some(id)`.
#[allow(dead_code)] // dead until the image-link-rewrite task lands
#[allow(unused_variables)] // params kept verbatim for the follow-up task
fn rewrite_markdown_images(
    src: &str,
    f: impl FnMut(&str) -> Option<String>,
) -> std::borrow::Cow<'_, str> {
    unimplemented!("image-link-rewrite: rewrite markdown image targets")
}

#[cfg(test)]
mod tests {
    use super::rewrite_markdown_images;
    use std::borrow::Cow;
    use std::cell::RefCell;

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
}
