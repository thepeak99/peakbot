//! Path confinement for the Local edition's file tools.
//!
//! The six file tools hold a [`Workspace`] instead of a bare session cwd:
//! host builds `Workspace::Open` (today's `resolve_against`, no checks);
//! Local builds `Workspace::Confined`, which resolves every call against
//! the current conversation's upload folder:
//!
//! ```text
//! <uploads_root>/<convo>/      read boundary
//!     <upload-id>/<name>       attachments, read-only to the tools
//!     work/                    write boundary, base for relative paths
//! ```
//!
//! [`confine`] is the one check. Tools must use only the path it returns,
//! never the raw argument.

use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::attachments::WORK_DIR;
use crate::state::StateManager;

/// A path the tools may not touch, carrying the EXACT message the model
/// must see (design part 1 §3).
#[derive(Debug, thiserror::Error)]
pub enum PathRefused {
    #[error(
        "Path '{0}' is outside this conversation's folder. Use a relative path, or a path from the attached-files list."
    )]
    Outside(String),
    #[error(
        "Path '{0}' is read-only: attached files cannot be changed. Write to a relative path instead, for example 'notes.md'."
    )]
    ReadOnly(String),
    #[error("No active conversation, so there is no folder to read or write.")]
    NoConversation,
}

/// Where the file tools resolve paths. The confined root is read per call,
/// never snapshotted: `/new` after the agent was built must move it.
#[derive(Clone)]
pub enum Workspace {
    /// Session cwd, no checks — today's `file_edit::resolve_against`.
    /// Host-only: the Local edition cannot construct an unconfined tool.
    #[cfg(feature = "host")]
    Open(PathBuf),
    /// Confined to the conversation's upload folder; `None` = the session
    /// has no conversation folder (ShellBgTool precedent).
    Confined(Option<Arc<StateManager>>),
}

/// The no-state-manager fallback for `Tool::default()` call sites: host
/// keeps the historical empty-cwd behaviour; Local refuses every call
/// until a conversation exists.
impl Default for Workspace {
    fn default() -> Self {
        #[cfg(feature = "host")]
        {
            Workspace::Open(PathBuf::new())
        }
        #[cfg(not(feature = "host"))]
        {
            Workspace::Confined(None)
        }
    }
}

impl Workspace {
    /// Resolve `raw` for a READ: the whole conversation folder is allowed.
    /// Relative paths join onto `<convo>/work`.
    pub fn read(&self, raw: &str) -> Result<PathBuf, PathRefused> {
        match self {
            #[cfg(feature = "host")]
            Workspace::Open(cwd) => Ok(crate::tools::file_edit::resolve_against(cwd, raw)),
            Workspace::Confined(state) => {
                let (dir, work) = folders(state.as_deref())?;
                confine(&dir, &work, raw).ok_or_else(|| PathRefused::Outside(raw.to_string()))
            }
        }
    }

    /// Resolve `raw` for a WRITE: only `<convo>/work` is allowed. A path that
    /// is readable but not writable (an attachment) is read-only, not outside.
    pub fn write(&self, raw: &str) -> Result<PathBuf, PathRefused> {
        match self {
            #[cfg(feature = "host")]
            Workspace::Open(cwd) => Ok(crate::tools::file_edit::resolve_against(cwd, raw)),
            Workspace::Confined(state) => {
                let (dir, work) = folders(state.as_deref())?;
                // Both checks must pass, so a write is always also a legal
                // read, even if `work` itself were a symlink pointing out.
                match (confine(&dir, &work, raw), confine(&work, &work, raw)) {
                    (Some(_), Some(path)) => Ok(path),
                    (Some(_), None) => Err(PathRefused::ReadOnly(raw.to_string())),
                    (None, _) => Err(PathRefused::Outside(raw.to_string())),
                }
            }
        }
    }
}

/// The conversation folder and its `work/`, created here: with no upload yet
/// there is no folder, and a first write must not find its boundary missing.
fn folders(state: Option<&StateManager>) -> Result<(PathBuf, PathBuf), PathRefused> {
    let dir = state
        .and_then(StateManager::conversation_dir)
        .ok_or(PathRefused::NoConversation)?;
    let work = dir.join(WORK_DIR);
    if let Err(error) = std::fs::create_dir_all(&work) {
        // There is no IO variant; "no folder" is the true part of this one,
        // and it stops the model retrying with other paths.
        tracing::warn!(dir = %work.display(), %error, "cannot create the conversation work folder");
        return Err(PathRefused::NoConversation);
    }
    Ok((dir, work))
}

