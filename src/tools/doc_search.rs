//! `doc_search` tool: semantic search over indexed documents.

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

use crate::vector::{DEFAULT_K, VectorStore, VectorStoreError};

#[derive(Debug, Deserialize)]
pub struct DocSearchArgs {
    /// The natural-language query to search for.
    pub query: String,
    /// Maximum number of chunks to return (default: 5).
    #[serde(default)]
    pub k: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum DocSearchError {
    #[error(transparent)]
    Store(#[from] VectorStoreError),
}

/// Semantic search tool over the shared vector store.
#[derive(Clone)]
pub struct DocSearchTool {
    store: VectorStore,
}

impl DocSearchTool {
    pub fn new(store: VectorStore) -> Self {
        Self { store }
    }
}

impl Tool for DocSearchTool {
    const NAME: &'static str = "doc_search";
    type Error = DocSearchError;
    type Args = DocSearchArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Semantic search over documents previously indexed with doc_index. \
                Embeds the query and returns the most relevant text chunks, each with its \
                source file and similarity score."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Briefly explain what you're about to do and why, before acting."
                    },
                    "query": {
                        "type": "string",
                        "description": "The natural-language query to search for."
                    },
                    "k": {
                        "type": "integer",
                        "description": "Maximum number of chunks to return (default: 5).",
                        "default": DEFAULT_K
                    }
                },
                "required": ["thought", "query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let k = args.k.unwrap_or(DEFAULT_K).clamp(1, 50);
        let hits = self.store.search(&args.query, k).await?;

        if hits.is_empty() {
            return Ok(format!(
                "No indexed chunks matched the query: {}",
                args.query
            ));
        }

        let mut out = format!("Top {} result(s) for: {}\n", hits.len(), args.query);
        for (i, hit) in hits.iter().enumerate() {
            out.push_str(&format!(
                "\n[{}] {} (chunk {}, score {:.3}){}\n{}\n",
                i + 1,
                hit.source,
                hit.chunk_index,
                hit.score,
                format_metadata(&hit.metadata),
                hit.text.trim()
            ));
        }
        Ok(out)
    }
}

/// Render user metadata as ` [key=value, …]`, keys sorted for stable output.
/// Returns an empty string when there's no metadata so the header line stays
/// clean for un-annotated documents.
fn format_metadata(md: &std::collections::HashMap<String, serde_json::Value>) -> String {
    if md.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<String> = md
        .iter()
        .map(|(k, v)| {
            // Render strings bare (author=Tolkien, not author="Tolkien").
            let val = v
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string());
            format!("{k}={val}")
        })
        .collect();
    pairs.sort();
    format!(" [{}]", pairs.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn format_metadata_empty_is_blank() {
        assert_eq!(format_metadata(&HashMap::new()), "");
    }

    #[test]
    fn format_metadata_sorts_and_unquotes_strings() {
        let mut md = HashMap::new();
        md.insert("year".to_string(), serde_json::json!("1954"));
        md.insert("author".to_string(), serde_json::json!("Tolkien"));
        // Keys sorted; string values rendered bare (no surrounding quotes).
        assert_eq!(format_metadata(&md), " [author=Tolkien, year=1954]");
    }

    #[test]
    fn format_metadata_renders_non_string_values() {
        let mut md = HashMap::new();
        md.insert("page".to_string(), serde_json::json!(12));
        assert_eq!(format_metadata(&md), " [page=12]");
    }
}
