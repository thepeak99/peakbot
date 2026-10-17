//! Host fontconfig tests for the DESIGN §4.12 emoji-isolation config.
//!
//! The AppImage's bundled WebKit/Skia crashes on the host's COLRv1 emoji
//! fonts (FreeType ABI mismatch). The generated config includes the host
//! config, adds the bundled CBDT emoji dir, and rejects fonts with
//! `color=true` AND `outline=true`. These tests prove that against a real
//! host fontconfig (`fc-match`/`fc-list`).
//!
//! Every test is `#[ignore]`: they need a host with fontconfig installed
//! and the bundled font. Run them via `make desktop-fonttest`, which
//! fetches `NotoColorEmoji.ttf` into `desktop/src-tauri/linux/fonts` and
//! exports `PEAKBOT_EMOJI_FONT_DIR`. A missing env var is a hard panic —
//! never a silent skip.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use peakbot_desktop_core::fontconfig::{EMOJI_FONT_FILE, SYSTEM_FONTS_CONF, render_fonts_conf};

/// The directory holding `NotoColorEmoji.ttf` (set by `make desktop-fonttest`).
/// Missing/empty ⇒ hard panic, no silent skip.
fn font_dir() -> PathBuf {
    match std::env::var_os("PEAKBOT_EMOJI_FONT_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => panic!(
            "PEAKBOT_EMOJI_FONT_DIR is not set — run `make desktop-fonttest` \
             (it fetches NotoColorEmoji.ttf into desktop/src-tauri/linux/fonts \
             and exports the variable)"
        ),
    }
}

/// Per-test sandbox: a temp dir holding `fonts.conf` (rendered by
/// `render_fonts_conf`) plus a fresh `XDG_CACHE_HOME`, so no test reads
/// or writes the host's font cache.
struct Host {
    dir: tempfile::TempDir,
    /// `tmp/fonts.conf` — the generated config under test.
    conf: PathBuf,
    /// `tmp/cache` — XDG_CACHE_HOME for every fc-* invocation.
    cache: PathBuf,
    /// `font_dir/NotoColorEmoji.ttf` — the bundled font the config must
    /// surface.
    bundled: PathBuf,
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let bundled = font_dir().join(EMOJI_FONT_FILE);
        assert!(
            bundled.is_file(),
            "bundled emoji font not found at {bundled:?} — run `make desktop-fonttest`"
        );
        let conf = dir.path().join("fonts.conf");
        let xml = render_fonts_conf(
            font_dir()
                .to_str()
                .expect("PEAKBOT_EMOJI_FONT_DIR must be valid UTF-8"),
        );
        std::fs::write(&conf, xml).expect("write fonts.conf");
        let cache = dir.path().join("cache");
        Self {
            dir,
            conf,
            cache,
            bundled,
        }
    }
}

