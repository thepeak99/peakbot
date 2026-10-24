//! 0.24 port of the profile `mcps:` (CONTRACT A) and agent `agents_md:`
//! (CONTRACT B) parse + validation contract, driven through THE boundary
//! (`Config::parse`) only.
//!
//! The 0.23 seams are gone and their replacements are what this file pins:
//!
//! * `Profile.mcp_servers` ⇒ the profile's `mcps:` `NameFilter`
//!   (`enabled` / `disabled` XOR `only`) over MCP SERVER NAMES.
//! * `Profile.agents_md` ⇒ the per-AGENT `agents_md:` bool (main agents
//!   default ON, subagents OFF).
//! * The 0.23 "only the ACTIVE profile is validated" rule is gone: EVERY
//!   profile's `mcps:` gate is validated at parse time.
//! * `overridden_fields()` (the 0.23 boot-banner source) is gone: the 0.24
//!   boot banner derives from the catalog.
//!
//! Locked contract:
//!
//! * `mcps:` is a `NameFilter` — same semantics as `tools:`.
//! * Unknown key under it ⇒ parse error (NameFilter is `deny_unknown_fields`).
//! * `only` + `disabled` together ⇒ parse error naming `profiles.<p>.mcps`.
//! * Names are validated against the global `mcps[].name` list. The error
//!   names the bad server and lists the declared ones.
//! * `agents_md:` is a plain bool per agent. Non-bool ⇒ parse error.
//!
//! The EFFECT half of CONTRACT A (the agent receives only the allowed
//! servers' tools) is pinned at `peakbot::tools::env::McpSource`; the EFFECT
//! half of CONTRACT B (the prompt omits agents.md) is pinned in
//! `tests/agent_prompt_tests.rs` (`agents_md_toggle`).

use std::path::Path;

use peakbot::config::resolve::{Config, ParseCtx, Source};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";

fn parse_global(yaml: &str) -> Result<Config, peakbot::config::ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
}

/// `ConfigErrors` has no Display — project segment paths + messages.
fn err_text(err: &peakbot::config::ConfigErrors) -> String {
    err.0
        .iter()
        .map(|e| format!("{}: {}", e.dotted_path(), e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A global config with two MCP servers (`alpha`, `beta`) and the profile
/// under test spliced in. The servers are never connected here — only
/// name-validated.
fn global_with_two_servers(profile_yaml: &str) -> String {
    format!(
        "providers:
  - name: patchnotes
    type: openai
    base_url: https://ai.patchnotes.com/v1
    api_key: pn-test
    models:
      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet }}
mcps:
  - name: alpha
    command: npx
  - name: beta
    command: npx
profiles:
{profile_yaml}"
    )
}

// ===========================================================================
// CONTRACT A — the profile's `mcps:` NameFilter
// ===========================================================================

/// Happy path: an allowlist under `mcps:` parses and is carried by the
/// resolved profile.
#[test]
fn profile_mcps_allowlist_parses() {
    let cfg = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      only: [alpha]
    agents:
      main:
        model: sonnet
",
    ))
    .expect("mcps: {only: …} must parse");
    let web = cfg.catalog.profile("web").expect("the web profile");
    assert_eq!(web.mcps.only, vec!["alpha".to_string()]);
    assert!(web.mcps.enabled, "an allowlist is an enabled filter");
}

/// Happy path: the blocklist form parses too.
#[test]
fn profile_mcps_blocklist_parses() {
    let cfg = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      disabled: [alpha]
    agents:
      main:
        model: sonnet
",
    ))
    .expect("mcps: {disabled: …} must parse");
    let web = cfg.catalog.profile("web").expect("the web profile");
    assert_eq!(web.mcps.disabled, vec!["alpha".to_string()]);
}

/// Happy path: `enabled: false` (allow nothing) parses.
#[test]
fn profile_mcps_enabled_false_parses() {
    let cfg = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      enabled: false
    agents:
      main:
        model: sonnet
",
    ))
    .expect("mcps: {enabled: false} must parse");
    let web = cfg.catalog.profile("web").expect("the web profile");
    assert!(!web.mcps.enabled);
}

/// `NameFilter` is `deny_unknown_fields`: a typo inside `mcps:` is a parse
/// error naming the bad key.
#[test]
fn profile_mcps_unknown_key_is_rejected() {
    let err = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      only: [alpha]
      bogus: []
    agents:
      main:
        model: sonnet
",
    ))
    .expect_err("unknown key under mcps: must be rejected");
    assert!(
        err_text(&err).contains("bogus"),
        "the error must name the unknown key, got: {}",
        err_text(&err)
    );
}

/// Boot error: a gate naming an unknown server fails the parse, naming the
/// bad server and listing the declared (global) ones.
#[test]
fn profile_mcps_unknown_name_is_an_error() {
    let err = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      only: [gamma]
    agents:
      main:
        model: sonnet
",
    ))
    .expect_err("a gate naming an unknown MCP server must be rejected");
    let msg = err_text(&err);
    assert!(
        msg.contains("gamma"),
        "the error must name the unknown server, got: {msg}"
    );
    assert!(
        msg.contains("Declared servers: alpha, beta"),
        "the error must list the declared server names, got: {msg}"
    );
}

