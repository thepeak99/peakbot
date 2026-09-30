//! RED — `Profile.mcp_servers` (CONTRACT A) and `Profile.agents_md`
//! (CONTRACT B) parse + validation contract, driven through the PUBLIC
//! boundary seams only (`serde_yaml::from_str::<Config>`, `Config::load`,
//! `Config::reload_for`, `Profile::overridden_fields`).
//!
//! This file compiles against the CURRENT API on purpose: every test fails
//! at RUNTIME today, for the right reason — the master config rejects the
//! unknown `mcp_servers` / `agents_md` keys under a profile
//! (`deny_unknown_fields`), or the load path never validates them. When the
//! fields land, the parse tests flip green and the validation tests start
//! exercising the real gate.
//!
//! Locked contract (mirrors the profile `pipelines:` gate exactly):
//!
//! * `mcp_servers:` is a `NameFilter` (`enabled` / `disabled` XOR `only`)
//!   over MCP SERVER NAMES — same semantics as `pipelines:`.
//! * Unknown key under it ⇒ parse error (NameFilter is `deny_unknown_fields`).
//! * `only` + `disabled` together ⇒ boot error naming `profiles.<p>.mcp_servers`.
//! * Names are validated against the MASTER config's `mcp_servers[].name`
//!   list, for the ACTIVE profile only (an unselected profile's bad filter
//!   is inert — the `pipelines:` rule). The error names the bad server and
//!   lists the known ones.
//! * `agents_md:` is a plain bool: `false` = do not inject agents.md,
//!   `true` = inject as usual, absent = unchanged. Non-bool ⇒ parse error.
//! * `overridden_fields()` reports each key that is set (boot-banner source).
//!
//! The EFFECT half of CONTRACT A (the agent receives only the allowed
//! servers' tools) is pinned at the pure seam `mcp_tools_for_profile` in
//! `src/lib.rs::tests`; the EFFECT half of CONTRACT B (the rebuilt system
//! prompt omits agents.md) is pinned in `src/lib.rs::tests`
//! (`system_prompt_*` + the `handle_select_profile` harness).

use peakbot::config::Config;
use std::sync::Mutex;

/// This binary is its own process — the crate-internal `CONFIG_ENV_LOCK` is
/// not reachable from here, so the env-mutating tests serialize on this
/// file-local lock instead (same discipline, same reason: `XDG_CONFIG_HOME`
/// is process-global).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Point `XDG_CONFIG_HOME` at a tempdir holding `master_yaml` for the
/// duration of `body`, with an empty repo tree as the session cwd. `body`
/// returns owned values so the assertions run OUTSIDE the lock — a panic
/// while holding it would poison the lock for the rest of the binary
/// (memory.md: find the FIRST real panic, never the PoisonError).
fn with_master_config<T>(master_yaml: &str, body: impl FnOnce(&std::path::Path) -> T) -> T {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let master_tmp = tempfile::tempdir().expect("master tmpdir");
    let repo_tmp = tempfile::tempdir().expect("repo tmpdir");
    let original = std::env::var("XDG_CONFIG_HOME").ok();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", master_tmp.path()) };
    let config_dir = peakbot::config::get_config_dir().expect("config dir under XDG_CONFIG_HOME");
    std::fs::create_dir_all(&config_dir).expect("mkdir config dir");
    std::fs::write(config_dir.join("config.yaml"), master_yaml).expect("write master config");
    let result = body(repo_tmp.path());
    unsafe {
        match original {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
    result
}

/// A master config with two SSE MCP servers (`alpha`, `beta`) and the
/// profile under test spliced in. SSE needs only a `url` — the servers are
/// never connected here, only name-validated.
fn master_with_two_servers(profile_yaml: &str) -> String {
    format!(
        "providers:
  - name: ollama
    type: ollama
    base_url: http://localhost:11434
    models:
      - name: llama3
        alias: local
default_model: local
mcp_servers:
  - name: alpha
    type: sse
    url: http://alpha.test/sse
  - name: beta
    type: sse
    url: http://beta.test/sse
profiles:
{profile_yaml}"
    )
}

// ===========================================================================
// CONTRACT A — `Profile.mcp_servers: Option<NameFilter>`
// ===========================================================================

/// Happy path: an allowlist under `mcp_servers:` parses and is reported by
/// `overridden_fields` (the boot-banner source).
#[test]
fn profile_mcp_servers_allowlist_parses_and_reports_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  web:
    mcp_servers:
      only: [alpha]
",
    )
    .expect("mcp_servers: {only: …} must parse");
    let fields = cfg
        .profiles
        .get("web")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        fields.contains(&"mcp_servers"),
        "overridden_fields must report `mcp_servers`; got: {fields:?}"
    );
}

