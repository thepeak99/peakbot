//! Automatic memory.md compaction.
//!
//! Triggered once at conversation start when memory.md exceeds the configured
//! threshold. Loads the raw file content, passes it to a tool-free compaction
//! model with a specialized prompt, and writes the result back to disk.
//!
//! Mirrors the pattern of context compaction (ContextManager + CompactionModel)
//! but operates on the memory file instead of chat history.

use crate::providers::CompactionModel;
use std::path::Path;

/// Prompt sent to the compaction model for memory.md rewriting.
/// Instructs the model to preserve structure while condensing content.
const MEMORY_COMPACTION_PROMPT: &str = r#"You are a memory file compactor. Given a memory.md file, rewrite it to be more concise while preserving all durable knowledge.

Rules:
1. EPISODIC section: Keep only the 5 most recent entries. For older entries, extract any lasting lessons, patterns, or rules and move them to PROCEDURAL or SEMANTIC. Reduce old episodic entries to one-line mentions with dates, or remove entirely if fully absorbed.
2. SEMANTIC section: Merge redundant entries. Remove outdated facts. Keep only knowledge still relevant to the project.
3. PROCEDURAL section: Merge related rules. Remove rules that are now second-nature or superseded. Keep distilled wisdom.
4. Preserve the overall structure: # PeakBot Memory header, ## Episodic, ## Semantic, ## Procedural sections.
5. Update the "Last compacted" line to today's date.

Output ONLY the rewritten memory.md content, nothing else.

Here is the current memory.md file:

"#;

/// Check if a memory file exists and exceeds the threshold.
/// Returns the file content if it needs compaction, `None` otherwise.
pub fn read_if_oversized(path: &Path, threshold_bytes: usize) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let size = metadata.len() as usize;

    if size <= threshold_bytes {
        return None;
    }

    std::fs::read_to_string(path).ok()
}

/// Compact memory.md content using the compaction model.
/// Prepends the compaction prompt and returns the model's rewritten output.
pub async fn compact_memory(
    content: &str,
    model: &CompactionModel,
) -> Result<String, rig::completion::PromptError> {
    let prompt = format!("{}{}", MEMORY_COMPACTION_PROMPT, content);
    model.summarize(&prompt).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_if_oversized_file_not_found() {
        let result = read_if_oversized(Path::new("/nonexistent/path/to/memory.md"), 100);
        assert!(result.is_none());
    }

    #[test]
    fn read_if_oversized_under_threshold() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "small content").unwrap();
        let result = read_if_oversized(tmp.path(), 100);
        assert!(result.is_none());
    }

    #[test]
    fn read_if_oversized_at_threshold() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let content = "x".repeat(100);
        write!(tmp, "{}", content).unwrap();
        // At threshold exactly — should NOT trigger
        let result = read_if_oversized(tmp.path(), 100);
        assert!(result.is_none());
    }

    #[test]
    fn read_if_oversized_over_threshold() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let content = "x".repeat(200);
        write!(tmp, "{}", content).unwrap();
        let result = read_if_oversized(tmp.path(), 100);
        assert!(result.is_some());
        let read = result.unwrap();
        assert_eq!(read.len(), 200);
        assert_eq!(read, content);
    }

    #[test]
    fn read_if_oversized_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Empty file (0 bytes) is under any positive threshold
        let result = read_if_oversized(tmp.path(), 1);
        assert!(result.is_none());
    }

    #[cfg(feature = "mock")]
    mod mock_tests {
        use super::*;

        #[tokio::test]
        async fn compact_memory_success() {
            let (model, mock) = crate::providers::create_mock_compaction_model();
            mock.add_response(crate::mock::MockResponse::text("compacted memory content"));

            let result = compact_memory("original memory content", &model).await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "compacted memory content");
        }

        #[tokio::test]
        async fn compact_memory_error() {
            let (model, mock) = crate::providers::create_mock_compaction_model();
            mock.add_response(crate::mock::MockResponse::error("model failed"));

            let result = compact_memory("original memory content", &model).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn compact_memory_prompt_includes_rules_and_content() {
            let (model, mock) = crate::providers::create_mock_compaction_model();
            mock.add_response(crate::mock::MockResponse::text("compacted"));

            let original = "# PeakBot Memory\n\n## Episodic\n\n- Entry 1\n";
            let _ = compact_memory(original, &model).await;

            let requests = mock.get_recorded_requests();
            assert_eq!(requests.len(), 1);

            // The chat history should contain our memory compaction prompt + content
            let last_msg = requests[0].chat_history.last().unwrap();
            let msg_text = match last_msg {
                rig::completion::message::Message::User { content } => content
                    .iter()
                    .map(|c| match c {
                        rig::completion::message::UserContent::Text(t) => t.text.clone(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join(""),
                _ => panic!("Expected user message"),
            };
            assert!(
                msg_text.contains("EPISODIC section"),
                "prompt should contain compaction rules"
            );
            assert!(
                msg_text.contains("Entry 1"),
                "prompt should contain original memory content"
            );
        }
    }
}
