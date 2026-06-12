//! Mock completion model for integration testing
//!
//! This module is only compiled when running tests (`cargo test`).
//! It provides MockCompletionModel for testing the agent loop without real API calls.

use crate::mock::response::MockResponse;
use rig_core::completion::message::Message;
use rig_core::completion::message::{Text, ToolCall, ToolFunction};
use rig_core::completion::{
    AssistantContent, CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    GetTokenUsage, Usage as RigUsage,
};
use rig_core::one_or_many::OneOrMany;
use rig_core::streaming::StreamingCompletionResponse;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A recorded LLM request captured by MockCompletionModel.
///
/// Stores the fields from `CompletionRequest` that tests need to inspect.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// The system prompt / preamble
    pub preamble: Option<String>,
    /// The full chat history (including the prompt as the last message)
    pub chat_history: Vec<Message>,
    /// Number of tool definitions attached to the request
    pub tool_count: usize,
}

/// A mock completion model for testing.
///
/// Queue mock responses and the model will return them in order.
/// Records every `CompletionRequest` it receives for later inspection.
/// Used to test the agent loop without real API calls.
#[derive(Clone)]
pub struct MockCompletionModel {
    responses: Arc<Mutex<VecDeque<MockResponse>>>,
    recorded_requests: Arc<Mutex<Vec<RecordedRequest>>>,
    default_usage: RigUsage,
}

impl MockCompletionModel {
    /// Create a new MockCompletionModel with no responses queued.
    ///
    /// Use `add_response()` to queue responses before running tests.
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            recorded_requests: Arc::new(Mutex::new(Vec::new())),
            default_usage: RigUsage::default(),
        }
    }

    /// Queue a mock response.
    ///
    /// Responses are returned in FIFO order.
    pub fn add_response(&self, response: MockResponse) {
        self.responses.lock().unwrap().push_back(response);
    }

    /// Queue multiple responses at once.
    pub fn add_responses(&self, responses: impl IntoIterator<Item = MockResponse>) {
        let mut queue = self.responses.lock().unwrap();
        for response in responses {
            queue.push_back(response);
        }
    }

    /// Set the default usage for responses that don't specify it.
    pub fn set_default_usage(&mut self, input: u64, output: u64) {
        self.default_usage = RigUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        };
    }

    /// Clear all queued responses.
    pub fn clear(&self) {
        self.responses.lock().unwrap().clear();
    }

    /// Check if there are responses queued.
    pub fn has_responses(&self) -> bool {
        !self.responses.lock().unwrap().is_empty()
    }

    /// Get the number of responses remaining in the queue.
    pub fn remaining(&self) -> usize {
        self.responses.lock().unwrap().len()
    }

    /// Get all recorded requests in order.
    pub fn get_recorded_requests(&self) -> Vec<RecordedRequest> {
        self.recorded_requests.lock().unwrap().clone()
    }

    /// Get the total number of LLM calls made.
    pub fn request_count(&self) -> usize {
        self.recorded_requests.lock().unwrap().len()
    }
}

impl Default for MockCompletionModel {
    fn default() -> Self {
        Self::new()
    }
}

/// Response type for MockCompletionModel
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MockModelResponse {
    pub content: String,
    pub is_tool_call: bool,
}

impl GetTokenUsage for MockModelResponse {
    fn token_usage(&self) -> Option<RigUsage> {
        // Default usage - caller can set via MockCompletionModel::set_default_usage
        Some(RigUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        })
    }
}

impl CompletionModel for MockCompletionModel {
    type Response = MockModelResponse;
    type StreamingResponse = MockModelResponse;
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        Self::new()
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        // Record the request for test inspection
        self.recorded_requests
            .lock()
            .unwrap()
            .push(RecordedRequest {
                preamble: request.preamble.clone(),
                chat_history: request.chat_history.iter().cloned().collect(),
                tool_count: request.tools.len(),
            });

        let response = self.responses.lock().unwrap().pop_front().ok_or_else(|| {
            CompletionError::ProviderError("MockCompletionModel: no responses queued".to_string())
        });

