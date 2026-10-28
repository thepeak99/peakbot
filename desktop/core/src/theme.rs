//! The web UI's theme: the wire type of the `theme_set` / `theme_get`
//! commands.

use serde::{Deserialize, Serialize};

/// The web UI's light/dark theme, as reported over the `theme_set`
/// command and read back by the bundled pages via `theme_get`
/// (desktop-pane design §3). Lowercase wire form; the web page's value is
/// untrusted input, so nothing else deserializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
}

#[cfg(test)]
mod tests {
    use super::Theme;

    // Desktop-pane design §5 item 1: `Theme` is the wire type of the
    // `theme_set` / `theme_get` commands — lowercase "light" | "dark", and
    // nothing else deserializes (the web page's value is untrusted input).
    #[test]
    fn theme_serde() {
        // Round-trip both variants through their exact wire form.
        for (wire, theme) in [("light", Theme::Light), ("dark", Theme::Dark)] {
            let d: Theme = serde_json::from_str(&format!("\"{wire}\""))
                .unwrap_or_else(|e| panic!("{wire}: {e}"));
            assert_eq!(d, theme, "\"{wire}\" deserializes");
            assert_eq!(
                serde_json::to_string(&theme).expect("Theme serialises"),
                format!("\"{wire}\""),
                "{theme:?} serialises lowercase"
            );
        }

        // Anything else is refused: unknown name, wrong case, null, wrong
        // JSON types, empty string, padded string.
        for bad in [
            "\"blue\"",
            "\"Light\"",
            "\"DARK\"",
            "\"\"",
            "\" light\"",
            "null",
            "true",
            "0",
            "[]",
            "{}",
        ] {
            assert!(
                serde_json::from_str::<Theme>(bad).is_err(),
                "{bad} must not deserialize as a Theme"
            );
        }

        // `theme_get` returns Option<Theme>: null is the "not reported yet"
        // answer and must round-trip through Option, not through Theme.
        assert_eq!(
            serde_json::from_str::<Option<Theme>>("null").expect("null is None"),
            None
        );
        assert_eq!(
            serde_json::to_string(&Some(Theme::Dark)).expect("serialises"),
            "\"dark\""
        );
        assert_eq!(
            serde_json::to_string(&None::<Theme>).expect("serialises"),
            "null"
        );
    }
}
