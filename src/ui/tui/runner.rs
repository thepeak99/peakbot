//! TUI Agent Runner
//!
//! This module provides the integration between the TUI and the LLM agent.
//! It processes user input (messages and commands) and coordinates with the UI.

use crate::ui::app_state::ChatMessage;
use crate::ui::state_manager::StateManager;
use crate::ui::ui_trait::UiAction;
use crate::{Config, CostTracker, DynAgent, ProviderInfo, SkillRegistry};
use anyhow::Result;
use rig::completion::Message;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Events emitted by TuiAgentRunner for UI updates
#[derive(Debug, Clone)]
pub enum RunnerEvent {
    /// Agent started processing
    AgentBusy,
    /// Agent finished processing
    AgentIdle,
    /// Error occurred
    Error(String),
    /// Stats updated
    StatsUpdated,
    /// Exit requested
    Exit,
}

/// TUI Agent Runner - integrates the LLM agent with the TUI
///
/// This struct runs in a separate task and processes UiActions from the TUI,
/// calling the agent for messages and executing commands locally.
pub struct TuiAgentRunner {
    /// The agent for processing messages
    agent: Arc<DynAgent>,
    
    /// Configuration
    config: Config,
    
    /// Provider info
    provider_info: ProviderInfo,
    
    /// Skills registry
    skills: SkillRegistry,
    
    /// Cost tracker for stats
    cost_tracker: CostTracker,
    
    /// State manager for UI updates
    state_manager: Arc<StateManager>,
    
    /// Channel to send events back to the TUI
    event_sender: Option<mpsc::UnboundedSender<RunnerEvent>>,
    
    /// Chat history for the agent
    chat_history: Vec<Message>,
    
    /// Whether the runner is processing a request
    is_busy: bool,
}

impl TuiAgentRunner {
    /// Create a new TuiAgentRunner
    pub fn new(
        agent: DynAgent,
        config: Config,
        provider_info: ProviderInfo,
        skills: SkillRegistry,
        cost_tracker: CostTracker,
        state_manager: Arc<StateManager>,
        event_sender: Option<mpsc::UnboundedSender<RunnerEvent>>,
    ) -> Self {
        Self {
            agent: Arc::new(agent),
            config,
            provider_info,
            skills,
            cost_tracker,
            state_manager,
            event_sender,
            chat_history: Vec::new(),
            is_busy: false,
        }
    }

    /// Process a UiAction
    pub async fn process_action(&mut self, action: UiAction) -> Result<()> {
        match action {
            UiAction::SendMessage(msg) => {
                self.handle_message(msg).await?;
            }
            UiAction::ExecuteCommand(cmd) => {
                self.handle_command(&cmd).await?;
            }
            UiAction::Exit => {
                self.handle_exit().await?;
            }
            _ => {
                // Other actions are handled by the TUI directly
            }
        }
        Ok(())
    }

    /// Handle a message - send to the agent
    async fn handle_message(&mut self, msg: String) -> Result<()> {
        if self.is_busy {
            self.emit_event(RunnerEvent::Error("Agent is busy".to_string()));
            return Ok(());
        }

        self.is_busy = true;
        self.emit_event(RunnerEvent::AgentBusy);

        // Add user message to chat
        let user_msg = ChatMessage::user(msg.clone());
        self.state_manager.update_chat(user_msg);

        // Add loading indicator
        self.state_manager.set_loading(true);

        // Call the agent
        match self.agent.prompt_with_history(&msg, &mut self.chat_history).await {
            Ok(response) => {
                // Add assistant response to chat
                let assistant_msg = ChatMessage::assistant(response);
                self.state_manager.update_chat(assistant_msg);
            }
            Err(e) => {
                // Add error message
                let error_msg = ChatMessage::error(format!("Error: {}", e));
                self.state_manager.update_chat(error_msg);
                self.emit_event(RunnerEvent::Error(e.to_string()));
            }
        }

        self.is_busy = false;
        self.state_manager.set_loading(false);
        self.emit_event(RunnerEvent::AgentIdle);
        
        // Update stats
        self.update_stats();

        Ok(())
    }