        match response {
            Ok(MockResponse::Text { content, usage }) => {
                let usage = usage
                    .map(|u| RigUsage {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        total_tokens: u.input_tokens + u.output_tokens,
                        cached_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        tool_use_prompt_tokens: 0,
                        reasoning_tokens: 0,
                    })
                    .unwrap_or(self.default_usage);

                let content_clone = content.clone();
                Ok(CompletionResponse {
                    choice: OneOrMany::one(AssistantContent::Text(Text::new(content))),
                    usage,
                    raw_response: MockModelResponse {
                        content: content_clone,
                        is_tool_call: false,
                    },
                    message_id: None,
                })
            }
            Ok(MockResponse::ToolCall {
                tool,
                args,
                follow_up,
                usage,
            }) => {
                let tool_call = ToolCall::new(
                    format!("call_{}", uuid::Uuid::new_v4()),
                    ToolFunction::new(tool.clone(), args),
                );

                let mut contents = vec![AssistantContent::ToolCall(tool_call)];

                // Add follow-up text if present
                let raw_text = if let Some(ref text) = follow_up {
                    contents.push(AssistantContent::Text(Text::new(text.clone())));
                    text.clone()
                } else {
                    format!("Tool call: {}", tool)
                };

                let usage = usage
                    .map(|u| RigUsage {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        total_tokens: u.input_tokens + u.output_tokens,
                        cached_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                        tool_use_prompt_tokens: 0,
                        reasoning_tokens: 0,
                    })
                    .unwrap_or(self.default_usage);

                Ok(CompletionResponse {
                    choice: OneOrMany::many(contents)
                        .expect("Tool call response always has content"),
                    usage,
                    raw_response: MockModelResponse {
                        content: raw_text,
                        is_tool_call: true,
                    },
                    message_id: None,
                })
            }
            Ok(MockResponse::Error { message }) => Err(CompletionError::ProviderError(message)),
            Err(e) => Err(e),
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        use futures::stream;
        use rig_core::streaming::RawStreamingChoice;

        // For simplicity, return a single-item stream with the completion
        let response = self.completion(request).await?;
        let stream_response =
            RawStreamingChoice::<MockModelResponse>::Message(response.raw_response.content);
        let stream = stream::once(async move { Ok(stream_response) });
        Ok(StreamingCompletionResponse::stream(Box::pin(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::response::MockResponse;

    #[tokio::test]
    async fn test_text_response() {
        let mock = MockCompletionModel::new();
        mock.add_response(MockResponse::text("Hello, world!"));

        let request = MockCompletionModel::make(&(), "test-model").completion_request("test");
        let response = mock.completion(request.build()).await.unwrap();

        let text = response.choice.first();
        match text {
            AssistantContent::Text(t) => assert_eq!(t.text, "Hello, world!"),
            _ => panic!("Expected text response"),
        }
    }

    #[tokio::test]
    async fn test_tool_call_response() {
        let mock = MockCompletionModel::new();
        mock.add_response(MockResponse::tool_call(
            "todo",
            serde_json::json!({
                "action": "add",
                "tasks": ["Test task"]
            }),
        ));

        let request = MockCompletionModel::make(&(), "test-model").completion_request("test");
        let response = mock.completion(request.build()).await.unwrap();

        let choice = response.choice.first();
        match choice {
            AssistantContent::ToolCall(tc) => {
                assert_eq!(tc.function.name, "todo");
            }
            _ => panic!("Expected tool call response"),
        }
    }

    #[tokio::test]
    async fn test_empty_queue_error() {
        let mock = MockCompletionModel::new();

        let request = MockCompletionModel::make(&(), "test-model").completion_request("test");
        let result = mock.completion(request.build()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_multiple_responses() {
        let mock = MockCompletionModel::new();
        mock.add_responses(vec![
            MockResponse::text("First"),
            MockResponse::text("Second"),
            MockResponse::text("Third"),
        ]);

        assert_eq!(mock.remaining(), 3);

        let _r1 = mock
            .completion(
                MockCompletionModel::make(&(), "test-model")
                    .completion_request("test")
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(mock.remaining(), 2);

        let _r2 = mock
            .completion(
                MockCompletionModel::make(&(), "test-model")
                    .completion_request("test")
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(mock.remaining(), 1);

        let _r3 = mock
            .completion(
                MockCompletionModel::make(&(), "test-model")
                    .completion_request("test")
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(mock.remaining(), 0);
    }
}