/// The one confinement check (design part 1 §3). `None` = refused; callers
/// must use only the returned path, never `raw`.
pub fn confine(boundary: &Path, base: &Path, raw: &str) -> Option<PathBuf> {
    // A blank name is no file, and a NUL would otherwise pass as a "new"
    // component and only fail later, inside the tool's IO.
    if raw.trim().is_empty() || raw.contains('\0') {
        return None;
    }
    // Canonical because the root can sit behind a symlink (on Android
    // `/data/user/0` is one). A boundary that does not exist refuses.
    let boundary = std::fs::canonicalize(boundary).ok()?;
    // `join` keeps an absolute `raw` as-is. Rebuilt from components to drop
    // a trailing `/` or `/.`: with one, the OS follows a symlink leaf, so a
    // dangling link would look missing and pass below as a new file.
    let joined: PathBuf = base.join(raw).components().collect();
    let existing = nearest_existing(&joined)?;
    let mut resolved = std::fs::canonicalize(existing).ok()?;
    for part in joined.strip_prefix(existing).ok()?.components() {
        // Only plain names past the last existing component: a `..` there
        // cannot be resolved against the disk, so it is not guessed at.
        let Component::Normal(name) = part else {
            return None;
        };
        // Pushed one by one: `join` of an empty remainder would add a
        // trailing separator, which makes opening a regular file fail.
        resolved.push(name);
    }
    // `Path::starts_with` compares whole components, never a string prefix.
    resolved.starts_with(&boundary).then_some(resolved)
}

/// The nearest ancestor of `path` that exists, `path` itself included.
fn nearest_existing(path: &Path) -> Option<&Path> {
    for candidate in path.ancestors() {
        // `symlink_metadata`, not `exists()`: a dangling symlink leaf must be
        // found here and fail `canonicalize`, not pass as a new file.
        match candidate.symlink_metadata() {
            Ok(_) => return Some(candidate),
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
            // Name too long, no permission: the entry may be a symlink we
            // could not see, and calling it "new" would let a tool follow it.
            Err(_) => return None,
        }
    }
    None
}

// ─── Test support ─────────────────────────────────────────────────────────

/// Shared by the file tools' unit tests: a workspace rooted at a fresh temp
/// folder the caller owns. Host is `Open(base)`; Local is the minted
/// conversation's `work/` — so a relative path lands in `base` in both
/// editions and the same tests run on both.
#[cfg(test)]
pub mod test_support {
    use super::Workspace;
    use std::path::PathBuf;

    pub fn rooted() -> (tempfile::TempDir, PathBuf, Workspace) {
        #[cfg(feature = "host")]
        {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().to_path_buf();
            (dir, base.clone(), Workspace::Open(base))
        }
        #[cfg(not(feature = "host"))]
        {
            let root = tempfile::tempdir().unwrap();
            let store = crate::attachments::UploadStore::new(
                root.path().to_path_buf(),
                crate::config::UploadsConfig::default(),
            );
            let sm = crate::state::StateManager::new_arc_for_session(None, store, None);
            sm.create_conversation("c".into(), "p".into(), "m".into(), String::new());
            let convo = sm.get_current_conversation_id().expect("minted");
            let base = root
                .path()
                .join(convo.to_string())
                .join(crate::attachments::WORK_DIR);
            std::fs::create_dir_all(&base).unwrap();
            (root, base, Workspace::Confined(Some(sm)))
        }
    }
}

