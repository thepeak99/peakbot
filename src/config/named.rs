//! `Named<T>` — ordered, duplicate-rejecting, name-validated map for the
//! 0.24 config layout (spec §4.2, §5.2).
//!
//! `serde_yaml` silently last-wins on duplicate mapping keys, so a plain
//! `HashMap` field would boot a config that discarded entries without a
//! peep. `Named` carries a manual map visitor that rejects the second
//! occurrence (N2) and any name outside the §4.2 rule (N1). Consumed by
//! `Config::parse` in the config cutover.

use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use serde::de::MapAccess;
use serde::de::Visitor;

/// Ordered name → value map with the §4.2 name rule enforced at parse:
/// `^[A-Za-z0-9_ .-]{1,64}$` and no leading/trailing whitespace. Duplicates
/// are a parse error (N2). Non-emptiness is NOT checked here (E2/E3 need
/// paths and live in `Config::parse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named<T>(Vec<(String, T)>);

impl<T> Default for Named<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Named<T> {
    /// Entries in declaration order (first = default, spec §1.2).
    pub fn iter(&self) -> impl Iterator<Item = &(String, T)> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&T> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// The first entry in declaration order — positional, not sorted.
    pub fn first(&self) -> Option<(&String, &T)> {
        self.0.first().map(|(k, v)| (k, v))
    }
}

/// §4.3 N1, verbatim (the "1–64" is an en dash, U+2013).
fn invalid_name_error(name: &str) -> String {
    format!(
        "name '{name}' must be 1–64 characters of letters, digits, space, '_', '.', '-' with no leading/trailing space"
    )
}

/// §4.2: `^[A-Za-z0-9_ .-]{1,64}$` plus "trimmed == raw". Plain char checks —
/// no regex dependency for a 64-character whitelist.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 64
        && name.trim() == name
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ' ' | '.' | '-'))
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Named<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NamedVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for NamedVisitor<T> {
            type Value = Named<T>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of name to entry")
            }

            // `next_entry` goes through `next_key`/`next_value` on the wrapped
            // MapAccess, so `serde_path_to_error` tracks each entry key as a
            // path segment for value-level errors (spec §13.2).
            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Named<T>, M::Error> {
                let mut entries: Vec<(String, T)> =
                    Vec::with_capacity(access.size_hint().unwrap_or(0));
                while let Some((name, value)) = access.next_entry::<String, T>()? {
                    if !valid_name(&name) {
                        return Err(serde::de::Error::custom(invalid_name_error(&name)));
                    }
                    // Generic N2: the resolve layer (A4) rewrites this to
                    // `duplicate profile/agent/subagent '<n>'` by path.
                    if entries.iter().any(|(k, _)| k == &name) {
                        return Err(serde::de::Error::custom(format!("duplicate name '{name}'")));
                    }
                    entries.push((name, value));
                }
                Ok(Named(entries))
            }
        }

        deserializer.deserialize_map(NamedVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §13.2 risk row: an error nested inside an entry must keep serde
    /// path fidelity — the entry key must appear as a path segment when the
    /// parse is wrapped in `serde_path_to_error`.
    #[test]
    fn nested_entry_error_keeps_entry_key_in_path() {
        let de = serde_yaml::Deserializer::from_str("dev: not-a-number\n");
        let result: Result<Named<i32>, serde_path_to_error::Error<serde_yaml::Error>> =
            serde_path_to_error::deserialize(de);
        let path = result.unwrap_err().path().to_string();
        assert!(
            path.contains("dev"),
            "the entry key must be a path segment, got: {path}"
        );
    }
}
