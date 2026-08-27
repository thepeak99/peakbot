//! Auto-generated conversation titles.
//!
//! Titling is two-phase: the opening user message immediately yields a
//! local, LLM-free provisional title (so listings are never blank while the
//! first reply is still in flight), and the LLM upgrades it to a definitive
//! title once a transcript exists. This mirrors how ChatGPT titles
//! conversations — it helps users identify them in the `/conversations`
//! listing without remembering timestamps.
//!
//! Title generation is fire-and-forget: failures are logged and silently
//! ignored. The conversation continues with its creation-name as the
//! fallback display string.

use crate::providers::CompactionModel;
use anyhow::Result;

/// Hard cap on title length, in chars (not bytes). Single source of the 60
/// the prompt also states; `clamp_title` enforces it.
pub const TITLE_MAX_CHARS: usize = 60;

/// Prompt sent to the LLM to generate a conversation title.
const TITLE_PROMPT_TEMPLATE: &str = "\
Given this conversation (it may contain only the user's opening message), generate a short, descriptive title (max 60 characters).
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

    // Char-safe clamp (never byte-indexes) — see `clamp_title`.
    Ok(clamp_title(&title))
}

/// Trim and clamp a title to [`TITLE_MAX_CHARS`] chars, cutting back to a
/// whole word. Char-based (never byte-indexed) so multi-byte input can't
/// panic or split a character — the old `&title[..60]` did.
pub fn clamp_title(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TITLE_MAX_CHARS {
        return trimmed.to_string();
    }
    let window: String = trimmed.chars().take(TITLE_MAX_CHARS).collect();
    // Cut back to the last space so the title ends on a whole word.
    window
        .rsplit_once(' ')
        .map(|(pre, _)| pre.to_string())
        .unwrap_or(window)
}

/// Derive the LLM-free provisional title from a user message: collapse
/// whitespace runs to single spaces, trim, then clamp. `None` when blank
/// (empty, whitespace-only, or image-only). Bounded to the first ~60 output
/// chars, so a huge paste never allocates a full collapsed copy.
pub fn provisional_title(text: &str) -> Option<String> {
    let mut out = String::with_capacity(TITLE_MAX_CHARS + 1);
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        // Only enough of each word to decide the clamp; the rest is dropped.
        out.extend(word.chars().take(TITLE_MAX_CHARS));
        if out.chars().count() > TITLE_MAX_CHARS {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(clamp_title(&out))
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

    /// The prompt must tell the LLM the transcript may be user-only — the
    /// escape hatch fires on two user rows with no assistant reply yet.
    #[test]
    fn title_prompt_tolerates_user_only_transcripts() {
        assert!(
            TITLE_PROMPT_TEMPLATE.contains("may contain only the user's opening message"),
            "prompt must tolerate user-only transcripts; got: {TITLE_PROMPT_TEMPLATE}"
        );
    }

    /// provisional_title collapses every whitespace run to a single space
    /// and trims the ends.
    #[test]
    fn provisional_title_collapses_whitespace() {
        assert_eq!(
            provisional_title("  fix   the\n\n sudo  bug "),
            Some("fix the sudo bug".to_string())
        );
    }

    /// Blank input (empty, or whitespace only) yields no title.
    #[test]
    fn provisional_title_is_none_for_blank_input() {
        assert_eq!(provisional_title(""), None);
        assert_eq!(provisional_title("  \n\t "), None);
    }

    /// A single 200-char word (no spaces) is clamped to exactly
    /// TITLE_MAX_CHARS characters.
    #[test]
    fn provisional_title_clamps_to_sixty_chars() {
        let word = "a".repeat(200);
        let s = provisional_title(&word).expect("a long word must still produce a title");
        assert_eq!(s.chars().count(), TITLE_MAX_CHARS);
    }

    /// Multibyte input must be clamped by CHARS, never bytes: no panic,
    /// no split character, result within the limit.
    #[test]
    fn provisional_title_never_splits_a_multibyte_char() {
        // 100 × é (a 2-byte UTF-8 char).
        let accented = "é".repeat(100);
        let s = provisional_title(&accented).expect("accented input must produce a title");
        assert!(s.chars().count() <= TITLE_MAX_CHARS);

        // Mixed 4-byte emoji.
        let emoji = "🚀🎉🌟".repeat(40);
        let s = provisional_title(&emoji).expect("emoji input must produce a title");
        assert!(s.chars().count() <= TITLE_MAX_CHARS);
    }

    /// A long multi-word sentence is cut back to a whole word: within the
    /// limit, ending on an alphanumeric char with a space right after it in
    /// the input.
    #[test]
    fn clamp_title_cuts_back_to_word_boundary() {
        // 86 chars, no leading/trailing whitespace, so the result is a
        // prefix of the input and clamping actually fires.
        let input = "This is a very long conversation title that exceeds the sixty character limit by a lot";
        let result = clamp_title(input);
        assert!(result.chars().count() <= TITLE_MAX_CHARS);
        let n = result.chars().count();
        // Ends on a whole word: the last char is alphanumeric...
        assert!(
            result.chars().next_back().unwrap().is_alphanumeric(),
            "must end on a whole word, got: {result:?}"
        );
        // ...and the next char of the input at that point is the space we cut at.
        assert_eq!(
            input.chars().nth(n),
            Some(' '),
            "the char right after the clamp must be a space; got: {result:?}"
        );
    }

    /// 100 CJK chars (3 bytes each in UTF-8) clamp to exactly 60 chars
    /// without panicking — regression for the byte-indexing panic in the
    /// old inline truncation.
    #[test]
    fn clamp_title_is_char_safe_on_cjk() {
        let cjk = "中".repeat(100);
        let result = clamp_title(&cjk);
        assert_eq!(result.chars().count(), TITLE_MAX_CHARS);
    }

    /// A short title is trimmed and returned untouched.
    #[test]
    fn clamp_title_leaves_short_titles_untouched() {
        assert_eq!(
            clamp_title("  Fix bug in auth flow "),
            "Fix bug in auth flow"
        );
    }
}
