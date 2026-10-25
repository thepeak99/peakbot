//! `Token` — the web-access secret (DESIGN §3.2). The desktop only ever
//! READS it, from config.yaml `general.web.token` (spec §8.4): it never
//! generates, validates for entry, or writes a token.

use std::fmt;

/// A web-access token. Non-empty, URL-safe after percent-encoding; never
/// logged. `Debug` prints `Token(***)` only.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Trims; `None` if empty/whitespace-only or containing any
    /// whitespace/control char.
    pub fn parse(s: &str) -> Option<Token> {
        let t = s.trim();
        if t.is_empty() || t.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return None;
        }
        Some(Token(t.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Locked: prints "Token(***)" — never the secret (§3.2 / §3.8-7).
        f.write_str("Token(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // §9.1 case 1.
    #[test]
    fn t01_token_parse_trims_and_rejects_blank_or_inner_whitespace() {
        let ok = Token::parse(" abc ").expect("trimmed token must parse");
        assert_eq!(ok.as_str(), "abc");
        assert_eq!(Token::parse(""), None, "empty must not parse");
        assert_eq!(Token::parse("   "), None, "whitespace-only must not parse");
        assert_eq!(Token::parse("a b"), None, "inner space must not parse");
        assert_eq!(Token::parse("a\nb"), None, "inner newline must not parse");
    }

    // §9.1 case 2.
    #[test]
    fn t02_token_debug_never_prints_the_secret() {
        let t = Token::parse("super-secret-token").expect("parses");
        let dbg = format!("{t:?}");
        assert!(
            !dbg.contains("super-secret-token"),
            "Debug leaked the secret: {dbg}"
        );
        assert!(!dbg.is_empty(), "Debug must print something");
    }
}
