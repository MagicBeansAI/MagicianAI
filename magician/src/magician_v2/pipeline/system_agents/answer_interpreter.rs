//! Pipeline agent wrapper for [`AnswerInterpreter`].
//!
//! Reads an `ElicitationResult` artifact for question context and the
//! [`PipelineContext::user_answer`] field, delegates to the underlying
//! interpreter, and produces an `InterpretedAnswer` artifact.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json;
use tracing::{debug, info};

use crate::magician_v2::{
    ask_loop::answer_interpreter::{AnswerInterpretationLLM, AnswerInterpreter, InterpretedAnswer},
    execution::PromptIdentityContext,
    pipeline::{
        agent::{
            PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext,
            AGENT_ID_ANSWER_INTERPRETER,
        },
        artifact::{AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION},
    },
};

const PROMPT_IDENTITY_ARTIFACT_KIND: &str = "prompt_identity_context";

/// Pipeline agent that wraps [`AnswerInterpreter`].
///
/// Reads `ElicitationResult` artifacts to derive question context, takes the
/// user's answer from [`PipelineContext::user_answer`], and produces an
/// `InterpretedAnswer` artifact.
pub struct AnswerInterpreterAgent<L: AnswerInterpretationLLM> {
    interpreter: Arc<AnswerInterpreter<L>>,
}

impl<L: AnswerInterpretationLLM> AnswerInterpreterAgent<L> {
    /// Create a new wrapper around an existing `AnswerInterpreter`.
    pub fn new(interpreter: Arc<AnswerInterpreter<L>>) -> Self {
        Self { interpreter }
    }
}

#[async_trait]
impl<L> PipelineAgent for AnswerInterpreterAgent<L>
where
    L: AnswerInterpretationLLM + 'static,
{
    fn agent_id(&self) -> &str {
        AGENT_ID_ANSWER_INTERPRETER
    }

    fn required_inputs(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::ElicitationResult]
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::InterpretedAnswer]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        // ---- Extract question_text from the ElicitationResult artifact ----
        //
        // I-05: When context.question_id is set, search recommended_questions for
        // the matching question so the answer is correlated to the exact question
        // asked, not just the most-recent elicitation.
        //
        // Search order:
        //   1. If question_id is Some: find matching question in recommended_questions.
        //   2. Primary (no question_id): top-level "question_text" key projected by ElicitorAgent.
        //   3. Fallback1: recommended_questions[0]["question_text"].
        //   4. Fallback2: context.query.
        let question_text = store
            .latest_of_type(&ArtifactType::ElicitationResult)
            .and_then(|a| {
                // I-05: question_id-targeted lookup
                if let Some(ref qid) = context.question_id {
                    let matched = a
                        .content
                        .get("recommended_questions")
                        .and_then(|arr| arr.as_array())
                        .and_then(|arr| {
                            arr.iter().find(|q| {
                                q.get("source_slot_id")
                                    .and_then(|v| v.as_str())
                                    .map(|id| id == qid)
                                    .unwrap_or(false)
                            })
                        })
                        .and_then(|q| q.get("question_text"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    if matched.is_some() {
                        return matched;
                    }
                    // Fall through to generic extraction if question_id not found
                }
                // Primary: projected top-level key (set by ElicitorAgent)
                if let Some(v) = a.content.get("question_text") {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
                // Fallback: recommended_questions[0]["question_text"]
                a.content
                    .get("recommended_questions")
                    .and_then(|arr| arr.get(0))
                    .and_then(|q| q.get("question_text"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| context.query.clone());

        // ---- Extract user answer ----
        let user_answer = match &context.user_answer {
            Some(answer) => answer.clone(),
            None => {
                // No user answer available -- produce a pass-through artifact
                info!(
                    "[PIPELINE:answer-interpreter] No user_answer in context; emitting pass-through artifact"
                );
                let passthrough = InterpretedAnswer {
                    answer_type:
                        crate::magician_v2::ask_loop::answer_interpreter::AnswerType::Ambiguous,
                    slots: vec![],
                    correction_target: None,
                    correction_value: None,
                    clarification_text: None,
                    rejection_reason: None,
                    confidence: 0.0,
                    requires_replan: false,
                    reasoning: "No user answer provided; pass-through".to_string(),
                };

                let artifact_id = uuid::Uuid::new_v4().to_string();
                let artifact = AgentArtifact {
                    artifact_id: artifact_id.clone(),
                    artifact_type: ArtifactType::InterpretedAnswer,
                    producer_agent_id: self.agent_id().to_string(),
                    producer_cycle_id: context.cycle_id.clone(),
                    content: serde_json::to_value(&passthrough)
                        .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
                    schema_version: ARTIFACT_SCHEMA_VERSION,
                    produced_at: Utc::now(),
                    render_hints: None,
                };
                store.put(artifact);
                return Ok(PipelineAgentResult::Completed {
                    artifact_ids: vec![artifact_id],
                });
            },
        };

        debug!(
            "[PIPELINE:answer-interpreter] question_text='{}', user_answer='{}'",
            question_text, user_answer
        );

        // ---- Derive optional context string from the ElicitationResult content ----
        let slot_context = store
            .latest_of_type(&ArtifactType::ElicitationResult)
            .map(|a| a.content.to_string());
        let prompt_identity = load_prompt_identity(store);

        // ---- Call the underlying service ----
        let interpreted = self
            .interpreter
            .interpret_answer_with_identity(
                &question_text,
                &user_answer,
                slot_context.as_deref(),
                prompt_identity.as_ref(),
            )
            .await
            .map_err(|e| PipelineAgentError::ServiceError(e.to_string()))?;

        info!(
            "[PIPELINE:answer-interpreter] Interpreted as {:?} confidence={:.2}",
            interpreted.answer_type, interpreted.confidence
        );

        // ---- Store the artifact ----
        let artifact_id = uuid::Uuid::new_v4().to_string();
        let artifact = AgentArtifact {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::InterpretedAnswer,
            producer_agent_id: self.agent_id().to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content: serde_json::to_value(&interpreted)
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        };
        store.put(artifact);

        Ok(PipelineAgentResult::Completed {
            artifact_ids: vec![artifact_id],
        })
    }
}

fn load_prompt_identity(store: &ArtifactStore) -> Option<PromptIdentityContext> {
    let artifact = store.latest_of_type(&ArtifactType::Custom(
        PROMPT_IDENTITY_ARTIFACT_KIND.to_string(),
    ))?;
    artifact.deserialize_content::<PromptIdentityContext>().ok()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// Concrete mock to satisfy the AnswerInterpretationLLM bound.
    struct MockInterpretLLM;

    #[async_trait]
    impl AnswerInterpretationLLM for MockInterpretLLM {
        async fn interpret(&self, _prompt: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    #[test]
    fn answer_interpreter_agent_compiles_as_pipeline_agent() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<AnswerInterpreterAgent<MockInterpretLLM>>();
    }

    #[test]
    fn answer_interpreter_agent_compiles_with_arc_dyn() {
        fn _assert_pipeline_agent<T: PipelineAgent>() {}
        _assert_pipeline_agent::<AnswerInterpreterAgent<Arc<dyn AnswerInterpretationLLM>>>();
    }
}
