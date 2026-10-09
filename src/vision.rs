//! Vision support — image attachments on user turns.
//!
//! Scope: only images. Audio, video, documents explicitly not covered.
//!
//! ## Entry points
//!
//! - [`load_image_from_path`] — direct path → [`LoadedImage`] (bytes + media
//!   type), enforcing [`MAX_IMAGE_BYTES`] and media-type inference.
//! - [`media_type_from_mime`] — stored attachment mime → wire media type.
//! - [`model_supports_vision`] — model name → whether image input is accepted.
//!
//! `[img:…]` parsing and attachment storage live in `crate::attachments`;
//! the wire conversion lives in `state_manager.rs` (`user_content_from_chat_message`).

use rig_core::completion::message::ImageMediaType;
use std::path::{Path, PathBuf};

/// Maximum file size accepted for a single image attachment. Bigger files are
/// rejected with [`AttachmentError::TooLarge`] before any bytes are read.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// A local image loaded from disk: the bytes, the inferred media type, and
/// the file's basename for display. The one result of
/// [`load_image_from_path`] — it can never be a URL, which is what makes the
/// old `ImageSource` match at the call sites unnecessary.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    pub display_name: String,
    pub bytes: Vec<u8>,
    pub media_type: ImageMediaType,
}

/// Errors produced when parsing or loading attachments.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("file too large: {} ({size} bytes, max {max})", path.display())]
    TooLarge {
        path: PathBuf,
        size: usize,
        max: usize,
    },
    #[error("unsupported media type: {0} (supported: png, jpeg, gif, webp)")]
    UnsupportedMediaType(String),
    #[error("failed to read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Detect `ImageMediaType` from a file extension (any case). Returns `None`
/// for unsupported types — keeping the supported list narrow (PNG/JPEG/GIF/WEBP)
/// avoids surprising users with "the model rejected your HEIC".
pub fn media_type_from_extension(ext: &str) -> Option<ImageMediaType> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some(ImageMediaType::PNG),
        "jpg" | "jpeg" => Some(ImageMediaType::JPEG),
        "gif" => Some(ImageMediaType::GIF),
        "webp" => Some(ImageMediaType::WEBP),
        _ => None,
    }
}

/// Detect `ImageMediaType` from an image MIME type (e.g. `"image/png"`).
/// Mirrors [`media_type_from_extension`] for stored attachments, where the
/// media type is spelled as a MIME rather than a file extension.
pub fn media_type_from_mime(mime: &str) -> Option<ImageMediaType> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some(ImageMediaType::PNG),
        "image/jpeg" | "image/jpg" => Some(ImageMediaType::JPEG),
        "image/gif" => Some(ImageMediaType::GIF),
        "image/webp" => Some(ImageMediaType::WEBP),
        _ => None,
    }
}

