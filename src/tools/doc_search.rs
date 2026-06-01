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
                "\n[{}] {} (chunk {}, score {:.3})\n{}\n",
                i + 1,
                hit.source,
                hit.chunk_index,
                hit.score,
                hit.text.trim()
            ));
        }
        Ok(out)
    }
}
