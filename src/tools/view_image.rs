//! `view_image` — load a local image file into the model's vision context.
//!
//! This is the tool counterpart to the user-facing `[img:…]` attachment
//! syntax: it lets the *agent* pull an image it discovers during a task
//! (a screenshot, a diagram, a UI capture) into its own sight.
//!
//! The image is returned as the structured JSON shape rig recognises in
//! [`rig_core::completion::message::ToolResultContent::from_tool_output`]:
//! `{"type":"image","data":"<base64>","mimeType":"image/png"}`. rig converts
//! that into an image tool-result block on the wire.
//!
//! **Provider note:** only the Anthropic Messages API actually delivers an
//! image tool-result to the model. Registration is therefore gated to the
//! Anthropic provider in `add_builtin_tools` — see `register_view_image`.

use crate::vision::{AttachmentError, ImageSource, load_image_from_path};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rig_core::completion::ToolDefinition;
use rig_core::completion::message::MimeType;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ViewImageError {
    #[error(transparent)]
    Load(#[from] AttachmentError),
}

#[derive(Deserialize)]
pub struct ViewImageArgs {
    /// Path to the image file. Supports `/abs`, `./rel`, and `~/home` forms.
    path: String,
}

/// rig serializes a tool's `Output` to JSON via serde. These field names and
/// the `type: "image"` tag are the exact contract `from_tool_output` parses.
#[derive(Debug, Serialize)]
pub struct ViewImageOutput {
    #[serde(rename = "type")]
    kind: &'static str,
    data: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

#[derive(Serialize, Deserialize)]
pub struct ViewImageTool;

impl Tool for ViewImageTool {
    const NAME: &'static str = "view_image";
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
                `~/path.png`). The image is fed directly into your vision context."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Filesystem path to the image (e.g. /tmp/shot.png, ./diagram.jpg, ~/pics/ui.webp)."
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

        let (data, mime_type) = match attachment.source {
            ImageSource::Base64 { bytes, media_type } => (
                STANDARD.encode(bytes),
                media_type.to_mime_type().to_string(),
            ),
            // load_image_from_path only ever returns Base64; Url is unreachable
            // here but handled to keep the match total without a panic.
            ImageSource::Url(url) => (url, "image/png".to_string()),
        };

        tracing::info!(
            target: "peakbot",
            tool_type = "view_image",
            path = args.path,
            "view_image tool executed"
        );

        Ok(ViewImageOutput {
            kind: "image",
            data,
            mime_type,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::ToolResultContent;
    use std::io::Write;
    use std::path::PathBuf;

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
            })
            .await
            .expect("should load");
        assert_eq!(out.kind, "image");
        assert_eq!(out.mime_type, "image/png");
        assert_eq!(STANDARD.decode(out.data).unwrap(), b"fake png bytes");
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

    /// Regression pin for the rig 0.36 bug (fixed in 0.38): the Anthropic
    /// provider serialized an image tool-result as a newtype enum variant,
    /// emitting a *duplicate* `type` key (`{"type":"image","type":"base64",…}`).
    /// Parsers take the last key → `base64` → Anthropic rejects the request.
    /// The correct shape nests the source: `{"type":"image","source":{…}}`.
    /// We convert a generic image tool-result through rig's own Anthropic
    /// `TryFrom` and assert the wire shape so a future regression fails loudly.
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
            })
            .await
            .expect_err("should error");
        assert!(matches!(
            err,
            ViewImageError::Load(AttachmentError::UnsupportedMediaType(_))
        ));
        let _ = std::fs::remove_file(&path);
    }
}
