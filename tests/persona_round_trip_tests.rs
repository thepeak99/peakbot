//! 0.24 port of the old P1 `Config::persona()` round-trip tests.
//!
//! The top-level `persona:` key is gone: the persona is a per-AGENT field
//! (`profiles.<p>.agents.<a>.persona`), and the YAML round-trip properties
//! are pinned through THE boundary — `Config::parse` resolving the agent's
//! `Head`. The locked expectations carry over:
//!
//! - the explicit `|2-` indent indicator preserves a leading-space first line
//! - blank interior lines survive
//! - trailing whitespace on a line is content, not stripped
//! - the resolved `Head` trims surrounding whitespace
//!
//! One behaviour changed on purpose: a whitespace-only persona is no longer
//! silently "absent" (the old accessor returned `None`) — `Config::parse`
//! REJECTS it, because in 0.24 the absence (a plain `Head::BuiltIn`) and an
//! explicit persona are distinct. The parse error is pinned here too.

use std::path::Path;

use peakbot::config::resolve::{Config, Head, ParseCtx, Source};

const GLOBAL_PATH: &str = "/home/u/.config/peakbot/config.yaml";

fn parse_global(yaml: &str) -> Config {
    Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .unwrap_or_else(|e| panic!("config must parse: {e:?}; yaml was:\n{yaml}"))
}

/// The 0.24 skeleton every test hangs its persona off: one provider, one
/// profile `dev`, one agent `main` with the given (already indented) agent
/// body.
fn cfg_with(agent_body: &str) -> Config {
    parse_global(&format!(
        "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet }}\nprofiles:\n  dev:\n    agents:\n      main:\n{agent_body}"
    ))
}

/// Helper: parse and unwrap the agent's resolved head. Centralises the API
/// so the test bodies read like the spec.
fn parsed_head(agent_body: &str) -> Head {
    cfg_with(agent_body)
        .catalog
        .profile("dev")
        .expect("the dev profile")
        .agents[0]
        .head
        .clone()
}

/// `ConfigErrors` has no Display — project segment paths + messages.
fn err_text(err: &peakbot::config::ConfigErrors) -> String {
    err.0
        .iter()
        .map(|e| format!("{}: {}", e.dotted_path(), e.message))
        .collect::<Vec<_>>()
        .join("; ")
}

#[test]
fn persona_absent_is_the_built_in_head() {
    assert_eq!(
        parsed_head("        model: sonnet\n"),
        Head::BuiltIn,
        "absent persona ⇒ the built-in persona"
    );
}

#[test]
fn persona_whitespace_only_is_a_parse_error() {
    // 0.24: a blank persona is a parse error, not a silent "absent" — the
    // old accessor's trim-to-`None` is gone, so this pins the replacement.
    for body in [
        "        model: sonnet\n        persona: \"   \"\n",
        "        model: sonnet\n        persona: |2-\n           \n           \n",
    ] {
        let err = Config::parse(
            Source {
                path: Path::new(GLOBAL_PATH),
                text: &format!(
                    "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet }}\nprofiles:\n  dev:\n    agents:\n      main:\n{body}"
                ),
            },
            None,
            &ParseCtx { known_skills: &[] },
        )
        .expect_err("a whitespace-only persona must be rejected");
        assert!(
            err_text(&err).contains("must not be blank"),
            "the error names the blank persona: {err:?}"
        );
    }
}

#[test]
fn persona_block_with_explicit_indent_indicator_preserves_leading_space() {
    // The load-bearing case: the first non-empty line starts with a space,
    // and `|2-` is the only thing keeping it. Without `2`, YAML infers the
    // indent from the first non-empty line and strips every leading space.
    let head = parsed_head(
        "        model: sonnet\n        persona: |2-\n          text starting with space\n          second\n",
    );
    assert_eq!(
        head,
        Head::Persona("text starting with space\nsecond".to_string())
    );
}

#[test]
fn persona_block_preserves_blank_interior_line() {
    let head = parsed_head(
        "        model: sonnet\n        persona: |2-\n          first paragraph\n\n          second paragraph\n",
    );
    assert_eq!(
        head,
        Head::Persona("first paragraph\n\nsecond paragraph".to_string()),
    );
}

#[test]
fn persona_block_preserves_trailing_space_on_a_line() {
    let head = parsed_head(
        "        model: sonnet\n        persona: |2-\n          ends-with-space \n          next\n",
    );
    assert_eq!(head, Head::Persona("ends-with-space \nnext".to_string()));
}

#[test]
fn persona_block_handles_hash_and_colon_in_content() {
    // Lines that start with `#` (comment marker) or contain `:` (mapping
    // marker) must NOT be re-interpreted by the parser. Block scalars
    // protect against both; this test pins that.
    let head = parsed_head(
        "        model: sonnet\n        persona: |2-\n          uses #: not a comment\n          has: colon\n",
    );
    assert_eq!(
        head,
        Head::Persona("uses #: not a comment\nhas: colon".to_string()),
    );
}

#[test]
fn persona_block_strip_chomping_strips_only_trailing_newlines() {
    // `-` chomping means the parsed value has no trailing newline. The
    // resolved head is trimmed — the trailing newlines are gone, the
    // interior structure is untouched.
    let head = parsed_head(
        "        model: sonnet\n        persona: |2-\n          hello\n          world\n\n\n",
    );
    assert_eq!(head, Head::Persona("hello\nworld".to_string()));
}

#[test]
fn persona_coexists_with_other_top_level_keys() {
    // The persona is a peer of every other key. It must not steal any
    // fields, and a config carrying unrelated keys still parses.
    let yaml = "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: sonnet\n        persona: |2-\n          short persona\ngeneral:\n  cost_tracking: true\n";
    let cfg = parse_global(yaml);
    assert!(cfg.general.cost_tracking, "the general block survives");
    let head = cfg.catalog.profile("dev").expect("the dev profile").agents[0]
        .head
        .clone();
    assert_eq!(head, Head::Persona("short persona".to_string()));
}

#[test]
fn blank_persona_with_core_guidance_off_is_a_parse_error() {
    // The old "blank Full falls back to the built-in" is gone in 0.24: a
    // blank persona is rejected outright…
    let err = Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: sonnet\n        core_guidance: false\n        persona: \"   \"\n",
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect_err("a blank persona must be rejected");
    assert!(
        err_text(&err).contains("must not be blank"),
        "the error names the blank persona: {err:?}"
    );

    // …and `core_guidance: false` with NO persona at all is its own error —
    // it needs the persona, which is the whole head when core guidance is off.
    let err = Config::parse(
        Source {
            path: Path::new(GLOBAL_PATH),
            text: "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - { name: anthropic/claude-sonnet-4.5, alias: sonnet }\nprofiles:\n  dev:\n    agents:\n      main:\n        model: sonnet\n        core_guidance: false\n",
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
    .expect_err("core_guidance: false without a persona must be rejected");
    assert!(
        err_text(&err).contains("core_guidance: false needs a persona"),
        "the error explains why: {err:?}"
    );
}