// ─── RED tests: the confinement contract ──────────────────────────────────
//
// Written against the design's interface, before the implementation. The
// refuse-side `confine` tests pin the boundary (green on the refusing
// stub); the accept-side and message tests are RED until `confine` and
// `Workspace` are implemented. Symlink cases are unix-only.

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use uuid::Uuid;

    /// The exact model-facing messages (design part 1 §3) — pinned
    /// verbatim: the model self-corrects from them.
    const OUTSIDE_FMT: &str = "Path '{raw}' is outside this conversation's folder. Use a relative path, or a path from the attached-files list.";
    const READONLY_FMT: &str = "Path '{raw}' is read-only: attached files cannot be changed. Write to a relative path instead, for example 'notes.md'.";
    const NO_CONVO: &str = "No active conversation, so there is no folder to read or write.";

    fn outside_msg(raw: &str) -> String {
        OUTSIDE_FMT.replace("{raw}", raw)
    }
    fn readonly_msg(raw: &str) -> String {
        READONLY_FMT.replace("{raw}", raw)
    }

    fn expect_refused(boundary: &Path, base: &Path, raw: &str) {
        assert!(
            confine(boundary, base, raw).is_none(),
            "confine accepted {raw:?} against boundary {} base {} — escape",
            boundary.display(),
            base.display()
        );
    }

    fn expect_allowed(boundary: &Path, base: &Path, raw: &str, want: &Path) {
        let got = confine(boundary, base, raw).unwrap_or_else(|| {
            panic!(
                "confine refused {raw:?} against boundary {} base {}",
                boundary.display(),
                base.display()
            )
        });
        assert_eq!(
            got,
            want.to_path_buf(),
            "confine({raw:?}) resolved to {}, want {}",
            got.display(),
            want.display()
        );
    }

    /// Pure-`confine` layout: `root/{boundary, boundary/work, outside}`,
    /// with `boundary` and `outside` canonical (a temp dir may itself sit
    /// behind a symlink on some platforms). No state manager involved.
    fn layout() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempdir().unwrap();
        let boundary_raw = root.path().join("boundary");
        std::fs::create_dir_all(&boundary_raw).unwrap();
        let boundary = std::fs::canonicalize(&boundary_raw).unwrap();
        let work = boundary.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let outside_raw = root.path().join("outside");
        std::fs::create_dir_all(&outside_raw).unwrap();
        let outside = std::fs::canonicalize(&outside_raw).unwrap();
        (root, boundary, work, outside)
    }

    /// Workspace layout: a state manager whose CURRENT conversation's
    /// upload folder is `<root>/<convo>` (= the boundary). The folder is
    /// derived from the minted conversation id, so `conversation_dir()`
    /// (the stub these tests are RED against) can find it. `work/` is NOT
    /// pre-created — the design says `read` creates it.
    ///
    /// Returns (root guard, state manager, canonical boundary, work path).
    fn conversation_layout() -> (tempfile::TempDir, Arc<StateManager>, PathBuf, PathBuf) {
        let root = tempdir().unwrap();
        let store = crate::attachments::UploadStore::new(
            root.path().to_path_buf(),
            crate::config::UploadsConfig::default(),
        );
        let sm = StateManager::new_arc_for_session(None, store, None);
        sm.create_conversation(
            "test convo".into(),
            "prov".into(),
            "model".into(),
            String::new(),
        );
        let convo = sm.get_current_conversation_id().expect("minted");
        std::fs::create_dir_all(root.path().join(convo.to_string())).unwrap();
        let boundary = std::fs::canonicalize(root.path().join(convo.to_string())).unwrap();
        let work = boundary.join("work");
        (root, sm, boundary, work)
    }

    /// Like [`conversation_layout`], plus one stored attachment
    /// `<boundary>/<uuid>/<name>` (the read-only area).
    fn attachment_layout() -> (
        tempfile::TempDir,
        Arc<StateManager>,
        PathBuf,
        PathBuf,
        String, // uuid dir name
        String, // attachment file name
    ) {
        let (root, sm, boundary, work) = conversation_layout();
        let uuid = Uuid::new_v4().to_string();
        let name = "invoice.txt".to_string();
        let id_dir = boundary.join(&uuid);
        std::fs::create_dir_all(&id_dir).unwrap();
        std::fs::write(id_dir.join(&name), "attached bytes").unwrap();
        (root, sm, boundary, work, uuid, name)
    }

    // ── confine: escapes are refused ─────────────────────────────────────

    #[test]
    fn confine_refuses_dotdot_escapes_outside_the_boundary() {
        let (_root, boundary, work, outside) = layout();
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        // From base `<boundary>/work`, these climb to `<uploads_root>` or
        // higher — outside the conversation folder, whatever the names say.
        for raw in ["../../outside/secret.txt", "../..", "../../../etc/passwd"] {
            expect_refused(&boundary, &work, raw);
        }
        // `work/../../outside/secret.txt` would land at
        // `<boundary>/outside/secret.txt` — inside the read boundary — and
        // is refused by the fail-closed rule, not as an escape: a `..`
        // after a not-yet-existing component (`work/work`) is not resolved
        // against the disk.
        expect_refused(&boundary, &work, "work/../../outside/secret.txt");
    }

    #[test]
    fn confine_resolves_dotdot_that_stays_inside_the_read_boundary() {
        // A `..` that climbs to the conversation folder and stays inside
        // the READ boundary is legal — joined onto `<convo>/work`,
        // `../x` → `<convo>/x` and `../other/secret.txt` →
        // `<convo>/other/secret.txt`. (A WRITE to them is refused — see
        // `workspace_write_into_a_non_work_path_is_read_only_not_outside`.)
        let (_root, boundary, work, _outside) = layout();
        expect_allowed(&boundary, &work, "../x", &boundary.join("x"));
        expect_allowed(
            &boundary,
            &work,
            "../other/secret.txt",
            &boundary.join("other").join("secret.txt"),
        );
    }

    #[test]
    fn confine_refuses_dotdot_after_a_missing_component() {
        // The decided fail-closed rule: a `..` that comes AFTER a
        // not-yet-existing component cannot be resolved against the disk,
        // so it is refused — even though `a/../../x` would land inside the
        // conversation folder if `a` existed.
        let (_root, sm, boundary, work) = conversation_layout();
        expect_refused(&boundary, &work, "a/../../x");
        // Through the workspace the refusal carries the outside message.
        let ws = Workspace::Confined(Some(sm));
        let err = ws.read("a/../../x").unwrap_err();
        assert_eq!(
            err.to_string(),
            outside_msg("a/../../x"),
            "the refusal must carry the exact model-facing outside text"
        );
    }

    #[test]
    fn confine_refuses_absolute_path_outside_the_boundary() {
        let (_root, boundary, work, outside) = layout();
        std::fs::write(outside.join("secret.txt"), "x").unwrap();
        expect_refused(
            &boundary,
            &work,
            outside.join("secret.txt").to_str().unwrap(),
        );
    }

    #[test]
    fn confine_accepts_absolute_path_inside_the_boundary() {
        let (_root, boundary, work, _outside) = layout();
        let file = work.join("note.txt");
        std::fs::write(&file, "hi").unwrap();
        expect_allowed(
            &boundary,
            &work,
            file.to_str().unwrap(),
            &std::fs::canonicalize(&file).unwrap(),
        );
    }

    // ── confine: symlinks (unix) ─────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn confine_refuses_symlink_inside_the_boundary_pointing_outside() {
        let (_root, boundary, work, outside) = layout();
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(&outside, boundary.join("linked")).unwrap();
        // The model can name the link; the target must never resolve out.
        expect_refused(&boundary, &work, "../linked/secret.txt");
        expect_refused(&boundary, &work, "../linked");
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_create_through_a_symlinked_dir_pointing_outside() {
        let (_root, boundary, work, outside) = layout();
        std::os::unix::fs::symlink(&outside, work.join("escape")).unwrap();
        // A NEW file through the link would land in `outside` — refused.
        expect_refused(&boundary, &work, "escape/new.txt");
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_dangling_symlink_leaf() {
        let (_root, boundary, work, _outside) = layout();
        std::os::unix::fs::symlink(work.join("no-such-target"), work.join("dangling")).unwrap();
        // A dangling symlink is not a file: the nearest existing ancestor
        // is the link itself, whose canonicalization fails.
        expect_refused(&boundary, &work, "dangling");
        expect_refused(&boundary, &work, "dangling/sub.txt");
    }

    #[cfg(unix)]
    #[test]
    fn confine_accepts_inside_paths_when_the_boundary_itself_is_a_symlink() {
        let root = tempdir().unwrap();
        let real_raw = root.path().join("real");
        std::fs::create_dir_all(real_raw.join("work")).unwrap();
        let real = std::fs::canonicalize(&real_raw).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let link = std::fs::canonicalize(&link).unwrap(); // the boundary, via symlink
        // Inside paths must resolve against the REAL target (the design
        // calls this out for Android, where /data/user/0 is a symlink).
        expect_allowed(
            &link,
            &link.join("work"),
            "note.txt",
            &real.join("work").join("note.txt"),
        );
        expect_refused(&link, &link.join("work"), "../../outside.txt");
    }

    // ── confine: component-wise prefix, not string prefix ────────────────

    #[test]
    fn confine_refuses_sibling_dir_sharing_a_string_prefix() {
        // boundary `<root>/abc`, sibling `<root>/abcd`: a string
        // `starts_with` on the canonicalized paths would accept
        // `<root>/abcd/x` — component-wise comparison must not.
        let root = tempdir().unwrap();
        let boundary_raw = root.path().join("abc");
        std::fs::create_dir_all(boundary_raw.join("work")).unwrap();
        let boundary = std::fs::canonicalize(&boundary_raw).unwrap();
        let work = boundary.join("work");
        let sibling_file = root.path().join("abcd").join("x");
        std::fs::create_dir_all(sibling_file.parent().unwrap()).unwrap();
        std::fs::write(&sibling_file, "sibling").unwrap();
        let sibling_file = std::fs::canonicalize(&sibling_file).unwrap();
        assert!(
            sibling_file
                .to_string_lossy()
                .as_ref()
                .starts_with(boundary.to_string_lossy().as_ref()),
            "fixture must be a string-prefix sibling for this test to mean anything"
        );
        expect_refused(&boundary, &work, sibling_file.to_str().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_symlink_to_sibling_dir_sharing_a_string_prefix() {
        let root = tempdir().unwrap();
        let boundary_raw = root.path().join("abc");
        std::fs::create_dir_all(boundary_raw.join("work")).unwrap();
        let boundary = std::fs::canonicalize(&boundary_raw).unwrap();
        let work = boundary.join("work");
        let sibling = root.path().join("abcd");
        std::fs::create_dir_all(sibling.join("x")).unwrap();
        std::os::unix::fs::symlink(&sibling, boundary.join("abc2")).unwrap();
        expect_refused(&boundary, &work, "../abc2/x");
    }

    // ── confine: accepts ──────────────────────────────────────────────────

    #[test]
    fn confine_accepts_existing_file_inside() {
        let (_root, boundary, work, _outside) = layout();
        let file = work.join("note.txt");
        std::fs::write(&file, "hi").unwrap();
        expect_allowed(&boundary, &work, "note.txt", &file);
    }

    #[test]
    fn confine_accepts_not_yet_existing_file_inside() {
        let (_root, boundary, work, _outside) = layout();
        expect_allowed(&boundary, &work, "new.txt", &work.join("new.txt"));
    }

    #[test]
    fn confine_accepts_nested_not_yet_existing_path() {
        let (_root, boundary, work, _outside) = layout();
        expect_allowed(
            &boundary,
            &work,
            "a/b/c/deep.txt",
            &work.join("a").join("b").join("c").join("deep.txt"),
        );
    }

    #[test]
    fn confine_refuses_when_the_boundary_does_not_exist() {
        let root = tempdir().unwrap();
        let missing = root.path().join("never-created");
        assert!(!missing.exists());
        expect_refused(&missing, &missing.join("work"), "note.txt");
    }

    // ── Workspace: refusal messages the model sees ───────────────────────

    #[test]
    fn workspace_no_conversation_refuses_with_the_locked_message() {
        let ws = Workspace::Confined(None);
        assert_eq!(
            ws.read("note.txt").unwrap_err().to_string(),
            NO_CONVO,
            "read"
        );
        assert_eq!(
            ws.write("note.txt").unwrap_err().to_string(),
            NO_CONVO,
            "write"
        );
    }

    #[test]
    fn workspace_without_a_minted_conversation_refuses_no_conversation() {
        let root = tempdir().unwrap();
        let sm = state_manager_without_conversation(&root);
        let ws = Workspace::Confined(Some(sm));
        assert_eq!(
            ws.read("x.txt").unwrap_err().to_string(),
            NO_CONVO,
            "no conversation ⇒ no folder, for reads"
        );
        assert_eq!(
            ws.write("x.txt").unwrap_err().to_string(),
            NO_CONVO,
            "no conversation ⇒ no folder, for writes"
        );
    }

    #[test]
    fn workspace_read_outside_is_the_locked_outside_message() {
        let (root, sm, boundary, work) = conversation_layout();
        let outside_file = root.path().join("outside").join("secret.txt");
        std::fs::create_dir_all(outside_file.parent().unwrap()).unwrap();
        std::fs::write(&outside_file, "top secret").unwrap();
        let _ = (boundary, work);
        let ws = Workspace::Confined(Some(sm));
        let raw = "../../outside/secret.txt";
        let err = ws.read(raw).unwrap_err();
        assert_eq!(
            err.to_string(),
            outside_msg(raw),
            "the outside refusal must carry the exact model-facing text"
        );
    }

    #[test]
    fn workspace_write_outside_is_the_locked_outside_message() {
        let (root, sm, _boundary, _work) = conversation_layout();
        let outside_file = root.path().join("outside").join("secret.txt");
        std::fs::create_dir_all(outside_file.parent().unwrap()).unwrap();
        std::fs::write(&outside_file, "top secret").unwrap();
        let ws = Workspace::Confined(Some(sm));
        let raw = "../../outside/secret.txt";
        let err = ws.write(raw).unwrap_err();
        assert_eq!(
            err.to_string(),
            outside_msg(raw),
            "the outside refusal must carry the exact model-facing text"
        );
    }

    // ── Workspace: attachments are read-only, work/ is writable ──────────

    #[test]
    fn workspace_read_of_an_attachment_is_accepted() {
        let (_root, sm, boundary, _work, uuid, name) = attachment_layout();
        let ws = Workspace::Confined(Some(sm));
        let file = boundary.join(&uuid).join(&name);
        // The attachment is addressed by its absolute path (the form the
        // attached-files note gives the model) and as `../<uuid>/<name>` —
        // the relative form `<uuid>/<name>` would be a NEW file in work/.
        for raw in [
            file.to_str().unwrap().to_string(),
            format!("../{uuid}/{name}"),
        ] {
            let got = ws
                .read(&raw)
                .unwrap_or_else(|e| panic!("reading the attachment must be allowed: {e}"));
            assert_eq!(got, file, "read({raw:?})");
        }
    }

    #[test]
    fn workspace_write_into_an_attachment_dir_is_the_locked_read_only_message() {
        let (_root, sm, boundary, _work, uuid, name) = attachment_layout();
        let ws = Workspace::Confined(Some(sm));
        let file = boundary.join(&uuid).join(&name);
        // Overwriting the stored file, addressed by absolute path (the
        // attached-files note's form) and as `../<uuid>/<name>`…
        for raw in [
            file.to_str().unwrap().to_string(),
            format!("../{uuid}/{name}"),
        ] {
            let err = ws.write(&raw).unwrap_err();
            assert_eq!(
                err.to_string(),
                readonly_msg(&raw),
                "writing an attachment must be the read-only refusal (write({raw:?}))"
            );
        }
        // …and adding a file beside it (a `summary.md` next to an upload
        // would 404 the upload — `UploadStore::find` wants ONE entry) is
        // refused the same way.
        let raw2 = format!("../{uuid}/summary.md");
        let err2 = ws.write(&raw2).unwrap_err();
        assert_eq!(
            err2.to_string(),
            readonly_msg(&raw2),
            "writing beside an attachment must be the read-only refusal"
        );
    }

    #[test]
    fn workspace_write_into_a_non_work_path_is_read_only_not_outside() {
        // A `..` that climbs out of work/ but stays inside the
        // conversation folder: the read side accepts it, so the write side
        // must say READ-ONLY, not outside.
        let (_root, sm, _boundary, _work) = conversation_layout();
        let ws = Workspace::Confined(Some(sm));
        let raw = "../x";
        assert!(
            ws.read(raw).is_ok(),
            "the read side accepts ../x (inside the read boundary)"
        );
        let err = ws.write(raw).unwrap_err();
        assert_eq!(err.to_string(), readonly_msg(raw));
    }

    #[test]
    fn workspace_write_into_work_is_accepted_and_read_creates_work() {
        let (_root, sm, boundary, work) = conversation_layout();
        assert!(!work.exists(), "fixture: work/ is not pre-created");
        let ws = Workspace::Confined(Some(sm));
        let got = ws.read("notes.md").expect("reading work/ is allowed");
        assert_eq!(got, work.join("notes.md"));
        assert!(work.is_dir(), "read must create <convo>/work (design §3)");
        let got2 = ws.write("notes.md").expect("writing into work/ is allowed");
        assert_eq!(got2, work.join("notes.md"));
        let _ = boundary;
    }

    // ── Workspace::Open (host): identical to resolve_against ─────────────

    #[cfg(feature = "host")]
    #[test]
    fn workspace_open_resolves_exactly_like_resolve_against() {
        use crate::tools::file_edit::resolve_against;
        let cwd = tempdir().unwrap().path().to_path_buf();
        let ws = Workspace::Open(cwd.clone());
        for raw in [
            "/etc/hosts",
            "sub/file.txt",
            "../up.txt",
            "a/../../x",
            "notes.md",
        ] {
            assert_eq!(
                ws.read(raw).expect("Open never refuses"),
                resolve_against(&cwd, raw),
                "read({raw:?})"
            );
            assert_eq!(
                ws.write(raw).expect("Open never refuses"),
                resolve_against(&cwd, raw),
                "write({raw:?})"
            );
        }
    }

    /// A state manager with an upload store rooted at `root` and NO
    /// current conversation: `conversation_dir()` must be `None` there, so
    /// both `read` and `write` refuse with the no-conversation message.
    fn state_manager_without_conversation(root: &tempfile::TempDir) -> Arc<StateManager> {
        let store = crate::attachments::UploadStore::new(
            root.path().to_path_buf(),
            crate::config::UploadsConfig::default(),
        );
        let sm = StateManager::new_arc_for_session(None, store, None);
        assert!(
            sm.get_current_conversation_id().is_none(),
            "fixture: no conversation"
        );
        sm
    }

    // ── Hardening probes ─────────────────────────────────────────────────

    /// One stored attachment, `<convo>/<upload-id>/invoice.txt`.
    fn store_attachment(convo_dir: &Path) -> PathBuf {
        let id_dir = convo_dir.join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&id_dir).unwrap();
        let file = id_dir.join("invoice.txt");
        std::fs::write(&file, "attached bytes").unwrap();
        file
    }

    fn expect_read_and_write_outside(ws: &Workspace, raw: &str) {
        assert_eq!(
            ws.read(raw).unwrap_err().to_string(),
            outside_msg(raw),
            "read"
        );
        assert_eq!(
            ws.write(raw).unwrap_err().to_string(),
            outside_msg(raw),
            "write"
        );
    }

    #[test]
    fn confine_ignores_a_trailing_slash() {
        let (_root, boundary, work, _outside) = layout();
        std::fs::create_dir(work.join("sub")).unwrap();
        expect_allowed(&boundary, &work, "sub/", &work.join("sub"));
        expect_allowed(&boundary, &work, "new/", &work.join("new"));
        expect_refused(&boundary, &work, "../../outside/");
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_a_dangling_symlink_leaf_with_a_trailing_slash_or_dot() {
        // With a trailing `/` or `/.` the OS follows the link and reports it
        // missing; accepted as a "new file", a write would create the target.
        let (_root, boundary, work, outside) = layout();
        std::os::unix::fs::symlink(outside.join("planted.txt"), work.join("dangling")).unwrap();
        let absolute = format!("{}/dangling/", work.display());
        for raw in [
            "dangling/",
            "dangling//",
            "dangling/.",
            "./dangling/./",
            &absolute,
        ] {
            expect_refused(&boundary, &work, raw);
        }
    }

    #[test]
    fn confine_resolves_dot_segments() {
        let (_root, boundary, work, _outside) = layout();
        expect_allowed(&boundary, &work, "./notes.md", &work.join("notes.md"));
        expect_allowed(
            &boundary,
            &work,
            "a/./b/./c.txt",
            &work.join("a").join("b").join("c.txt"),
        );
        expect_allowed(&boundary, &work, ".", &work);
        expect_allowed(&boundary, &work, "./", &work);
        expect_allowed(&boundary, &work, "./..", &boundary);
        expect_refused(&boundary, &work, "./../..");
    }

    #[test]
    fn confine_keeps_a_path_below_a_regular_file_inside_the_boundary() {
        // Not creatable, but not an escape: the tool's own IO reports it.
        let (_root, boundary, work, _outside) = layout();
        std::fs::write(work.join("file.txt"), "hi").unwrap();
        expect_allowed(
            &boundary,
            &work,
            "file.txt/sub/x",
            &work.join("file.txt").join("sub").join("x"),
        );
        expect_refused(&boundary, &work, "file.txt/../../../outside/secret.txt");
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_a_path_below_a_symlink_to_an_outside_file() {
        let (_root, boundary, work, outside) = layout();
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), work.join("alias")).unwrap();
        expect_refused(&boundary, &work, "alias");
        expect_refused(&boundary, &work, "alias/sub/x");
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_a_symlink_loop() {
        let (_root, boundary, work, _outside) = layout();
        std::os::unix::fs::symlink(work.join("b"), work.join("a")).unwrap();
        std::os::unix::fs::symlink(work.join("a"), work.join("b")).unwrap();
        std::os::unix::fs::symlink("self", work.join("self")).unwrap();
        for raw in ["a", "a/x", "b/../a", "self", "self/x"] {
            expect_refused(&boundary, &work, raw);
        }
    }

    #[test]
    fn confine_accepts_unicode_and_long_legal_names() {
        let (_root, boundary, work, _outside) = layout();
        let dir = work.join("café ☕");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("日本語.txt"), "x").unwrap();
        expect_allowed(
            &boundary,
            &work,
            "café ☕/日本語.txt",
            &dir.join("日本語.txt"),
        );
        expect_allowed(
            &boundary,
            &work,
            "новая/заметка.md",
            &work.join("новая").join("заметка.md"),
        );
        let long = "n".repeat(200);
        expect_allowed(&boundary, &work, &long, &work.join(&long));
    }

    #[test]
    fn confine_refuses_names_too_long_to_stat_without_panicking() {
        // Past the OS limits `symlink_metadata` cannot say whether the entry
        // exists, so nothing is assumed about it.
        let (_root, boundary, work, _outside) = layout();
        let name = "n".repeat(5000);
        expect_refused(&boundary, &work, &name);
        expect_refused(
            &boundary,
            &work,
            &format!("{name}/../../../outside/secret.txt"),
        );
        expect_refused(&boundary, &work, &format!("{}leaf.txt", "d/".repeat(5000)));
        expect_refused(
            &boundary,
            &work,
            &format!("{}etc/passwd", "../".repeat(5000)),
        );
    }

    #[cfg(unix)]
    #[test]
    fn confine_refuses_a_symlink_hidden_behind_a_path_too_long_to_stat() {
        // `x/../` padding pushes the full path just past PATH_MAX while its
        // parent still fits: `lstat` fails on a link that does exist, and
        // treating that as "missing" would return `<work>/link` to follow.
        let (_root, boundary, work, outside) = layout();
        std::fs::create_dir(work.join("x")).unwrap();
        std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), work.join("link")).unwrap();
        // Each repeat adds 5 bytes, so a window around the limit must hit it
        // whatever the temp dir's length. 4096: Linux/Android; 1024: Apple.
        for path_max in [1024_usize, 4096] {
            let at_limit = path_max.saturating_sub(work.as_os_str().len()) / 5;
            for repeats in at_limit.saturating_sub(4)..=at_limit + 4 {
                let raw = format!("{}link", "x/../".repeat(repeats));
                expect_refused(&boundary, &work, &raw);
            }
        }
    }

    #[test]
    fn confine_refuses_blank_and_nul_input() {
        let (_root, boundary, work, _outside) = layout();
        for raw in [
            "",
            " ",
            "\t\n",
            "\0",
            "a\0b",
            "note.txt\0",
            "\0/../../outside",
        ] {
            expect_refused(&boundary, &work, raw);
        }
    }

    #[cfg(unix)]
    #[test]
    fn confine_accepts_a_boundary_named_through_a_symlink() {
        // The Android shape: the caller's boundary and the model's absolute
        // path both go through the link; the answer is the real path.
        let root = tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join("work")).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let real = std::fs::canonicalize(&real).unwrap();
        let via_link = link.join("work").join("note.txt");
        expect_allowed(
            &link,
            &link.join("work"),
            "note.txt",
            &real.join("work").join("note.txt"),
        );
        expect_allowed(
            &link,
            &link.join("work"),
            via_link.to_str().unwrap(),
            &real.join("work").join("note.txt"),
        );
        expect_refused(&link, &link.join("work"), "../../outside.txt");
    }

    #[test]
    fn workspace_relative_paths_resolve_under_work_not_the_conversation_folder() {
        let (_root, sm, boundary, work) = conversation_layout();
        let file = store_attachment(&boundary);
        let id = file.parent().unwrap().file_name().unwrap();
        let ws = Workspace::Confined(Some(sm));
        let raw = format!("{}/invoice.txt", id.to_str().unwrap());
        // Same name as the attachment, but relative: a new file in work/.
        assert_eq!(ws.read(&raw).unwrap(), work.join(id).join("invoice.txt"));
        assert_eq!(ws.write(&raw).unwrap(), work.join(id).join("invoice.txt"));
    }

    #[test]
    fn workspace_conversation_folder_itself_is_readable_but_read_only() {
        let (_root, sm, boundary, _work) = conversation_layout();
        let ws = Workspace::Confined(Some(sm));
        let raw = boundary.to_str().unwrap();
        assert_eq!(ws.read(raw).unwrap(), boundary);
        assert_eq!(ws.write(raw).unwrap_err().to_string(), readonly_msg(raw));
        assert_eq!(ws.read("..").unwrap(), boundary);
        assert_eq!(ws.write("..").unwrap_err().to_string(), readonly_msg(".."));
    }

    #[test]
    fn workspace_work_folder_named_by_absolute_path_is_accepted() {
        let (_root, sm, _boundary, work) = conversation_layout();
        let ws = Workspace::Confined(Some(sm));
        let raw = work.to_str().unwrap();
        assert_eq!(ws.read(raw).unwrap(), work);
        assert_eq!(ws.write(raw).unwrap(), work);
        let inside = work.join("notes.md");
        assert_eq!(ws.write(inside.to_str().unwrap()).unwrap(), inside);
    }

    #[test]
    fn workspace_refuses_a_sibling_conversations_folder() {
        let (root, sm, _boundary, _work) = conversation_layout();
        let other = Uuid::new_v4().to_string();
        let other_work = root.path().join(&other).join("work");
        std::fs::create_dir_all(&other_work).unwrap();
        std::fs::write(other_work.join("x"), "theirs").unwrap();
        let ws = Workspace::Confined(Some(sm));
        let absolute = other_work.join("x");
        for raw in [
            format!("../../{other}/work/x"),
            format!("../../{other}/work/new.txt"),
            format!("../../{other}"),
            absolute.to_str().unwrap().to_string(),
        ] {
            expect_read_and_write_outside(&ws, &raw);
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_write_through_a_symlink_to_an_attachment_is_read_only() {
        let (_root, sm, boundary, work) = conversation_layout();
        let file = store_attachment(&boundary);
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(&file, work.join("alias.txt")).unwrap();
        std::os::unix::fs::symlink(file.parent().unwrap(), work.join("alias-dir")).unwrap();
        let ws = Workspace::Confined(Some(sm));
        assert_eq!(ws.read("alias.txt").unwrap(), file);
        for raw in [
            "alias.txt",
            "alias-dir",
            "alias-dir/invoice.txt",
            "alias-dir/summary.md",
        ] {
            assert_eq!(
                ws.write(raw).unwrap_err().to_string(),
                readonly_msg(raw),
                "write({raw:?})"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_refuses_relative_paths_when_work_is_a_symlink_pointing_out() {
        let (root, sm, _boundary, work) = conversation_layout();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, &work).unwrap();
        let ws = Workspace::Confined(Some(sm));
        for raw in ["notes.md", ".", "a/b.txt"] {
            expect_read_and_write_outside(&ws, raw);
        }
    }

    #[test]
    fn workspace_refuses_when_work_cannot_be_created() {
        let (_root, sm, _boundary, work) = conversation_layout();
        std::fs::write(&work, "a file where the folder should be").unwrap();
        let ws = Workspace::Confined(Some(sm));
        assert!(ws.read("notes.md").is_err());
        assert!(ws.write("notes.md").is_err());
    }

    #[test]
    fn workspace_refuses_blank_and_nul_input_as_outside() {
        let (_root, sm, _boundary, _work) = conversation_layout();
        let ws = Workspace::Confined(Some(sm));
        for raw in ["", "   ", "\t\n", "a\0b", "\0"] {
            expect_read_and_write_outside(&ws, raw);
        }
    }

    #[test]
    fn workspace_follows_a_conversation_minted_after_it_was_built() {
        let (root, sm, _boundary, work) = conversation_layout();
        let ws = Workspace::Confined(Some(sm.clone()));
        let first = work.join("notes.md");
        assert_eq!(ws.write("notes.md").unwrap(), first);

        // `/new` after the tools were built: no folder exists for it yet.
        sm.create_conversation("next".into(), "prov".into(), "model".into(), String::new());
        let next = sm.get_current_conversation_id().expect("minted");
        let next_work = std::fs::canonicalize(root.path())
            .unwrap()
            .join(next.to_string())
            .join("work");
        assert_ne!(next_work, work, "fixture: a second conversation");
        assert_eq!(ws.write("notes.md").unwrap(), next_work.join("notes.md"));
        // The previous conversation is now a sibling like any other.
        expect_read_and_write_outside(&ws, first.to_str().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn workspace_works_when_the_uploads_root_sits_behind_a_symlink() {
        // Android: the app's data dir is reached through `/data/user/0`.
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let store = crate::attachments::UploadStore::new(
            link.clone(),
            crate::config::UploadsConfig::default(),
        );
        let sm = StateManager::new_arc_for_session(None, store, None);
        sm.create_conversation("c".into(), "prov".into(), "model".into(), String::new());
        let convo = sm
            .get_current_conversation_id()
            .expect("minted")
            .to_string();
        let real_dir = std::fs::canonicalize(&real).unwrap().join(&convo);
        let ws = Workspace::Confined(Some(sm));

        assert_eq!(
            ws.write("notes.md").unwrap(),
            real_dir.join("work").join("notes.md")
        );
        // The attached-files list names the upload through the link.
        let via_link = store_attachment(&link.join(&convo));
        let raw = via_link.to_str().unwrap();
        assert_eq!(
            ws.read(raw).unwrap(),
            std::fs::canonicalize(&via_link).unwrap()
        );
        assert_eq!(ws.write(raw).unwrap_err().to_string(), readonly_msg(raw));
    }

    #[cfg(feature = "host")]
    #[test]
    fn workspace_open_passes_blank_and_nul_input_through_like_resolve_against() {
        use crate::tools::file_edit::resolve_against;
        let cwd = tempdir().unwrap().path().to_path_buf();
        let ws = Workspace::Open(cwd.clone());
        for raw in ["", "   ", "a\0b", "dir/", "./x"] {
            assert_eq!(
                ws.read(raw).unwrap(),
                resolve_against(&cwd, raw),
                "read({raw:?})"
            );
            assert_eq!(
                ws.write(raw).unwrap(),
                resolve_against(&cwd, raw),
                "write({raw:?})"
            );
        }
    }
}
