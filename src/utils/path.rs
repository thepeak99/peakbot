//! Pure path helpers shared by config parsing, the vector store, the TUI's
//! `/cd` and the web cwd picker.

use std::path::{Path, PathBuf};

/// Lexically normalize a path: drop `.` components, pop on `..` (never past
/// root for absolute paths). No filesystem access — the DB file may not exist.
pub fn normalize_lexical(p: &Path) -> PathBuf {
    let mut components = p.components().peekable();
    let mut result = PathBuf::new();

    // Preserve absolute prefix
    if let Some(first) = components.peek() {
        use std::path::Component;
        if let Component::RootDir = first {
            result.push("/");
            components.next();
        }
    }

    for component in components {
        if component.as_os_str() == "." {
            continue;
        }
        if component.as_os_str() == ".." {
            // Never pop past the root for absolute paths
            if result.components().count() > 1 || !p.is_absolute() {
                result.pop();
            }
        } else {
            result.push(component);
        }
    }

    result
}

/// Resolve a `/cd` argument to a canonical, absolute directory path.
///
/// Expands a leading `~`/`~/` to `$HOME`, resolves relative paths against
/// the current cwd, then canonicalises. Returns a human-readable error
/// (no filesystem mutation) when the path doesn't exist or isn't a
/// directory — the caller surfaces it as a system message and never
/// dispatches the switch. Pure enough to unit-test against a temp dir.
pub fn resolve_cd_path(arg: &str) -> Result<String, String> {
    let arg = arg.trim();
    let expanded: std::path::PathBuf = if arg == "~" {
        std::path::PathBuf::from(std::env::var("HOME").map_err(|_| "$HOME is not set".to_string())?)
    } else if let Some(rest) = arg.strip_prefix("~/") {
        let home = std::env::var("HOME").map_err(|_| "$HOME is not set".to_string())?;
        std::path::PathBuf::from(home).join(rest)
    } else {
        std::path::PathBuf::from(arg)
    };
    // `dunce::canonicalize` (not `std::path::Path::canonicalize`): resolves
    // relative paths against the process cwd and fails on a non-existent
    // path — the "clear error before any mutation" guarantee we want — and
    // on Windows strips the verbatim `\\?\` prefix that std's canonicalize
    // returns. A verbatim path leaking out here would surface in the web cwd
    // picker and make client-side path composition fail (os error 123).
    let canonical =
        dunce::canonicalize(&expanded).map_err(|e| format!("cannot resolve `{arg}`: {e}"))?;
    if !canonical.is_dir() {
        return Err(format!("not a directory: {}", canonical.display()));
    }
    Ok(canonical.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_lexical_removes_current_directory_components() {
        assert_eq!(normalize_lexical(Path::new("./a/b")), PathBuf::from("a/b"));
    }

    #[test]
    fn normalize_lexical_resolves_parent_components() {
        assert_eq!(normalize_lexical(Path::new("a/../b")), PathBuf::from("b"));
    }

    #[test]
    fn normalize_lexical_preserves_absolute_paths() {
        assert_eq!(normalize_lexical(Path::new("/a/../b")), PathBuf::from("/b"));
    }

    #[test]
    fn resolve_cd_path_rejects_nonexistent() {
        assert!(resolve_cd_path("/no/such/dir/anywhere/xyz").is_err());
    }

    #[test]
    fn resolve_cd_path_canonicalises_existing_dir() {
        let tmp = std::env::temp_dir();
        let expected = tmp.canonicalize().unwrap().to_string_lossy().into_owned();
        let got = resolve_cd_path(&tmp.to_string_lossy()).expect("temp dir resolves");
        assert_eq!(got, expected);
    }

    #[test]
    fn resolve_cd_path_expands_tilde() {
        // `~` should expand to $HOME and resolve, when HOME is a real dir.
        if let Ok(home) = std::env::var("HOME")
            && std::path::Path::new(&home).is_dir()
        {
            let got = resolve_cd_path("~").expect("~ resolves");
            let expected = std::path::Path::new(&home)
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert_eq!(got, expected);
        }
    }

    // ── win-verbatim-paths ────────────────────────────────────────────────
    //
    // `std::path::canonicalize` on Windows returns a verbatim `\\?\C:\...`
    // path (required once the resolved length crosses MAX_PATH); the cwd
    // picker then displays `\\?\C:\Users\Docker` and child navigation fails
    // with "os error 123" because the client `/`-joins onto it. The
    // resolved path must NEVER carry the verbatim prefix —
    // `dunce::canonicalize` strips it.
    #[cfg(windows)]
    #[test]
    fn resolve_cd_path_never_returns_verbatim_prefix() {
        let tmp = std::env::temp_dir();
        let got = resolve_cd_path(&tmp.to_string_lossy()).expect("temp dir resolves");
        assert!(
            !got.starts_with(r"\\?\"),
            "resolved path must not carry the verbatim `\\\\?\\` prefix: {got}"
        );
    }
}
