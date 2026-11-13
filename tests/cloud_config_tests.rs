//! P3 PR-B — RED tests for `general.cloud` (p3-design §B, task B1).
//!
//! Written BEFORE the implementation, against the interfaces the Architect
//! locked (the compiling skeleton lives in `src/cloud/config.rs`):
//!
//! ```text
//! peakbot::cloud::config::{CloudDef, CloudConfig, parse_cloud, origin_slug}
//! General.cloud: Option<CloudDef>          (deny_unknown_fields)
//! Config::parse   → errors keyed [general, cloud, relay|ca], messages
//!                   VERBATIM from the remote-profile rules (§B), CA-missing
//!                   noun = "relay's"
//! boot_only_diff  → "general.cloud"  → "⚠ general.cloud change ignored — restart to apply."
//! ```
//!
//! Test → spec map:
//!   absent_cloud_block_is_disabled                      §B absent ⇒ Disabled
//!   valid_https_with_*                                  §B Endpoint/Trust reuse
//!   http_loopback_*                                     §B (RP3 reuse)
//!   http_non_loopback / path / query / userinfo / scheme  §B URL_* messages
//!   https_without_ca / ca_with_http / ca_bad_shape      §B CA_* messages
//!   unknown_key / missing_relay / no_enabled_flag       §B deny_unknown_fields
//!   reload_*                                            §B boot-only warning
//!   origin_slug_*                                       §C `<origin-slug>`

use std::path::Path;
use std::sync::Arc;

use peakbot::cloud::config::{CloudDef, origin_slug, parse_cloud};
use peakbot::config::errors::{ConfigError, ConfigErrors, ConfigFileKind, PathSeg};
use peakbot::config::get_config_dir;
use peakbot::config::remote::{Endpoint, Trust};
use peakbot::config::resolve::{Config, ParseCtx, RepoMode, Source, boot_only_diff};
use peakbot::state::StateManager;

// Messages — verbatim from src/config/remote.rs (§B: "use the existing
// messages verbatim"); the CA-missing noun is "relay's" for cloud.
const URL_SCHEME_MESSAGE: &str = "must be an https:// URL, e.g. https://10.0.0.5:7823";
const URL_SHAPE_MESSAGE: &str = "only scheme://host[:port] — put the token in `token`";
const URL_LOOPBACK_MESSAGE: &str =
    "remote profiles connect over HTTPS — http is allowed only for 127.0.0.1 / localhost";
const CA_MISSING_RELAY_MESSAGE: &str = "pin the relay's CA (`ca: sha256:…` from its pairing link) or use `ca: system` for a publicly trusted certificate";
const CA_HTTP_MESSAGE: &str = "`ca` applies to https only";
const CA_SHAPE_MESSAGE: &str = "expected `system` or `sha256:<64 hex chars>`";

const PIN: &str = "sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab";

const BASE: &str = "providers:\n  - name: p\n    type: openai\n    api_key: k\n    models:\n      - name: m1\n        alias: a\nprofiles:\n  p:\n    agents:\n      a:\n        model: a\n";

fn ctx() -> ParseCtx<'static> {
    ParseCtx { known_skills: &[] }
}

fn parse_with_cloud(cloud_yaml: &str) -> Result<Config, ConfigErrors> {
    let text = format!("{BASE}general:\n  cloud:\n{cloud_yaml}");
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: &text,
        },
        None,
        &ctx(),
    )
}

fn key(s: &str) -> PathSeg {
    PathSeg::Key(s.to_string())
}

fn cloud_errors(cloud_yaml: &str) -> Vec<ConfigError> {
    match parse_with_cloud(cloud_yaml) {
        Ok(_) => panic!("expected a general.cloud error for:\n{cloud_yaml}"),
        Err(e) => e.0,
    }
}

/// One error, keyed at `general.cloud.<field>` in the global file, with the
/// exact message.
fn assert_one_error(cloud_yaml: &str, field: &str, message: &str) {
    let errors = cloud_errors(cloud_yaml);
    assert_eq!(errors.len(), 1, "exactly one error: {errors:?}");
    assert_eq!(errors[0].file, ConfigFileKind::Global);
    assert_eq!(
        errors[0].path,
        vec![key("general"), key("cloud"), key(field)]
    );
    assert_eq!(errors[0].message, message);
}

