//! `PurposeGate`: a tool decorator that owns the cross-cutting `purpose`
//! concern for every wrapped tool (built-in and MCP).
//!
//! Tools no longer declare `purpose` themselves. The gate:
//! 1. injects a required `purpose` property into the inner tool's schema,
//! 2. strips `purpose` from the args before delegating (the inner tool /
//!    MCP server never declared it), and
//! 3. appends a soft nudge to the result when `purpose` is absent or blank
//!    — the task still runs.
//!
//! A tool that declares its own `purpose` parameter is left completely
//! untouched: schema, args, and result pass through.
//!
//! Implemented over `ToolDyn` (the dynamic-dispatch trait) so a missing
//! `purpose` can never produce a `JsonError` that skips the call: the gate
//! parses the args itself and a blank `purpose` is a coaching nudge, not a
//! hard error.

use rig_core::completion::ToolDefinition;
use rig_core::tool::{ToolDyn, ToolError};
use rig_core::wasm_compat::WasmBoxedFuture;
use serde_json::{Value, json};

// Keep name and description free of thought/reasoning wording: some models
// refuse requests whose tool inputs ask for it.

/// The field the gate injects into every wrapped tool's schema and strips
/// from its args before the inner call.
pub(crate) const PURPOSE_FIELD: &str = "purpose";

/// Description attached to the injected `purpose` schema property.
const DESCRIPTION: &str = "One-line statement of what this call is for.";

/// Soft reminder appended to a tool result when `purpose` was omitted.
const NUDGE: &str = "⚠️ Reminder: fill in the 'purpose' field — a one-line statement of \
     what this call is for. (Tool ran anyway.)";

/// Wraps any `Box<dyn ToolDyn>`, adding the `purpose` field + nudge.
pub struct PurposeGate {
    inner: Box<dyn ToolDyn>,
}

impl PurposeGate {
    pub fn wrap(inner: Box<dyn ToolDyn>) -> Self {
        Self { inner }
    }
}

/// A tool with its own `purpose` parameter keeps it: the gate must never
/// strip or demand a real argument.
fn owns_purpose(def: &ToolDefinition) -> bool {
    def.parameters
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|props| props.contains_key(PURPOSE_FIELD))
}

/// `purpose` counts as missing when absent, JSON null, or blank/whitespace.
fn purpose_missing(args: &Value) -> bool {
    match args.get(PURPOSE_FIELD) {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(_) => false,
    }
}

/// Inject a required `purpose` property into a tool schema. A tool that
/// declares its own `purpose` passes through unchanged.
fn inject_purpose(mut def: ToolDefinition) -> ToolDefinition {
    if owns_purpose(&def) {
        return def;
    }
    let Some(obj) = def.parameters.as_object_mut() else {
        return def;
    };

    if let Some(props) = obj
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        props.insert(
            PURPOSE_FIELD.to_string(),
            json!({ "type": "string", "description": DESCRIPTION }),
        );
    }

    if let Some(required) = obj
        .entry("required")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        && !required.iter().any(|v| v.as_str() == Some(PURPOSE_FIELD))
    {
        required.insert(0, json!(PURPOSE_FIELD));
    }

    def
}

impl ToolDyn for PurposeGate {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition(&self, prompt: String) -> WasmBoxedFuture<'_, ToolDefinition> {
        Box::pin(async move { inject_purpose(self.inner.definition(prompt).await) })
    }

    fn call(&self, args: String) -> WasmBoxedFuture<'_, Result<String, ToolError>> {
        Box::pin(async move {
            // Ownership is read from the inner schema on every call — no
            // state to keep in sync, and every definition here is an
            // in-memory builder, so it is free next to the round-trip.
            let def = self.inner.definition(String::new()).await;
            if owns_purpose(&def) {
                return self.inner.call(args).await;
            }

            // Non-object args (e.g. "null") aren't gateable — pass through.
            let Some(mut value) = serde_json::from_str::<Value>(&args)
                .ok()
                .filter(Value::is_object)
            else {
                return self.inner.call(args).await;
            };

            let missing = purpose_missing(&value);
            // `purpose` is synthetic — the inner tool/MCP server never
            // declared it, and a strict server may reject unknown params.
            value.as_object_mut().unwrap().remove(PURPOSE_FIELD);

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
