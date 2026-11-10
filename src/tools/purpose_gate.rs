//! `PurposeGate`: a tool decorator that owns the cross-cutting `thought`
//! concern for every wrapped tool (built-in and MCP).
//!
//! Tools no longer declare `thought` themselves. The gate:
//! 1. injects a required `thought` property into the inner tool's schema,
//! 2. strips `thought` from the args before delegating (the inner tool /
//!    MCP server never declared it), and
//! 3. appends a soft nudge to the result when `thought` is absent or blank
//!    — the task still runs.
//!
//! Implemented over `ToolDyn` (the dynamic-dispatch trait) so a missing
//! `thought` can never produce a `JsonError` that skips the call: the gate
//! parses the args itself and a blank `thought` is a coaching nudge, not a
//! hard error.

use rig_core::completion::ToolDefinition;
use rig_core::tool::{ToolDyn, ToolError};
use rig_core::wasm_compat::WasmBoxedFuture;
use serde_json::{Value, json};

/// Soft reminder appended to a tool result when `thought` was omitted.
const NUDGE: &str = "⚠️ Reminder: provide the 'thought' field — briefly explain what you're \
     about to do and why, before acting. (Tool ran anyway.)";

/// Description attached to the injected `thought` schema property.
const THOUGHT_DESC: &str = "Briefly explain what you're about to do and why, before acting.";

/// Wraps any `Box<dyn ToolDyn>`, adding the `thought` field + nudge.
pub struct PurposeGate {
    inner: Box<dyn ToolDyn>,
}

impl PurposeGate {
    pub fn wrap(inner: Box<dyn ToolDyn>) -> Self {
        Self { inner }
    }
}

/// `thought` counts as missing when absent, JSON null, or blank/whitespace.
fn thought_missing(args: &Value) -> bool {
    match args.get("thought") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(_) => false,
    }
}

/// Inject a required `thought` property into a tool schema, idempotently.
fn inject_thought(mut def: ToolDefinition) -> ToolDefinition {
    let Some(obj) = def.parameters.as_object_mut() else {
        return def;
    };

    if let Some(props) = obj
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        && !props.contains_key("thought")
    {
        props.insert(
            "thought".to_string(),
            json!({ "type": "string", "description": THOUGHT_DESC }),
        );
    }

    if let Some(required) = obj
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        && !required.iter().any(|v| v.as_str() == Some("thought"))
    {
        required.insert(0, json!("thought"));
    }

    def
}

impl ToolDyn for PurposeGate {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        Box::pin(async move { inject_thought(self.inner.definition(prompt).await) })
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        Box::pin(async move {
            // Non-object args (e.g. "null") aren't gateable — pass through.
            let Some(mut value) = serde_json::from_str::<Value>(&args)
                .ok()
                .filter(Value::is_object)
            else {
                return self.inner.call(args).await;
            };

            let missing = thought_missing(&value);
            // `thought` is synthetic — the inner tool/MCP server never
            // declared it, and a strict server may reject unknown params.
            value.as_object_mut().unwrap().remove("thought");

            let mut output = self.inner.call(value.to_string()).await?;
            if missing {
                output.push_str("\n\n");
                output.push_str(NUDGE);
            }
            Ok(output)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::tool::Tool;

    /// A minimal inner tool: declares `path` only, echoes its args back so
    /// tests can assert exactly what the gate forwarded.
    struct Echo;

    impl Tool for Echo {
        const NAME: &'static str = "echo";
        type Error = std::convert::Infallible;
        type Args = Value;
        type Output = String;

        async fn definition(&self, _p: String) -> ToolDefinition {
            ToolDefinition {
                name: "echo".to_string(),
                description: "echo".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }),
            }
        }

        async fn call(&self, args: Value) -> Result<String, Self::Error> {
            Ok(args.to_string())
        }
    }

    fn gate() -> PurposeGate {
        PurposeGate::wrap(Box::new(Echo))
    }

    #[test]
    fn nudge_text_is_locked() {
        assert_eq!(
            NUDGE,
            "⚠️ Reminder: fill in the 'purpose' field — a one-line statement of \
             what this call is for. (Tool ran anyway.)"
        );
    }

    #[tokio::test]
    async fn definition_injects_required_purpose() {
        let def = gate().definition(String::new()).await;
        let props = def.parameters["properties"].as_object().unwrap();
        assert_eq!(
            props["purpose"],
            json!({ "type": "string", "description": "One-line statement of what this call is for." }),
            "injected property is exact: {props:?}"
        );
        assert!(props.contains_key("path"), "inner property preserved");
        assert!(
            !props.contains_key("thought"),
            "no `thought` in properties: {props:?}"
        );

        let required = def.parameters["required"].as_array().unwrap();
        assert_eq!(
            required[0],
            json!("purpose"),
            "purpose first in required: {required:?}"
        );
        assert!(required.iter().any(|v| v == "path"), "inner required kept");
        assert!(
            !required.iter().any(|v| v == "thought"),
            "no `thought` in required: {required:?}"
        );
    }

