//! `view_image` — load a local image file into the model's vision context.
//! Anthropic-only: no other provider's tool-result channel carries images, so
//! registration is gated to that provider.

use crate::image_cache::{self, ImageRef};
use crate::vision::{AttachmentError, ImageSource, load_image_from_path};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rig_core::completion::ToolDefinition;
use rig_core::completion::message::MimeType;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

/// Also `Tool::NAME` below — kept as one source of truth so other modules
/// (e.g. T4) can reference `view_image::NAME` without risking drift.
pub const NAME: &str = "view_image";

#[derive(Debug, thiserror::Error)]
pub enum ViewImageError {
    #[error(transparent)]
    Load(#[from] AttachmentError),
}

#[derive(Deserialize)]
pub struct ViewImageArgs {
    /// Path to the image file. Supports `/abs`, `./rel`, and `~/home` forms.
    path: String,
    /// Models routinely omit optional params, so the serde default and the
    /// schema default must both be `true` — see `auto_resize_defaults_to_true_in_serde_and_in_the_schema`.
    #[serde(default = "default_true")]
    auto_resize: bool,
}

/// `#[serde(default)]` on a `bool` yields `false`; this named fn is what
/// actually makes the default `true`.
const fn default_true() -> bool {
    true
}

/// rig parses two shapes (`ToolResultContent::from_tool_output`). `Plain`
/// yields exactly one Image block — today's contract, byte-identical.
/// `Resized` yields `[Text, Image]`, the only channel through which a
/// notice actually reaches the model (an extra field bolted onto `Plain`
/// is silently dropped by rig, never seen by the model).
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ViewImageOutput {
    Plain(PlainImage),
    Resized(ResizedImage),
}

/// These field names and the `type: "image"` tag are the exact contract
/// `from_tool_output` parses.
#[derive(Debug, Serialize)]
pub struct PlainImage {
    #[serde(rename = "type")]
    kind: &'static str,
    data: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_ref: Option<ImageRef>,
}

/// `response` becomes rig's Text block; `parts[0]` becomes the Image block.
#[derive(Debug, Serialize)]
pub struct ResizedImage {
    response: String,
    parts: [PlainImage; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    image_ref: Option<ImageRef>,
}

impl ViewImageOutput {
    fn kind(&self) -> &'static str {
        match self {
            Self::Plain(p) => p.kind,
            Self::Resized(r) => r.parts[0].kind,
        }
    }

    fn data(&self) -> &str {
        match self {
            Self::Plain(p) => &p.data,
            Self::Resized(r) => &r.parts[0].data,
        }
    }

    fn mime_type(&self) -> &str {
        match self {
            Self::Plain(p) => &p.mime_type,
            Self::Resized(r) => &r.parts[0].mime_type,
        }
    }