/// Happy path: the blocklist form parses and is reported too.
#[test]
fn profile_mcp_servers_blocklist_parses_and_reports_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  web:
    mcp_servers:
      disabled: [alpha]
",
    )
    .expect("mcp_servers: {disabled: …} must parse");
    let fields = cfg
        .profiles
        .get("web")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        fields.contains(&"mcp_servers"),
        "overridden_fields must report `mcp_servers`; got: {fields:?}"
    );
}

/// Happy path: `enabled: false` (allow nothing) parses and is reported.
#[test]
fn profile_mcp_servers_enabled_false_parses_and_reports_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  web:
    mcp_servers:
      enabled: false
",
    )
    .expect("mcp_servers: {enabled: false} must parse");
    let fields = cfg
        .profiles
        .get("web")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        fields.contains(&"mcp_servers"),
        "overridden_fields must report `mcp_servers`; got: {fields:?}"
    );
}

/// `NameFilter` is `deny_unknown_fields`: a typo inside `mcp_servers:` is a
/// parse error naming the bad key (mirrors `profile_ui_tabs_unknown_key_is_rejected`).
#[test]
fn profile_mcp_servers_unknown_key_is_rejected() {
    let err = serde_yaml::from_str::<Config>(
        "profiles:
  web:
    mcp_servers:
      only: [alpha]
      bogus: []
",
    )
    .expect_err("unknown key under mcp_servers: must be rejected");
    assert!(
        err.to_string().contains("bogus"),
        "the error must name the unknown key, got: {err}"
    );
}

/// Guard: a profile that does not set `mcp_servers:` must not report it as
/// overridden (absent = unchanged, the whole profile contract).
#[test]
fn profile_mcp_servers_absent_is_not_reported_as_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  tools_only:
    tools:
      disabled: [bash]
",
    )
    .expect("a tools-only profile must parse");
    let fields = cfg
        .profiles
        .get("tools_only")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        !fields.contains(&"mcp_servers"),
        "absent mcp_servers: must not be reported as overridden; got: {fields:?}"
    );
}

/// Boot error: a gate naming an unknown server fails the load, naming the
/// bad server and listing the known (master) ones. Mirrors
/// `profile_pipelines_unknown_name_is_a_boot_error` end to end.
#[test]
fn profile_mcp_servers_unknown_name_is_a_boot_error() {
    let master = master_with_two_servers(
        "  web:
    mcp_servers:
      only: [gamma]
",
    );
    let result = with_master_config(&master, |_| Config::load(Some("web")));
    let msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a gate naming an unknown MCP server must be a boot error"),
    };
    assert!(
        msg.contains("gamma"),
        "the error must name the unknown server, got: {msg}"
    );
    assert!(
        msg.contains("Known servers: alpha, beta"),
        "the error must list the known (master) server names, got: {msg}"
    );
}

/// The blocklist is validated too: an unknown name in `disabled:` is the
/// same boot error.
#[test]
fn profile_mcp_servers_disabled_unknown_name_is_a_boot_error() {
    let master = master_with_two_servers(
        "  web:
    mcp_servers:
      disabled: [gamma]
",
    );
    let result = with_master_config(&master, |_| Config::load(Some("web")));
    let msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a blocklist naming an unknown MCP server must be a boot error"),
    };
    assert!(
        msg.contains("gamma"),
        "the error must name the unknown server, got: {msg}"
    );
    assert!(
        msg.contains("Known servers: alpha, beta"),
        "the error must list the known (master) server names, got: {msg}"
    );
}

