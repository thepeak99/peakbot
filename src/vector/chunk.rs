//! Text chunking for the vector store.
//!
//! Splits a document's plain text into overlapping character windows. Chunk
//! size and overlap are constants, not config — "config fields cost forever;
//! default constants cost nothing." If a real need to tune them appears, they
//! graduate to config then, not before.

/// Target chunk size in characters.
const CHUNK_SIZE: usize = 1000;
/// Overlap between consecutive chunks, in characters. Preserves context that
/// would otherwise be severed at a chunk boundary.
const CHUNK_OVERLAP: usize = 200;

/// Split `text` into overlapping chunks of `CHUNK_SIZE` chars with
/// `CHUNK_OVERLAP` chars of overlap between neighbours.
///
/// Operates on `char` boundaries (not bytes), so multi-byte UTF-8 is never
/// split mid-codepoint. Whitespace-only input yields no chunks. Input shorter
/// than one chunk yields a single chunk (when non-empty after trimming).
pub fn split(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    // Stride is the distance between chunk starts. Overlap < size is enforced
    // by the constants above; the `.max(1)` is a belt-and-braces guard so a
    // future bad edit can never produce a zero stride (infinite loop).
    let stride = CHUNK_SIZE.saturating_sub(CHUNK_OVERLAP).max(1);

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + CHUNK_SIZE).min(chars.len());
        let chunk: String = chars[start..end].iter().collect();
        let trimmed = chunk.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        if end == chars.len() {
            break;
        }
        start += stride;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_chunks() {
        assert!(split("").is_empty());
        assert!(split("   \n\t ").is_empty());
    }

    #[test]
    fn short_input_is_one_chunk() {
        let chunks = split("hello world");
        assert_eq!(chunks, vec!["hello world".to_string()]);
    }

    #[test]
    fn long_input_splits_with_overlap() {
        let text: String = "a".repeat(2500);
        let chunks = split(&text);
        // stride = 800, size = 1000 over 2500 chars → starts at 0, 800, 1600.
        // At start=1600 the window end = min(2600, 2500) = 2500, reaching the
        // end, so the loop stops: 3 chunks.
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chars().count(), 1000);
        // Last chunk: chars 1600..2500 = 900 chars.
        assert_eq!(chunks[2].chars().count(), 900);
    }

    #[test]
    fn multibyte_is_not_split_mid_codepoint() {
        // 1500 emoji → each is one `char` but 4 bytes. Must never panic and
        // every chunk must be valid UTF-8 (guaranteed by collecting chars).
        let text: String = "😀".repeat(1500);
        let chunks = split(&text);
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.chars().all(|ch| ch == '😀'));
        }
    }
}