/// The blocklist is validated too: an unknown name in `disabled:` is the
/// same error.
#[test]
fn profile_mcps_disabled_unknown_name_is_an_error() {
    let err = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      disabled: [gamma]
    agents:
      main:
        model: sonnet
",
    ))
    .expect_err("a blocklist naming an unknown MCP server must be rejected");
    let msg = err_text(&err);
    assert!(
        msg.contains("gamma"),
        "the error must name the unknown server, got: {msg}"
    );
    assert!(
        msg.contains("Declared servers: alpha, beta"),
        "the error must list the declared server names, got: {msg}"
    );
}

/// Boundary parse: `only` + `disabled` together is an error naming the
/// profile's gate.
#[test]
fn profile_mcps_shape_error_when_only_and_disabled_both_set() {
    let err = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      only: [alpha]
      disabled: [beta]
    agents:
      main:
        model: sonnet
",
    ))
    .expect_err("only + disabled together must be rejected");
    let msg = err_text(&err);
    assert!(
        msg.contains("profiles.web.mcps"),
        "the error must name the profile's gate, got: {msg}"
    );
    assert!(
        msg.contains("not both"),
        "the error must say the two lists are exclusive, got: {msg}"
    );
}

/// Empty universe: the global declares NO `mcps:` at all — the gate still
/// fails, naming the offending server.
#[test]
fn profile_mcps_no_servers_declared_names_the_offending_server() {
    let yaml = "providers:
  - name: patchnotes
    type: openai
    base_url: https://ai.patchnotes.com/v1
    api_key: pn-test
    models:
      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }
profiles:
  web:
    mcps:
      only: [alpha]
    agents:
      main:
        model: sonnet
";
    let err = parse_global(yaml).expect_err("a gate with no servers to gate must be rejected");
    let msg = err_text(&err);
    assert!(
        msg.contains("alpha"),
        "the error must name the offending server, got: {msg}"
    );
    assert!(
        msg.contains("no MCP servers are declared in mcps"),
        "the error must say no MCP servers are declared, got: {msg}"
    );
}

/// 0.24: EVERY profile's gate is validated at parse time — the old "only
/// the active profile" rule is gone, so a second profile with a bad gate is
/// an error right away (no selection needed).
#[test]
fn profile_mcps_validation_applies_to_every_profile() {
    let err = parse_global(&global_with_two_servers(
        "  web:
    mcps:
      only: [alpha]
    agents:
      main:
        model: sonnet
  ghost:
    mcps:
      only: [bogus]
    agents:
      main:
        model: sonnet
",
    ))
    .expect_err("every profile's gate is validated at parse time");
    let msg = err_text(&err);
    assert!(
        msg.contains("bogus"),
        "the error must name the unknown server, got: {msg}"
    );
    assert!(
        msg.contains("profiles.ghost.mcps"),
        "the error must name the offending profile, got: {msg}"
    );
}

// ===========================================================================
// CONTRACT B — the agent's `agents_md:` bool
// ===========================================================================

/// `agents_md: false` parses and is carried by the resolved member.
#[test]
fn agent_agents_md_false_parses() {
    let cfg = parse_global(&global_with_two_servers(
        "  web:
    agents:
      main:
        model: sonnet
        agents_md: false
",
    ))
    .expect("agents_md: false must parse");
    let web = cfg.catalog.profile("web").expect("the web profile");
    assert!(!web.agents[0].main.agents_md);
}

/// `agents_md: true` parses too.
#[test]
fn agent_agents_md_true_parses() {
    let cfg = parse_global(&global_with_two_servers(
        "  web:
    agents:
      main:
        model: sonnet
        agents_md: true
",
    ))
    .expect("agents_md: true must parse");
    let web = cfg.catalog.profile("web").expect("the web profile");
    assert!(web.agents[0].main.agents_md);
}

/// The main agent's default is ON; the subagent's default is OFF.
#[test]
fn agent_agents_md_defaults_main_on_subagents_off() {
    let cfg = parse_global(&global_with_two_servers(
        "  web:
    agents:
      team:
        model: sonnet
        subagents:
          helper:
            prompt: help
",
    ))
    .expect("defaults must parse");
    let web = cfg.catalog.profile("web").expect("the web profile");
    assert!(
        web.agents[0].main.agents_md,
        "the main agent's agents_md default is on"
    );
    assert!(
        !web.agents[0].subagents[0].member.agents_md,
        "the subagent's agents_md default is off"
    );
}

/// Type guard: a non-bool `agents_md:` is a parse error (type mismatch, not
/// a silently-ignored field).
#[test]
fn agent_agents_md_rejects_non_bool() {
    let err = parse_global(&global_with_two_servers(
        "  web:
    agents:
      main:
        model: sonnet
        agents_md: \"no\"
",
    ))
    .expect_err("a non-bool agents_md: must be rejected");
    let msg = err_text(&err);
    assert!(
        msg.contains("agents_md"),
        "the error must name the field, got: {msg}"
    );
}
