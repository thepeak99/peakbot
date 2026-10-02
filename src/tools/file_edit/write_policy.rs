//! WRITE-path sandbox policy for `file_create` / `file_str_replace` /
//! `file_insert`. Reads are unrestricted in every mode and the shell is
//! never sandboxed — this module only ever gates a write.

use std::path::{Path, PathBuf};

use crate::config::SandboxConfig;

/// A resolved, ready-to-check write policy for one session. Built once (from
/// `SandboxConfig` + the session cwd) and consulted on every write; never
/// re-reads config or does I/O beyond what `check` itself performs.
#[derive(Debug, Clone, Default)]
pub enum WritePolicy {
    #[default]
    Unrestricted,
    Denied,
    /// Canonical roots a write must resolve under. `[0]` is always the
    /// session cwd — the architect's invariant, not an accident of
    /// construction order.
    Within(Vec<PathBuf>),
}

/// A write was refused by the sandbox. Carries the (non-canonical)
/// resolved path the caller asked to write, plus why.
#[derive(Debug, thiserror::Error)]
#[error("{}", self.message())]
pub struct SandboxDenied {
    pub path: PathBuf,
    pub reason: DenyReason,
}

#[derive(Debug)]
pub enum DenyReason {
    WritesDisabled,
    OutsideRoots {
        resolved: PathBuf,
        roots: Vec<PathBuf>,
    },
    Unresolvable(String),
}

impl SandboxDenied {
    /// Renders the 🔒 SANDBOX operator-facing message. Exact wording is the
    /// locked design's contract (see `write_policy_message_tests`).
    fn message(&self) -> String {
        match &self.reason {
            DenyReason::WritesDisabled => format!(
                "🔒 SANDBOX: write to '{}' denied — the sandbox is read-only (sandbox.mode: read-only).",
                self.path.display()
            ),
            DenyReason::OutsideRoots { resolved, roots } => {
                let roots_display = format!(
                    "[{}]",
                    roots
                        .iter()
                        .map(|r| r.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                format!(
                    "🔒 SANDBOX: write to '{}' denied — resolves to '{}', which is outside the \
                     workspace-write roots {roots_display}. Add it to sandbox.writable_roots if \
                     this write is intentional.",
                    self.path.display(),
                    resolved.display()
                )
            }
            DenyReason::Unresolvable(detail) => format!(
                "🔒 SANDBOX: write to '{}' denied — could not resolve the path ({detail}). \
                 Add the target to sandbox.writable_roots if this write is intentional.",
                self.path.display()
            ),
        }
    }
}

impl WritePolicy {
    /// Builds the policy for one session from the resolved `sandbox:` config
    /// and the session cwd. `Off` -> `Unrestricted`, `ReadOnly` -> `Denied`,
    /// `WorkspaceWrite` -> `Within` of the canonicalized (cwd ∪
    /// writable_roots), skipping any root that fails to canonicalize (warn,
    /// don't fail boot).
    pub fn new(config: &SandboxConfig, session_cwd: &Path) -> Self {
        match config {
            SandboxConfig::Off {} => WritePolicy::Unrestricted,
            SandboxConfig::ReadOnly {} => WritePolicy::Denied,
            SandboxConfig::WorkspaceWrite { writable_roots } => {
                let mut roots = Vec::with_capacity(1 + writable_roots.len());
                for candidate in
                    std::iter::once(session_cwd).chain(writable_roots.iter().map(|r| r.as_path()))
                {
                    match canonicalize_lenient(candidate) {
                        Ok(resolved) => roots.push(resolved),
                        Err(e) => tracing::warn!(
                            "sandbox.writable_roots: skipping '{}' — {e}",
                            candidate.display()
                        ),
                    }
                }
                WritePolicy::Within(roots)
            }
        }
    }

    /// Checks whether a write to `path` (already resolved against the
    /// session cwd, NOT yet canonicalized) is allowed.
    pub fn check(&self, path: &Path) -> Result<(), SandboxDenied> {
        match self {
            WritePolicy::Unrestricted => Ok(()),
            WritePolicy::Denied => Err(SandboxDenied {
                path: path.to_path_buf(),
                reason: DenyReason::WritesDisabled,
            }),
            WritePolicy::Within(roots) => {
                let resolved = canonicalize_lenient(path).map_err(|e| SandboxDenied {
                    path: path.to_path_buf(),
                    reason: DenyReason::Unresolvable(e.to_string()),
                })?;
                if roots.iter().any(|root| resolved.starts_with(root)) {
                    Ok(())
                } else {
                    Err(SandboxDenied {
                        path: path.to_path_buf(),
                        reason: DenyReason::OutsideRoots {
                            resolved,
                            roots: roots.clone(),
                        },
                    })
                }
            }
        }
    }
}

/// Resolves the longest existing prefix of `path` (via `symlink_metadata`,
/// so a dangling symlink counts as existing) and canonicalizes only that
/// prefix, then re-appends the remaining components. `..` after a
/// non-existent part fails (there is nothing on disk to resolve it against);
/// any non-`NotFound` lstat error fails closed.
pub(crate) fn canonicalize_lenient(path: &Path) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    use std::path::Component;

    let components: Vec<Component> = path.components().collect();

    // Find the longest existing prefix, walking from the full path down to
    // the root, using symlink_metadata (lstat) so a dangling symlink still
    // counts as "exists" at that component.
    let mut split = components.len();
    loop {
        let prefix: PathBuf = components[..split].iter().collect();
        if prefix.as_os_str().is_empty() {
            break;
        }
        match std::fs::symlink_metadata(&prefix) {
            Ok(_) => break,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                split -= 1;
                if split == 0 {
                    break;
                }
            }
            Err(e) => return Err(e),
        }
    }

