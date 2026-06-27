//! Plain-text extraction from documents, dispatched by file extension.
//!
//! The 80% case is "just read the file" (txt, md, source code). PDF and DOCX
//! get dedicated pure-Rust extractors; HTML reuses `spider_transformations`
//! (already a dep via `fetch_page`). Unsupported extensions are reported by
//! the caller, not hard-failed here.

use std::path::Path;

use spider_transformations::transformation::content::transform_text;
use thiserror::Error;

/// Extensions we know how to extract text from. Lower-cased, no leading dot.
/// `doc_index` uses this to filter directory walks and to decide per-file
/// whether to parse or skip. Plain-text formats are read verbatim; the
/// structured formats route to dedicated extractors in [`extract_text`].
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    // Plain text / markup / source — read verbatim.
    "txt", "md", "markdown", "rst", "org", "csv", "tsv", "log", "json", "yaml", "yml", "toml", "rs",
    "py", "js", "ts", "tsx", "jsx", "go", "java", "c", "h", "cpp", "hpp", "cc", "rb", "php", "sh",
    "bash", "zsh", "sql", "xml", "tex", // Structured — dedicated extractors.
    "html", "htm", "pdf", "docx",
];

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unsupported file type: {0}")]
    Unsupported(String),
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to extract PDF text from {path}: {source}")]
    Pdf {
        path: String,
        #[source]
        source: pdf_oxide::Error,
    },
    #[error("failed to extract DOCX text from {path}: {source}")]
    Docx {
        path: String,
        #[source]
        source: docx_lite::DocxError,
    },
}

/// Lower-cased extension of `path`, or empty string if none.
pub fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// True if `path`'s extension is one we can extract text from.
pub fn is_supported(path: &Path) -> bool {
    SUPPORTED_EXTENSIONS.contains(&extension_of(path).as_str())
}

/// Extract plain text from `path`, dispatching on its extension.
///
/// Returns [`ParseError::Unsupported`] for extensions not in
/// [`SUPPORTED_EXTENSIONS`] — callers should pre-filter with [`is_supported`]
/// and treat this as a skip, never a fatal error for a whole directory.
pub fn extract_text(path: &Path) -> Result<String, ParseError> {
    let ext = extension_of(path);
    let path_str = path.display().to_string();
    match ext.as_str() {
        "html" | "htm" => {
            let html = read_to_string(path, &path_str)?;
            Ok(transform_text(&html))
        }
        "pdf" => {
            let doc = pdf_oxide::PdfDocument::open(path).map_err(|source| ParseError::Pdf {
                path: path_str.clone(),
                source,
            })?;
            let pages = doc.page_count().map_err(|source| ParseError::Pdf {
                path: path_str.clone(),
                source,
            })?;
            let mut text = String::new();
            for page in 0..pages {
                let page_text = doc.extract_text(page).map_err(|source| ParseError::Pdf {
                    path: path_str.clone(),
                    source,
                })?;
                text.push_str(&page_text);
                text.push('\n');
            }
            Ok(text)
        }
        "docx" => docx_lite::extract_text(path).map_err(|source| ParseError::Docx {
            path: path_str,
            source,
        }),
        other if SUPPORTED_EXTENSIONS.contains(&other) => read_to_string(path, &path_str),
        other => Err(ParseError::Unsupported(other.to_string())),
    }
}

fn read_to_string(path: &Path, path_str: &str) -> Result<String, ParseError> {
    std::fs::read_to_string(path).map_err(|source| ParseError::Io {
        path: path_str.to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extension_is_lowercased() {
        assert_eq!(extension_of(Path::new("/a/B.MD")), "md");
        assert_eq!(extension_of(Path::new("/a/noext")), "");
    }

    #[test]
    fn supported_detection() {
        assert!(is_supported(Path::new("notes.txt")));
        assert!(is_supported(Path::new("README.MD")));
        assert!(is_supported(Path::new("doc.pdf")));
        assert!(!is_supported(Path::new("photo.png")));
        assert!(!is_supported(Path::new("archive.zip")));
    }

    #[test]
    fn reads_plain_text_verbatim() {
        let mut f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        write!(f, "hello vectors").unwrap();
        let text = extract_text(f.path()).unwrap();
        assert_eq!(text, "hello vectors");
    }

    #[test]
    fn html_is_stripped_to_text() {
        let mut f = tempfile::Builder::new().suffix(".html").tempfile().unwrap();
        write!(
            f,
            "<html><body><h1>Title</h1><p>Body text.</p></body></html>"
        )
        .unwrap();
        let text = extract_text(f.path()).unwrap();
        assert!(text.contains("Title"));
        assert!(text.contains("Body text."));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn unsupported_extension_errors() {
        let err = extract_text(Path::new("/tmp/photo.png")).unwrap_err();
        assert!(matches!(err, ParseError::Unsupported(ext) if ext == "png"));
    }

    #[test]
    fn extracts_pdf_text() {
        // Minimal single-page PDF whose only text is "Hello PDF".
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
        let mut f = tempfile::Builder::new().suffix(".pdf").tempfile().unwrap();
        f.write_all(HELLO_PDF).unwrap();
        f.flush().unwrap();
        let text = extract_text(f.path()).unwrap();
        assert!(text.contains("Hello PDF"), "got: {text:?}");
    }
}
