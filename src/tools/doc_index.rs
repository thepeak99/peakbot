//! `doc_index` tool: parse → chunk → embed → store a file or directory.

use std::path::{Path, PathBuf};

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::vector::{IndexOutcome, IndexReport, VectorStore, VectorStoreError, is_supported};

#[derive(Debug, Deserialize)]
pub struct DocIndexArgs {
    /// File or directory to index.
    pub path: String,
    /// When `path` is a directory, recurse into subdirectories (default: false).
    #[serde(default)]
    pub recursive: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum DocIndexError {
    #[error("path does not exist: {0}")]
    NotFound(String),
    #[error(transparent)]
    Store(#[from] VectorStoreError),
}

/// Indexing tool over the shared vector store.
#[derive(Clone)]
pub struct DocIndexTool {
    store: VectorStore,
}

impl DocIndexTool {
    pub fn new(store: VectorStore) -> Self {
        Self { store }
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
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain what you're about to do and why, before acting."
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file or directory to index."
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "When path is a directory, recurse into subdirectories (default: false).",
                        "default": false
                    }
                },
                "required": ["thought", "path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let root = PathBuf::from(&args.path);
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
            match self.store.index_file(&file).await {
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
}
