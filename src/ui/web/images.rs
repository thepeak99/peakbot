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