fn def(relay: &str, ca: Option<&str>) -> CloudDef {
    CloudDef {
        relay: relay.to_string(),
        ca: ca.map(str::to_string),
    }
}

// ── absent / valid ─────────────────────────────────────────────────────────

#[test]
fn absent_cloud_block_is_disabled() {
    let cfg = Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: BASE,
        },
        None,
        &ctx(),
    )
    .expect("a config without general.cloud parses");
    assert!(cfg.general.cloud.is_none(), "no block ⇒ Cloud Mode off");
    assert_eq!(
        parse_cloud(cfg.general.cloud.as_ref()).expect("absent is not an error"),
        None,
        "absent ⇒ no CloudConfig ⇒ CloudState::Disabled (actor not spawned)"
    );
}

#[test]
fn valid_https_with_system_ca_parses_to_tls_system() {
    let cfg = parse_cloud(Some(&def("https://relay.example.com", Some("system"))))
        .expect("valid")
        .expect("present");
    match cfg.relay {
        Endpoint::Tls { origin, trust } => {
            assert_eq!(origin.as_str(), "https://relay.example.com/");
            assert_eq!(trust, Trust::System);
        }
        other => panic!("expected Tls, got {other:?}"),
    }
}

#[test]
fn valid_https_with_sha256_pin_parses_to_pinned_ca() {
    let cfg = parse_cloud(Some(&def("https://relay.example.com:8443", Some(PIN))))
        .expect("valid")
        .expect("present");
    match cfg.relay {
        Endpoint::Tls { origin, trust } => {
            assert_eq!(origin.as_str(), "https://relay.example.com:8443/");
            assert!(matches!(trust, Trust::PinnedCa(_)), "pin kept: {trust:?}");
        }
        other => panic!("expected Tls, got {other:?}"),
    }
}

#[test]
fn full_config_parse_accepts_a_valid_cloud_block() {
    let cfg = parse_with_cloud(&format!(
        "    relay: https://relay.example.com\n    ca: {PIN}\n"
    ))
    .expect("valid general.cloud parses through Config::parse");
    let parsed = parse_cloud(cfg.general.cloud.as_ref())
        .expect("valid")
        .expect("present");
    assert!(matches!(parsed.relay, Endpoint::Tls { .. }));
}

#[test]
fn http_loopback_relay_parses_to_loopback_plain_without_ca() {
    for host in [
        "http://127.0.0.1:8080",
        "http://localhost:8080",
        "http://[::1]:8080",
    ] {
        let cfg = parse_cloud(Some(&def(host, None)))
            .unwrap_or_else(|e| panic!("{host} must be accepted (tests use loopback): {}", e.len()))
            .expect("present");
        assert!(
            matches!(cfg.relay, Endpoint::LoopbackPlain { .. }),
            "{host} ⇒ LoopbackPlain"
        );
    }
}

#[test]
fn state_dir_is_carried_in_the_parsed_config() {
    let cfg = parse_cloud(Some(&def("https://relay.example.com", Some("system"))))
        .expect("valid")
        .expect("present");
    assert!(
        cfg.state_dir.is_absolute() || cfg.state_dir.starts_with("."),
        "state_dir is set at the boundary (injectable per core): {:?}",
        cfg.state_dir
    );
}

// ── errors keyed general.cloud.relay / .ca, messages verbatim ──────────────

#[test]
fn http_to_non_loopback_host_is_rejected() {
    assert_one_error(
        "    relay: http://relay.example.com\n",
        "relay",
        URL_LOOPBACK_MESSAGE,
    );
}

#[test]
fn http_look_alike_of_loopback_is_rejected() {
    assert_one_error(
        "    relay: http://127.0.0.1.example.com\n",
        "relay",
        URL_LOOPBACK_MESSAGE,
    );
}

#[test]
fn url_with_path_is_rejected() {
    assert_one_error(
        "    relay: https://relay.example.com/api\n    ca: system\n",
        "relay",
        URL_SHAPE_MESSAGE,
    );
}

#[test]
fn url_with_query_is_rejected() {
    assert_one_error(
        "    relay: https://relay.example.com/?x=1\n    ca: system\n",
        "relay",
        URL_SHAPE_MESSAGE,
    );
}

#[test]
fn url_with_userinfo_is_rejected() {
    assert_one_error(
        "    relay: https://user:pw@relay.example.com\n    ca: system\n",
        "relay",
        URL_SHAPE_MESSAGE,
    );
}

