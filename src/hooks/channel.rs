//! Event channel and processor for streaming agent events.
//!
//! This module provides an async channel-based event streaming system that replaces
//! the old blocking Vec<Mutex<ToolEvent>> approach.

use crate::hooks::events::AgentEvent;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Event channel for streaming agent events (UNBOUNDED - no backpressure)
///
/// The design uses an unbounded channel so events won't be dropped under high load.
/// This is appropriate for the use case since we want all events to be processed.
#[derive(Clone)]
pub struct EventChannel {
    /// Sender for producing events
    sender: mpsc::UnboundedSender<AgentEvent>,
}

impl EventChannel {
    /// Create a new UNBOUNDED event channel (no capacity limit)
    pub fn new() -> (Self, mpsc::UnboundedReceiver<AgentEvent>) {
        // Unbounded channel - no backpressure, events won't be dropped
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }

    /// Send an event (never blocks - unbounded channel)
    pub fn send(&self, event: AgentEvent) -> Result<(), EventChannelError> {
        self.sender
            .send(event)
            .map_err(|_| EventChannelError::ChannelClosed)
    }

    /// Check if channel is closed
    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Get a clone of the sender for passing to hooks
    pub fn sender_clone(&self) -> mpsc::UnboundedSender<AgentEvent> {
        self.sender.clone()
    }
}

/// Create a new event channel
pub fn create_event_channel() -> (EventChannel, mpsc::UnboundedReceiver<AgentEvent>) {
    EventChannel::new()
}

/// Errors that can occur with event channels
#[derive(Debug, thiserror::Error)]
pub enum EventChannelError {
    #[error("Event channel has been closed")]
    ChannelClosed,
}

/// Trait for event handlers that process agent events
///
/// Implement this trait to create custom handlers for different event types.
/// The event processor holds a list of handlers and dispatches events to all of them.
pub trait EventHandler: Send + Sync {
    /// Handle an incoming agent event
    fn handle_event(&self, event: &AgentEvent);

    /// Get a name for debugging/logging purposes
    fn name(&self) -> &str;
}

/// Event processor that consumes events from the channel and dispatches to handlers
///
/// Uses a list of trait objects to allow decoupled handlers that implement the EventHandler trait.
pub struct EventProcessor {
    receiver: mpsc::UnboundedReceiver<AgentEvent>,
    handlers: Vec<Arc<dyn EventHandler>>,
}

impl EventProcessor {
    /// Create a new event processor with the given handlers
    pub fn new(
        receiver: mpsc::UnboundedReceiver<AgentEvent>,
        handlers: Vec<Arc<dyn EventHandler>>,
    ) -> Self {
        Self {
            receiver,
            handlers,
        }
    }

    /// Run the event processor (blocking call for use in spawned task)
    pub async fn run(&mut self) {
        while let Some(event) = self.receiver.recv().await {
            // Dispatch to all handlers
            for handler in &self.handlers {
                handler.handle_event(&event);
            }
        }
        tracing::info!("Event processor finished - channel closed");
    }
}

// ============================================================================
// Concrete Handler Implementations
// ============================================================================

use crate::hooks::{ModelPricing, SessionStats};

/// Cost tracking handler that calculates and logs token usage costs
pub struct CostHandler {
    stats: Arc<std::sync::Mutex<SessionStats>>,
    pricing: Arc<ModelPricing>,
}

impl CostHandler {
    /// Create a new cost handler with the given pricing and stats
    pub fn new(pricing: ModelPricing, stats: Arc<std::sync::Mutex<SessionStats>>) -> Self {
        Self { stats, pricing: Arc::new(pricing) }
    }
}

impl EventHandler for CostHandler {
    fn handle_event(&self, event: &AgentEvent) {
        if let AgentEvent::CompletionResponse { usage, .. } = event {
            // Calculate cost
            let cost = (usage.input_tokens as f64 * self.pricing.input_per_token)
                + (usage.output_tokens as f64 * self.pricing.output_per_token);

            // Use lock since handle_event is sync (can't be async in trait)
            // For async handling, users should spawn a task
            let mut stats = self.stats.lock().unwrap();
            stats.add_request(usage.input_tokens, usage.output_tokens, cost);

            tracing::info!(
                "Tokens: {} in / {} out | Cost: ${:.4} | Total: ${:.4}",
                usage.input_tokens,
                usage.output_tokens,
                cost,
                stats.total_cost
            );
        }
    }

    fn name(&self) -> &str {
        "CostHandler"
    }
}

