//! `paths::Roots` seam contract (mobile plan §2.6, task L2) — RED.
//!
//! RED today by COMPILE ERROR: `peakbot::paths` does not exist yet. The
//! builder (L2) must expose, in a new `src/paths.rs` — the ONLY module that
//! may call `dirs::` / use `ProjectDirs` (the lib test
//! `dirs_calls_appear_only_in_paths_module` locks that):
//!
//! ```ignore
//! pub mod paths {
//!     pub struct Roots { pub config: PathBuf, pub data: PathBuf, pub cache: PathBuf }
//!     pub fn init_roots(r: Roots); // boot-only OnceLock; unset = today's dirs:: paths
//! }
//! ```
//!
//! Own integration-test binary because `init_roots` is a process-global
//! OnceLock: one test function per process.
//!
//! Default-features build only (the Local-edition CI job runs `--lib`):
//! the TLS-dir assertion is edition-neutral, everything else is core.
//!
//! Every CURRENT `dirs::`/`ProjectDirs` call site and the public path fn
//! it backs. Sites with no public fn are covered by the text-scan guard
//! only and are listed here so L2 routes them too:
//!   config root : config::get_config_dir / get_config_file_path
//!                 (src/config/mod.rs, via directories_next::ProjectDirs)
//!   data root   : attachments::UploadStore::default_root
//!                 (src/attachments.rs: dirs::data_local_dir)
//!                 config::ConversationConfig::storage_dir_or_default
//!                 (src/config/file.rs: dirs::data_local_dir)
//!                 skills::discovery::get_default_skills_dirs global entry
//!                 (src/skills/discovery.rs: dirs::home_dir)
//!                 [private: ui/web/settings `facts().data_dir`
//!                 (src/ui/web/settings.rs: dirs::data_dir),
//!                 install/* service paths (src/install/{linux,macos,mod}.rs),
//!                 `~` expansion in src/attachments.rs, src/config/resolve.rs,
//!                 src/ui/web/images.rs]
//!   cache root  : ui::web::tls::default_tls_dir
//!                 (src/ui/web/tls.rs: dirs::cache_dir)
//!                 [private: mcp_auth::FilesystemCredentialStore::for_server
//!                 (src/mcp_auth.rs: dirs::cache_dir) — private module]
//!
//! (There is no log-file path in the core crate — the desktop shell logs
//! via tauri-plugin-log; nothing to route.)

use std::path::Path;

#[test]
fn every_resolved_path_lives_under_the_init_roots() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_root = tmp.path().join("config");
    let data_root = tmp.path().join("data");
    let cache_root = tmp.path().join("cache");
    peakbot::paths::init_roots(peakbot::paths::Roots {
        config: config_root.clone(),
        data: data_root.clone(),
        cache: cache_root.clone(),
    });

    let under = |path: &Path, root: &Path, what: &str| {
        assert!(
            path.starts_with(root),
            "{what} is {path:?}, not under the {root:?} root"
        );
    };

    // ── config root ──
    let config_dir = peakbot::config::get_config_dir().expect("config dir");
    under(&config_dir, &config_root, "config dir");
    let config_file = peakbot::config::get_config_file_path().expect("config file");
    under(&config_file, &config_root, "config file");

    // ── data root ──
    under(
        &peakbot::attachments::UploadStore::default_root(),
        &data_root,
        "uploads root",
    );
    let conversations = peakbot::config::ConversationConfig {
        auto_save: true,
        storage_dir: None,
    }
    .storage_dir_or_default();
    under(&conversations, &data_root, "conversations dir");
    // The global skills dir is the home-based entry (first); the session
    // entry stays cwd-relative by design.
    let skill_dirs = peakbot::skills::discovery::get_default_skills_dirs(Path::new("/work/repo"));
    under(&skill_dirs[0], &data_root, "global skills dir");

    // ── cache root ──
    let tls_dir = peakbot::ui::web::tls::default_tls_dir().expect("tls dir");
    under(&tls_dir, &cache_root, "TLS dir");
}
