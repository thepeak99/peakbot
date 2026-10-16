//! `Token` — the web-access secret (DESIGN §3.2).

use std::fmt;

use serde::Serialize;

use crate::sys::Sys;

/// The boundary for a USER-entered token (design #377b §3.1): the charset
/// is required (the desktop form-encodes the token into the URL and the
/// backend compares the raw query value), 16..=256 for loopback and
/// remote alike (Q2: loopback is brute-forceable from local pages, #378).
pub const TOKEN_MIN_LEN: usize = 16;
pub const TOKEN_MAX_LEN: usize = 256;

/// A web-access token. Non-empty, URL-safe after percent-encoding; never
/// logged. `Debug` prints `Token(***)` only.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

/// Why a USER-entered token was rejected (design #377b §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    BadChars,
    TooShort,
    TooLong,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TokenError::BadChars => {
                "The access token may only contain letters, digits, dashes, underscores and dots."
            }
            TokenError::TooShort => "The access token must be at least 16 characters.",
            TokenError::TooLong => "The access token must be at most 256 characters.",
        })
    }
}

impl std::error::Error for TokenError {}

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

    /// Delegates to `sys.random_token()`.
    pub fn generate(sys: &impl Sys) -> Token {
        sys.random_token()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A token the USER typed (settings page). Stricter than `Token::parse`
/// (which still reads whatever is in the file): trim, then
/// `""` ⇒ `Ok(None)` ("keep the current one, or generate one"), any
/// char ∉ `[A-Za-z0-9._-]` ⇒ `BadChars` (checked FIRST), then
/// `TOKEN_MIN_LEN..=TOKEN_MAX_LEN`.
pub fn parse_new_token(raw: &str) -> Result<Option<Token>, TokenError> {
    let t = raw.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(TokenError::BadChars);
    }
    if t.len() < TOKEN_MIN_LEN {
        return Err(TokenError::TooShort);
    }
    if t.len() > TOKEN_MAX_LEN {
        return Err(TokenError::TooLong);
    }
    Ok(Some(Token(t.to_string())))
}

/// Serialises as the raw string — ONLY so `SettingsView` can carry it to
/// the settings window. `Debug` stays `Token(***)`.
impl Serialize for Token {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
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

    // Design #377b t90: the boundary rule for a USER-entered token —
    // trim; "" ⇒ Ok(None) ("keep / generate"); charset
    // [A-Za-z0-9._-] checked FIRST; then 16..=256 (Q2: the 16 minimum
    // applies to loopback too).
    #[test]
    fn t90_parse_new_token_trims_and_enforces_charset_and_lengths() {
        // Empty after trim ⇒ "keep the current token, or generate one".
        assert_eq!(parse_new_token(""), Ok(None));
        assert_eq!(parse_new_token("  \n"), Ok(None));

        // Surrounding whitespace (a pasted newline) is trimmed first.
        let sixteen = "a".repeat(16);
        assert_eq!(
            parse_new_token(&format!("  {sixteen}\n")),
            Ok(Some(Token::parse(&sixteen).expect("16 chars parse")))
        );

        // Lengths, checked after the charset: 15 is too short, 257 is
        // too long, and the boundaries 16 / 256 are accepted.
        assert_eq!(parse_new_token(&"a".repeat(15)), Err(TokenError::TooShort));
        assert_eq!(
            parse_new_token(&"a".repeat(TOKEN_MIN_LEN)),
            Ok(Some(
                Token::parse(&"a".repeat(16)).expect("boundary 16 parses")
            ))
        );
        assert!(
            matches!(parse_new_token(&"a".repeat(256)), Ok(Some(_))),
            "the 256 boundary must be accepted"
        );
        assert_eq!(parse_new_token(&"a".repeat(257)), Err(TokenError::TooLong));

        // The charset is checked FIRST: a short string with a bad char
        // is BadChars, not TooShort.
        assert_eq!(parse_new_token("a b"), Err(TokenError::BadChars));
        assert_eq!(parse_new_token("ab\tc"), Err(TokenError::BadChars));
        // …and at the 16 length each of the reported bad chars fails:
        assert_eq!(
            parse_new_token(&format!("x~{}", "a".repeat(14))),
            Err(TokenError::BadChars),
            "~ must be rejected (it percent-encodes in the URL)"
        );
        assert_eq!(
            parse_new_token(&format!("x!{}", "a".repeat(14))),
            Err(TokenError::BadChars)
        );
        assert_eq!(
            parse_new_token(&format!("x*{}", "a".repeat(14))),
            Err(TokenError::BadChars)
        );
        assert_eq!(
            parse_new_token(&format!("é{}", "a".repeat(15))),
            Err(TokenError::BadChars),
            "non-ASCII must be rejected"
        );

        // The full charset [A-Za-z0-9._-] is accepted (6 + 10 = 16).
        assert!(
            matches!(parse_new_token("Ab9._-aaaaaaaaaa"), Ok(Some(_))),
            "letters, digits, dash, underscore and dot are all legal"
        );

        // Locked strings (asserted verbatim).
        assert_eq!(
            TokenError::BadChars.to_string(),
            "The access token may only contain letters, digits, dashes, underscores and dots."
        );
        assert_eq!(
            TokenError::TooShort.to_string(),
            "The access token must be at least 16 characters."
        );
        assert_eq!(
            TokenError::TooLong.to_string(),
            "The access token must be at most 256 characters."
        );
    }

    // Design #377b t91: Token serialises as the raw string so the
    // SettingsView can carry it to the settings window; Debug stays
    // masked (t02 must stay green).
    #[test]
    fn t91_token_serialises_as_the_raw_string() {
        let t = Token::parse("abc1234567890123").expect("parses");
        assert_eq!(
            serde_json::to_string(&t).expect("a Token serialises"),
            "\"abc1234567890123\""
        );
    }
}
