//! Intent-aware message processing for V2
//!
//! This module implements smart message routing based on detected user intent,
//! execution status, and pending slots. It provides the core intelligence for
//! determining whether a message is a new task or answering existing
//! parameters.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use tracing::{debug, info};

use crate::magician_v2::{
    orchestrator::{StateTransitionService, WorkflowEvent},
    storage::{V2ConversationStore, V2Slot, WaitingState},
    QueryIntent, UnifiedQueryAnalysis, UnifiedQueryAnalyzer,
};
use runtime_core::ExecutionContext;

/// Smart message processor with intent detection
pub struct IntentAwareProcessor {
    analyzer: Arc<UnifiedQueryAnalyzer>,
    store: Arc<dyn V2ConversationStore>,
    state_transition: Arc<dyn StateTransitionService>,
}

impl IntentAwareProcessor {
    pub fn new(
        analyzer: Arc<UnifiedQueryAnalyzer>,
        store: Arc<dyn V2ConversationStore>,
        state_transition: Arc<dyn StateTransitionService>,
    ) -> Self {
        Self {
            analyzer,
            store,
            state_transition,
        }
    }

    /// Analyze message with full context (pending slots, history, status)
    /// NOW USES SINGLE LLM CALL for intent detection + slot matching + query
    /// analysis
    pub async fn analyze_with_context(
        &self,
        execution_id: &str,
        message: &str,
        correlation_id: &str,
        execution_context: &ExecutionContext,
        available_tools: Option<&[runtime_core::ToolInfo]>,
    ) -> Result<UnifiedQueryAnalysis> {
        debug!(
            "[MAGICIAN-V2-INTENT] Analyzing message with context for execution {}",
            execution_id
        );

        // Get execution status
        let status = self.store.get_execution_status(execution_id).await?;
        debug!("[MAGICIAN-V2-INTENT] Execution status: {:?}", status);

        // Get pending slots
        let pending_slots = self.store.get_pending_slots(execution_id).await?;
        debug!(
            "[MAGICIAN-V2-INTENT] Found {} pending slots",
            pending_slots.len()
        );

        // Get conversation history
        let turns = self.store.get_turns(execution_id).await?;
        debug!(
            "[MAGICIAN-V2-INTENT] Found {} turns in history",
            turns.len()
        );

        // Clone execution context for downstream analysis.
        let exec_context = execution_context.clone();

        // Build conversation context
        let conversation_context = crate::magician_v2::query_analysis::ConversationContext {
            execution_status: status,
            pending_slots,
            recent_turns: turns,
        };

        // ✅ NEW: Single LLM call that does EVERYTHING:
        // - Intent detection (NewTask, AnswerElicitation, StatusQuery, etc.)
        // - Slot matching (if AnswerElicitation)
        // - Query analysis (complexity, categories, dependencies, entities)
        let analysis = self
            .analyzer
            .analyze_query_with_context(
                message,
                &exec_context,
                available_tools,
                conversation_context,
                Some(execution_id),
                Some(correlation_id),
            )
            .await?;

        info!(
            "[MAGICIAN-V2-INTENT] Unified analysis complete: intent={:?}, slot_match={}, \
             complexity={:.2}",
            analysis.intent,
            analysis.slot_match.is_some(),
            analysis.complexity.score
        );

        Ok(analysis)
    }