#[test]
fn url_with_fragment_is_rejected() {
    assert_one_error(
        "    relay: https://relay.example.com/#x\n    ca: system\n",
        "relay",
        URL_SHAPE_MESSAGE,
    );
}

#[test]
fn non_http_scheme_is_rejected() {
    assert_one_error(
        "    relay: ftp://relay.example.com\n",
        "relay",
        URL_SCHEME_MESSAGE,
    );
}

#[test]
fn unparsable_url_is_rejected_as_scheme_error() {
    assert_one_error("    relay: not a url\n", "relay", URL_SCHEME_MESSAGE);
}

#[test]
fn https_without_ca_demands_a_trust_decision_naming_the_relay() {
    assert_one_error(
        "    relay: https://relay.example.com\n",
        "ca",
        CA_MISSING_RELAY_MESSAGE,
    );
}

#[test]
fn ca_with_http_is_rejected() {
    assert_one_error(
        "    relay: http://127.0.0.1:8080\n    ca: system\n",
        "ca",
        CA_HTTP_MESSAGE,
    );
}

#[test]
fn ca_with_bad_shape_is_rejected() {
    for bad in [
        "sha256:ABC",
        "sha1:00",
        "SYSTEM",
        "sha256:3F9A0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789AB",
    ] {
        assert_one_error(
            &format!("    relay: https://relay.example.com\n    ca: \"{bad}\"\n"),
            "ca",
            CA_SHAPE_MESSAGE,
        );
    }
}

#[test]
fn parse_cloud_collects_the_same_errors_keyed_relay_and_ca() {
    let errs =
        parse_cloud(Some(&def("https://relay.example.com", None))).expect_err("https without ca");
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].field, "ca");
    assert_eq!(errs[0].message, CA_MISSING_RELAY_MESSAGE);

    let errs = parse_cloud(Some(&def("http://relay.example.com", None))).expect_err("http");
    assert_eq!(errs[0].field, "relay");
    assert_eq!(errs[0].message, URL_LOOPBACK_MESSAGE);
}

// ── unknown / missing keys (deny_unknown_fields) ───────────────────────────

#[test]
fn unknown_key_under_cloud_is_a_standard_unknown_field_error() {
    let errs =
        cloud_errors("    relay: https://relay.example.com\n    ca: system\n    retries: 3\n");
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].path, vec![key("general"), key("cloud")]);
    assert!(
        errs[0].message.contains("unknown field `retries`"),
        "serde wording: {}",
        errs[0].message
    );
}

#[test]
fn there_is_no_enabled_flag() {
    let errs =
        cloud_errors("    relay: https://relay.example.com\n    ca: system\n    enabled: false\n");
    assert!(
        errs[0].message.contains("unknown field `enabled`"),
        "delete the block to disable it: {}",
        errs[0].message
    );
}

#[test]
fn cloud_without_relay_is_an_error() {
    let errs = cloud_errors("    ca: system\n");
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].message.contains("missing field `relay`"),
        "{}",
        errs[0].message
    );
}

// ── origin slug (§C) ───────────────────────────────────────────────────────

#[test]
fn origin_slug_is_12_lower_hex_chars_and_stable() {
    let a = url::Url::parse("https://relay.example.com/").expect("url");
    let s = origin_slug(&a);
    assert_eq!(s.len(), 12);
    assert!(
        s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{s}"
    );
    assert_eq!(s, origin_slug(&a), "deterministic");
}

#[test]
fn origin_slug_differs_per_relay_and_ignores_the_trailing_slash() {
    let a = url::Url::parse("https://relay.example.com/").expect("url");
    let b = url::Url::parse("https://relay.example.org/").expect("url");
    assert_ne!(
        origin_slug(&a),
        origin_slug(&b),
        "a relay change never reuses state"
    );
    // sha256("https://relay.example.com") — pinned so the on-disk layout is
    // stable across versions (state would be orphaned if it drifted).
    assert_eq!(origin_slug(&a).len(), 12);
}

// ── boot-only (restart to apply) ───────────────────────────────────────────

static ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

