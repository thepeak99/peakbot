//! Pure selection logic (spec §6.5–§6.6): classifying `/profile` and
//! `/agent` lines, resolving a saved selection (resume/`/load`), one
//! selection decision, and the two list renderings.
//!
//! Pure on purpose: no I/O, no `StateManager`. The T8b handler wires these
//! into the runtime and formats the Ok confirmation lines (`Profile: <p> ›
//! <a> — model <alias>.` / `Agent: …`) from the applied [`Bound`] — that is
//! why [`SelectChoice::Apply`] carries no message.
//!
//! The types (`Catalog`, `Bound`, `Selection`, …) come from
//! `crate::config::resolve` (T1).

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::config::ModelRegistry;
use crate::config::resolve::{Bound, Catalog, Origin, OverrideRejection, ResolvedAgent};

/// One selection command line, classified.
#[derive(Debug, PartialEq)]
pub enum SelectSubmission {
    /// Bare `/profile` — render the profile list.
    ListProfiles,
    /// `/profile <name>` (rest of the line, trimmed).
    Profile(String),
    /// Bare `/agent` — render the agent list.
    ListAgents,
    /// `/agent <name>` (rest of the line, trimmed).
    Agent(String),
}

/// Only `/profile …` and `/agent …` are selection commands: exact,
/// case-sensitive keywords, the rest of the line trimmed, an empty rest the
/// bare list form. `None` for everything else — the removed `/pipeline`
/// hits the generic unknown-command path (spec §6.6).
pub fn classify_select(line: &str) -> Option<SelectSubmission> {
    let (rest, list, named) = match line.strip_prefix("/profile") {
        Some(rest) => (
            rest,
            SelectSubmission::ListProfiles,
            SelectSubmission::Profile as fn(String) -> SelectSubmission,
        ),
        None => {
            let rest = line.strip_prefix("/agent")?;
            (
                rest,
                SelectSubmission::ListAgents,
                SelectSubmission::Agent as fn(String) -> SelectSubmission,
            )
        }
    };
    // A word boundary after the keyword: `/profiles` is not `/profile`, and
    // `strip_prefix` on the empty rest is `None`, so the bare form is the
    // explicit empty case.
    let rest = if rest.is_empty() {
        ""
    } else {
        rest.strip_prefix(char::is_whitespace)?
    };
    let name = rest.trim();
    if name.is_empty() {
        Some(list)
    } else {
        Some(named(name.to_string()))
    }
}

/// Resume/`/load` fallback (spec §6.6, Q2): the pin wins over the saved
/// profile, a saved profile wins over the first, a saved agent (inside the
/// chosen profile) wins over the first agent. A missing field falls back
/// silently; an unknown name falls back with the exact ⚠ warning (profile
/// warning before agent warning). An unknown pin is a boot error
/// (`Catalog::start_bound`) and never reaches here.
///
/// The saved overrides apply leniently (drop + ⚠ per bad entry) only when
/// the saved agent resolved EXACTLY — a fallback agent starts without
/// them, its ⚠ already explaining the switch.
pub fn resolve_saved_selection(
    cat: &Catalog,
    registry: &ModelRegistry,
    profile: Option<&str>,
    agent: Option<&str>,
    subagent_models: &BTreeMap<String, String>,
    pin: Option<&str>,
) -> (Bound, Vec<String>) {
    let mut warnings = Vec::new();
    let first = cat.default_bound();
    let profile = match pin {
        Some(name) => cat
            .profile(name)
            .cloned()
            .unwrap_or_else(|| Arc::clone(&first.profile)),
        None => match profile {
            Some(name) => match cat.profile(name) {
                Some(found) => Arc::clone(found),
                None => {
                    warnings.push(format!(
                        "⚠ Profile '{name}' from this conversation is not configured here; using '{}'.",
                        first.profile.name
                    ));
                    Arc::clone(&first.profile)
                }
            },
            None => Arc::clone(&first.profile),
        },
    };
    let first_agent = profile.agents[0].main.name.clone();
    let (agent_index, agent_exact) = match agent {
        None => (0, false),
        Some(name) => match profile.agents.iter().position(|a| a.main.name == name) {
            Some(index) => (index, true),
            None => {
                warnings.push(format!(
                    "⚠ Agent '{name}' is not in profile '{}'; using '{first_agent}'.",
                    profile.name
                ));
                (0, false)
            }
        },
    };
    let mut bound = Bound::new(Arc::clone(&profile), agent_index);
    if agent_exact {
        let (applied, rejected) = bound.with_subagent_models(registry, subagent_models);
        for rej in rejected {
            warnings.push(override_warning(&rej, applied.agent()));
        }
        bound = applied;
    }
    (bound, warnings)
}

/// The lenient override carry used by rebinds (cd-rebind): re-validate
/// `prev`'s subagent model overrides against `next`'s roster — valid
/// entries carry over, vanished ones drop with their ⚠ line. Shared by the
/// zero-turn `Rebind::Session` rebind and the `/cd` rebind.
pub fn carry_overrides(
    next: Bound,
    prev: &Bound,
    registry: &ModelRegistry,
) -> (Bound, Vec<String>) {
    let wanted = prev.selection().subagent_models;
    let (bound, rejected) = next.with_subagent_models(registry, &wanted);
    let warnings = rejected
        .iter()
        .map(|rej| override_warning(rej, bound.agent()))
        .collect();
    (bound, warnings)
}