/// Run `fc-match`/`fc-list`. `conf = Some` → `FONTCONFIG_FILE=conf`;
/// `None` → `FONTCONFIG_FILE` removed (the host baseline). `XDG_CACHE_HOME`
/// always points at the sandbox cache. Returns stdout; a non-zero exit or
/// a missing binary is a panic, never a silent pass.
fn run_fc(host: &Host, prog: &str, args: &[&str], conf: Option<&Path>) -> String {
    let mut cmd = Command::new(prog);
    cmd.args(args);
    match conf {
        Some(c) => {
            cmd.env("FONTCONFIG_FILE", c);
        }
        None => {
            cmd.env_remove("FONTCONFIG_FILE");
        }
    }
    cmd.env("XDG_CACHE_HOME", &host.cache);
    let out = cmd.output().unwrap_or_else(|e| {
        panic!(
            "{prog} did not start: {e} — these tests need fontconfig \
             (fc-match/fc-list) installed on the host"
        )
    });
    assert!(
        out.status.success(),
        "{prog} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `fc-list -f '%{file}\n'` → the UNIQUE set of font files.
///
/// Robustness: fc-list prints one line per font FACE, so a file with
/// several faces (or several families) prints several times — counting
/// raw lines would overcount. Dedupe on the file path before counting.
fn fc_files(host: &Host, pattern: Option<&str>, conf: Option<&Path>) -> Vec<String> {
    let mut args: Vec<&str> = vec!["-f", "%{file}\n"];
    if let Some(p) = pattern {
        args.push(p);
    }
    let out = run_fc(host, "fc-list", &args, conf);
    let mut files: Vec<String> = out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    files.sort();
    files.dedup();
    files
}

/// `fc-match -f '%{file}' <pattern>` under the generated config.
fn fc_match_file(host: &Host, pattern: &str, conf: Option<&Path>) -> String {
    let args = ["-f", "%{file}", pattern];
    run_fc(host, "fc-match", &args, conf).trim().to_string()
}

// h1: the bundled CBDT font must answer a family query for itself.
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h1_fc_match_noto_color_emoji_is_bundled() {
    let host = Host::new();
    let got = fc_match_file(&host, "Noto Color Emoji", Some(&host.conf));
    assert_eq!(
        got,
        host.bundled.display().to_string(),
        "fc-match 'Noto Color Emoji' must resolve to the bundled font"
    );
}

// h2: the generic "emoji" family must resolve to the bundled font
// (via the <alias binding="same"> preference).
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h2_fc_match_emoji_is_bundled() {
    let host = Host::new();
    let got = fc_match_file(&host, "emoji", Some(&host.conf));
    assert_eq!(
        got,
        host.bundled.display().to_string(),
        "fc-match 'emoji' must resolve to the bundled font"
    );
}

// h3: charset queries for real emoji code points (🌐 U+1F310, 🧩 U+1F9E9,
// 🎬 U+1F3AC) with color=true must each resolve to the bundled font.
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h3_fc_match_color_charset_is_bundled() {
    let host = Host::new();
    for cp in ["1f310", "1f9e9", "1f3ac"] {
        let pattern = format!(":color=true:charset={cp}");
        let got = fc_match_file(&host, &pattern, Some(&host.conf));
        assert_eq!(
            got,
            host.bundled.display().to_string(),
            "fc-match {pattern:?} must resolve to the bundled font"
        );
    }
}

// h4: NOTHING color+outline may survive the generated config — the host's
// COLRv1 emoji (the crash trigger) must be rejected.
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h4_fc_list_color_outline_is_empty() {
    let host = Host::new();
    let out = run_fc(
        &host,
        "fc-list",
        &["-f", "%{file}\n", ":color=true:outline=true"],
        Some(&host.conf),
    );
    assert!(
        out.trim().is_empty(),
        "no color+outline font may match under the generated config; got:\n{out}"
    );
}

// h5: no font in the generated config may carry "COLRv1" in its file
// name or family (case-insensitive) — the crash trigger is gone.
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h5_fc_list_has_no_colrv1() {
    let host = Host::new();
    let out = run_fc(&host, "fc-list", &[], Some(&host.conf));
    let offenders: Vec<&str> = out
        .lines()
        .filter(|l| l.to_lowercase().contains("colrv1"))
        .collect();
    assert!(
        offenders.is_empty(),
        "no COLRv1 font may be visible under the generated config; got:\n{}",
        offenders.join("\n")
    );
}

// h6: adding the emoji dir must not change non-emoji matching —
// sans-serif and monospace must resolve exactly as on the host baseline
// (FONTCONFIG_FILE removed).
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h6_fc_match_default_families_unchanged() {
    let host = Host::new();
    for family in ["sans-serif", "monospace"] {
        let with_conf = fc_match_file(&host, family, Some(&host.conf));
        let baseline = fc_match_file(&host, family, None);
        assert_eq!(
            with_conf, baseline,
            "fc-match {family:?} must be unchanged by the generated config"
        );
    }
}

// h7: the generated config must expose EXACTLY
//     host baseline − (host's color+outline fonts) + (the bundled emoji).
//
// Robustness: all three counts are UNIQUE file sets (`fc_files` dedupes,
// since fc-list prints one line per face). The baselines run with
// FONTCONFIG_FILE removed so they span the same host universe of fonts.
// The bundled CBDT font is color=true but NOT outline (bitmap), so it is
// the single addition; the rejectfont removes exactly the baseline's
// color+outline set.
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h7_fc_list_file_count_is_baseline_minus_rejected_plus_bundled() {
    let host = Host::new();
    let baseline = fc_files(&host, None, None);
    let rejected = fc_files(&host, Some(":color=true:outline=true"), None);
    let generated = fc_files(&host, None, Some(&host.conf));
    let expected = baseline.len() as i64 - rejected.len() as i64 + 1;
    assert_eq!(
        generated.len() as i64,
        expected,
        "generated config must list exactly baseline({}) − color+outline({}) + bundled(1) = {}; \
         got {} files:\n{}",
        baseline.len(),
        rejected.len(),
        expected,
        generated.len(),
        generated.join("\n")
    );
}

// h8: the config must also work WITHOUT the host config file (the
// include is ignore_missing): a bare <dir> set still matches
// sans-serif from /usr/share/fonts/, and the rejectfont still hides
// every color+outline font.
#[test]
#[ignore = "host fontconfig test — run via `make desktop-fonttest`"]
fn h8_config_works_without_host_conf() {
    let host = Host::new();
    let xml = std::fs::read_to_string(&host.conf).expect("read generated config");
    let isolated = xml.replace(SYSTEM_FONTS_CONF, "/nonexistent/fonts.conf");
    let conf = host.dir.path().join("isolated.conf");
    std::fs::write(&conf, isolated).expect("write isolated config");

    let got = fc_match_file(&host, "sans-serif", Some(&conf));
    assert!(
        !got.is_empty(),
        "sans-serif must still match with the host config missing"
    );
    assert!(
        got.starts_with("/usr/share/fonts/"),
        "sans-serif must come from /usr/share/fonts/ without the host config; got {got:?}"
    );

    let out = run_fc(
        &host,
        "fc-list",
        &["-f", "%{file}\n", ":color=true:outline=true"],
        Some(&conf),
    );
    assert!(
        out.trim().is_empty(),
        "no color+outline font may match without the host config either; got:\n{out}"
    );
}
