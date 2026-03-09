use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ThinkError {
    #[error("{0}")]
    Validation(String),
}

#[derive(Deserialize)]
pub struct ThinkArgs {
    thought: String,
}

#[derive(Serialize, Deserialize)]
pub struct ThinkTool;

impl Tool for ThinkTool {
    const NAME: &'static str = "think";
    type Error = ThinkError;
    type Args = ThinkArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "think".to_string(),
            description: "Use it when complex reasoning or brainstorming is needed.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": "Your thoughts. Use it when complex reasoning or brainstorming is needed. For example, if you explore the repo and discover the source of a bug, call this tool to brainstorm several unique ways of fixing the bug, and assess which change(s) are likely to be simplest and most effective."
                    }
                },
                "required": ["thought"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Log before execution
        tracing::info!(
            target: "peakbot",
            tool_type = "think",
            thought_length = args.thought.len(),
            "Think tool executed"
        );

        Ok(format!("Thinking: {}", args.thought))
    }
}