/// The locked `/cd` rebind rule (cd-rebind): the turn count does not
/// matter — `/cd` always starts a fresh conversation.
///
/// | `cat.profile(bound.profile.name)` | kept target profile's origin        | result                                       |
/// |-----------------------------------|-------------------------------------|----------------------------------------------|
/// | `None`                            | —                                   | `cat.default_bound()` (no overrides)         |
/// | `Some`                            | `Repo { .. }`                       | re-resolve the name (overrides carried)      |
/// | `Some`                            | `Global`, catalog has Repo profiles | `cat.default_bound()` (no overrides)         |
/// | `Some`                            | `Global`, no Repo profiles          | keep, re-resolved on the fresh catalog       |
///
/// A kept name re-resolves by `(profile, agent)`: the agent resolved →
/// `carry_overrides`; it did not → the kept profile's first agent, no
/// overrides. Under a `--profile` pin the repo file is ignored, so the
/// catalog has no Repo profiles and the pinned name always resolves
/// (`reload_config` bails out before this when the pin vanished) — no pin
/// parameter is needed. The `Vec` holds override warnings only.
pub fn rebind_for_cd(
    cat: &Catalog,
    registry: &ModelRegistry,
    bound: &Bound,
) -> (Bound, Vec<String>) {
    let has_repo_profiles = cat.iter().any(|p| matches!(p.origin, Origin::Repo { .. }));
    let kept = cat
        .profile(&bound.profile.name)
        .filter(|profile| matches!(profile.origin, Origin::Repo { .. }) || !has_repo_profiles);
    let Some(kept) = kept else {
        return (cat.default_bound(), Vec::new());
    };
    match cat.resolve(&kept.name, &bound.agent().main.name) {
        Some(next) => carry_overrides(next, bound, registry),
        None => (Bound::new(Arc::clone(kept), 0), Vec::new()),
    }
}

