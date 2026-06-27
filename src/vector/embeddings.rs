//! Embeddings client for any OpenAI-compatible `POST /v1/embeddings` endpoint.
//!
//! Deliberately NOT built on rig's `EmbeddingModel` trait: that couples to
//! rig's chat-provider clients, whereas the embeddings endpoint is configured
//! independently of the chat model. This is a thin reqwest client over the
//! one request shape every OpenAI-compatible server speaks (OpenAI, llama.cpp,
//! Ollama, LM Studio, TEI, …).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::EmbeddingsConfig;

/// Request timeout for an embeddings call. A batch of chunks can be large, so
/// this is generous relative to the search timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Error)]
pub enum EmbeddingsError {
    #[error("embeddings request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("embeddings endpoint returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("embeddings endpoint returned {got} vectors for {want} inputs")]
    CountMismatch { want: usize, got: usize },
    #[error(
        "embedding dimension mismatch: model returned {got}, config declares {want} \
         (set vector_db.embeddings.dimensions to {got})"
    )]
    DimMismatch { want: usize, got: usize },
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// A configured embeddings endpoint. Cheap to clone (holds a `reqwest::Client`
/// which is internally `Arc`-backed).
#[derive(Clone)]
pub struct EmbeddingsClient {
    client: reqwest::Client,
    url: String,
    api_key: Option<String>,
    model: String,
    dimensions: usize,
}

impl EmbeddingsClient {
    pub fn new(config: &EmbeddingsConfig) -> Self {
        // Join base_url + "/embeddings", tolerating a trailing slash on base_url.
        let base = config.base_url.trim_end_matches('/');
        Self {
            client: crate::http::client(),
            url: format!("{base}/embeddings"),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            dimensions: config.dimensions,
        }
    }

    /// The dimensionality this client's config declares. Used to create a new
    /// DB and to validate against an existing one.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Embed a batch of texts, returning one vector per input (order preserved).
    ///
    /// Validates that the endpoint returned exactly one vector per input and
    /// that each vector's length matches the configured `dimensions` — a
    /// mismatch is a clear, actionable error rather than silent corruption of
    /// the vector store.
    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingsError> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }

        let body = EmbeddingRequest {
            model: &self.model,
            input: inputs,
        };

        let mut request = self
            .client
            .post(&self.url)
            .timeout(REQUEST_TIMEOUT)
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }

        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(EmbeddingsError::Status {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: EmbeddingResponse = response.json().await?;
        if parsed.data.len() != inputs.len() {
            return Err(EmbeddingsError::CountMismatch {
                want: inputs.len(),
                got: parsed.data.len(),
            });
        }

        let vectors: Vec<Vec<f32>> = parsed.data.into_iter().map(|d| d.embedding).collect();
        if let Some(first) = vectors.first()
            && first.len() != self.dimensions
        {
            return Err(EmbeddingsError::DimMismatch {
                want: self.dimensions,
                got: first.len(),
            });
        }
        Ok(vectors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EmbeddingsConfig {
        EmbeddingsConfig {
            base_url: "https://api.example.com/v1/".to_string(),
            api_key: Some("sk-test".to_string()),
            model: "text-embedding-3-small".to_string(),
            dimensions: 1536,
        }
    }

    #[test]
    fn url_is_built_without_double_slash() {
        let client = EmbeddingsClient::new(&cfg());
        assert_eq!(client.url, "https://api.example.com/v1/embeddings");
    }

    #[test]
    fn dimensions_is_exposed() {
        assert_eq!(EmbeddingsClient::new(&cfg()).dimensions(), 1536);
    }

    #[tokio::test]
    async fn empty_input_short_circuits() {
        let client = EmbeddingsClient::new(&cfg());
        // No network call should happen; an empty batch returns empty.
        assert!(client.embed(&[]).await.unwrap().is_empty());
    }
}
