//! `ThoughtGate`: a tool decorator that owns the cross-cutting `thought`
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
pub struct ThoughtGate {
    inner: Box<dyn ToolDyn>,
}

impl ThoughtGate {
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

impl ToolDyn for ThoughtGate {
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

    fn gate() -> ThoughtGate {
        ThoughtGate::wrap(Box::new(Echo))
    }

    #[tokio::test]
    async fn definition_injects_thought_as_required_property() {
        let def = gate().definition(String::new()).await;
        let props = def.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("thought"), "thought property injected");
        assert!(props.contains_key("path"), "inner property preserved");

        let required = def.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "thought"), "thought required");
        assert!(required.iter().any(|v| v == "path"), "inner required kept");
    }

    #[tokio::test]
    async fn name_is_inner_name() {
        assert_eq!(gate().name(), "echo");
    }

    #[tokio::test]
    async fn missing_thought_runs_and_appends_nudge() {
        let out = gate()
            .call(json!({ "path": "/tmp/x" }).to_string())
            .await
            .expect("runs despite missing thought");
        assert!(out.ends_with(NUDGE), "nudge appended: {out}");
        assert!(out.contains("\"path\":\"/tmp/x\""), "inner ran");
        assert!(!out.contains("thought\":"), "thought stripped before inner");
    }

    #[tokio::test]
    async fn present_thought_is_clean_passthrough() {
        let out = gate()
            .call(json!({ "thought": "doing X", "path": "/tmp/x" }).to_string())
            .await
            .unwrap();
        assert!(!out.contains(NUDGE), "no nudge when thought present");
        assert!(!out.contains("thought"), "thought stripped before inner");
        assert!(out.contains("\"path\":\"/tmp/x\""));
    }

    #[tokio::test]
    async fn blank_and_null_thought_count_as_missing() {
        for bad in [json!("   "), json!(""), Value::Null] {
            let out = gate()
                .call(json!({ "thought": bad, "path": "/p" }).to_string())
                .await
                .unwrap();
            assert!(out.ends_with(NUDGE), "blank/null thought nudges: {bad:?}");
        }
    }

    #[tokio::test]
    async fn non_object_args_pass_through_without_nudge() {
        let out = gate().call("null".to_string()).await.unwrap();
        assert!(!out.contains(NUDGE), "non-object args are not gated");
        assert_eq!(out, "null");
    }

    #[tokio::test]
    async fn idempotent_when_inner_already_has_thought() {
        struct HasThought;
        impl Tool for HasThought {
            const NAME: &'static str = "ht";
            type Error = std::convert::Infallible;
            type Args = Value;
            type Output = String;
            async fn definition(&self, _p: String) -> ToolDefinition {
                ToolDefinition {
                    name: "ht".to_string(),
                    description: "ht".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": { "thought": { "type": "string", "description": "own" } },
                        "required": ["thought"]
                    }),
                }
            }
            async fn call(&self, args: Value) -> Result<String, Self::Error> {
                Ok(args.to_string())
            }
        }

        let def = ThoughtGate::wrap(Box::new(HasThought))
            .definition(String::new())
            .await;
        assert_eq!(
            def.parameters["properties"]["thought"]["description"],
            "own"
        );
        let required = def.parameters["required"].as_array().unwrap();
        assert_eq!(
            required.iter().filter(|v| *v == "thought").count(),
            1,
            "thought not duplicated in required"
        );
    }
}