    /// Handle a message based on detected intent
    pub async fn handle_message(
        &self,
        execution_id: &str,
        message: &str,
        correlation_id: &str,
        execution_context: &ExecutionContext,
        available_tools: Option<&[runtime_core::ToolInfo]>,
    ) -> Result<MessageHandlingResult> {
        let analysis = self
            .analyze_with_context(
                execution_id,
                message,
                correlation_id,
                execution_context,
                available_tools,
            )
            .await?;

        match analysis.intent {
            QueryIntent::NewTask => {
                info!("[MAGICIAN-V2-INTENT] Handling new task request");
                Ok(MessageHandlingResult::NewTask { analysis })
            },

            QueryIntent::AnswerElicitation => {
                info!("[MAGICIAN-V2-INTENT] Handling slot answer");
                if let Some(ref slot_match) = analysis.slot_match {
                    if slot_match.is_confident() {
                        // Automatically update the slot
                        self.store
                            .update_slot_answer(
                                execution_id,
                                &slot_match.slot_id,
                                slot_match.extracted_value.clone(),
                            )
                            .await?;

                        // Check if all required slots are filled
                        let pending = self.store.get_pending_slots(execution_id).await?;
                        let all_filled = pending.iter().filter(|s| s.required).count() == 0;

                        if all_filled {
                            // Move to executing state using state machine
                            self.state_transition
                                .transition_status(execution_id, WorkflowEvent::AllSlotsAnswered)
                                .await
                                .map_err(|e| anyhow!("State transition failed: {}", e))?;

                            Ok(MessageHandlingResult::SlotAnswered {
                                analysis: analysis.clone(),
                                slot_id: slot_match.slot_id.clone(),
                                value: slot_match.extracted_value.clone(),
                                ready_to_execute: true,
                            })
                        } else {
                            Ok(MessageHandlingResult::SlotAnswered {
                                analysis: analysis.clone(),
                                slot_id: slot_match.slot_id.clone(),
                                value: slot_match.extracted_value.clone(),
                                ready_to_execute: false,
                            })
                        }
                    } else if slot_match.needs_clarification() {
                        Ok(MessageHandlingResult::NeedsClarification {
                            analysis: analysis.clone(),
                            slot_id: slot_match.slot_id.clone(),
                            reasoning: slot_match.reasoning.clone(),
                        })
                    } else {
                        Ok(MessageHandlingResult::AmbiguousAnswer {
                            analysis: analysis.clone(),
                            message: "Could not confidently match your answer to a parameter"
                                .to_string(),
                        })
                    }
                } else {
                    Ok(MessageHandlingResult::AmbiguousAnswer {
                        analysis,
                        message: "Expected a parameter answer but couldn't determine which one"
                            .to_string(),
                    })
                }
            },

            QueryIntent::StatusQuery => {
                info!("[MAGICIAN-V2-INTENT] Handling status query");
                let status = self.store.get_execution_status(execution_id).await?;
                let pending_slots = self.store.get_pending_slots(execution_id).await?;
                Ok(MessageHandlingResult::StatusResponse {
                    analysis,
                    status,
                    pending_slots,
                })
            },

            QueryIntent::Cancellation => {
                info!("[MAGICIAN-V2-INTENT] Handling cancellation");
                // Cancellation is a control-tree operation, not merely a
                // status transition. The canonical path signals the live
                // generation, fences terminal publication, clears exact pause
                // authority, and cascades to currently owned children before
                // publishing the terminal runtime state.
                let cancelled = self
                    .state_transition
                    .cancel_execution_tree(execution_id)
                    .await
                    .map_err(|e| anyhow!("Execution cancellation failed: {}", e))?;
                if !cancelled {
                    return Err(anyhow!(
                        "Execution {} is already terminal and cannot be cancelled",
                        execution_id
                    ));
                }
                Ok(MessageHandlingResult::Cancelled { analysis })
            },

            QueryIntent::ContinueWorkflow => {
                info!("[MAGICIAN-V2-INTENT] Handling continue/resume");
                let resumed = self
                    .state_transition
                    .resume_execution_tree(execution_id)
                    .await
                    .map_err(|e| anyhow!("Exact resume failed: {}", e))?;
                if !resumed {
                    return Err(anyhow!(
                        "Exact resume refused because execution {} is not in a resumable paused state",
                        execution_id
                    ));
                }
                Ok(MessageHandlingResult::Resumed { analysis })
            },

            QueryIntent::Clarification => {
                info!("[MAGICIAN-V2-INTENT] Handling clarification request");
                Ok(MessageHandlingResult::ClarificationNeeded { analysis })
            },
        }
    }
}

/// Result of handling a user message
#[derive(Debug, Clone)]
pub enum MessageHandlingResult {
    /// User started a new task
    NewTask { analysis: UnifiedQueryAnalysis },

    /// User answered a slot successfully
    SlotAnswered {
        analysis: UnifiedQueryAnalysis,
        slot_id: String,
        value: serde_json::Value,
        ready_to_execute: bool,
    },

    /// Need to ask user for clarification
    NeedsClarification {
        analysis: UnifiedQueryAnalysis,
        slot_id: String,
        reasoning: String,
    },

    /// Answer was too ambiguous
    AmbiguousAnswer {
        analysis: UnifiedQueryAnalysis,
        message: String,
    },

    /// User asked for status
    StatusResponse {
        analysis: UnifiedQueryAnalysis,
        status: WaitingState,
        pending_slots: Vec<V2Slot>,
    },

    /// User cancelled the workflow
    Cancelled { analysis: UnifiedQueryAnalysis },

    /// User wants to resume
    Resumed { analysis: UnifiedQueryAnalysis },

    /// User needs clarification about something
    ClarificationNeeded { analysis: UnifiedQueryAnalysis },
}

#[cfg(test)]
mod tests {
    #[test]
    fn natural_language_cancellation_uses_the_full_control_tree_owner() {
        let source = include_str!("intent_handler.rs");
        let cancellation = source
            .split("QueryIntent::Cancellation =>")
            .nth(1)
            .and_then(|tail| tail.split("QueryIntent::ContinueWorkflow =>").next())
            .expect("cancellation routing branch");
        assert!(cancellation.contains("cancel_execution_tree(execution_id)"));
        assert!(!cancellation.contains("WorkflowEvent::UserCancelled"));
    }
}