    let existing_prefix: PathBuf = components[..split].iter().collect();
    let mut resolved = if existing_prefix.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        existing_prefix.canonicalize()?
    };

    for component in &components[split..] {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "'..' after a non-existent directory",
                ));
            }
            // RootDir/Prefix only ever appear in `components[..split]` (the
            // existing-prefix walk starts from the full path), never here.
            Component::RootDir | Component::Prefix(_) => {}
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Canonicalize the tempdir root itself before building expectations —
    /// on macOS `/tmp` is itself a symlink to `/private/tmp`, so a naive
    /// `dir.path()` and the lenient-canonicalized result would differ for
    /// reasons that have nothing to do with the behavior under test.
    fn real(p: &Path) -> PathBuf {
        p.canonicalize().expect("tempdir must exist")
    }

    // ── contract 11: Off (Unrestricted) never touches the filesystem ────

    #[test]
    fn unrestricted_allows_write_to_arbitrary_absolute_path() {
        let policy = WritePolicy::Unrestricted;
        assert!(policy.check(Path::new("/etc/x")).is_ok());
    }

    #[test]
    fn unrestricted_allows_write_through_dangling_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("nonexistent-target");
        let link = dir.path().join("dangling-link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        #[cfg(unix)]
        {
            let policy = WritePolicy::Unrestricted;
            assert!(policy.check(&link).is_ok());
        }
    }

    #[test]
    fn unrestricted_allows_path_with_dotdot_escaping_twice() {
        let policy = WritePolicy::Unrestricted;
        assert!(policy.check(Path::new("a/../../x")).is_ok());
    }

    // ── contract 12: ReadOnly (Denied) ───────────────────────────────────

    #[test]
    fn denied_rejects_write_inside_cwd_with_writes_disabled_reason() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("inside.txt");
        let policy = WritePolicy::Denied;
        let err = policy
            .check(&path)
            .expect_err("read-only must deny every write");
        assert!(matches!(err.reason, DenyReason::WritesDisabled));
    }

    // ── contract 13: WorkspaceWrite — relative/absolute in/out, siblings ──

    #[test]
    fn workspace_write_allows_relative_path_inside_cwd() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let target = cwd.join("sub/file.txt");
        let policy = WritePolicy::Within(vec![cwd]);
        assert!(policy.check(&target).is_ok());
    }

    #[test]
    fn workspace_write_allows_absolute_path_inside_cwd() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let target = cwd.join("file.txt");
        let policy = WritePolicy::Within(vec![cwd]);
        assert!(policy.check(&target).is_ok());
    }

    #[test]
    fn workspace_write_denies_absolute_path_outside_roots() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let outside = PathBuf::from("/etc/passwd");
        let policy = WritePolicy::Within(vec![cwd.clone()]);
        let err = policy
            .check(&outside)
            .expect_err("outside root must be denied");
        match err.reason {
            DenyReason::OutsideRoots { roots, .. } => assert_eq!(roots, vec![cwd]),
            other => panic!("expected OutsideRoots, got {other:?}"),
        }
    }

    #[test]
    fn workspace_write_denies_shared_prefix_sibling_directory() {
        // `/w2` must not be considered "under" `/w` just because it shares a
        // string prefix — the check must be component-wise.
        let parent = tempdir().unwrap();
        let cwd = real(parent.path()).join("w");
        let sibling = real(parent.path()).join("w2");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let target = sibling.join("file.txt");
        let policy = WritePolicy::Within(vec![cwd]);
        let err = policy
            .check(&target)
            .expect_err("sibling directory with shared string prefix must be denied");
        assert!(matches!(err.reason, DenyReason::OutsideRoots { .. }));
    }

    // ── contract 14: `..` traversal ───────────────────────────────────────

    #[test]
    fn workspace_write_denies_dotdot_escaping_above_cwd() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        let escaping = cwd.join("sub/../../x");
        let policy = WritePolicy::Within(vec![cwd]);
        let err = policy
            .check(&escaping)
            .expect_err("`../..` must escape the root and be denied");
        assert!(matches!(err.reason, DenyReason::OutsideRoots { .. }));
    }

    #[test]
    fn workspace_write_allows_dotdot_that_stays_inside_cwd() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        std::fs::create_dir_all(cwd.join("a")).unwrap();
        let inside = cwd.join("a/../b");
        let policy = WritePolicy::Within(vec![cwd]);
        assert!(policy.check(&inside).is_ok());
    }

    // ── contract 15: symlinks ─────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn workspace_write_denies_write_through_symlink_pointing_outside_cwd() {
        let outside = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let cwd = real(workspace.path());
        let link = cwd.join("escape-link");
        std::os::unix::fs::symlink(real(outside.path()), &link).unwrap();
        let target = link.join("file.txt");
        let policy = WritePolicy::Within(vec![cwd]);
        let err = policy
            .check(&target)
            .expect_err("a symlink resolving outside the roots must be denied");
        assert!(matches!(err.reason, DenyReason::OutsideRoots { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_write_denies_nonexistent_file_under_symlinked_outside_dir() {
        let outside = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        let cwd = real(workspace.path());
        let link = cwd.join("escape-link");
        std::os::unix::fs::symlink(real(outside.path()), &link).unwrap();
        let target = link.join("does-not-exist-yet.txt");
        let policy = WritePolicy::Within(vec![cwd]);
        let err = policy
            .check(&target)
            .expect_err("nonexistent file under an escaping symlink must still be denied");
        assert!(matches!(err.reason, DenyReason::OutsideRoots { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn workspace_write_dangling_symlink_inside_cwd_pointing_outside_is_unresolvable() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let link = cwd.join("dangling");
        std::os::unix::fs::symlink("/definitely/does/not/exist/anywhere", &link).unwrap();
        let policy = WritePolicy::Within(vec![cwd]);
        let err = policy
            .check(&link)
            .expect_err("a dangling symlink must fail closed, not pass");
        assert!(matches!(err.reason, DenyReason::Unresolvable(_)));
    }

    // ── contract 16: nonexistent paths / `..` after a nonexistent part ───

    #[test]
    fn workspace_write_allows_deeply_nonexistent_path_inside_cwd() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let target = cwd.join("new/dir/f");
        let policy = WritePolicy::Within(vec![cwd]);
        assert!(policy.check(&target).is_ok());
    }

    #[test]
    fn workspace_write_dotdot_after_nonexistent_component_is_unresolvable() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let target = cwd.join("missing/../../x");
        let policy = WritePolicy::Within(vec![cwd]);
        let err = policy
            .check(&target)
            .expect_err("`..` after a non-existent directory must not resolve");
        assert!(matches!(err.reason, DenyReason::Unresolvable(_)));
    }

    // ── contract 17: extra writable roots ────────────────────────────────

    #[test]
    fn workspace_write_allows_write_under_extra_writable_root() {
        let cwd_dir = tempdir().unwrap();
        let extra_dir = tempdir().unwrap();
        let cwd = real(cwd_dir.path());
        let extra = real(extra_dir.path());
        let target = extra.join("file.txt");
        let policy = WritePolicy::Within(vec![cwd, extra]);
        assert!(policy.check(&target).is_ok());
    }

    #[test]
    fn workspace_write_allows_write_under_root_whose_leaf_does_not_exist_yet() {
        let cwd_dir = tempdir().unwrap();
        let cwd = real(cwd_dir.path());
        // The configured root's own leaf directory doesn't exist yet, only
        // its parent — writable_roots need not exist ahead of time.
        let root = cwd.join("not-created-yet");
        let target = root.join("file.txt");
        let policy = WritePolicy::Within(vec![cwd, root]);
        assert!(policy.check(&target).is_ok());
    }

    // ── contract 18: Windows path consistency ────────────────────────────

    #[cfg(windows)]
    #[test]
    fn workspace_write_allows_relative_path_inside_cwd_windows() {
        let dir = tempdir().unwrap();
        let cwd = real(dir.path());
        let target = cwd.join("sub").join("file.txt");
        let policy = WritePolicy::Within(vec![cwd]);
        assert!(policy.check(&target).is_ok());
    }
}

/// Contract 19 — the error messages must carry the operator-facing
/// vocabulary the locked design specifies, verbatim enough to grep for.
#[cfg(test)]
mod write_policy_message_tests {
    use super::*;

    #[test]
    fn writes_disabled_message_names_the_path_and_mode_and_sandbox_lock_emoji() {
        let err = SandboxDenied {
            path: PathBuf::from("/tmp/x/file.txt"),
            reason: DenyReason::WritesDisabled,
        };
        let msg = err.to_string();
        assert!(msg.contains("🔒 SANDBOX"), "got: {msg}");
        assert!(msg.contains("/tmp/x/file.txt"), "got: {msg}");
        assert!(msg.contains("read-only"), "got: {msg}");
    }

    #[test]
    fn outside_roots_message_names_the_path_resolved_path_and_every_root() {
        let err = SandboxDenied {
            path: PathBuf::from("/ws/../etc/passwd"),
            reason: DenyReason::OutsideRoots {
                resolved: PathBuf::from("/etc/passwd"),
                roots: vec![PathBuf::from("/ws"), PathBuf::from("/extra")],
            },
        };
        let msg = err.to_string();
        assert!(msg.contains("🔒 SANDBOX"), "got: {msg}");
        assert!(msg.contains("/ws/../etc/passwd"), "got: {msg}");
        assert!(msg.contains("/etc/passwd"), "got: {msg}");
        assert!(msg.contains("workspace-write"), "got: {msg}");
        assert!(msg.contains("/ws"), "got: {msg}");
        assert!(msg.contains("/extra"), "got: {msg}");
        assert!(msg.contains("sandbox.writable_roots"), "got: {msg}");
    }

    #[test]
    fn unresolvable_message_names_the_path_and_the_detail() {
        let err = SandboxDenied {
            path: PathBuf::from("/ws/dangling"),
            reason: DenyReason::Unresolvable("dangling symlink".to_string()),
        };
        let msg = err.to_string();
        assert!(msg.contains("🔒 SANDBOX"), "got: {msg}");
        assert!(msg.contains("/ws/dangling"), "got: {msg}");
        assert!(msg.contains("dangling symlink"), "got: {msg}");
        assert!(msg.contains("sandbox.writable_roots"), "got: {msg}");
    }
}
