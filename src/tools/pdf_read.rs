use crate::tools::file_edit::resolve_against;
use crate::utils::strings::truncate_with_suffix;
use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;

const MAX_OUTPUT_CHARS: usize = 50_000;
const TRUNCATION_NOTICE: &str =
    "\n... [output truncated] Use start_page/end_page to read specific pages.";

#[derive(Debug, thiserror::Error)]
pub enum PdfReadError {
    #[error("{0}")]
    Validation(String),
    #[error("PDF error: {0}")]
    Pdf(#[from] pdf_oxide::Error),
}

/// Output format. A serde enum so an unknown value is rejected at parse time
/// instead of needing a runtime string check.
#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Text,
    Markdown,
}

#[derive(Deserialize)]
pub struct PdfReadArgs {
    path: String,
    /// 1-indexed, inclusive. Mirrors file_read's start_line.
    start_page: Option<usize>,
    /// 1-indexed, inclusive. Mirrors file_read's end_line.
    end_page: Option<usize>,
    #[serde(default)]
    format: Format,
}

/// PDF-read tool. `session_cwd` is the base for relative path resolution;
/// `None` (tests / no state manager) falls back to the process cwd.
#[derive(Serialize, Deserialize, Default)]
pub struct PdfReadTool {
    #[serde(skip)]
    session_cwd: Option<PathBuf>,
}

impl PdfReadTool {
    pub fn new(session_cwd: Option<PathBuf>) -> Self {
        Self { session_cwd }
    }
}

impl Tool for PdfReadTool {
    const NAME: &'static str = "pdf_read";
    type Error = PdfReadError;
    type Args = PdfReadArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "pdf_read".to_string(),
            description: "Extract text or Markdown from a PDF file. Returns the document \
                content (all pages by default, or a page range via start_page/end_page). \
                Use format='markdown' to preserve headings, lists, and tables; format='text' \
                (the default) for plain text. Output is truncated to 50,000 characters."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the PDF file to read (absolute, or relative to the working directory)"
                    },
                    "start_page": {
                        "type": "integer",
                        "description": "Optional: first page to read (1-indexed, inclusive). Defaults to page 1."
                    },
                    "end_page": {
                        "type": "integer",
                        "description": "Optional: last page to read (1-indexed, inclusive). Defaults to the last page."
                    },
                    "format": {
                        "type": "string",
                        "enum": ["text", "markdown"],
                        "description": "Output format: 'text' (default) or 'markdown'."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            target: "peakbot",
            tool_type = "pdf_read",
            path = %args.path,
            start_page = args.start_page,
            end_page = args.end_page,
            "Starting pdf_read tool execution"
        );

        let start_time = std::time::Instant::now();
        let resolved = resolve_against(self.session_cwd.as_deref(), &args.path);
        let path = resolved.as_path();

        if !path.exists() {
            return Err(PdfReadError::Validation(format!(
                "File '{}' does not exist.",
                args.path
            )));
        }
        if path.is_dir() {
            return Err(PdfReadError::Validation(format!(
                "'{}' is a directory, not a PDF file.",
                args.path
            )));
        }

        let doc = pdf_oxide::PdfDocument::open(path)?;
        let total = doc.page_count()?;

        // 1-indexed inclusive range → 0-indexed half-open [start, end).
        let start = args.start_page.map(|p| p.saturating_sub(1)).unwrap_or(0);
        let end = args.end_page.unwrap_or(total).min(total);

        if start >= total {
            return Err(PdfReadError::Validation(format!(
                "start_page {} exceeds the document's {total} pages",
                start + 1
            )));
        }
        if end <= start {
            return Err(PdfReadError::Validation(format!(
                "end_page {end} must be greater than or equal to start_page {}",
                start + 1
            )));
        }

        let opts = pdf_oxide::converters::ConversionOptions::default();
        let mut out = String::new();
        for page in start..end {
            let chunk = match args.format {
                Format::Text => doc.extract_text(page)?,
                Format::Markdown => doc.to_markdown(page, &opts)?,
            };
            out.push_str(&chunk);
            out.push('\n');
        }

        let output = if out.len() > MAX_OUTPUT_CHARS {
            truncate_with_suffix(&out, MAX_OUTPUT_CHARS, TRUNCATION_NOTICE)
        } else {
            out
        };

        tracing::info!(
            target: "peakbot",
            tool_type = "pdf_read",
            path = %args.path,
            total_pages = total,
            pages_read = end - start,
            output_len = output.len(),
            duration_ms = start_time.elapsed().as_millis(),
            "PDF read completed successfully"
        );

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Minimal valid single-page PDF whose only text is "Hello PDF".
    /// Hand-built (ASCII, byte-exact xref offsets) so the test needs no
    /// committed binary fixture and no external tool.
    const HELLO_PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n\
4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n\
5 0 obj\n<< /Length 40 >>\nstream\nBT /F1 24 Tf 72 700 Td (Hello PDF) Tj ET\nendstream\nendobj\n\
xref\n0 6\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000058 00000 n \n\
0000000115 00000 n \n\
0000000241 00000 n \n\
0000000311 00000 n \n\
trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n401\n%%EOF\n";

    fn write_pdf() -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        f.write_all(HELLO_PDF).unwrap();
        f.flush().unwrap();
        f
    }

    async fn run(args: PdfReadArgs) -> Result<String, PdfReadError> {
        PdfReadTool::default().call(args).await
    }

    #[tokio::test]
    async fn extracts_text() {
        let f = write_pdf();
        let out = run(PdfReadArgs {
            path: f.path().display().to_string(),
            start_page: None,
            end_page: None,
            format: Format::Text,
        })
        .await
        .unwrap();
        assert!(out.contains("Hello PDF"), "got: {out:?}");
    }

    #[tokio::test]
    async fn markdown_format_works() {
        let f = write_pdf();
        let out = run(PdfReadArgs {
            path: f.path().display().to_string(),
            start_page: None,
            end_page: None,
            format: Format::Markdown,
        })
        .await
        .unwrap();
        assert!(out.contains("Hello PDF"), "got: {out:?}");
    }

    #[tokio::test]
    async fn rejects_relative_path() {
        let err = run(PdfReadArgs {
            path: "relative.pdf".to_string(),
            start_page: None,
            end_page: None,
            format: Format::Text,
        })
        .await
        .unwrap_err();
        assert!(matches!(err, PdfReadError::Validation(_)));
    }

    #[tokio::test]
    async fn rejects_start_page_past_end() {
        let f = write_pdf();
        let err = run(PdfReadArgs {
            path: f.path().display().to_string(),
            start_page: Some(5),
            end_page: None,
            format: Format::Text,
        })
        .await
        .unwrap_err();
        assert!(matches!(err, PdfReadError::Validation(_)));
    }

    #[test]
    fn format_defaults_to_text() {
        let args: PdfReadArgs = serde_json::from_value(serde_json::json!({
            "path": "/x.pdf"
        }))
        .unwrap();
        assert!(matches!(args.format, Format::Text));
    }

    #[test]
    fn format_rejects_unknown_value() {
        let r: Result<PdfReadArgs, _> = serde_json::from_value(serde_json::json!({
            "path": "/x.pdf", "format": "html"
        }));
        assert!(r.is_err());
    }
}
