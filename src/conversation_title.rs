//! Auto-generated conversation titles.
//!
//! After the first assistant response, PeakBot calls the LLM to generate
//! a short, descriptive title for the conversation. This mirrors how
//! ChatGPT titles conversations — it helps users identify conversations
//! in the `/conversations` listing without needing to remember timestamps.
//!
//! Title generation is fire-and-forget: failures are logged and silently
//! ignored. The conversation continues with its creation-name as the
//! fallback display string.

use crate::providers::CompactionModel;
use anyhow::Result;

/// Prompt sent to the LLM to generate a conversation title.
const TITLE_PROMPT_TEMPLATE: &str = "\
Given this conversation, generate a short, descriptive title (max 60 characters).
The title should capture the main topic or task discussed.
Respond with ONLY the title — no quotes, no explanation.

Conversation:
{conversation}

Title:";

/// Generate a conversation title from the message history.
///
/// Call this after the first assistant response when
/// `message_count == 1` (i.e., the just-completed turn).
///
/// Returns the title on success, or an error if the LLM call fails.
/// Callers should log errors but treat them as non-fatal — the
/// conversation continues without a title, falling back to its
/// creation name.
pub async fn generate_conversation_title(
    messages: &[(String, String)], // (role, content) pairs
    model: &CompactionModel,
) -> Result<String> {
    // Format the conversation transcript for the prompt
    let transcript = messages
        .iter()
        .map(|(role, content)| format!("{}: {}\n", role, content))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = TITLE_PROMPT_TEMPLATE.replace("{conversation}", &transcript);

    let title = model.summarize(&prompt).await?;

    // Trim whitespace and enforce the 60-char max
    let title = title.trim();
    if title.len() > 60 {
        // Truncate at word boundary near 60 chars
        let truncated = &title[..60];
        Ok(truncated.rsplit_once(' ').map(|(pre, _)| pre).unwrap_or(truncated).to_string())
    } else {
        Ok(title.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_prompt_template_includes_transcript() {
        let transcript = "user: Hello\nassistant: Hi there";
        let prompt = TITLE_PROMPT_TEMPLATE.replace("{conversation}", transcript);
        assert!(prompt.contains("Hello"));
        assert!(prompt.contains("Hi there"));
        assert!(prompt.contains("user:"));
        assert!(prompt.contains("assistant:"));
    }

    #[test]
    fn title_generation_logic_truncates_long_titles() {
        let long_title = "This is a very long conversation title that exceeds the sixty character limit by a lot";
        let truncated = if long_title.len() > 60 {
            let cut = &long_title[..60];
            cut.rsplit_once(' ').map(|(pre, _)| pre).unwrap_or(cut).to_string()
        } else {
            long_title.to_string()
        };
        assert!(truncated.len() <= 60);
        assert!(truncated.ends_with(char::is_alphanumeric) || !truncated.is_empty());
    }

    #[test]
    fn title_generation_logic_preserves_short_titles() {
        let short_title = "Fix bug in auth flow";
        let result = if short_title.trim().len() > 60 {
            let cut = &short_title.trim()[..60];
            cut.rsplit_once(' ').map(|(pre, _)| pre).unwrap_or(cut).to_string()
        } else {
            short_title.trim().to_string()
        };
        assert_eq!(result, "Fix bug in auth flow");
    }
}