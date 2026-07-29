//! `TimeBudget`: a tool decorator that owns the cross-cutting *liveness*
//! concern for every tool the model can call.
//!
//! Postmortem (0.16.1): `fetch_page` awaited a spider future that never
//! completed. Nothing between that await and the top of the process armed a
//! wall clock, so a sub-agent wedged for over an hour and 150+ completed tool
//! calls were lost. This decorator is the seam that makes "a tool call always
//! returns" true by construction, for the whole class — not for the one tool
//! where it last surfaced.
//!
//! A fired budget is reported as `Ok(timeout_message(..))`, not `Err`: rig
//! stringifies tool errors into tool results anyway, so `Err` is
//! observationally identical but arrives at the model triple-prefixed with
//! rig's error wrappers. `Ok` keeps the wording ours.
//!
//! See `docs/tool-time-budget-design.md`.

use rig_core::completion::ToolDefinition;
use rig_core::tool::{ToolDyn, ToolError};
use rig_core::wasm_compat::WasmBoxedFuture;
use std::time::Duration;

/// Wall-clock ceiling for a tool call that does not name itself in
/// [`budget_for`].
pub(crate) const DEFAULT_TOOL_BUDGET: Duration = Duration::from_secs(600);

/// Tools owning a *longer* internal deadline get an entry that sits ABOVE it,
/// so their own (more informative) bound always fires first and this one stays
/// a pure backstop. There is deliberately no opt-out: "exempt from the
/// liveness guarantee" is the sentence that writes the next postmortem.
pub(crate) fn budget_for(tool: &str) -> Duration {
    match tool {
        // bash/powershell clamp their own `timeout_seconds` to 7200s.
        "bash" | "powershell" => Duration::from_secs(7_500),
        // delegate bounds its own prompt loop at DELEGATE_BUDGET (1800s) and
        // then spends time summarising the dead sub-agent (handoff::build
        // makes an LLM call); the slack covers that summarisation.
        "delegate" => Duration::from_secs(2_100),
        _ => DEFAULT_TOOL_BUDGET,
    }
}

/// The one and only wording for "a call was cut off by its deadline". Shared
/// with any tool that bounds itself (`fetch_page`) so the model sees one
/// consistent, learnable shape.
pub(crate) fn timeout_message(tool: &str, budget: Duration) -> String {
    format!(
        "⏱ TIMEOUT: tool `{tool}` did not return within {}s and was cancelled. \
         Its work was abandoned mid-flight and may be incomplete. \
         Do not retry the identical call — narrow the input, use a different tool, \
         or try a different source.",
        budget.as_secs()
    )
}

/// Wraps any `Box<dyn ToolDyn>` in a wall-clock deadline.
pub struct TimeBudget {
    inner: Box<dyn ToolDyn>,
    /// Resolved once at wrap time. There is no "unbudgeted" state to represent.
    budget: Duration,
    /// Cached so the timeout path does not re-enter the inner tool.
    name: String,
}

impl TimeBudget {
    /// Production constructor: the budget is resolved from the tool's name.
    pub fn wrap(inner: Box<dyn ToolDyn>) -> Self {
        let budget = budget_for(&inner.name());
        Self::with_budget(inner, budget)
    }

    /// Explicit budget — used by tests, and the honest way to express "this
    /// caller knows better" if that ever becomes true.
    pub fn with_budget(inner: Box<dyn ToolDyn>, budget: Duration) -> Self {
        Self {
            name: inner.name(),
            inner,
            budget,
        }
    }

    /// Test-only: the resolved budget is otherwise an implementation detail,
    /// but the name-table wiring has to be observable to be pinned.
    #[cfg(test)]
    pub fn budget(&self) -> Duration {
        self.budget
    }
}