/// The exact refusal line for a subagent override that a user frame asked
/// for and the registry/roster refused (plan §5.2).
pub fn override_refusal(
    rej: &OverrideRejection,
    agent: &ResolvedAgent,
    registry: &ModelRegistry,
) -> String {
    match rej {
        OverrideRejection::UnknownSubagent { subagent } => {
            let roster = if agent.subagents.is_empty() {
                "none".to_string()
            } else {
                agent
                    .subagents
                    .iter()
                    .map(|s| s.member.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!(
                "agent '{}' has no subagent '{}'. Subagents: {roster}",
                agent.main.name, subagent
            )
        }
        OverrideRejection::UnknownAlias { subagent, alias } => format!(
            "unknown alias `{alias}` for subagent '{subagent}'. Available: {}",
            registry.aliases_sorted().join(", ")
        ),
    }
}

/// The exact ⚠ line for a saved override that a rebind dropped (plan §5.2).
pub fn override_warning(rej: &OverrideRejection, agent: &ResolvedAgent) -> String {
    match rej {
        OverrideRejection::UnknownAlias { subagent, alias } => format!(
            "⚠ Model '{alias}' for subagent '{subagent}' is no longer configured; using its default."
        ),
        OverrideRejection::UnknownSubagent { subagent } => format!(
            "⚠ Subagent '{subagent}' is not in agent '{}'; its model override was dropped.",
            agent.main.name
        ),
    }
}

/// One `/profile <name>` | `/agent <name>` decision (spec §6.6).
/// Precedence: locked (`has_turns`) > pin > unknown > apply. Selecting a
/// profile picks its first agent; selecting an agent keeps the current
/// profile. Under a pin only the pinned profile may be selected (selecting
/// it applies); agent switches are always allowed within the current
/// (pinned) profile.
#[derive(Debug)]
pub enum SelectChoice {
    /// The selection applies; the handler formats the confirmation line.
    Apply(Bound),
    /// Refused, with the exact copy string.
    Refuse(String),
}

pub fn resolve_selection_choice(
    cat: &Catalog,
    cur: &Bound,
    sub: SelectSubmission,
    has_turns: bool,
    pin: Option<&str>,
) -> SelectChoice {
    match sub {
        SelectSubmission::Profile(name) => {
            if has_turns {
                return SelectChoice::Refuse(
                    "the profile is locked once the conversation has started — /new for a fresh one."
                        .to_string(),
                );
            }
            // Selecting the pinned profile itself applies; only `<other>` is refused.
            if let Some(pin) = pin
                && pin != name
            {
                return SelectChoice::Refuse(format!(
                    "the profile is pinned by --profile {pin} — restart without --profile to switch."
                ));
            }
            let Some(profile) = cat.profile(&name).cloned() else {
                let available: Vec<&str> = cat.iter().map(|p| p.name.as_str()).collect();
                return SelectChoice::Refuse(format!(
                    "unknown profile '{name}'. Available profiles: {}",
                    available.join(", ")
                ));
            };
            SelectChoice::Apply(Bound::new(profile, 0))
        }
        SelectSubmission::Agent(name) => {
            if has_turns {
                return SelectChoice::Refuse(
                    "the agent is locked once the conversation has started — /new for a fresh one."
                        .to_string(),
                );
            }
            // Agent switches are always allowed, within the current (pinned) profile.
            let profile = &cur.profile;
            let Some(agent_index) = profile.agents.iter().position(|a| a.main.name == name) else {
                let available: Vec<&str> = profile
                    .agents
                    .iter()
                    .map(|a| a.main.name.as_str())
                    .collect();
                return SelectChoice::Refuse(format!(
                    "unknown agent '{name}' in profile '{}'. Available agents: {}",
                    profile.name,
                    available.join(", ")
                ));
            };
            SelectChoice::Apply(Bound::new(Arc::clone(profile), agent_index))
        }
        // Bare forms are rendered by the handler, never "chosen".
        SelectSubmission::ListProfiles | SelectSubmission::ListAgents => {
            unreachable!("list forms are rendered, not resolved")
        }
    }
}

/// `/profile` listing (spec §6.6): one line per profile in catalog order —
/// `→ ` marks the current, `📁 ` marks a repo profile, the first line gets
/// `  (default)` (never under a pin, whose effective default is the pin),
/// and a repo profile shadowing a same-named global gets
/// `  (overrides global <name>)`. Under a pin the header gets
/// ` (pinned by --profile X)` appended.
pub fn render_profile_list(cat: &Catalog, cur: &Bound, pin: Option<&str>) -> String {
    let mut lines = vec!["Profiles (📁 = this repo; first = default):".to_string()];
    if let Some(pin) = pin {
        lines[0].push_str(" (pinned by --profile ");
        lines[0].push_str(pin);
        lines[0].push(')');
    }
    for (i, profile) in cat.iter().enumerate() {
        let marker = if profile.name == cur.profile.name {
            "→ "
        } else {
            "  "
        };
        let repo = match profile.origin {
            Origin::Global => "",
            Origin::Repo { .. } => "📁 ",
        };
        let mut line = format!("{marker}{repo}{}", profile.name);
        if i == 0 && pin.is_none() {
            line.push_str("  (default)");
        }
        if matches!(
            profile.origin,
            Origin::Repo {
                overrides_global: true
            }
        ) {
            line.push_str("  (overrides global ");
            line.push_str(&profile.name);
            line.push(')');
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// `/agent` listing (spec §6.6): one line per agent of the current profile
/// in declaration order — `→ ` marks the current, then `<name>  <alias> ·
/// single` (no subagents) or `<name>  <alias> · N subagents (a, b, …)`
/// (singular `1 subagent (x)`; names in declaration order).
pub fn render_agent_list(cur: &Bound) -> String {
    let profile = &cur.profile;
    let mut lines = vec![format!(
        "Agents in profile '{}' (first = default):",
        profile.name
    )];
    for agent in &profile.agents {
        let marker = if agent.main.name == cur.agent().main.name {
            "→ "
        } else {
            "  "
        };
        let roster = if agent.subagents.is_empty() {
            "single".to_string()
        } else {
            let names: Vec<&str> = agent
                .subagents
                .iter()
                .map(|s| s.member.name.as_str())
                .collect();
            let plural = if agent.subagents.len() == 1 { "" } else { "s" };
            format!(
                "{} subagent{plural} ({})",
                agent.subagents.len(),
                names.join(", ")
            )
        };
        lines.push(format!(
            "{marker}{}  {} · {roster}",
            agent.main.name, agent.main.model.alias
        ));
    }
    lines.join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
// T5 tests (spec §6.6): one per user-visible copy string, string-equal.
// Fixtures follow the T1 style: `Config::parse` over YAML strings (global
// always, repo where a test needs it).
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod t5_tests {
    use std::path::Path;

    use crate::config::ModelRegistry;
    use crate::config::resolve::{Bound, Catalog, Config, Origin, ParseCtx, Selection, Source};
    // NB: no short import of `render_agent_list` — the test fn below owns
    // that name in this module and calls the production fn by full path.
    use crate::config::resolve::OverrideRejection;
    use crate::selection::{
        SelectChoice, SelectSubmission, classify_select, override_refusal, override_warning,
        rebind_for_cd, render_profile_list, resolve_saved_selection, resolve_selection_choice,
    };

    /// A valid 0.24 global config: two profiles, `dev` first; `dev` has two
    /// agents (`team` first, with exactly the spec §6.6 example's three
    /// subagents) and `research` one.
    const GLOBAL_YAML: &str = r#"
providers:
  - name: patchnotes
    type: openai
    base_url: https://ai.patchnotes.com/v1
    api_key: pn-test
    models:
      - { name: anthropic/claude-sonnet-4.5, alias: sonnet, max_tokens: 8192 }
      - { name: anthropic/claude-opus-4.1, alias: opus, max_tokens: 8192 }
      - { name: qwen3-coder, alias: qwen, max_tokens: 32768 }
profiles:
  dev:
    agents:
      team:
        model: sonnet
        subagents:
          researcher:
            model: qwen
            prompt: Gather background, cite sources, hand back a brief.
          senior:
            model: opus
            prompt: Review the design and the code.
          tester:
            prompt: Write the failing tests first.
      solo:
        model: sonnet
  research:
    agents:
      scholar:
        model: opus
"#;

    /// A valid repo config (spec §3.2): one new profile plus one that
    /// shadows the global `dev` — together with `GLOBAL_YAML` they produce
    /// exactly the spec §6.6 `/profile` example list.
    const REPO_YAML: &str = r#"
profiles:
  peakbot-dev:
    agents:
      team:
        model: sonnet
        subagents:
          mid: { model: qwen, prompt: Implement the well-scoped piece you are given. }
  dev:
    agents:
      solo: { model: qwen }
"#;

    fn ctx() -> ParseCtx<'static> {
        ParseCtx { known_skills: &[] }
    }

    /// `Config::parse` over the fixtures; the paths only anchor the repo.
    fn config(repo: Option<&str>) -> Config {
        Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text: GLOBAL_YAML,
            },
            repo.map(|text| Source {
                path: Path::new("/repo/.peakbot/config.yaml"),
                text,
            }),
            &ctx(),
        )
        .expect("fixture config must parse")
    }

    fn catalog(repo: Option<&str>) -> Catalog {
        config(repo).catalog
    }

    /// The registry behind the same fixtures (for the override legs of
    /// `resolve_saved_selection`, plan §5.1).
    fn registry(repo: Option<&str>) -> std::sync::Arc<ModelRegistry> {
        config(repo).registry
    }

    /// The exact `(profile, agent)` of a fixture catalog.
    fn bound(repo: Option<&str>, profile: &str, agent: &str) -> Bound {
        catalog(repo)
            .resolve(profile, agent)
            .unwrap_or_else(|| panic!("fixture bound {profile}/{agent} must exist"))
    }

    /// Unwrap an `Apply` (panicking with the refusal if the choice went the
    /// other way) — the Apply tests then assert on the bound.
    fn apply(choice: SelectChoice, what: &str) -> Bound {
        match choice {
            SelectChoice::Apply(bound) => bound,
            SelectChoice::Refuse(msg) => panic!("expected Apply for {what}, got Refuse({msg:?})"),
        }
    }

    /// Unwrap a `Refuse` (panicking with the bound if the choice went the
    /// other way) — the refusal tests then assert the exact copy string.
    fn refuse(choice: SelectChoice, what: &str) -> String {
        match choice {
            SelectChoice::Apply(bound) => panic!(
                "expected Refuse for {what}, got Apply({:?})",
                bound.selection()
            ),
            SelectChoice::Refuse(msg) => msg,
        }
    }

    // ── classify_select ────────────────────────────────────────────────────

    /// §6.6: bare `/profile` asks for the profile list.
    #[test]
    fn classify_profile_bare_lists_profiles() {
        assert_eq!(
            classify_select("/profile"),
            Some(SelectSubmission::ListProfiles)
        );
    }

    /// §6.6: `/profile <name>` carries the name.
    #[test]
    fn classify_profile_with_name() {
        assert_eq!(
            classify_select("/profile x"),
            Some(SelectSubmission::Profile("x".into()))
        );
    }

    /// §6.6: bare `/agent` asks for the agent list.
    #[test]
    fn classify_agent_bare_lists_agents() {
        assert_eq!(
            classify_select("/agent"),
            Some(SelectSubmission::ListAgents)
        );
    }

    /// §6.6: `/agent <name>` carries the name.
    #[test]
    fn classify_agent_with_name() {
        assert_eq!(
            classify_select("/agent y"),
            Some(SelectSubmission::Agent("y".into()))
        );
    }

    /// §6.6 "rest of line, trimmed": outer whitespace is trimmed, inner
    /// spaces are part of the name.
    #[test]
    fn classify_trims_and_keeps_inner_spaces() {
        assert_eq!(
            classify_select("/profile   my dev profile  "),
            Some(SelectSubmission::Profile("my dev profile".into()))
        );
        assert_eq!(
            classify_select("/agent\ta"),
            Some(SelectSubmission::Agent("a".into()))
        );
    }

    /// Edge: a name that is only whitespace is the bare list form.
    #[test]
    fn classify_whitespace_only_name_is_bare_list() {
        assert_eq!(
            classify_select("/profile   "),
            Some(SelectSubmission::ListProfiles)
        );
        assert_eq!(
            classify_select("/agent  "),
            Some(SelectSubmission::ListAgents)
        );
    }

    /// §6.6: `/pipeline` is removed — it is no longer a selection command.
    #[test]
    fn classify_pipeline_is_none() {
        assert_eq!(classify_select("/pipeline"), None);
        assert_eq!(classify_select("/pipeline team"), None);
    }

    /// Edge: other commands and lookalikes are not selection commands.
    #[test]
    fn classify_non_selection_lines_are_none() {
        assert_eq!(classify_select("/model sonnet"), None);
        assert_eq!(
            classify_select("/profiles"),
            None,
            "longer keyword is not /profile"
        );
        assert_eq!(
            classify_select("/agents"),
            None,
            "longer keyword is not /agent"
        );
        assert_eq!(classify_select(""), None);
        assert_eq!(classify_select("profile x"), None, "no leading slash");
    }

    // ── resolve_saved_selection (resume / /load, spec §6.6 + Q2) ───────────

    /// Q2: a MISSING saved selection falls back to the first profile/agent
    /// SILENTLY — no warnings at all.
    #[test]
    fn saved_missing_is_silent() {
        let catalog = catalog(Some(REPO_YAML));
        let (bound, warnings) = resolve_saved_selection(
            &catalog,
            &registry(None),
            None,
            None,
            &std::collections::BTreeMap::new(),
            None,
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "peakbot-dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        assert!(
            warnings.is_empty(),
            "missing fields fall back silently, got: {warnings:?}"
        );
    }

    /// §6.6, verbatim: an UNKNOWN saved profile falls back to the first
    /// profile with the exact warning.
    #[test]
    fn saved_unknown_profile_warns() {
        let catalog = catalog(Some(REPO_YAML));
        let (bound, warnings) = resolve_saved_selection(
            &catalog,
            &registry(Some(REPO_YAML)),
            Some("ghost"),
            None,
            &std::collections::BTreeMap::new(),
            None,
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "peakbot-dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        assert_eq!(
            warnings,
            vec![
                "⚠ Profile 'ghost' from this conversation is not configured here; using 'peakbot-dev'."
            ]
        );
    }

    /// §6.6, verbatim: an UNKNOWN saved agent falls back to the profile's
    /// first agent with the exact warning.
    #[test]
    fn saved_unknown_agent_warns() {
        let catalog = catalog(None);
        let (bound, warnings) = resolve_saved_selection(
            &catalog,
            &registry(None),
            Some("dev"),
            Some("ghost"),
            &std::collections::BTreeMap::new(),
            None,
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        assert_eq!(
            warnings,
            vec!["⚠ Agent 'ghost' is not in profile 'dev'; using 'team'."]
        );
    }

    /// Edge: BOTH names unknown — both warnings, profile first, and the
    /// agent warning names the FALLBACK profile.
    #[test]
    fn saved_unknown_profile_and_agent_warn_in_order() {
        let catalog = catalog(None);
        let (bound, warnings) = resolve_saved_selection(
            &catalog,
            &registry(None),
            Some("ghost"),
            Some("phantom"),
            &std::collections::BTreeMap::new(),
            None,
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        assert_eq!(
            warnings,
            vec![
                "⚠ Profile 'ghost' from this conversation is not configured here; using 'dev'.",
                "⚠ Agent 'phantom' is not in profile 'dev'; using 'team'.",
            ]
        );
    }

    /// §6.6: the pin wins over the saved profile (no profile warning for a
    /// known-but-ignored saved name), and the saved agent is checked inside
    /// the PINNED profile.
    #[test]
    fn saved_pin_wins_over_saved_profile() {
        let catalog = catalog(None);
        let (bound, warnings) = resolve_saved_selection(
            &catalog,
            &registry(None),
            Some("dev"),
            Some("team"),
            &std::collections::BTreeMap::new(),
            Some("research"),
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "research".into(),
                agent: "scholar".into(),
                ..Default::default()
            }
        );
        assert_eq!(
            warnings,
            vec!["⚠ Agent 'team' is not in profile 'research'; using 'scholar'."]
        );
    }

    /// Happy path: a saved pair that exists resolves exactly, with no
    /// warnings.
    #[test]
    fn saved_known_pair_is_quiet() {
        let catalog = catalog(Some(REPO_YAML));
        let (bound, warnings) = resolve_saved_selection(
            &catalog,
            &registry(Some(REPO_YAML)),
            Some("research"),
            Some("scholar"),
            &std::collections::BTreeMap::new(),
            None,
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "research".into(),
                agent: "scholar".into(),
                ..Default::default()
            }
        );
        assert!(
            warnings.is_empty(),
            "known pair must be quiet, got: {warnings:?}"
        );
    }

    // ── resolve_selection_choice (/profile <name>, /agent <name>) ─────────

    /// §6.5: selecting a profile picks its FIRST agent (and its model).
    #[test]
    fn profile_picks_first_agent() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "solo");
        let bound = apply(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Profile("research".into()),
                false,
                None,
            ),
            "profile switch",
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "research".into(),
                agent: "scholar".into(),
                ..Default::default()
            }
        );
        assert_eq!(bound.agent().main.model.alias, "opus");
    }

    /// §6.5: selecting an agent KEEPS the current profile.
    #[test]
    fn agent_keeps_profile() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let bound = apply(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Agent("solo".into()),
                false,
                None,
            ),
            "agent switch",
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "solo".into(),
                ..Default::default()
            }
        );
        assert_eq!(bound.agent().main.model.alias, "sonnet");
    }

    /// §6.6, verbatim: after the first turn the profile is locked.
    #[test]
    fn profile_locked_after_turns() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let msg = refuse(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Profile("research".into()),
                true,
                None,
            ),
            "locked profile switch",
        );
        assert_eq!(
            msg,
            "the profile is locked once the conversation has started — /new for a fresh one."
        );
    }

    /// §6.6, verbatim: after the first turn the agent is locked.
    #[test]
    fn agent_locked_after_turns() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let msg = refuse(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Agent("solo".into()),
                true,
                None,
            ),
            "locked agent switch",
        );
        assert_eq!(
            msg,
            "the agent is locked once the conversation has started — /new for a fresh one."
        );
    }

    /// Precedence: the lock beats the pin (a locked conversation is refused
    /// with the lock message, not the pin message).
    #[test]
    fn profile_locked_beats_pin() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let msg = refuse(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Profile("research".into()),
                true,
                Some("dev"),
            ),
            "locked + pinned profile switch",
        );
        assert_eq!(
            msg,
            "the profile is locked once the conversation has started — /new for a fresh one."
        );
    }

    /// §6.5 pin, verbatim: under `--profile X`, switching to ANOTHER profile
    /// is refused with today's exact message.
    #[test]
    fn profile_refused_under_pin() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let msg = refuse(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Profile("research".into()),
                false,
                Some("dev"),
            ),
            "pinned profile switch",
        );
        assert_eq!(
            msg,
            "the profile is pinned by --profile dev — restart without --profile to switch."
        );
    }

    /// §6.5 pin: only `<other>` is refused — selecting the pinned profile
    /// itself applies (its first agent).
    #[test]
    fn profile_same_as_pin_applies() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let bound = apply(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Profile("dev".into()),
                false,
                Some("dev"),
            ),
            "selecting the pinned profile",
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            }
        );
        assert_eq!(bound.agent().main.model.alias, "sonnet");
    }

    /// §6.5 pin: `/agent` still works within the pinned profile.
    #[test]
    fn agent_allowed_under_pin() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let bound = apply(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Agent("solo".into()),
                false,
                Some("dev"),
            ),
            "agent switch under pin",
        );
        assert_eq!(
            bound.selection(),
            Selection {
                profile: "dev".into(),
                agent: "solo".into(),
                ..Default::default()
            }
        );
        assert_eq!(bound.agent().main.model.alias, "sonnet");
    }

    /// §6.6, verbatim: an unknown profile is refused and lists the profiles
    /// in catalog order (repo first when a repo file is present).
    #[test]
    fn profile_unknown_refused_lists_catalog() {
        let catalog = catalog(Some(REPO_YAML));
        let cur = bound(Some(REPO_YAML), "peakbot-dev", "team");
        let msg = refuse(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Profile("ghost".into()),
                false,
                None,
            ),
            "unknown profile",
        );
        assert_eq!(
            msg,
            "unknown profile 'ghost'. Available profiles: peakbot-dev, dev, research"
        );
    }

    /// §6.6, verbatim: an unknown agent is refused within the current
    /// profile and lists its agents in declaration order.
    #[test]
    fn agent_unknown_refused_lists_agents() {
        let catalog = catalog(None);
        let cur = bound(None, "dev", "team");
        let msg = refuse(
            resolve_selection_choice(
                &catalog,
                &cur,
                SelectSubmission::Agent("ghost".into()),
                false,
                None,
            ),
            "unknown agent",
        );
        assert_eq!(
            msg,
            "unknown agent 'ghost' in profile 'dev'. Available agents: team, solo"
        );
    }

    // ── render_profile_list (/profile, spec §6.6) ──────────────────────────

    /// §6.6, verbatim: repo markers, the default note, the override note —
    /// the spec's own example lines, with `→` on the current profile.
    #[test]
    fn render_profile_list_repo_markers() {
        let catalog = catalog(Some(REPO_YAML));
        let cur = bound(Some(REPO_YAML), "peakbot-dev", "team");
        assert_eq!(
            render_profile_list(&catalog, &cur, None),
            r#"Profiles (📁 = this repo; first = default):
→ 📁 peakbot-dev  (default)
  📁 dev  (overrides global dev)
  research"#
        );
    }

    /// `→` follows the current profile; the default/override notes stay on
    /// their own lines.
    #[test]
    fn render_profile_list_current_marker_moves() {
        let catalog = catalog(Some(REPO_YAML));
        let cur = bound(Some(REPO_YAML), "research", "scholar");
        assert_eq!(
            render_profile_list(&catalog, &cur, None),
            r#"Profiles (📁 = this repo; first = default):
  📁 peakbot-dev  (default)
  📁 dev  (overrides global dev)
→ research"#
        );
    }

    /// Under a pin the header appends `(pinned by --profile X)`; the catalog
    /// is global-only (repo files are ignored under a pin), so no `📁` line
    /// can appear.
    #[test]
    fn render_profile_list_under_pin() {
        let catalog = catalog(None);
        let cur = bound(None, "research", "scholar");
        assert_eq!(
            render_profile_list(&catalog, &cur, Some("research")),
            r#"Profiles (📁 = this repo; first = default): (pinned by --profile research)
  dev
→ research"#
        );
    }

    // ── render_agent_list (/agent, spec §6.6) ──────────────────────────────

    /// §6.6, verbatim: the spec's own example — current agent marked with
    /// `→`, model alias, `N subagents (…)` and `single`.
    #[test]
    fn render_agent_list() {
        let cur = bound(None, "dev", "team");
        assert_eq!(
            crate::selection::render_agent_list(&cur),
            r#"Agents in profile 'dev' (first = default):
→ team  sonnet · 3 subagents (researcher, senior, tester)
  solo  sonnet · single"#
        );
    }

    /// `→` follows the current agent, even when it is not the first.
    #[test]
    fn render_agent_list_current_not_first() {
        let cur = bound(None, "dev", "solo");
        assert_eq!(
            crate::selection::render_agent_list(&cur),
            r#"Agents in profile 'dev' (first = default):
  team  sonnet · 3 subagents (researcher, senior, tester)
→ solo  sonnet · single"#
        );
    }

    // ── subagent model overrides (selection-models plan §9.1, items 10–14) ──
    //
    // The plan's SHARED fixture (all new Rust tests use it): an anthropic
    // provider with the three aliases, `dev/team` with one inherited
    // (`reviewer`), one explicit (`tester`), one explicit-equal (`twin`,
    // Z4), a subagent-free `solo`, and `research/main`.

    /// The plan's shared fixture (see the section comment above).
    const TEAM_MODELS_YAML: &str = r#"
