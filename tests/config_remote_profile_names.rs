//! Remote-profiles SECURITY FIX — the remote profile-name rule must refuse
//! the dot segments `.` and `..` (design §4.2 name rule + §6.2/§7 `/r/` re-rooting).
//!
//! Why: the name rule `^[A-Za-z0-9_ .-]{1,64}$` admits `.` and `..`. A remote
//! profile's name is a URL path segment (`/r/<name>/…`); browsers collapse
//! `/r/../api/config` to `/api/config`, which defeats the `/r/` re-rooting
//! that keeps a remote's markdown images off local same-origin endpoints.
//!
//! Contract (the simplest correct rule, REMOTE kind only):
//! * a profile entry with a `remote:` block whose NAME is exactly `.` or `..`
//!   is a parse error keyed at `profiles.<name>` with the message
//!   `'<name>' isn't a valid remote profile name`;
//! * every other name the existing rule allows — including names that merely
//!   CONTAIN dots (`...`, `v1.2`, `.hidden`, `a..b`) — stays valid;
//! * LOCAL profiles are unaffected (their names never become URL segments).
//!
//! Public-API integration test: independent of how the implementer wires it.

use std::path::Path;

use peakbot::config::errors::PathSeg;
use peakbot::config::resolve::{Config, ParseCtx, Source};

const REMOTE_BLOCK: &str = "    remote:\n      url: https://10.2.0.48:7823\n      token: pito\n      ca: sha256:3f9a0123456789abcdef0123456789abcdef0123456789abcdef0123456789ab\n      profile: dev\n";

fn yaml_with_remote_named(name_yaml: &str) -> String {
    format!(
        "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }}\nprofiles:\n  {name_yaml}:\n{REMOTE_BLOCK}  local:\n    agents:\n      a:\n        model: sonnet\n"
    )
}

fn parse(yaml: &str) -> Result<Config, peakbot::config::errors::ConfigErrors> {
    Config::parse(
        Source {
            path: Path::new("/global/config.yaml"),
            text: yaml,
        },
        None,
        &ParseCtx { known_skills: &[] },
    )
}

fn key(s: &str) -> PathSeg {
    PathSeg::Key(s.to_string())
}

fn assert_dot_name_refused(name: &str) {
    // Quote so YAML reads the scalar as a string, not null / a path.
    let yaml = yaml_with_remote_named(&format!("\"{name}\""));
    let err = parse(&yaml).expect_err(&format!(
        "a remote profile named {name:?} must not parse (it collapses /r/{name}/… in browsers)"
    ));
    assert_eq!(err.0.len(), 1, "exactly one error for {name:?}: {err:?}");
    let e = &err.0[0];
    assert_eq!(
        e.path,
        vec![key("profiles"), key(name)],
        "keyed at profiles.<name>: {e:?}"
    );
    assert_eq!(
        e.message,
        format!("'{name}' isn't a valid remote profile name")
    );
}

#[test]
fn remote_profile_named_single_dot_is_a_parse_error() {
    assert_dot_name_refused(".");
}

#[test]
fn remote_profile_named_double_dot_is_a_parse_error() {
    assert_dot_name_refused("..");
}

// Names that merely contain dots are not path segments and stay valid.
#[test]
fn remote_profile_names_that_only_contain_dots_stay_valid() {
    for name in [
        "...", ".hidden", "a..b", "v1.2", "home.dev", "a.", "..a", "-.", ". x",
    ] {
        let yaml = yaml_with_remote_named(&format!("\"{name}\""));
        let cfg = parse(&yaml)
            .unwrap_or_else(|e| panic!("remote profile {name:?} must stay valid, got {e:?}"));
        assert!(
            cfg.catalog.remote(name).is_some(),
            "{name:?} resolves as a remote profile"
        );
    }
}

// The fix must not break the ordinary case it sits next to.
#[test]
fn ordinary_remote_profile_names_still_parse() {
    for name in ["home-dev", "home dev", "vps_2", "X"] {
        let yaml = yaml_with_remote_named(&format!("\"{name}\""));
        let cfg = parse(&yaml).unwrap_or_else(|e| panic!("{name:?} must parse, got {e:?}"));
        assert!(cfg.catalog.remote(name).is_some());
    }
}

// The refusal is about the remote kind: the dot name is rejected even when
// the rest of the block is perfectly valid, and the error does not cascade
// into a second error for the same entry (checked in assert_dot_name_refused);
// here: a SECOND, valid remote next to the bad one does not mask the error.
#[test]
fn a_dot_named_remote_is_still_reported_next_to_valid_remotes() {
    let yaml = format!(
        "providers:\n  - name: patchnotes\n    type: openai\n    base_url: https://ai.patchnotes.com/v1\n    api_key: pn-test\n    models:\n      - {{ name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }}\nprofiles:\n  \"..\":\n{REMOTE_BLOCK}  home-dev:\n    remote:\n      url: https://10.2.0.49:7823\n      token: pito\n      ca: system\n      profile: dev\n  local:\n    agents:\n      a:\n        model: sonnet\n"
    );
    let err = parse(&yaml).expect_err("the dot-named remote must fail the parse");
    assert_eq!(err.0.len(), 1, "only the bad entry is reported: {err:?}");
    assert_eq!(err.0[0].path, vec![key("profiles"), key("..")]);
    assert_eq!(err.0[0].message, "'..' isn't a valid remote profile name");
}