/// Load an image from disk. Infers media type from the extension, enforces
/// [`MAX_IMAGE_BYTES`], and returns the bytes with their media type.
pub fn load_image_from_path(path: &Path) -> Result<LoadedImage, AttachmentError> {
    // Read metadata first — rejects oversize without touching the file body.
    let metadata = match std::fs::metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AttachmentError::NotFound(path.to_path_buf()));
        }
        Err(e) => {
            return Err(AttachmentError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };

    let size = metadata.len() as usize;
    if size > MAX_IMAGE_BYTES {
        return Err(AttachmentError::TooLarge {
            path: path.to_path_buf(),
            size,
            max: MAX_IMAGE_BYTES,
        });
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let media_type = media_type_from_extension(ext)
        .ok_or_else(|| AttachmentError::UnsupportedMediaType(ext.to_string()))?;

    let bytes = std::fs::read(path).map_err(|e| AttachmentError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let display_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image")
        .to_string();

    Ok(LoadedImage {
        display_name,
        bytes,
        media_type,
    })
}

/// Known-vision model patterns. Conservative: unknown models → `false`.
const VISION_MODEL_PATTERNS: &[&str] = &[
    "gpt-4o",
    "gpt-4-turbo",
    "gpt-4.1",
    "gpt-5",
    "o1",
    "o3",
    "o4",
    "claude-3",
    "claude-opus",
    "claude-sonnet",
    "claude-haiku",
    "claude-4",
    "gemini-1.5",
    "gemini-2",
    "gemini-pro-vision",
    "pixtral",
    "llama-3.2-vision",
    "llava",
    "qwen2-vl",
    "qwen2.5-vl",
];

/// True iff the model name is known to accept image input. Case-insensitive
/// substring match on the patterns in [`VISION_MODEL_PATTERNS`].
pub fn model_supports_vision(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    VISION_MODEL_PATTERNS
        .iter()
        .any(|pat| lower.contains(&pat.to_ascii_lowercase()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tempfile(ext: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir();
        let name = format!(
            "peakbot-vision-{}-{}.{ext}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(bytes).expect("write tempfile");
        path
    }

    #[test]
    fn media_type_from_extension_recognizes_common_formats() {
        assert_eq!(media_type_from_extension("png"), Some(ImageMediaType::PNG));
        assert_eq!(media_type_from_extension("jpg"), Some(ImageMediaType::JPEG));
        assert_eq!(
            media_type_from_extension("jpeg"),
            Some(ImageMediaType::JPEG)
        );
        assert_eq!(media_type_from_extension("gif"), Some(ImageMediaType::GIF));
        assert_eq!(
            media_type_from_extension("webp"),
            Some(ImageMediaType::WEBP)
        );
        assert_eq!(media_type_from_extension("txt"), None);
        assert_eq!(media_type_from_extension(""), None);
    }

    #[test]
    fn media_type_from_extension_is_case_insensitive() {
        assert_eq!(media_type_from_extension("PNG"), Some(ImageMediaType::PNG));
        assert_eq!(media_type_from_extension("JpG"), Some(ImageMediaType::JPEG));
    }

    #[test]
    fn load_image_from_path_reads_bytes_and_infers_type() {
        let path = write_tempfile("png", b"fake png bytes");
        let loaded = load_image_from_path(&path).expect("load");
        assert_eq!(
            loaded.display_name,
            path.file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(loaded.bytes, b"fake png bytes");
        assert_eq!(loaded.media_type, ImageMediaType::PNG);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_image_from_path_errors_on_missing_file() {
        let missing = PathBuf::from("/this/path/does/not/exist-12345.png");
        let err = load_image_from_path(&missing).expect_err("should error");
        assert!(matches!(err, AttachmentError::NotFound(_)));
    }

    #[test]
    fn load_image_from_path_errors_on_too_large() {
        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        let path = write_tempfile("png", &big);
        let err = load_image_from_path(&path).expect_err("should error");
        assert!(matches!(err, AttachmentError::TooLarge { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_image_from_path_errors_on_unsupported_ext() {
        let path = write_tempfile("txt", b"hello");
        let err = load_image_from_path(&path).expect_err("should error");
        assert!(matches!(err, AttachmentError::UnsupportedMediaType(_)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn media_type_from_mime_recognizes_image_mimes() {
        assert_eq!(media_type_from_mime("image/png"), Some(ImageMediaType::PNG));
        assert_eq!(
            media_type_from_mime("image/jpeg"),
            Some(ImageMediaType::JPEG)
        );
        assert_eq!(
            media_type_from_mime("IMAGE/JPG"),
            Some(ImageMediaType::JPEG)
        );
        assert_eq!(media_type_from_mime("image/gif"), Some(ImageMediaType::GIF));
        assert_eq!(
            media_type_from_mime("image/webp"),
            Some(ImageMediaType::WEBP)
        );
        assert_eq!(media_type_from_mime("image/heic"), None);
        assert_eq!(media_type_from_mime("text/plain"), None);
    }

    #[test]
    fn model_supports_vision_table_driven() {
        // true cases
        for m in [
            "gpt-4o",
            "openai/gpt-4o",
            "GPT-4O",
            "anthropic/claude-3.5-sonnet",
            "claude-sonnet-4",
            "google/gemini-2.0-flash-001",
            "google/gemini-1.5-pro",
            "pixtral-12b",
            "meta-llama/llama-3.2-vision-90b",
            "llava:7b",
            "qwen2.5-vl-72b",
        ] {
            assert!(model_supports_vision(m), "expected true for {m}");
        }
        // false cases (unknown / known-no)
        for m in [
            "gpt-3.5-turbo",
            "qwen/qwq-32b",
            "mistralai/mistral-7b",
            "",
            "random-unknown-model",
        ] {
            assert!(!model_supports_vision(m), "expected false for {m}");
        }
    }
}