    fn image_ref(&self) -> Option<ImageRef> {
        match self {
            Self::Plain(p) => p.image_ref.clone(),
            Self::Resized(r) => r.image_ref.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct ViewImageTool;

impl Tool for ViewImageTool {
    const NAME: &'static str = NAME;
    type Error = ViewImageError;
    type Args = ViewImageArgs;
    type Output = ViewImageOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Load a local image file so you can SEE it. Use this when you need to \
                visually inspect an image on disk — a screenshot, diagram, photo, chart, or UI \
                capture — rather than reading its raw bytes as text. Supported formats: PNG, JPEG, \
                GIF, WEBP (max 10 MB). Provide a filesystem path (`/abs/path.png`, `./rel.png`, or \
                `~/path.png`). The image is fed directly into your vision context. It is loaded \
                for the current turn only; call this tool again later if you need to see it again. \
                Oversized images are downscaled to fit automatically; pass auto_resize:false to \
                send the original unmodified."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Filesystem path to the image (e.g. /tmp/shot.png, ./diagram.jpg, ~/pics/ui.webp)."
                    },
                    "auto_resize": {
                        "type": "boolean",
                        "default": true,
                        "description": "Downscale the image if it would exceed this endpoint's image size limit (default true). Set false to send the file untouched; an oversized file then fails with an error instead."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Reuse the user-attachment loader: path resolution, the 10 MB cap,
        // media-type inference, and the format allowlist all live there.
        let attachment = load_image_from_path(Path::new(&args.path))?;

        let (data, mime_type, image_ref) = match attachment.source {
            ImageSource::Base64 { bytes, media_type } => {
                let mime_type = media_type.to_mime_type().to_string();
                let display_name = Path::new(&args.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| args.path.clone());
                // A spill failure (unsupported type, I/O error, ...) must
                // never fail the call — the model still gets the image.
                let image_ref = image_cache::spill(&bytes, media_type, &display_name);
                (STANDARD.encode(bytes), mime_type, image_ref)
            }
            // load_image_from_path only ever returns Base64.
            ImageSource::Url(_) => unreachable!("this should never happen"),
        };

        tracing::info!(
            target: "peakbot",
            tool_type = "view_image",
            path = args.path,
            "view_image tool executed"
        );

        Ok(ViewImageOutput::Plain(PlainImage {
            kind: "image",
            data,
            mime_type,
            image_ref,
        }))
    }
}

/// Mirrors only the fields we need from a serialized `ViewImageOutput`.
/// `data` is typed `IgnoredAny` so serde skips its (potentially multi-MB)
/// base64 bytes without allocating a `String` for them.
#[derive(Deserialize)]
struct PartialViewImageOutput {
    #[serde(default, rename = "data")]
    _data: serde::de::IgnoredAny,
    #[serde(default, rename = "parts")]
    _parts: serde::de::IgnoredAny,
    #[serde(default)]
    image_ref: Option<ImageRef>,
}

/// Extracts the `image_ref` from a tool's serialized JSON output, or `None`
/// if it isn't a `view_image` output (or carries no ref). Never panics.
/// Also covers `PartialViewImageOutput`, which is only reachable through
/// this fn. Called by `ui::app_state` (T4) to populate `ChatMessage.images`.
pub fn image_ref_from_output(json: &str) -> Option<ImageRef> {
    serde_json::from_str::<PartialViewImageOutput>(json)
        .ok()?
        .image_ref
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::ToolResultContent;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    fn write_png(bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "peakbot-view-image-{}-{}.png",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(bytes).expect("write tempfile");
        path
    }

    #[tokio::test]
    async fn loads_image_as_image_json() {
        let path = write_png(b"fake png bytes");
        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");
        assert_eq!(out.kind(), "image");
        assert_eq!(out.mime_type(), "image/png");
        assert_eq!(STANDARD.decode(out.data()).unwrap(), b"fake png bytes");
        let _ = std::fs::remove_file(&path);
    }

    /// Pin our output to rig's contract: the serialized JSON must round-trip
    /// through `from_tool_output` into an actual image tool-result. If a rig
    /// upgrade changes the convention, this fails loudly.
    #[tokio::test]
    async fn output_roundtrips_into_rig_image_tool_result() {
        let path = write_png(b"x");
        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");
        let json = serde_json::to_string(&out).expect("serialize");
        let content = ToolResultContent::from_tool_output(json);
        assert_eq!(content.len(), 1);
        assert!(
            matches!(content.first(), ToolResultContent::Image(_)),
            "expected rig to parse our output as an Image tool result"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Pin the wire shape: rig 0.36 flattened the source as a duplicate
    /// `type` key; rig 0.38 nests it under `source`. A regression fails here.
    #[test]
    fn anthropic_image_tool_result_wire_shape_has_nested_source() {
        use rig_core::completion::message::{
            ImageMediaType, Message as GenericMessage, ToolResult, ToolResultContent, UserContent,
        };
        use rig_core::one_or_many::OneOrMany;
        use rig_core::providers::anthropic::completion::Message as AnthropicMessage;

        let generic = GenericMessage::User {
            content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                id: "call_1".into(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::image_base64(
                    "AAAA",
                    Some(ImageMediaType::PNG),
                    None,
                )),
            })),
        };

        let wire = AnthropicMessage::try_from(generic).expect("convert to anthropic message");
        let value = serde_json::to_value(&wire).expect("serialize anthropic message");

        // content[0] is the tool_result block; its content[0] is the image block.
        let image_block = &value["content"][0]["content"][0];
        assert_eq!(
            image_block["type"], "image",
            "image tool-result block must be tagged `image`, got: {image_block}"
        );
        assert_eq!(
            image_block["source"]["type"], "base64",
            "image source must nest under `source`, not flatten a duplicate `type` key: {image_block}"
        );
        assert_eq!(image_block["source"]["data"], "AAAA");
    }

    #[tokio::test]
    async fn missing_file_errors() {
        let err = ViewImageTool
            .call(ViewImageArgs {
                path: "/does/not/exist-view-image-xyz.png".to_string(),
                auto_resize: true,
            })
            .await
            .expect_err("should error");
        assert!(matches!(
            err,
            ViewImageError::Load(AttachmentError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn unsupported_extension_errors() {
        let path = std::env::temp_dir().join(format!(
            "peakbot-view-image-{}-{}.txt",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"hi").unwrap();
        let err = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect_err("should error");
        assert!(matches!(
            err,
            ViewImageError::Load(AttachmentError::UnsupportedMediaType(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    // ==================================================================
    // T3: `NAME` const, image_cache spill integration, `image_ref_from_output`.
    //
    // Isolation: `image_cache::spill` writes into the real, process-wide
    // `<temp>/peakbot/images/` — the injectable `spill_in`/`path_for_in`
    // seam lives inside `image_cache` and is not visible from here. Every
    // image payload below is therefore made content-unique (process id +
    // a monotonically increasing counter + a fresh UUID baked into the
    // bytes) so no two tests — in this file, under a parallel test run, or
    // against a developer's real cache — can ever collide on the same
    // content address. Spilled files and temp source files/dirs are
    // removed best-effort at the end of each test.
    // ==================================================================

    use crate::image_cache::{self, ImageRef};
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Bytes that can never collide with any other spill in the system:
    /// pid + a monotonic counter + a fresh UUID, padded (with a
    /// non-repeating tail) up to `min_len`.
    fn unique_bytes(tag: &str, min_len: usize) -> Vec<u8> {
        let n = UNIQUE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut bytes = format!(
            "peakbot-view-image-test-{tag}-{}-{}-{n}\n",
            std::process::id(),
            uuid::Uuid::new_v4()
        )
        .into_bytes();
        let mut filler: u8 = 0;
        let target = min_len.max(bytes.len());
        bytes.resize_with(target, || {
            filler = filler.wrapping_add(1);
            filler
        });
        bytes
    }

    /// Write `bytes` to a fresh, uniquely-named temp file named
    /// `<file_stem>.<ext>`. When `nested` is true the file is placed under
    /// a freshly created, uniquely-named subdirectory of the system temp
    /// dir, so tests can prove `display_name` is a *basename* and not the
    /// full path (the subdir name never matches the file's basename).
    fn write_temp_image(bytes: &[u8], ext: &str, nested: bool, file_stem: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        if nested {
            dir = dir.join(format!(
                "peakbot-view-image-nested-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).expect("mkdir nested temp dir");
        }
        let path = dir.join(format!("{file_stem}.{ext}"));
        let mut f = std::fs::File::create(&path).expect("create tempfile");
        f.write_all(bytes).expect("write tempfile");
        path
    }

    /// Best-effort cleanup of a temp source file (and, if `write_temp_image`
    /// created a nested dir for it, that dir too).
    fn cleanup_source(path: &Path) {
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent()
            && parent
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("peakbot-view-image-nested-"))
        {
            let _ = std::fs::remove_dir(parent);
        }
    }

    /// Best-effort cleanup of whatever `image_cache::spill` wrote for `id`.
    fn cleanup_spill(id: &str) {
        if let Some(p) = image_cache::path_for(id) {
            let _ = std::fs::remove_file(p);
        }
    }

    // -- 1. NAME const, no drift from the trait impl ---------------------

    #[test]
    fn name_const_equals_view_image() {
        assert_eq!(super::NAME, "view_image");
    }

    #[test]
    fn tool_name_trait_matches_name_const() {
        assert_eq!(
            <ViewImageTool as Tool>::NAME,
            super::NAME,
            "Tool::NAME must not drift from the module-level NAME const that other \
             modules (e.g. T4) will reference"
        );
    }

    // -- 2. spilled bytes are byte-identical to the source file ----------

    #[tokio::test]
    async fn call_spills_bytes_byte_identical_to_source_file() {
        let bytes = unique_bytes("byte-identical", 4096);
        let path = write_temp_image(
            &bytes,
            "png",
            false,
            &format!("byte-identical-{}", uuid::Uuid::new_v4()),
        );

        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");

        let image_ref = out
            .image_ref()
            .expect("expected image_ref to be Some after a successful spill");
        let spilled_path = image_cache::path_for(&image_ref.id)
            .expect("image_cache::path_for should resolve the id the tool just spilled");
        let on_disk = std::fs::read(&spilled_path).expect("read spilled file");
        assert_eq!(
            on_disk, bytes,
            "spilled bytes must be byte-identical to the source file"
        );

        cleanup_spill(&image_ref.id);
        cleanup_source(&path);
    }

    // -- 3. adding image_ref must not change the existing data/type/mimeType

    #[tokio::test]
    async fn call_output_still_carries_full_base64_data_type_and_mime_type() {
        let bytes = unique_bytes("full-payload-unchanged", 8192);
        let path = write_temp_image(
            &bytes,
            "jpg",
            false,
            &format!("full-payload-{}", uuid::Uuid::new_v4()),
        );

        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");

        assert_eq!(out.kind(), "image");
        assert_eq!(out.mime_type(), "image/jpeg");
        assert_eq!(
            STANDARD.decode(out.data()).expect("base64 decode"),
            bytes,
            "data must still carry the FULL image, unabridged, after adding image_ref"
        );

        if let Some(r) = out.image_ref() {
            cleanup_spill(&r.id);
        }
        cleanup_source(&path);
    }

    // -- 4. image_ref: Some, display_name is a basename, id resolves -----

    #[tokio::test]
    async fn call_output_image_ref_has_basename_display_name_and_resolvable_id() {
        let bytes = unique_bytes("basename-check", 2048);
        // nested=true: the source path has extra directory components, so a
        // display_name equal to the *full path* (a bug) is distinguishable
        // from one equal to just the basename (correct).
        let path = write_temp_image(&bytes, "png", true, "shot");

        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");

        let image_ref = out.image_ref().expect("expected image_ref to be Some");
        assert_eq!(
            image_ref.display_name, "shot.png",
            "display_name must be the source file's basename, not the full path"
        );
        assert_ne!(
            image_ref.display_name,
            path.to_string_lossy(),
            "display_name must not be the full path"
        );
        assert!(
            image_cache::path_for(&image_ref.id).is_some(),
            "image_cache::path_for must resolve the id to an existing spilled file"
        );

        cleanup_spill(&image_ref.id);
        cleanup_source(&path);
    }

    #[tokio::test]
    async fn call_output_image_ref_display_name_is_basename_for_unicode_filename() {
        let bytes = unique_bytes("unicode-basename", 2048);
        let stem = "\u{30b9}\u{30af}\u{30ea}\u{30fc}\u{30f3}\u{30b7}\u{30e7}\u{30c3}\u{30c8} 001";
        let path = write_temp_image(&bytes, "png", true, stem);
        let expected_basename = path.file_name().unwrap().to_string_lossy().into_owned();

        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");

        let image_ref = out.image_ref().expect("expected image_ref to be Some");
        assert_eq!(
            image_ref.display_name, expected_basename,
            "unicode basenames must pass through to display_name unmodified"
        );

        cleanup_spill(&image_ref.id);
        cleanup_source(&path);
    }

    // -- 5. calling the tool twice on the same file dedupes end-to-end ---

    #[tokio::test]
    async fn calling_tool_twice_on_same_file_dedupes_to_same_image_ref_id() {
        let bytes = unique_bytes("dedupe-through-tool", 4096);
        let path = write_temp_image(
            &bytes,
            "gif",
            false,
            &format!("dedupe-{}", uuid::Uuid::new_v4()),
        );

        let first = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("first call should load");
        let second = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("second call should load");

        let first_ref = first
            .image_ref()
            .expect("first call's image_ref should be Some");
        let second_ref = second
            .image_ref()
            .expect("second call's image_ref should be Some");
        assert_eq!(
            first_ref.id, second_ref.id,
            "calling the tool twice on identical bytes must dedupe to the same content address"
        );

        cleanup_spill(&first_ref.id);
        cleanup_source(&path);
    }

    // -- 6. image_ref_from_output on a real, large (>=1MB) output ---------

    #[tokio::test]
    async fn image_ref_from_output_extracts_ref_from_large_serialized_output() {
        // 2 MB raw -> well over 1 MB of output once base64-encoded, plus
        // the JSON envelope.
        let bytes = unique_bytes("large-output", 2 * 1024 * 1024);
        let path = write_temp_image(
            &bytes,
            "webp",
            false,
            &format!("large-{}", uuid::Uuid::new_v4()),
        );

        let out = ViewImageTool
            .call(ViewImageArgs {
                path: path.to_string_lossy().into_owned(),
                auto_resize: true,
            })
            .await
            .expect("should load");
        let expected_ref = out.image_ref().expect("expected image_ref to be Some");

        let json = serde_json::to_string(&out).expect("serialize output");
        assert!(
            json.len() >= 1_000_000,
            "test setup sanity check: serialized output should be at least 1 MB, was {} bytes",
            json.len()
        );

        let extracted = super::image_ref_from_output(&json).expect(
            "image_ref_from_output should extract the ref from a real, large serialized output",
        );
        assert_eq!(extracted, expected_ref);

        cleanup_spill(&expected_ref.id);
        cleanup_source(&path);
    }

    // -- 7. image_ref_from_output: None for everything that isn't a
    //       view_image output carrying an image_ref -----------------------

    #[test]
    fn image_ref_from_output_returns_none_for_bash_style_output() {
        // `bash`'s Tool::Output is a plain String; rig serializes that as a
        // bare JSON string scalar, not an object with an `image_ref` key.
        let bash_output = "total 4\ndrwxr-xr-x  2 user user 4096 Jan  1 00:00 .\n";
        let json = serde_json::to_string(bash_output).expect("serialize");
        assert!(super::image_ref_from_output(&json).is_none());
    }

    #[test]
    fn image_ref_from_output_returns_none_for_non_json_text() {
        assert!(super::image_ref_from_output("not json at all {{{").is_none());
        assert!(super::image_ref_from_output("").is_none());
        assert!(super::image_ref_from_output("   ").is_none());
    }

    #[test]
    fn image_ref_from_output_returns_none_for_unrelated_json_object() {
        let json = r#"{"foo":"bar","baz":42}"#;
        assert!(super::image_ref_from_output(json).is_none());
    }

    #[test]
    fn image_ref_from_output_returns_none_when_image_ref_field_absent() {
        // A well-formed view_image output from BEFORE image_ref existed, or
        // one produced on a spill failure with the field omitted.
        let json = r#"{"type":"image","data":"AAAA","mimeType":"image/png"}"#;
        assert!(super::image_ref_from_output(json).is_none());
    }

    // -- 8. image_ref is OMITTED (not null) when None ---------------------

    #[test]
    fn image_ref_omitted_from_json_when_none_not_serialized_as_null() {
        let out = ViewImageOutput::Plain(PlainImage {
            kind: "image",
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
            image_ref: None,
        });
        let json = serde_json::to_string(&out).expect("serialize");
        assert!(
            !json.contains("image_ref"),
            "image_ref must be omitted entirely when None, not serialized as \
             `\"image_ref\":null`; got: {json}"
        );
    }

    #[test]
    fn image_ref_present_in_json_with_id_and_display_name_when_some() {
        let out = ViewImageOutput::Plain(PlainImage {
            kind: "image",
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
            image_ref: Some(ImageRef {
                id: format!("{}.png", "ab".repeat(32)),
                display_name: "shot.png".to_string(),
            }),
        });
        let value: serde_json::Value = serde_json::to_value(&out).expect("to_value");
        assert_eq!(
            value["image_ref"]["display_name"].as_str(),
            Some("shot.png")
        );
        assert!(value["image_ref"]["id"].as_str().unwrap().ends_with(".png"));
    }

    // -- 9. definition() tells the model re-loading is the recovery path --

    #[tokio::test]
    async fn definition_description_mentions_reloading_for_current_turn() {
        let def = ViewImageTool.definition(String::new()).await;
        assert!(
            def.description.contains("current turn"),
            "description should tell the model the image is loaded for the current turn \
             and that it can call the tool again later to re-load it; got: {}",
            def.description
        );
    }

    // -- 10. a spill failure must not fail the tool call -------------------
    //
    // We cannot honestly force `image_cache::spill` to fail from outside
    // this module — the injectable `spill_in` seam is private to
    // `image_cache`, and there is no way from here to make the real,
    // process-wide cache dir unwritable without racing every other test in
    // this binary. So this pins the equivalent contract at the type level:
    // a `ViewImageOutput` shaped exactly as `call` would build it after a
    // spill failure (`image_ref: None`) must still be a well-formed, fully
    // usable tool output. The true I/O-failure path through `call` itself
    // is NOT directly covered by this suite — flagged in the report.

    #[test]
    fn output_with_none_image_ref_still_serializes_and_round_trips_and_extracts_none() {
        let out = ViewImageOutput::Plain(PlainImage {
            kind: "image",
            data: STANDARD.encode(b"still a real image payload"),
            mime_type: "image/png".to_string(),
            image_ref: None,
        });
        let json = serde_json::to_string(&out).expect("serialize");

        let content = ToolResultContent::from_tool_output(json.clone());
        assert_eq!(content.len(), 1);
        assert!(
            matches!(content.first(), ToolResultContent::Image(_)),
            "a spill-failure output (image_ref: None) must still parse as a valid image tool result"
        );
        assert!(
            super::image_ref_from_output(&json).is_none(),
            "image_ref_from_output must return None for an output with no image_ref, not error"
        );
    }
}
