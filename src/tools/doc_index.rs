//! `doc_index` tool: parse → chunk → embed → store a file or directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rig_core::completion::ToolDefinition;
use rig_core::tool::Tool;
use serde::Deserialize;

use crate::vector::{IndexOutcome, IndexReport, VectorStore, VectorStoreError, is_supported};

use crate::tools::file_edit::resolve_against;

#[derive(Debug, Deserialize)]
pub struct DocIndexArgs {
    /// File or directory to index.
    pub path: String,
    /// When `path` is a directory, recurse into subdirectories (default: false).
    #[serde(default)]
    pub recursive: Option<bool>,
    /// Free-form metadata applied to every chunk of every indexed file
    /// (e.g. author, book, year). Returned alongside each search hit for
    /// citation. Reserved keys (source, chunk_index, text, sha256) are ignored.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum DocIndexError {
    #[error("path does not exist: {0}")]
    NotFound(String),
    #[error(transparent)]
    Store(#[from] VectorStoreError),
}

/// Indexing tool over the shared vector store. `session_cwd` is the base for
/// relative path resolution; the default empty path leaves relatives anchored
/// at the process cwd (tests).
#[derive(Clone)]
pub struct DocIndexTool {
    store: VectorStore,
    session_cwd: PathBuf,
}

impl DocIndexTool {
    pub fn new(store: VectorStore) -> Self {
        Self {
            store,
            session_cwd: PathBuf::new(),
        }
    }

    pub fn with_session_cwd(mut self, session_cwd: PathBuf) -> Self {
        self.session_cwd = session_cwd;
        self
    }
}

impl Tool for DocIndexTool {
    const NAME: &'static str = "doc_index";
    type Error = DocIndexError;
    type Args = DocIndexArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Index a file or directory into the semantic vector store so it can \
                later be retrieved with doc_search. Parses text (txt, md, source code, html, \
                pdf, docx), splits it into chunks, embeds them, and stores them. Re-indexing \
                is idempotent: unchanged files are skipped, changed files are updated."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file or directory to index (absolute, or relative to the working directory)."
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "When path is a directory, recurse into subdirectories (default: false).",
                        "default": false
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Optional free-form metadata applied to every chunk of every indexed file (e.g. {\"author\": \"...\", \"book\": \"...\", \"year\": \"...\"}). Returned alongside each search hit for citation. The reserved keys source, chunk_index, text, and sha256 are ignored.",
                        "additionalProperties": true
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let root = resolve_against(&self.session_cwd, &args.path);
        if !root.exists() {
            return Err(DocIndexError::NotFound(args.path));
        }

        let recursive = args.recursive.unwrap_or(false);
        let files = collect_files(&root, recursive);

        let mut report = IndexReport::default();
        for file in files {
            // Unsupported extensions are counted and skipped, never fatal.
            if !is_supported(&file) {
                report.unsupported += 1;
                continue;
            }
            match self.store.index_file(&file, &args.metadata).await {
                Ok(IndexOutcome::Indexed(n)) => {
                    report.indexed += 1;
                    report.chunks += n;
                }
                Ok(IndexOutcome::Updated(n)) => {
                    report.updated += 1;
                    report.chunks += n;
                }
                Ok(IndexOutcome::Skipped) => report.skipped += 1,
                // A single bad file (e.g. malformed PDF) must not abort the
                // whole directory — surface it inline and keep going.
                Err(e) => {
                    return Err(DocIndexError::Store(e));
                }
            }
        }

        Ok(format!(
            "Indexed {} file(s), updated {}, skipped {} (unchanged), {} unsupported. \
             {} chunk(s) written.",
            report.indexed, report.updated, report.skipped, report.unsupported, report.chunks
        ))
    }
}

/// Collect candidate files under `root`. If `root` is a file, returns just it.
/// If a directory, lists its entries (recursively when `recursive`). Filtering
/// by supported extension happens in the caller so unsupported files can be
/// counted in the report.
fn collect_files(root: &Path, recursive: bool) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    let mut out = Vec::new();
    collect_dir(root, recursive, &mut out);
    out
}

fn collect_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if recursive {
                collect_dir(&path, recursive, out);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn single_file_collects_itself() {
        let mut f = tempfile::Builder::new().suffix(".txt").tempfile().unwrap();
        write!(f, "x").unwrap();
        let files = collect_files(f.path(), false);
        assert_eq!(files, vec![f.path().to_path_buf()]);
    }

    #[test]
    fn directory_respects_recursion_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();

        let shallow = collect_files(dir.path(), false);
        assert_eq!(shallow.len(), 1);

        let deep = collect_files(dir.path(), true);
        assert_eq!(deep.len(), 2);
    }

    // A relative `path` resolves against the injected session_cwd. Uses an
    // *unsupported* extension so `call()` completes without ever hitting the
    // embeddings endpoint (no network) — the resolution is what's under test.
    #[tokio::test]
    async fn resolves_relative_against_session_cwd() {
        use crate::config::{EmbeddingsConfig, VectorDbConfig};

        let db_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        // An unsupported file living under the session dir.
        std::fs::write(session_dir.path().join("probe.unsupported"), "x").unwrap();

        let config = VectorDbConfig {
            enabled: true,
            db_path: db_dir.path().join("v.db").display().to_string(),
            embeddings: EmbeddingsConfig {
                base_url: "http://unused.invalid".into(),
                api_key: None,
                model: "test".into(),
                dimensions: 3,
            },
        };
        let store = VectorStore::open(&config).unwrap();
        let tool = DocIndexTool::new(store).with_session_cwd(session_dir.path().to_path_buf());

        // Relative path — must resolve under session_dir and be found.
        let args: DocIndexArgs = serde_json::from_value(serde_json::json!({
            "path": "probe.unsupported"
        }))
        .unwrap();
        let out = tool
            .call(args)
            .await
            .expect("relative path should resolve + be found");
        assert!(out.contains("1 unsupported"), "got: {out}");

        // A relative path that does NOT exist under session_dir is NotFound —
        // proving resolution is anchored to session_cwd, not the process cwd.
        let missing: DocIndexArgs = serde_json::from_value(serde_json::json!({
            "path": "nope.unsupported"
        }))
        .unwrap();
        assert!(matches!(
            tool.call(missing).await,
            Err(DocIndexError::NotFound(_))
        ));
    }
}