providers:
  - name: anthropic
    type: anthropic
    api_key: sk-ant-test
    models:
      - { name: claude-opus-5,   alias: opus,   context_size: 200000 }
      - { name: claude-sonnet-5, alias: sonnet, context_size: 200000 }
      - { name: claude-haiku-5,  alias: haiku,  context_size: 200000 }
general: { context: { enabled: false } }
profiles:
  dev:
    agents:
      team:
        model: sonnet
        subagents:
          reviewer: { prompt: "Review." }
          tester:   { prompt: "Test.",  model: haiku }
          twin:     { prompt: "Twin.",  model: sonnet }
      solo: { model: opus }
  research:
    agents:
      main: { model: haiku }
"#;

    fn team_config() -> Config {
        Config::parse(
            Source {
                path: Path::new("/global/config.yaml"),
                text: TEAM_MODELS_YAML,
            },
            None,
            &ctx(),
        )
        .expect("the shared team-models fixture parses")
    }

    fn team_bound(profile: &str, agent: &str) -> Bound {
        let cfg = team_config();
        cfg.catalog
            .resolve(profile, agent)
            .unwrap_or_else(|| panic!("fixture bound {profile}/{agent} must exist"))
    }

    /// Plan §5.2: the refusal copy for an unknown subagent names the roster
    /// in declaration order (`none` for a subagent-free agent).
    #[test]
    fn override_refusal_strings_exact() {
        let cfg = team_config();
        let team = team_bound("dev", "team");
        let solo = team_bound("dev", "solo");
        assert_eq!(
            override_refusal(
                &OverrideRejection::UnknownSubagent {
                    subagent: "ghost".into()
                },
                team.agent(),
                &cfg.registry,
            ),
            "agent 'team' has no subagent 'ghost'. Subagents: reviewer, tester, twin"
        );
        assert_eq!(
            override_refusal(
                &OverrideRejection::UnknownSubagent {
                    subagent: "ghost".into()
                },
                solo.agent(),
                &cfg.registry,
            ),
            "agent 'solo' has no subagent 'ghost'. Subagents: none"
        );
        assert_eq!(
            override_refusal(
                &OverrideRejection::UnknownAlias {
                    subagent: "tester".into(),
                    alias: "nope".into(),
                },
                team.agent(),
                &cfg.registry,
            ),
            "unknown alias `nope` for subagent 'tester'. Available: haiku, opus, sonnet"
        );
    }

    /// Plan §5.2: the ⚠ copy for a dropped override on rebind.
    #[test]
    fn override_warning_strings_exact() {
        let team = team_bound("dev", "team");
        assert_eq!(
            override_warning(
                &OverrideRejection::UnknownAlias {
                    subagent: "tester".into(),
                    alias: "nope".into(),
                },
                team.agent(),
            ),
            "⚠ Model 'nope' for subagent 'tester' is no longer configured; using its default."
        );
        assert_eq!(
            override_warning(
                &OverrideRejection::UnknownSubagent {
                    subagent: "ghost".into()
                },
                team.agent(),
            ),
            "⚠ Subagent 'ghost' is not in agent 'team'; its model override was dropped."
        );
    }

    /// Plan §9.1/12: a saved pair that resolves exactly carries its saved
    /// overrides with no warnings.
    #[test]
    fn resolve_saved_selection_applies_saved_subagent_models() {
        let cfg = team_config();
        let mut wanted = std::collections::BTreeMap::new();
        wanted.insert("tester".to_string(), "opus".to_string());
        let (bound, warnings) = resolve_saved_selection(
            &cfg.catalog,
            &cfg.registry,
            Some("dev"),
            Some("team"),
            &wanted,
            None,
        );
        assert!(
            warnings.is_empty(),
            "a saved pair that resolves exactly is quiet: {warnings:?}"
        );
        assert_eq!(
            bound.selection().subagent_models,
            wanted,
            "the valid saved override survives"
        );
    }

    /// Plan §9.1/13: unknown saved overrides are dropped in subagent-name
    /// order, one ⚠ each, nothing applied.
    #[test]
    fn resolve_saved_selection_drops_unknown_override_with_warning() {
        let cfg = team_config();
        let mut wanted = std::collections::BTreeMap::new();
        wanted.insert("tester".to_string(), "nope".to_string());
        wanted.insert("ghost".to_string(), "opus".to_string());
        let (bound, warnings) = resolve_saved_selection(
            &cfg.catalog,
            &cfg.registry,
            Some("dev"),
            Some("team"),
            &wanted,
            None,
        );
        assert_eq!(
            warnings,
            vec![
                "⚠ Subagent 'ghost' is not in agent 'team'; its model override was dropped.",
                "⚠ Model 'nope' for subagent 'tester' is no longer configured; using its default.",
            ],
            "one ⚠ per dropped override, in subagent-name order"
        );
        assert!(
            bound.selection().subagent_models.is_empty(),
            "nothing applies when every entry is bad"
        );
    }

    /// Plan §9.1/14: when the saved AGENT fell back, its overrides are
    /// dropped SILENTLY — the agent ⚠ already explains the fallback.
    #[test]
    fn resolve_saved_selection_drops_overrides_when_agent_falls_back() {
        let cfg = team_config();
        let mut wanted = std::collections::BTreeMap::new();
        wanted.insert("tester".to_string(), "opus".to_string());
        let (bound, warnings) = resolve_saved_selection(
            &cfg.catalog,
            &cfg.registry,
            Some("dev"),
            Some("gone"),
            &wanted,
            None,
        );
        assert_eq!(
            warnings,
            vec!["⚠ Agent 'gone' is not in profile 'dev'; using 'team'."],
            "only the existing agent-fallback warning — no override noise"
        );
        assert!(
            bound.selection().subagent_models.is_empty(),
            "the fallback agent starts without the vanished agent's overrides"
        );
    }

    // ── rebind_for_cd (cd-rebind: `/cd` switches to the target dir's repo agent) ──
    //
    // The locked rule (the turn count is irrelevant):
    //   keep  = cat.profile(bound.profile.name) when (the bound's origin is
    //           Repo OR the catalog has no Repo profiles — a pin ⇒ no repo
    //           profiles, so a pin is kept by this branch)
    //   no keep → cat.default_bound() (first profile's first agent, no
    //             overrides)
    //   keep    → cat.resolve(name, agent): Some → carry the overrides
    //             (with_subagent_models + override_warning, lenient);
    //             None → Bound::new(kept profile, 0) — the kept profile's
    //             first agent, no overrides.
    // The `Vec` returned holds override warnings only.

    /// cd-rebind: a bound on a GLOBAL profile, targeting a catalog WITH repo
    /// profiles → the repo's default (first profile's first agent), no
    /// warnings.
    #[test]
    fn cd_global_bound_into_repo_takes_repo_default() {
        let cat = catalog(Some(REPO_YAML));
        let reg = registry(Some(REPO_YAML));
        let prev = bound(None, "research", "scholar");
        assert!(
            matches!(prev.profile.origin, Origin::Global),
            "fixture sanity: research is global-origin"
        );

        let (next, warnings) = rebind_for_cd(&cat, &reg, &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "peakbot-dev".into(),
                agent: "team".into(),
                ..Default::default()
            },
            "the target repo's default is the bound now"
        );
        assert!(
            matches!(next.profile.origin, Origin::Repo { .. }),
            "the new bound is repo-origin"
        );
        assert!(
            warnings.is_empty(),
            "a clean default carries no warnings: {warnings:?}"
        );
    }

    /// cd-rebind: a shadowed name (`dev`) re-resolves to the REPO definition
    /// in the fresh catalog — the same-named global is gone from the target
    /// catalog (I7).
    #[test]
    fn cd_shadowed_name_reresolves_to_repo_definition() {
        let cat = catalog(Some(REPO_YAML));
        let prev = bound(None, "dev", "solo");

        let (next, warnings) = rebind_for_cd(&cat, &registry(Some(REPO_YAML)), &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "dev".into(),
                agent: "solo".into(),
                ..Default::default()
            },
            "the kept name, re-resolved in the target catalog"
        );
        assert!(
            std::sync::Arc::ptr_eq(&next.profile, cat.profile("dev").expect("repo dev")),
            "the bound's profile IS the fresh catalog's repo dev"
        );
        assert!(
            matches!(
                next.profile.origin,
                Origin::Repo {
                    overrides_global: true
                }
            ),
            "…the repo definition shadowing the global"
        );
        assert!(
            warnings.is_empty(),
            "no warnings for a clean re-resolution: {warnings:?}"
        );
    }

    /// cd-rebind: the kept profile exists in the target catalog but the
    /// agent no longer does → the kept profile's FIRST agent, no overrides,
    /// no warnings.
    #[test]
    fn cd_shadowed_name_missing_agent_falls_to_profiles_first() {
        let cat = catalog(Some(REPO_YAML));
        // The global dev has `team`; the repo dev has only `solo`.
        let prev = bound(None, "dev", "team");

        let (next, warnings) = rebind_for_cd(&cat, &registry(Some(REPO_YAML)), &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "dev".into(),
                agent: "solo".into(),
                ..Default::default()
            },
            "the kept profile's first agent"
        );
        assert!(
            next.selection().subagent_models.is_empty(),
            "the fallback agent starts without overrides"
        );
        assert!(
            warnings.is_empty(),
            "the agent fallback is structural, not a warning: {warnings:?}"
        );
    }

    /// cd-rebind: a bound on a REPO profile, targeting a catalog WITHOUT
    /// repo profiles (cd'ing out of the repo) → the global default: the
    /// first global profile's first agent (`dev/team` in GLOBAL_YAML).
    #[test]
    fn cd_repo_bound_out_of_repo_takes_global_default() {
        let cat = catalog(None);
        let prev = bound(Some(REPO_YAML), "peakbot-dev", "team");
        assert!(
            matches!(prev.profile.origin, Origin::Repo { .. }),
            "fixture sanity: peakbot-dev is repo-origin"
        );

        let (next, warnings) = rebind_for_cd(&cat, &registry(None), &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "dev".into(),
                agent: "team".into(),
                ..Default::default()
            },
            "the global default (first global profile's first agent)"
        );
        assert!(
            matches!(next.profile.origin, Origin::Global),
            "the new bound is global-origin"
        );
        assert!(
            warnings.is_empty(),
            "a clean default carries no warnings: {warnings:?}"
        );
    }

    /// cd-rebind: a bound on a global profile, targeting a catalog WITHOUT
    /// repo profiles → the choice is kept, re-resolved on the FRESH
    /// catalog's profile object.
    #[test]
    fn cd_keeps_global_choice_without_repo_profiles() {
        let cat = catalog(None);
        let prev = bound(None, "research", "scholar");

        let (next, warnings) = rebind_for_cd(&cat, &registry(None), &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "research".into(),
                agent: "scholar".into(),
                ..Default::default()
            },
            "the kept choice"
        );
        assert!(
            std::sync::Arc::ptr_eq(&next.profile, cat.profile("research").expect("research")),
            "the bound's profile is the FRESH catalog's Arc"
        );
        assert!(
            warnings.is_empty(),
            "keeping a configured choice is quiet: {warnings:?}"
        );
    }

    /// cd-rebind: the SAME pair re-resolved in the target catalog carries
    /// its subagent model overrides, with no warnings.
    #[test]
    fn cd_same_pair_carries_overrides() {
        let cat = catalog(Some(REPO_YAML));
        let reg = registry(Some(REPO_YAML));
        let base = cat
            .resolve("peakbot-dev", "team")
            .expect("peakbot-dev/team");
        let mut wanted = std::collections::BTreeMap::new();
        wanted.insert("mid".to_string(), "opus".to_string());
        let (prev, rejected) = base.with_subagent_models(&reg, &wanted);
        assert!(
            rejected.is_empty(),
            "mid: opus is a valid override: {rejected:?}"
        );

        let (next, warnings) = rebind_for_cd(&cat, &reg, &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "peakbot-dev".into(),
                agent: "team".into(),
                subagent_models: wanted.clone(),
            },
            "the same pair, with its override carried over"
        );
        assert!(
            warnings.is_empty(),
            "a valid override carries quietly: {warnings:?}"
        );
    }

    /// cd-rebind: a pair that SWITCHES (the kept profile's agent vanished
    /// in the target catalog) starts on the new agent WITHOUT the old
    /// overrides.
    #[test]
    fn cd_switch_clears_overrides() {
        let cat = catalog(Some(REPO_YAML));
        let reg = registry(Some(REPO_YAML));
        let base = bound(None, "dev", "team");
        let mut wanted = std::collections::BTreeMap::new();
        wanted.insert("tester".to_string(), "opus".to_string());
        let (prev, rejected) = base.with_subagent_models(&reg, &wanted);
        assert!(
            rejected.is_empty(),
            "tester: opus is a valid override on the global dev/team: {rejected:?}"
        );

        let (next, warnings) = rebind_for_cd(&cat, &reg, &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "dev".into(),
                agent: "solo".into(),
                ..Default::default()
            },
            "the repo dev's only agent"
        );
        assert!(
            next.selection().subagent_models.is_empty(),
            "a switched pair starts without the old overrides"
        );
        assert!(warnings.is_empty(), "…and silently: {warnings:?}");
    }

    /// cd-rebind: a `--profile` pin needs no special case — a pin means the
    /// repo file is ignored (RepoMode::Ignore), so the target catalog has
    /// NO repo profiles and the pinned global profile is kept by the
    /// "no repo profiles" branch.
    #[test]
    fn cd_pinned_profile_kept() {
        // The pinned session's catalog is global-only (no repo file read).
        let cat = catalog(None);
        let prev = bound(None, "research", "scholar");

        let (next, warnings) = rebind_for_cd(&cat, &registry(None), &prev);

        assert_eq!(
            next.selection(),
            Selection {
                profile: "research".into(),
                agent: "scholar".into(),
                ..Default::default()
            },
            "the pinned choice is kept"
        );
        assert!(
            warnings.is_empty(),
            "keeping the pin is quiet: {warnings:?}"
        );
    }
}
