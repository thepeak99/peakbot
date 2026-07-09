//! Vision support — image attachments on user turns.
//!
//! Scope: only images. Audio, video, documents explicitly not covered.
//!
//! ## Entry points
//!
//! - [`parse_attachments_inline`] — strips `[img:TOKEN]` tokens from a user
//!   buffer and resolves them to [`ImageAttachment`]s. TOKEN is a `data:` URI
//!   (browser paste/drop, decoded to `Base64`), a path when it starts with
//!   `/`, `~`, or `./`, or otherwise a URL (must contain `://`).
//! - [`load_image_from_path`] — direct path → attachment, enforcing
//!   [`MAX_IMAGE_BYTES`] and media-type inference.
//! - [`model_supports_vision`] — model name → whether image input is accepted.
//!
//! ## Adapter (wire boundary)
//!
//! This module owns the UI-level types ([`ImageSource`], [`ImageAttachment`]).
//! The conversion to `rig_core::Image` lives in `state_manager.rs` alongside
//! `get_agent_history` — that is the single seam where chat-message data
//! becomes wire data.

use rig_core::completion::message::{ImageDetail, ImageMediaType};
use std::path::{Path, PathBuf};

/// Maximum file size accepted for a single image attachment. Bigger files are
/// rejected with [`AttachmentError::TooLarge`] before any bytes are read.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024; // 10 MB

/// Maximum number of `[img:…]` attachments allowed in a single submission.
pub const MAX_IMAGES_PER_TURN: usize = 8;

/// Where an image came from. Two variants, no hidden state.
///
/// - `Base64`: raw bytes held in-memory + media type (Anthropic-compatible).
/// - `Url`: pointer to an external URL (OpenAI accepts; Anthropic refuses).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum ImageSource {
    /// Raw bytes, base64-encoded in JSON.
    Base64 {
        #[serde(with = "base64_bytes")]
        bytes: Vec<u8>,
        media_type: ImageMediaType,
    },
    /// URL — OpenAI-style. Anthropic requires `Base64` instead.
    Url(String),
}

/// A single image attachment on a user message.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageAttachment {
    /// For UI display — e.g. `"cat.png"` or `"https://…/photo.jpg"`.
    /// Not trusted for path operations.
    pub display_name: String,
    pub source: ImageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
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
    #[error("too many images in one turn ({count}, max {max})")]
    TooMany { count: usize, max: usize },
    #[error(
        "invalid attachment token `{0}` (expected `/path`, `~/path`, `./path`, or `https://…`)"
    )]
    InvalidToken(String),
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
/// Mirrors [`media_type_from_extension`] for the `data:` URI grammar, where
/// the media type is spelled as a MIME rather than a file extension.
pub fn media_type_from_mime(mime: &str) -> Option<ImageMediaType> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/png" => Some(ImageMediaType::PNG),
        "image/jpeg" | "image/jpg" => Some(ImageMediaType::JPEG),
        "image/gif" => Some(ImageMediaType::GIF),
        "image/webp" => Some(ImageMediaType::WEBP),
        _ => None,
    }
}

/// Decode a `data:<mime>;base64,<payload>` URI into a `Base64` attachment.
///
/// Only base64-encoded image data URIs are accepted — the browser always
/// produces these for `FileReader.readAsDataURL` / canvas exports. A missing
/// `;base64`, missing comma, unsupported MIME, or bad base64 is an
/// [`AttachmentError::InvalidToken`]; oversize payloads are
/// [`AttachmentError::TooLarge`] (checked on the decoded bytes).
fn load_image_from_data_uri(token: &str) -> Result<ImageAttachment, AttachmentError> {
    let invalid = || AttachmentError::InvalidToken(token.to_string());

    let body = token.strip_prefix("data:").ok_or_else(invalid)?;
    let (header, payload) = body.split_once(',').ok_or_else(invalid)?;
    let mime = header.strip_suffix(";base64").ok_or_else(invalid)?;
    let media_type = media_type_from_mime(mime)
        .ok_or_else(|| AttachmentError::UnsupportedMediaType(mime.to_string()))?;

    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|_| invalid())?;

    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AttachmentError::TooLarge {
            path: PathBuf::from("<pasted image>"),
            size: bytes.len(),
            max: MAX_IMAGE_BYTES,
        });
    }

    Ok(ImageAttachment {
        display_name: format!("pasted.{}", extension_for(&media_type)),
        source: ImageSource::Base64 { bytes, media_type },
        detail: None,
    })
}