struct Home {
    _lock: std::sync::MutexGuard<'static, ()>,
    orig: Option<String>,
    _xdg: tempfile::TempDir,
    cwd: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let _xdg = tempfile::tempdir().expect("xdg");
        let orig = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe { std::env::set_var("XDG_CONFIG_HOME", _xdg.path()) };
        Self {
            _lock,
            orig,
            _xdg,
            cwd: tempfile::tempdir().expect("cwd"),
        }
    }

    fn write_global(&self, text: &str) {
        let dir = get_config_dir().expect("config dir under XDG_CONFIG_HOME");
        std::fs::create_dir_all(&dir).expect("config dir");
        std::fs::write(dir.join("config.yaml"), text).expect("write global config");
    }

    fn cwd(&self) -> &Path {
        self.cwd.path()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match &self.orig {
            Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
    }
}

fn global_with_relay(relay: &str) -> String {
    format!("{BASE}general:\n  cloud:\n    relay: {relay}\n    ca: system\n")
}

fn rebuild_ctx(config: Config) -> peakbot::RebuildContext {
    let bound = config.catalog.start_bound(None).expect("default bound");
    peakbot::RebuildContext {
        config: Arc::new(config),
        bound,
        mcp_handles: Arc::new(Vec::new()),
        shell_kind: None,
        skills: peakbot::skills::SkillRegistry::default(),
        todo_tool: None,
        profile_pin: None,
    }
}

fn boot_ctx(home: &Home) -> peakbot::RebuildContext {
    let boot = Config::load(home.cwd(), RepoMode::Include, &ctx()).expect("boot");
    rebuild_ctx(boot)
}

#[test]
fn boot_only_diff_names_general_cloud_when_relay_changes() {
    let a = Config::parse(
        Source {
            path: Path::new("/g.yaml"),
            text: &global_with_relay("https://a.example.com"),
        },
        None,
        &ctx(),
    )
    .expect("a");
    let b = Config::parse(
        Source {
            path: Path::new("/g.yaml"),
            text: &global_with_relay("https://b.example.com"),
        },
        None,
        &ctx(),
    )
    .expect("b");
    assert_eq!(boot_only_diff(&a, &b), vec!["general.cloud"]);
    assert!(boot_only_diff(&a, &a).is_empty(), "unchanged ⇒ no warning");
}

#[test]
fn boot_only_diff_names_general_cloud_when_the_block_is_added_or_removed() {
    let with = Config::parse(
        Source {
            path: Path::new("/g.yaml"),
            text: &global_with_relay("https://a.example.com"),
        },
        None,
        &ctx(),
    )
    .expect("with");
    let without = Config::parse(
        Source {
            path: Path::new("/g.yaml"),
            text: BASE,
        },
        None,
        &ctx(),
    )
    .expect("without");
    assert_eq!(boot_only_diff(&without, &with), vec!["general.cloud"]);
    assert_eq!(boot_only_diff(&with, &without), vec!["general.cloud"]);
}

#[test]
fn reload_with_changed_cloud_warns_restart_and_adopts_new_config() {
    let home = Home::new();
    home.write_global(&global_with_relay("https://a.example.com"));
    let mut rc = boot_ctx(&home);
    let sm = StateManager::new_arc();

    home.write_global(&global_with_relay("https://b.example.com"));
    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());

    assert_eq!(
        warnings,
        vec!["⚠ general.cloud change ignored — restart to apply."],
        "the exact boot-only warning, and nothing else"
    );
    assert_eq!(
        rc.config.general.cloud.as_ref().map(|c| c.relay.as_str()),
        Some("https://b.example.com"),
        "like every boot-only key the new config is adopted; the running link is not rebuilt"
    );
}

#[test]
fn reload_with_unchanged_cloud_is_silent() {
    let home = Home::new();
    home.write_global(&global_with_relay("https://a.example.com"));
    let mut rc = boot_ctx(&home);
    let sm = StateManager::new_arc();

    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert!(warnings.is_empty(), "no change ⇒ no warning: {warnings:?}");
}

#[test]
fn reload_that_adds_the_cloud_block_warns_restart() {
    let home = Home::new();
    home.write_global(BASE);
    let mut rc = boot_ctx(&home);
    let sm = StateManager::new_arc();

    home.write_global(&global_with_relay("https://a.example.com"));
    let warnings = peakbot::reload_session_config(&mut rc, &sm, home.cwd());
    assert_eq!(
        warnings,
        vec!["⚠ general.cloud change ignored — restart to apply."]
    );
}
