//! TDD tests for the `peakbot vectors merge-driver` CLI grammar and
//! dispatch ordering.
//!
//! `Cli`/`Command` live in `src/main.rs` (a binary target), so integration
//! tests cannot import them. We therefore exercise the *real built binary*
//! (`CARGO_BIN_EXE_peakbot`) as a subprocess — which is exactly the locked
//! interface git invokes — and assert two things:
//!
//! 1. The `vectors merge-driver` subcommand is recognised (not an
//!    "unrecognized subcommand" clap error).
//! 2. It dispatches **before `Config::load()`**: run with an empty `HOME` /
//!    config directory it must NOT print the "Config not found" boot message.
//!    (`peakbot install` already works config-free for the same reason — see
//!    main.rs "Subcommand dispatch BEFORE Config::load".)
//!
//! These are RED before the subcommand exists: the binary rejects `vectors`
//! with `unrecognized subcommand 'vectors'`.
//!
//! No DBs are built here — the paths are intentionally junk so the driver,
//! once reached, fails on its own terms. The tests assert *which* failure we
//! get, which is the dispatch/parse signal under test.

use std::path::Path;
use std::process::Command;

/// Path to the built `peakbot` binary. `None` when the target was not built
/// (e.g. `--no-default-features`, which disables the host-only bin).
fn peakbot_bin() -> Option<&'static str> {
    option_env!("CARGO_BIN_EXE_peakbot")
}

/// A hermetic, empty home/config so a stray `Config::load` cannot find the
/// developer's real config. Returns the temp dir (kept alive by the caller).
fn empty_home() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp home");
    // Some config lookups honour these on Linux/macOS.
    dir
}

/// Run the binary with an isolated environment. `home` is used for
/// `HOME`/`XDG_CONFIG_HOME`/`XDG_DATA_HOME`.
fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(peakbot_bin().expect("peakbot binary built for tests"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        // Keep it deterministic and quiet.
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "error")
        .output()
        .expect("run peakbot binary")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// True when clap rejected the subcommand name itself (i.e. `vectors` is not a
/// known subcommand). This is the RED signal for this slice.
fn is_unrecognized_subcommand(stderr: &str) -> bool {
    stderr.contains("unrecognized subcommand") || stderr.contains("unexpected argument 'vectors'")
}

/// True when the boot layer printed its config-missing guidance. If this fires
/// for a `vectors` invocation, subcommand dispatch happened after config load —
/// a bug.
fn printed_config_missing(stderr: &str) -> bool {
    stderr.contains("Config not found")
        || stderr.contains("No config file found")
        || stderr.contains("config not found")
}

// ══════════════════════════════════════════════════════════════════════════
// The locked command must be recognised and dispatch before Config::load
// ══════════════════════════════════════════════════════════════════════════

/// `peakbot vectors merge-driver %O %A %B %P` must be a recognised subcommand
/// that dispatches before any config load. With junk paths it fails inside the
/// driver (not at clap, not at the config gate).
#[test]
fn vectors_merge_driver_is_recognized_and_dispatches_before_config_load() {
    let home = empty_home();
    // Junk, non-existent inputs: the driver will fail — but *after* dispatch.
    let out = run(
        home.path(),
        &[
            "vectors",
            "merge-driver",
            "/nonexistent/o.db",
            "/nonexistent/a.db",
            "/nonexistent/b.db",
            "/nonexistent/p.db",
        ],
    );
    let stderr = stderr_of(&out);

    assert!(
        !is_unrecognized_subcommand(&stderr),
        "`vectors merge-driver` must be a recognised subcommand; got stderr:\n{stderr}"
    );
    assert!(
        !printed_config_missing(&stderr),
        "the vectors subcommand must dispatch BEFORE Config::load() — it must not \
         require a config file; got stderr:\n{stderr}"
    );
}

/// The verb requires the four positional git arguments; omitting them is a
/// parse error that names `merge-driver`.
#[test]
fn vectors_merge_driver_requires_four_positional_args() {
    let home = empty_home();
    let out = run(home.path(), &["vectors", "merge-driver"]);
    let stderr = stderr_of(&out);

    assert!(
        !out.status.success(),
        "the driver must reject a call with no positional args"
    );
    assert!(
        stderr.contains("merge-driver"),
        "the error must name the `merge-driver` verb (proves the verb parsed and \
         its args were validated); got stderr:\n{stderr}"
    );
    assert!(
        !is_unrecognized_subcommand(&stderr),
        "`vectors merge-driver` must be recognised; got stderr:\n{stderr}"
    );
}

/// Exactly four positional args are accepted; a fifth is rejected.
#[test]
fn vectors_merge_driver_rejects_an_extra_positional_arg() {
    let home = empty_home();
    let out = run(
        home.path(),
        &[
            "vectors",
            "merge-driver",
            "/n/o.db",
            "/n/a.db",
            "/n/b.db",
            "/n/p.db",
            "extra",
        ],
    );
    let stderr = stderr_of(&out);

    assert!(
        !out.status.success(),
        "a fifth positional arg must be rejected"
    );
    assert!(
        stderr.contains("merge-driver"),
        "the extra-arg error must name the `merge-driver` verb; got stderr:\n{stderr}"
    );
    // clap's usage errors exit with code 2.
    assert_eq!(
        out.status.code(),
        Some(2),
        "an arity violation is a clap usage error (exit 2); got {:?}, stderr:\n{stderr}",
        out.status.code()
    );
}