/// Canonical extension for a media type — used only to name pasted images.
fn extension_for(media_type: &ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::PNG => "png",
        ImageMediaType::JPEG => "jpg",
        ImageMediaType::GIF => "gif",
        ImageMediaType::WEBP => "webp",
        _ => "img",
    }
}

/// Load an image from disk. Infers media type from the extension, enforces
/// [`MAX_IMAGE_BYTES`], and returns a `Base64` attachment.
pub fn load_image_from_path(path: &Path) -> Result<ImageAttachment, AttachmentError> {
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

    Ok(ImageAttachment {
        display_name,
        source: ImageSource::Base64 { bytes, media_type },
        detail: None,
    })
}

/// Parse inline `[img:TOKEN]` tokens out of a user input buffer.
///
/// Token resolution:
/// - `data:<mime>;base64,<payload>` → inline bytes (`Base64`) — browser paste/drop.
/// - Starts with `/`, `~`, or `./` → filesystem path (loaded as `Base64`).
/// - Contains `://` → URL (`ImageSource::Url`).
/// - Anything else → [`AttachmentError::InvalidToken`].
///
/// Returns the stripped text (tokens removed, surrounding whitespace preserved)
/// and the resolved attachments in the order they appeared.
pub fn parse_attachments_inline(
    buffer: &str,
) -> Result<(String, Vec<ImageAttachment>), AttachmentError> {
    let marker = "[img:";
    let mut out_text = String::with_capacity(buffer.len());
    let mut attachments = Vec::new();
    let mut cursor = 0;

    while let Some(rel_start) = buffer[cursor..].find(marker) {
        let start = cursor + rel_start;
        // find the closing ']' after the marker
        let token_start = start + marker.len();
        let Some(rel_end) = buffer[token_start..].find(']') else {
            // No closing bracket — leave the rest of the buffer alone (literal).
            break;
        };
        let token_end = token_start + rel_end;
        let token = buffer[token_start..token_end].trim();

        // Copy the literal text before the marker into the output.
        out_text.push_str(&buffer[cursor..start]);

        // Resolve the token.
        if attachments.len() >= MAX_IMAGES_PER_TURN {
            return Err(AttachmentError::TooMany {
                count: attachments.len() + 1,
                max: MAX_IMAGES_PER_TURN,
            });
        }
        let attachment = resolve_token(token)?;
        attachments.push(attachment);

        // Advance past the closing ']'
        cursor = token_end + 1;
    }

    // Append any trailing text after the last matched token.
    out_text.push_str(&buffer[cursor..]);
    Ok((out_text, attachments))
}

fn resolve_token(token: &str) -> Result<ImageAttachment, AttachmentError> {
    if token.starts_with("data:") {
        load_image_from_data_uri(token)
    } else if token.starts_with('/') || token.starts_with("./") {
        load_image_from_path(Path::new(token))
    } else if let Some(rest) = token.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        let expanded = PathBuf::from(home).join(rest);
        load_image_from_path(&expanded)
    } else if token == "~" {
        let home = std::env::var("HOME").unwrap_or_default();
        load_image_from_path(Path::new(&home))
    } else if token.contains("://") {
        Ok(ImageAttachment {
            display_name: token.to_string(),
            source: ImageSource::Url(token.to_string()),
            detail: None,
        })
    } else {
        Err(AttachmentError::InvalidToken(token.to_string()))
    }
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