    #[tokio::test]
    async fn name_is_inner_name() {
        assert_eq!(gate().name(), "echo");
    }

    #[tokio::test]
    async fn missing_purpose_runs_and_appends_nudge() {
        let out = gate()
            .call(json!({ "path": "/tmp/x" }).to_string())
            .await
            .expect("runs despite missing purpose");
        assert!(out.ends_with(NUDGE), "nudge appended: {out}");
        assert!(out.contains("\"path\":\"/tmp/x\""), "inner ran");
    }

    #[tokio::test]
    async fn present_purpose_is_stripped_without_nudge() {
        let out = gate()
            .call(json!({ "purpose": "doing X", "path": "/tmp/x" }).to_string())
            .await
            .unwrap();
        assert!(!out.contains(NUDGE), "no nudge when purpose present: {out}");
        assert!(
            !out.contains("purpose"),
            "purpose stripped before inner: {out}"
        );
        assert!(out.contains("\"path\":\"/tmp/x\""));
    }

    #[tokio::test]
    async fn blank_and_null_purpose_count_as_missing() {
        for bad in [json!("   "), json!(""), Value::Null] {
            let out = gate()
                .call(json!({ "purpose": bad, "path": "/p" }).to_string())
                .await
                .unwrap();
            assert!(out.ends_with(NUDGE), "blank/null purpose nudges: {bad:?}");
        }
    }

    #[tokio::test]
    async fn non_object_args_pass_through_without_nudge() {
        let out = gate().call("null".to_string()).await.unwrap();
        assert!(!out.contains(NUDGE), "non-object args are not gated");
        assert_eq!(out, "null");
    }

    /// Inner tool that declares its own `purpose` parameter: a real
    /// argument, so the gate must never strip, demand, or nudge it.
    struct OwnsPurpose;

    impl Tool for OwnsPurpose {
        const NAME: &'static str = "owns";
        type Error = std::convert::Infallible;
        type Args = Value;
        type Output = String;
        async fn definition(&self, _p: String) -> ToolDefinition {
            ToolDefinition {
                name: "owns".to_string(),
                description: "owns".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "purpose": { "type": "string", "description": "own" },
                        "x": { "type": "integer" }
                    },
                    "required": ["x"]
                }),
            }
        }
        async fn call(&self, args: Value) -> Result<String, Self::Error> {
            Ok(args.to_string())
        }
    }

    #[tokio::test]
    async fn tool_that_owns_purpose_is_left_untouched() {
        let gated = PurposeGate::wrap(Box::new(OwnsPurpose));

        let def = gated.definition(String::new()).await;
        let inner = rig_core::tool::Tool::definition(&OwnsPurpose, String::new()).await;
        assert_eq!(
            def.parameters, inner.parameters,
            "gate is transparent for a tool that owns `purpose` (description, required): {def:?}"
        );

        let out = gated
            .call(json!({ "purpose": "real", "x": 1 }).to_string())
            .await
            .unwrap();
        assert!(
            out.contains("\"purpose\":\"real\""),
            "args forwarded verbatim: {out}"
        );
        assert!(!out.contains(NUDGE), "never nudges an owning tool: {out}");

        let out = gated.call(json!({ "x": 1 }).to_string()).await.unwrap();
        assert!(
            !out.contains(NUDGE),
            "the gate's nudge is not about the tool's own `purpose`: {out}"
        );
    }

    #[tokio::test]
    async fn legacy_thought_is_forwarded_and_does_not_satisfy_the_gate() {
        let out = gate()
            .call(json!({ "thought": "old habit", "path": "/p" }).to_string())
            .await
            .unwrap();
        assert!(
            out.contains("\"thought\":\"old habit\""),
            "legacy `thought` forwarded untouched: {out}"
        );
        assert!(
            out.ends_with(NUDGE),
            "`purpose` is missing, so nudge: {out}"
        );

        let out = gate()
            .call(json!({ "thought": "t", "purpose": "p", "path": "/p" }).to_string())
            .await
            .unwrap();
        assert!(
            out.contains("\"thought\":\"t\""),
            "`thought` forwarded: {out}"
        );
        assert!(!out.contains("purpose"), "`purpose` stripped: {out}");
        assert!(
            !out.contains(NUDGE),
            "`purpose` present, so no nudge: {out}"
        );
    }
}