impl ToolDyn for TimeBudget {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        self.inner.definition(prompt)
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        Box::pin(async move {
            match tokio::time::timeout(self.budget, self.inner.call(args)).await {
                Ok(result) => result,
                // `error!`, not `warn!`: a fired budget always means something
                // is broken — a pathological upstream or a wrong constant.
                Err(_elapsed) => {
                    tracing::error!(
                        target: "peakbot",
                        tool = %self.name,
                        budget_secs = self.budget.as_secs(),
                        "Tool call exceeded its wall-clock budget and was cancelled"
                    );
                    Ok(timeout_message(&self.name, self.budget))
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::future::Future;
    use std::pin::Pin;

    /// The `'static` future a fixture's `build` hands back. Naming it keeps
    /// the fixture's field type readable.
    type CallFuture = Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send>>;

    /// Per-instance-named inner tool used by the in-memory decorator tests.
    ///
    /// `Tool::NAME` is a `const`, so a per-instance name has to come from a
    /// hand-rolled `ToolDyn` impl. `build` yields a `'static` future, which
    /// borrows nothing from `self` and so coerces straight to the trait's
    /// `WasmBoxedFuture<'a, ...>`.
    struct Named {
        name: &'static str,
        build: Box<dyn Fn() -> CallFuture + Send + Sync>,
    }

    impl ToolDyn for Named {
        fn name(&self) -> String {
            self.name.to_string()
        }

        fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: self.name.to_string(),
                    description: "named".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": { "x": { "type": "string" } },
                        "required": ["x"]
                    }),
                }
            })
        }

        fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
            (self.build)()
        }
    }

    fn hang() -> Named {
        Named {
            name: "hung",
            build: Box::new(|| {
                Box::pin(async {
                    let _ = std::future::pending::<Result<String, ToolError>>().await;
                    unreachable!()
                })
            }),
        }
    }

    fn ok(name: &'static str, s: &'static str) -> Named {
        // `move ||` so the closure captures `s` by-copy (it's `&'static str` —
        // Copy). Without `move`, the closure borrows the *binding* `s`, whose
        // lifetime is the surrounding function's, not `'static`, and the
        // `Box<dyn Fn() + 'static>` coercion rejects that.
        Named {
            name,
            build: Box::new(move || Box::pin(async move { Ok(s.to_string()) })),
        }
    }

    fn err_with(name: &'static str, msg: &'static str) -> Named {
        Named {
            name,
            build: Box::new(move || {
                // `ToolError::ToolCallError` takes `Box<dyn Error + Send + Sync>`,
                // so the inner has to be an `Error`. `String` isn't one, so wrap
                // the message in a tiny local error type.
                let err: Box<dyn std::error::Error + Send + Sync> =
                    Box::new(InnerErr(msg.to_string()));
                Box::pin(async move { Err(ToolError::ToolCallError(err)) })
            }),
        }
    }

    /// Tiny `std::error::Error` newtype used by `err_with` so we can round-trip
    /// a message string through `ToolError::ToolCallError`. Only its `Display`
    /// matters — the test only asserts the `Display` output.
    #[derive(Debug)]
    struct InnerErr(String);
    impl std::fmt::Display for InnerErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for InnerErr {}

    /// Paused-time decorator contract: an inner future that never completes
    /// gets cut at the budget, and the model receives an `Ok` timeout message
    /// — NOT an `Err`. The exact wording is pinned (the marker, the tool
    /// name, and the budget seconds must all appear) so a future edit can't
    /// silently change the shape the model self-corrects from.
    ///
    /// Determinism note: tokio's `start_paused` knob needs the `test-util`
    /// feature, which the repo's `Cargo.toml` does not enable (only `full`).
    /// A short real-time budget (1 s, expressed as 1500 ms so the rendered
    /// `as_secs()` rounds to a stable "1s") keeps the test fast and
    /// deterministic — the inner future is `std::future::pending`, so the
    /// deadline always fires, and the `elapsed < 5s` assertion catches any
    /// regression to "blocks forever".
    #[tokio::test]
    async fn hung_tool_returns_a_timeout_result_within_its_budget() {
        let budget = Duration::from_millis(1_500);
        let wrapped = TimeBudget::with_budget(Box::new(hang()), budget);

        let started = tokio::time::Instant::now();
        let result = wrapped.call("{}".to_string()).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "decorator must return within seconds, not hang; took {:?}",
            elapsed
        );

        let s = result.expect("timeout must be Ok(String), not Err(ToolError)");
        assert!(s.contains("⏱ TIMEOUT"), "timeout marker missing from: {s}");
        assert!(s.contains("hung"), "tool name missing from: {s}");
        // `budget.as_secs()` is what the design pins (`timeout_message`
        // formats the bound as `{N}s`). 1500 ms rounds down to 1.
        assert!(s.contains("1s"), "budget seconds missing from: {s}");
    }

    /// The happy path: an inner tool's output is returned **byte-identical**
    /// through the decorator. This is the "no behaviour change on the fast
    /// path" guarantee — the decorator must not append nudges, rewrite the
    /// string, or convert `Ok` ↔ `Err`.
    #[tokio::test]
    async fn fast_tool_passes_through_untouched() {
        let wrapped = TimeBudget::with_budget(
            Box::new(ok("echo", "the inner output\n\twith tabs and unicode → ✓")),
            Duration::from_secs(60),
        );

        let result = wrapped.call("{}".to_string()).await.unwrap();
        assert_eq!(
            result, "the inner output\n\twith tabs and unicode → ✓",
            "the decorator must be a perfect pass-through on the happy path"
        );
    }

    /// A real tool error must NOT be swallowed into a fake `Ok(timeout)`. The
    /// decorator must let real failures through unchanged so the model's
    /// self-correction loop still works when the tool itself fails fast.
    #[tokio::test]
    async fn inner_error_is_propagated_unchanged() {
        let wrapped = TimeBudget::with_budget(
            Box::new(err_with("explode", "inner boom")),
            Duration::from_secs(60),
        );

        match wrapped.call("{}".to_string()).await {
            Err(e) => assert!(
                e.to_string().contains("inner boom"),
                "inner error must be preserved with its message; got: {e}"
            ),
            Ok(s) => panic!("inner error must surface as Err, not Ok; got Ok({s:?})"),
        }
    }

    /// `name()` and `definition()` must be exact pass-through. The name
    /// invariant is load-bearing because `tools_filter.allows(&t.name())` at
    /// `src/providers/mod.rs:592` keys on the wrapper's name (the gate
    /// already delegates). The definition invariant is load-bearing because
    /// `ThoughtGate`'s injected `thought` must survive — if the budget
    /// decorator mutated the schema, the gate's job would be undone.
    #[tokio::test]
    async fn name_and_definition_are_pass_through() {
        let inner = ok("named_fixture", "x");
        let inner_def = inner.definition(String::new()).await;
        let inner_name = inner.name();

        let wrapped = TimeBudget::with_budget(Box::new(inner), Duration::from_secs(60));
        assert_eq!(wrapped.name(), inner_name, "name must pass through");

        let wrapped_def = wrapped.definition(String::new()).await;
        assert_eq!(
            wrapped_def.name, inner_def.name,
            "definition().name must pass through"
        );
        assert_eq!(
            wrapped_def.parameters, inner_def.parameters,
            "definition().parameters must pass through (no `thought` injection here — that's ThoughtGate's job)"
        );
    }

    /// `TimeBudget::wrap` resolves the budget from the inner tool's name via
    /// the table in §3.3. The three-row override table must be wired
    /// correctly — any edit that breaks it loses the ordering invariant in
    /// §3.4 (informative inner bound must always fire first).
    #[test]
    fn wrap_uses_the_name_table() {
        let named = |name| Named {
            name,
            build: Box::new(|| Box::pin(async { Ok(String::new()) })),
        };

        assert_eq!(
            TimeBudget::wrap(Box::new(named("bash"))).budget(),
            Duration::from_secs(7_500),
            "bash gets the 2h+5min override"
        );
        assert_eq!(
            TimeBudget::wrap(Box::new(named("powershell"))).budget(),
            Duration::from_secs(7_500),
            "powershell gets the 2h+5min override"
        );
        assert_eq!(
            TimeBudget::wrap(Box::new(named("delegate"))).budget(),
            Duration::from_secs(2_100),
            "delegate gets the 35min override (slack above its own 30min inner bound)"
        );
        assert_eq!(
            TimeBudget::wrap(Box::new(named("fetch_page"))).budget(),
            DEFAULT_TOOL_BUDGET,
            "fetch_page uses the 600s default (its own inner ~165s bound fires first)"
        );
        assert_eq!(
            TimeBudget::wrap(Box::new(named("totally_unknown_tool"))).budget(),
            DEFAULT_TOOL_BUDGET,
            "any unknown tool name falls through to the 600s default"
        );
    }

    /// The §3.4 ordering invariant, expressed as a single assertion per row.
    /// If anyone edits one of these constants in isolation, this test fails
    /// loudly with the exact invariant that broke — rather than letting the
    /// informative inner bound become unreachable in production.
    ///
    /// Note: the delegate-side assertion (`budget_for("delegate") >
    /// DELEGATE_BUDGET`) lives in `src/pipeline/delegate_tool.rs` tests
    /// because `delegate_tool` is a private sibling module — this file
    /// can't reach its constants. Fetch-page's row lives here so the
    /// decorator-vs-fetch-page coherence is pinned in one place; the
    /// decorator-vs-bash / powershell rows are likewise pinned here.
    #[test]
    fn budget_exceeds_every_tool_owned_deadline() {
        // bash / powershell self-clamp at 7200s; their decorator entry must
        // sit strictly above that or the tool's own informative message
        // never wins.
        const BASH_MAX_SECS: u64 = 7_200;
        const POWERSHELL_MAX_SECS: u64 = 7_200;
        assert!(
            budget_for("bash") > Duration::from_secs(BASH_MAX_SECS),
            "budget_for(\"bash\") = {:?} must exceed bash's {}s self-clamp",
            budget_for("bash"),
            BASH_MAX_SECS
        );
        assert!(
            budget_for("powershell") > Duration::from_secs(POWERSHELL_MAX_SECS),
            "budget_for(\"powershell\") = {:?} must exceed powershell's {}s self-clamp",
            budget_for("powershell"),
            POWERSHELL_MAX_SECS
        );

        // fetch_page's worst case must fit inside the default — otherwise the
        // generic 600s backstop would cut the informative per-attempt message.
        assert!(
            DEFAULT_TOOL_BUDGET > crate::tools::fetch_page::worst_case_duration(),
            "DEFAULT_TOOL_BUDGET = {:?} must exceed fetch_page::worst_case_duration() = {:?}",
            DEFAULT_TOOL_BUDGET,
            crate::tools::fetch_page::worst_case_duration()
        );
    }

    /// There is no "disabled" or "0" budget. The whole point of this
    /// decorator is to make "no wall-clock deadline" unrepresentable; an
    /// empty / unknown tool name must still get a budget, never `Duration::ZERO`.
    ///
    /// Updated for the configurable-budget surface: iterates the same tool
    /// names against BOTH the default config AND the minimum-legal config
    /// (`tool_secs: 1, delegate_secs: 1`) so the no-zero invariant holds
    /// even at the configured floor. The shell clamp is checked separately
    /// by `shell_budget_never_drops_below_the_shell_clamp` so the bash
    /// floor of 7500s doesn't overshadow the "1s is allowed for tools that
    /// don't get a clamp" test below.
    #[test]
    fn budget_is_never_zero() {
        for cfg in [
            TimeoutsConfig::default(),
            TimeoutsConfig {
                tool_secs: 1,
                delegate_secs: 1,
            },
        ] {
            for name in ["", "fetch_page", "some_future_tool"] {
                assert!(
                    budget_for(name, &cfg) > Duration::ZERO,
                    "budget_for({name:?}, &{cfg:?}) = {:?} must be > 0",
                    budget_for(name, &cfg)
                );
            }
            // Delegate at tool_secs=1 must still inherit the 300s salvage
            // margin on top of delegate_secs, so its budget > 0 trivially.
            assert!(
                budget_for("delegate", &cfg) > Duration::ZERO,
                "delegate budget must always be > 0 even at minimum delegate_secs"
            );
        }
    }

    // ── budget table (configurable budgets) ───────────────────────────────
    //
    // The locked-name API surface: `budget_for(tool, cfg)` resolves any tool
    // name to a `Duration`, and `delegate_loop_budget(cfg)` exposes the
    // pure inner-loop budget. Tests 11–16 lock the new shape.

    #[test]
    fn default_config_budgets_every_unnamed_tool_at_30_minutes() {
        // Every tool that doesn't appear in the budget_for table (anything
        // other than bash / powershell / delegate) must get `cfg.tool_secs`
        // verbatim, with no clamping — the budget is the budget, full stop.
        // This is the only class of tool the model can ship NEW examples of,
        // so it's also the one most likely to be wrong.
        let cfg = TimeoutsConfig::default();
        for name in [
            "file_read",
            "web_search",
            "search",
            "thought_gate",
            "anything",
        ] {
            assert_eq!(
                budget_for(name, &cfg),
                Duration::from_secs(1_800),
                "unnamed tool {name:?} must get tool_secs verbatim (1800s with default cfg)"
            );
        }
    }

    #[test]
    fn tool_secs_drives_the_generic_budget() {
        // The generic row of `budget_for` is a direct pass-through to
        // `cfg.tool_secs`. Without `#[serde(default)]` per field or a default
        // regression, this is the cheapest knob the operator can use.
        let cfg = TimeoutsConfig {
            tool_secs: 42,
            delegate_secs: 3_600,
        };
        assert_eq!(
            budget_for("some_tool", &cfg),
            Duration::from_secs(42),
            "generic row must pass through cfg.tool_secs without clamping"
        );
        assert_eq!(
            budget_for("file_read", &cfg),
            Duration::from_secs(42),
            "file_read isn't in the override table either; must use cfg.tool_secs"
        );
    }

    #[test]
    fn shell_budget_never_drops_below_the_shell_clamp() {
        // The shell clamp at 7500s is load-bearing: bash / powershell self-
        // cap their `timeout_seconds` at 7200, and the decorator MUST stay
        // strictly above that, or the tool's informative "I cancelled
        // myself at 7200s" message never reaches the model — a wedged
        // sub-shell would be reported by the decorator instead, with no
        // exit code in the output.
        //
        // The clamp applies for ANY value of `cfg.tool_secs` (including the
        // minimum legal value of 1) and must hold even when the operator
        // sets tool_secs far above the clamp (86400 — saturating).
        let shell_clamp = Duration::from_secs(7_500);
        let shell_self_cap = Duration::from_secs(7_200);
        for tool_secs in [1, 1_800, 7_500, 86_400] {
            let cfg = TimeoutsConfig {
                tool_secs,
                delegate_secs: 3_600,
            };
            for shell in ["bash", "powershell"] {
                let budget = budget_for(shell, &cfg);
                assert!(
                    budget > shell_self_cap,
                    "{shell} budget ({:?}) at tool_secs={tool_secs} must exceed the 7200s self-cap; \
                     the decorator can't be allowed to cut a tool's own informative message",
                    budget
                );
                assert!(
                    budget >= std::cmp::max(shell_clamp, Duration::from_secs(tool_secs)),
                    "{shell} budget ({:?}) at tool_secs={tool_secs} must be at least \
                     max(shell_clamp=7500s, tool_secs={tool_secs}s)",
                    budget
                );
            }
        }
    }

    #[test]
    fn delegate_registration_strictly_exceeds_the_delegate_loop() {
        // The §3.4 ordering invariant, now expressed via the new API: the
        // outer decorator budget registered with the rig builder MUST sit
        // strictly above `delegate_loop_budget(cfg)`, by exactly the
        // SALVAGE_MARGIN (300s), so the salvaged-handoff summary LLM call
        // can run after the inner deadline fires without being cut.
        //
        // Holds at any `delegate_secs` an operator could legally pick (the
        // validate() boundary is 1..=86400).
        for delegate_secs in [1, 3_600, 86_400] {
            let cfg = TimeoutsConfig {
                tool_secs: 1_800,
                delegate_secs,
            };
            let inner = delegate_loop_budget(&cfg);
            let registered = budget_for("delegate", &cfg);
            assert_eq!(
                inner,
                Duration::from_secs(delegate_secs),
                "delegate_loop_budget must equal cfg.delegate_secs"
            );
            assert_eq!(
                registered,
                inner + Duration::from_secs(300),
                "the registered delegate budget must equal the inner budget + the salvage margin"
            );
            assert!(
                registered > inner,
                "registered budget ({registered:?}) must be strictly greater than the inner budget ({inner:?})"
            );
        }
    }

    /// `default_budget_exceeds_fetch_page_worst_case` — moved from
    /// `src/tools/fetch_page.rs` so the budget-vs-worst-case coherence lives
    /// next to the function that owns the budget half of the relationship.
    /// Single source of truth: fetch_page owns the worst-case calc, the
    /// decorator owns the resolved budget, and this is the one test that
    /// knows about both.
    ///
    /// Pre-existing assertion: `DEFAULT_TOOL_BUDGET > fetch_page::worst_case_duration()`.
    /// Post-fix: `budget_for("fetch_page", &TimeoutsConfig::default()) >
    /// fetch_page::worst_case_duration()`. The numeric value changes
    /// (600s → 1800s); the ordering invariant is what this test pins.
    #[test]
    fn default_budget_exceeds_fetch_page_worst_case() {
        let cfg = TimeoutsConfig::default();
        assert!(
            budget_for("fetch_page", &cfg) > crate::tools::fetch_page::worst_case_duration(),
            "budget_for(\"fetch_page\", &default) = {:?} must exceed fetch_page::worst_case_duration() = {:?} \
             (else the generic backstop cuts the informative per-attempt message before fetch_page's own bound fires)",
            budget_for("fetch_page", &cfg),
            crate::tools::fetch_page::worst_case_duration()
        );
    }

    /// The shared `timeout_message` must produce the canonical text — one
    /// source of truth so `TimeBudget` and `fetch_page` agree on the wording
    /// the model self-corrects from.
    #[test]
    fn timeout_message_carries_tool_name_and_budget_seconds() {
        let msg = timeout_message("fetch_page", Duration::from_secs(35));
        assert!(
            msg.contains("⏱ TIMEOUT"),
            "canonical marker missing from: {msg}"
        );
        assert!(msg.contains("fetch_page"), "tool name missing from: {msg}");
        assert!(msg.contains("35s"), "budget seconds missing from: {msg}");
    }
}