/// Private serde helper: base64-encode `Vec<u8>` as a JSON string.
/// Without this, `serde` would serialize bytes as an array of integers (2-4×
/// larger than base64 and essentially unreadable).
mod base64_bytes {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(d: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
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
        let att = load_image_from_path(&path).expect("load");
        assert_eq!(
            att.display_name,
            path.file_name().unwrap().to_str().unwrap()
        );
        match att.source {
            ImageSource::Base64 { bytes, media_type } => {
                assert_eq!(bytes, b"fake png bytes");
                assert_eq!(media_type, ImageMediaType::PNG);
            }
            _ => panic!("expected Base64 variant"),
        }
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
    fn parse_attachments_inline_returns_empty_for_plain_text() {
        let (text, atts) = parse_attachments_inline("hello world").expect("parse");
        assert_eq!(text, "hello world");
        assert!(atts.is_empty());
    }

    #[test]
    fn parse_attachments_inline_extracts_single_image() {
        let path = write_tempfile("png", b"x");
        let input = format!("see [img:{}]", path.display());
        let (text, atts) = parse_attachments_inline(&input).expect("parse");
        assert_eq!(text, "see ");
        assert_eq!(atts.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_attachments_inline_extracts_multiple_images_in_order() {
        let p1 = write_tempfile("png", b"1");
        let p2 = write_tempfile("jpg", b"2");
        let p3 = write_tempfile("gif", b"3");
        let input = format!(
            "a [img:{}] b [img:{}] c [img:{}] d",
            p1.display(),
            p2.display(),
            p3.display()
        );
        let (text, atts) = parse_attachments_inline(&input).expect("parse");
        assert_eq!(text, "a  b  c  d");
        assert_eq!(atts.len(), 3);
        // Order preserved by display_name
        assert!(atts[0].display_name.ends_with(".png"));
        assert!(atts[1].display_name.ends_with(".jpg"));
        assert!(atts[2].display_name.ends_with(".gif"));
        let _ = std::fs::remove_file(&p1);
        let _ = std::fs::remove_file(&p2);
        let _ = std::fs::remove_file(&p3);
    }

    #[test]
    fn parse_attachments_inline_preserves_surrounding_text() {
        let path = write_tempfile("png", b"x");
        let input = format!("before [img:{}] after", path.display());
        let (text, atts) = parse_attachments_inline(&input).expect("parse");
        assert_eq!(text, "before  after");
        assert_eq!(atts.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_attachments_inline_handles_url_tokens() {
        let (text, atts) =
            parse_attachments_inline("look [img:https://example.com/a.jpg]").expect("parse");
        assert_eq!(text, "look ");
        assert_eq!(atts.len(), 1);
        assert!(matches!(atts[0].source, ImageSource::Url(_)));
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
    fn parse_attachments_inline_decodes_data_uri() {
        use base64::Engine;
        let raw = b"the-real-png-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let input = format!("see [img:data:image/png;base64,{b64}] here");
        let (text, atts) = parse_attachments_inline(&input).expect("parse");
        assert_eq!(text, "see  here");
        assert_eq!(atts.len(), 1);
        match &atts[0].source {
            ImageSource::Base64 { bytes, media_type } => {
                assert_eq!(bytes, raw);
                assert_eq!(*media_type, ImageMediaType::PNG);
            }
            _ => panic!("expected Base64 variant"),
        }
        assert_eq!(atts[0].display_name, "pasted.png");
    }

    #[test]
    fn parse_attachments_inline_data_uri_too_large() {
        use base64::Engine;
        let big = vec![0u8; MAX_IMAGE_BYTES + 1];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&big);
        let input = format!("[img:data:image/png;base64,{b64}]");
        let err = parse_attachments_inline(&input).expect_err("should error");
        assert!(matches!(err, AttachmentError::TooLarge { .. }));
    }

    #[test]
    fn parse_attachments_inline_data_uri_unsupported_mime() {
        // Well-formed base64, but a MIME we don't accept.
        let input = "[img:data:image/heic;base64,AAAA]";
        let err = parse_attachments_inline(input).expect_err("should error");
        assert!(matches!(err, AttachmentError::UnsupportedMediaType(_)));
    }

    #[test]
    fn parse_attachments_inline_data_uri_malformed() {
        // No comma, no `;base64`, and bad base64 all collapse to InvalidToken.
        for input in [
            "[img:data:image/png;base64]",       // no comma
            "[img:data:image/png,AAAA]",         // not base64-tagged
            "[img:data:image/png;base64,@@@@@]", // bad base64 payload
        ] {
            let err = parse_attachments_inline(input).expect_err("should error");
            assert!(
                matches!(err, AttachmentError::InvalidToken(_)),
                "expected InvalidToken for {input}"
            );
        }
    }

    #[test]
    fn parse_attachments_inline_errors_on_missing_file() {
        let err = parse_attachments_inline("x [img:/does/not/exist-xyz.png] y")
            .expect_err("should error");
        assert!(matches!(err, AttachmentError::NotFound(_)));
    }

    #[test]
    fn parse_attachments_inline_errors_on_too_many() {
        let paths: Vec<PathBuf> = (0..=MAX_IMAGES_PER_TURN)
            .map(|_| write_tempfile("png", b"x"))
            .collect();
        let input = paths
            .iter()
            .map(|p| format!("[img:{}]", p.display()))
            .collect::<Vec<_>>()
            .join(" ");
        let err = parse_attachments_inline(&input).expect_err("should error");
        assert!(matches!(err, AttachmentError::TooMany { .. }));
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn parse_attachments_inline_ignores_unmatched_brackets() {
        // "[not an img: x]" does not match the marker "[img:" — stays literal.
        let (text, atts) = parse_attachments_inline("[not an img: x] hi").expect("parse");
        assert_eq!(text, "[not an img: x] hi");
        assert!(atts.is_empty());
    }

    #[test]
    fn parse_attachments_inline_expands_tilde() {
        // Use HOME env override (don't mutate global state — use a scoped var).
        let home = std::env::temp_dir();
        let tmp = home.join(format!(
            "peakbot-tilde-{}-{}.png",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, b"z").unwrap();
        let name = tmp.file_name().unwrap().to_str().unwrap().to_string();

        // Temporarily override HOME — done in a way that restores on drop.
        struct HomeGuard(Option<String>);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                // Environment mutation at drop is acceptable for tests because
                // they run serially here (the test itself sets HOME).
                match self.0.take() {
                    Some(v) => unsafe { std::env::set_var("HOME", v) },
                    None => unsafe { std::env::remove_var("HOME") },
                }
            }
        }
        let _guard = HomeGuard(std::env::var("HOME").ok());
        unsafe { std::env::set_var("HOME", home.display().to_string()) };

        let (text, atts) = parse_attachments_inline(&format!("[img:~/{name}]")).expect("parse");
        assert_eq!(text, "");
        assert_eq!(atts.len(), 1);
        let _ = std::fs::remove_file(&tmp);
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

    #[test]
    fn base64_bytes_serde_roundtrip() {
        // Pin the custom base64 encoding: bytes → JSON string → bytes.
        let a = ImageAttachment {
            display_name: "x.png".into(),
            source: ImageSource::Base64 {
                bytes: vec![0, 1, 2, 3, 4, 255],
                media_type: ImageMediaType::PNG,
            },
            detail: None,
        };
        let json = serde_json::to_string(&a).expect("ser");
        assert!(
            json.contains(r#""bytes":"AAECAwT/""#),
            "expected base64 encoding, got: {json}"
        );
        let b: ImageAttachment = serde_json::from_str(&json).expect("de");
        assert_eq!(a, b);
    }

    #[test]
    fn url_attachment_serde_roundtrip() {
        let a = ImageAttachment {
            display_name: "https://example.com/a.jpg".into(),
            source: ImageSource::Url("https://example.com/a.jpg".into()),
            detail: None,
        };
        let json = serde_json::to_string(&a).expect("ser");
        let b: ImageAttachment = serde_json::from_str(&json).expect("de");
        assert_eq!(a, b);
    }
}