/// Boundary parse: `only` + `disabled` together is a boot error naming the
/// profile's gate (mirrors `config_validate_rejects_profile_ui_tabs_with_both_only_and_disabled`).
#[test]
fn profile_mcp_servers_shape_error_when_only_and_disabled_both_set() {
    let master = master_with_two_servers(
        "  web:
    mcp_servers:
      only: [alpha]
      disabled: [beta]
",
    );
    let result = with_master_config(&master, |_| Config::load(Some("web")));
    let msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("only + disabled together must be rejected"),
    };
    assert!(
        msg.contains("profiles.web.mcp_servers"),
        "the error must name the profile's gate, got: {msg}"
    );
    assert!(
        msg.contains("not both"),
        "the error must say the two lists are exclusive, got: {msg}"
    );
}

/// Empty universe: master declares NO `mcp_servers:` at all — the gate
/// still fails, naming the first offending server (mirrors the pipelines
/// `declares no pipelines` branch).
#[test]
fn profile_mcp_servers_no_servers_declared_names_the_offending_server() {
    let master = "providers:
  - name: ollama
    type: ollama
    base_url: http://localhost:11434
    models:
      - name: llama3
        alias: local
default_model: local
profiles:
  web:
    mcp_servers:
      only: [alpha]
";
    let result = with_master_config(master, |_| Config::load(Some("web")));
    let msg = match result {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a gate with no servers to gate must be a boot error"),
    };
    assert!(
        msg.contains("alpha"),
        "the error must name the offending server, got: {msg}"
    );
    assert!(
        msg.contains("declares no MCP servers"),
        "the error must say the master config declares no MCP servers, got: {msg}"
    );
}

/// Only the ACTIVE profile is validated (the `pipelines:` rule): a second,
/// unselected profile with a bad gate is inert; selecting IT is the error.
#[test]
fn profile_mcp_servers_validation_only_for_active_profile() {
    let master = master_with_two_servers(
        "  web:
    mcp_servers:
      only: [alpha]
  ghost:
    mcp_servers:
      only: [bogus]
",
    );
    let (ok_when_web, err_when_ghost) = with_master_config(&master, |_| {
        (Config::load(Some("web")), Config::load(Some("ghost")))
    });
    assert!(
        ok_when_web.is_ok(),
        "the unselected profile's bad gate must be inert; got: {:?}",
        ok_when_web.err()
    );
    let msg = match err_when_ghost {
        Err(e) => e.to_string(),
        Ok(_) => panic!("selecting the profile with the bad gate must fail"),
    };
    assert!(
        msg.contains("bogus"),
        "the error must name the unknown server, got: {msg}"
    );
}

// ===========================================================================
// CONTRACT B — `Profile.agents_md: Option<bool>`
// ===========================================================================

/// Happy path: `agents_md: false` parses and is reported by
/// `overridden_fields`.
#[test]
fn profile_agents_md_false_parses_and_reports_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  web:
    agents_md: false
",
    )
    .expect("agents_md: false must parse");
    let fields = cfg
        .profiles
        .get("web")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        fields.contains(&"agents_md"),
        "overridden_fields must report `agents_md`; got: {fields:?}"
    );
}

/// Happy path: `agents_md: true` (inject as usual) parses and is reported.
#[test]
fn profile_agents_md_true_parses_and_reports_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  web:
    agents_md: true
",
    )
    .expect("agents_md: true must parse");
    let fields = cfg
        .profiles
        .get("web")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        fields.contains(&"agents_md"),
        "overridden_fields must report `agents_md`; got: {fields:?}"
    );
}

/// Type guard: a non-bool `agents_md:` is a parse error (type mismatch, not
/// a silently-ignored field).
#[test]
fn profile_agents_md_rejects_non_bool() {
    let err = serde_yaml::from_str::<Config>(
        "profiles:
  web:
    agents_md: \"no\"
",
    )
    .expect_err("a non-bool agents_md: must be rejected");
    assert!(
        err.to_string().contains("invalid type"),
        "the error must be a type mismatch, got: {err}"
    );
}

/// Guard: a profile that does not set `agents_md:` must not report it as
/// overridden (absent = unchanged).
#[test]
fn profile_agents_md_absent_is_not_reported_as_override() {
    let cfg: Config = serde_yaml::from_str(
        "profiles:
  tools_only:
    tools:
      disabled: [bash]
",
    )
    .expect("a tools-only profile must parse");
    let fields = cfg
        .profiles
        .get("tools_only")
        .expect("profile parsed")
        .overridden_fields();
    assert!(
        !fields.contains(&"agents_md"),
        "absent agents_md: must not be reported as overridden; got: {fields:?}"
    );
}