    /// Handle a slash command
    async fn handle_command(&mut self, cmd: &str) -> Result<()> {
        let cmd_lower = cmd.to_lowercase();
        
        match cmd_lower.as_str() {
            "/exit" | "/quit" | "/q" => {
                self.handle_exit().await?;
            }
            "/stats" => {
                self.show_stats().await?;
            }
            "/context" => {
                self.show_context().await?;
            }
            "/compact" => {
                self.force_compact().await?;
            }
            "/conversations" | "/history" => {
                self.list_conversations().await?;
            }
            "/clear" => {
                self.clear_chat().await?;
            }
            "/help" => {
                self.show_help().await?;
            }
            _ => {
                let msg = ChatMessage::system(format!("Unknown command: {}. Type /help for available commands.", cmd));
                self.state_manager.update_chat(msg);
            }
        }
        
        Ok(())
    }

    /// Handle exit command
    async fn handle_exit(&mut self) -> Result<()> {
        let msg = ChatMessage::system("Goodbye!".to_string());
        self.state_manager.update_chat(msg);
        
        // Show final stats
        self.show_stats().await?;
        
        self.emit_event(RunnerEvent::Exit);
        Ok(())
    }

    /// Show session stats
    async fn show_stats(&mut self) -> Result<()> {
        let mut stats_text = format!(
            "=== Session Statistics ===\nProvider: {}\nModel: {}",
            self.provider_info.name,
            self.provider_info.model
        );

        if let Some(summary) = self.cost_tracker.get_session_summary() {
            stats_text.push_str(&format!("\n{}", summary));
        } else {
            stats_text.push_str("\nToken tracking not available for this provider.");
        }

        let msg = ChatMessage::system(stats_text);
        self.state_manager.update_chat(msg);
        self.emit_event(RunnerEvent::StatsUpdated);
        Ok(())
    }

    /// Show context status
    async fn show_context(&mut self) -> Result<()> {
        let msg = ChatMessage::system("Context status: View context usage in the status bar.".to_string());
        self.state_manager.update_chat(msg);
        Ok(())
    }

    /// Force context compaction
    async fn force_compact(&mut self) -> Result<()> {
        let msg = ChatMessage::system("Context compaction: Requested (auto-compaction happens automatically when needed)".to_string());
        self.state_manager.update_chat(msg);
        Ok(())
    }

    /// List saved conversations
    async fn list_conversations(&mut self) -> Result<()> {
        let msg = ChatMessage::system("Conversation persistence: Available in full mode. Use REPL for conversation management.".to_string());
        self.state_manager.update_chat(msg);
        Ok(())
    }

    /// Clear chat history
    async fn clear_chat(&mut self) -> Result<()> {
        self.chat_history.clear();
        self.state_manager.clear_chat();
        let msg = ChatMessage::system("Chat cleared.".to_string());
        self.state_manager.update_chat(msg);
        Ok(())
    }

    /// Show help
    async fn show_help(&mut self) -> Result<()> {
        let help_text = r#"Available commands:
/stats       - Show session statistics (tokens, cost)
/context     - Show context usage status
/compact     - Force context compaction
/clear       - Clear chat history
/help        - Show this help message
/exit        - Exit the application
/quit        - Exit the application

Keyboard shortcuts:
Ctrl+Q       - Quit
Ctrl+T       - Toggle TODO panel
Esc          - Cancel popup
Tab          - Select command
Up/Down      - Navigate commands
Enter        - Send message"#;

        let msg = ChatMessage::system(help_text.to_string());
        self.state_manager.update_chat(msg);
        Ok(())
    }

    /// Update stats in state manager
    fn update_stats(&mut self) {
        let stats = self.cost_tracker.get_session_stats();
        self.state_manager.update_stats(&stats);
    }

    /// Emit an event
    fn emit_event(&self, event: RunnerEvent) {
        if let Some(ref sender) = self.event_sender {
            let _ = sender.send(event);
        }
    }

    /// Check if runner is busy
    pub fn is_busy(&self) -> bool {
        self.is_busy
    }
}

/// Extension trait for AppState to add helper methods
pub trait AppStateExt {
    fn clear_chat(&self);
}

impl AppStateExt for StateManager {
    fn clear_chat(&self) {
        let mut state = self.get_state();
        state.chat.clear();
        self.update_state(state);
    }
}
