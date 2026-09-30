use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

use super::{
    answer_interpreter::{AnswerInterpretationLLM, AnswerInterpreter, AnswerType},
    budget::Channel,
};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::OperationLlmTelemetryScope,
    execution::PromptIdentityContext,
    prompts::{constants, PromptManager},
    slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType},
    state_tracker::{AssetContent, AssetType, ObservationAsset, StageContext},
};

#[derive(Debug, Deserialize)]
struct RawClarifierTemplate {
    prompt_template: String,
    urgency_base: f64,
    preferred_channel: RawChannel,
    context_requirements: Vec<RawContextRequirement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawChannel {
    InApp,
    Email,
    PushNotification,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawContextRequirement {
    RecentObservations { limit: usize },
    CompetingHypotheses,
    RelatedSlots,
    PreviousAttempts,
}

impl RawChannel {
    fn into_channel(self) -> Channel {
        match self {
            RawChannel::InApp => Channel::InApp,
            RawChannel::Email => Channel::Email,
            RawChannel::PushNotification => Channel::PushNotification,
        }
    }
}

impl RawContextRequirement {
    fn into_requirement(self) -> ContextRequirement {
        match self {
            RawContextRequirement::RecentObservations { limit } => {
                ContextRequirement::RecentObservations(limit)
            },
            RawContextRequirement::CompetingHypotheses => ContextRequirement::CompetingHypotheses,
            RawContextRequirement::RelatedSlots => ContextRequirement::RelatedSlots,
            RawContextRequirement::PreviousAttempts => ContextRequirement::PreviousAttempts,
        }
    }
}

/// Context-aware clarifier that generates questions for the ask loop.
pub struct ClarifierLibrary {
    templates: HashMap<BlockerType, ClarifierTemplate>,
    llm_service: Arc<dyn LlmService>,
    /// Optional answer interpreter for intelligent parsing (Phase 3)
    answer_interpreter: Option<Arc<AnswerInterpreter<Arc<dyn AnswerInterpretationLLM>>>>,
}

impl ClarifierLibrary {
    pub fn new(
        templates: HashMap<BlockerType, ClarifierTemplate>,
        llm_service: Arc<dyn LlmService>,
    ) -> Self {
        Self {
            templates,
            llm_service,
            answer_interpreter: None,
        }
    }

    /// Set the answer interpreter for intelligent response parsing
    pub fn set_answer_interpreter(
        &mut self,
        interpreter: Arc<AnswerInterpreter<Arc<dyn AnswerInterpretationLLM>>>,
    ) {
        self.answer_interpreter = Some(interpreter);
    }

    /// Construct a library backed by the built-in template set.
    pub fn with_default_templates(llm_service: Arc<dyn LlmService>) -> Self {
        Self::new(default_templates(), llm_service)
    }

    /// Construct a library using templates sourced from PromptManager storage.
    pub async fn with_prompt_manager(
        prompt_manager: Arc<PromptManager>,
        version: &str,
        llm_service: Arc<dyn LlmService>,
    ) -> Result<Self, ClarifierError> {
        let prompt = prompt_manager
            .get_prompt(constants::names::ASK_LOOP_CLARIFIER, version)
            .await
            .map_err(|e| ClarifierError::Prompt(e.to_string()))?;

        let raw_templates: HashMap<BlockerType, RawClarifierTemplate> =
            serde_json::from_str(&prompt.content).map_err(|e| {
                ClarifierError::Prompt(format!("failed to parse clarifier prompt: {}", e))
            })?;

        let mut templates = HashMap::new();
        for (blocker_type, raw) in raw_templates.into_iter() {
            let context_requirements = raw
                .context_requirements
                .into_iter()
                .map(RawContextRequirement::into_requirement)
                .collect();

            let template = ClarifierTemplate {
                blocker_type,
                prompt_template: raw.prompt_template,
                urgency_base: raw.urgency_base,
                preferred_channel: raw.preferred_channel.into_channel(),
                context_requirements,
            };
            templates.insert(blocker_type, template);
        }

        Ok(Self::new(templates, llm_service))
    }

    /// Generate a clarifier question for the given blocker and workflow context.
    pub async fn generate_question(
        &self,
        blocker_type: BlockerType,
        workflow_context: &WorkflowContext,
    ) -> Result<ClarifierQuestion, ClarifierError> {
        self.generate_question_with_telemetry(blocker_type, workflow_context, None)
            .await
    }

    #[instrument(
        skip(self, workflow_context, telemetry_scope),
        fields(workflow_id = %workflow_context.workflow_id, blocker = ?blocker_type, stage = ?workflow_context.stage_context)
    )]
    pub async fn generate_question_with_telemetry(
        &self,
        blocker_type: BlockerType,
        workflow_context: &WorkflowContext,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> Result<ClarifierQuestion, ClarifierError> {
        let template = self
            .templates
            .get(&blocker_type)
            .ok_or(ClarifierError::TemplateMissing(blocker_type))?;

        let stage = workflow_context.stage_context;
        let styling = StageStyling::for_stage(stage);

        let context_snippets = self.collect_context_snippets(template, workflow_context);
        let prompt = self.render_prompt(template, workflow_context, &context_snippets, &styling);
        let llm_output = match self
            .llm_service
            .generate_with_telemetry(&prompt, telemetry_scope)
            .await
        {
            Ok(text) => text,
            Err(err) => {
                debug!(
                    "[MAGICIAN-V2-ASK] Clarifier LLM fallback for workflow {}: {}",
                    workflow_context.workflow_id, err
                );
                prompt.clone()
            },
        };

        let urgency = (template.urgency_base
            + styling.urgency_adjustment
            + self.urgency_from_context(workflow_context))
        .clamp(0.0, 1.0);

        // Generate options for tool selection questions
        let options = if blocker_type == BlockerType::ToolSelectionRequired {
            // Find tool_selection slot in the workflow context
            workflow_context
                .slot_graph
                .iter()
                .find(|slot| {
                    slot.slot_type == SlotType::ToolSelection
                        && slot.id.starts_with("tool_selection:")
                })
                .map(|slot| tool_selection_options(&slot.id))
        } else {
            None
        };

        let question = ClarifierQuestion {
            id: Uuid::new_v4().to_string(),
            blocker_type,
            stage,
            source_slot_id: None,
            question_text: llm_output,
            context_snippets: context_snippets.clone(),
            urgency,
            channel: template.preferred_channel,
            created_at: Utc::now(),
            options,
            batch_id: None, // Batch tracking will be set during batch generation in Phase 4
            batch_total: None,
            ..ClarifierQuestion::default()
        };

        Ok(question)
    }

    /// Parse a user response and extract slot records.
    ///
    /// If an answer interpreter is configured, uses it to intelligently parse the response
    /// and detect corrections, clarifications, or rejections. Otherwise falls back to
    /// simple string extraction.
    pub async fn parse_response(
        &self,
        workflow_id: &str,
        question_id: &str,
        question: &ClarifierQuestion,
        user_response: &str,
        enriched_query: Option<&str>,
    ) -> Result<ClarifierResponsePayload, ClarifierError> {
        self.parse_response_with_identity(
            workflow_id,
            question_id,
            question,
            user_response,
            enriched_query,
            None,
        )
        .await
    }

    /// Parse a user response and extract slot records with optional prompt identity.
    pub async fn parse_response_with_identity(
        &self,
        workflow_id: &str,
        question_id: &str,
        question: &ClarifierQuestion,
        user_response: &str,
        enriched_query: Option<&str>,
        prompt_identity: Option<&PromptIdentityContext>,
    ) -> Result<ClarifierResponsePayload, ClarifierError> {
        self.parse_response_with_identity_and_telemetry(
            workflow_id,
            question_id,
            question,
            user_response,
            enriched_query,
            prompt_identity,
            None,
        )
        .await
    }

    pub async fn parse_response_with_identity_and_telemetry(
        &self,
        workflow_id: &str,
        question_id: &str,
        question: &ClarifierQuestion,
        user_response: &str,
        enriched_query: Option<&str>,
        prompt_identity: Option<&PromptIdentityContext>,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> Result<ClarifierResponsePayload, ClarifierError> {
        info!(
            "[MAGICIAN-CLARIFIER] 📨 Parsing answer for workflow {} question {}",
            workflow_id, question_id
        );

        let now = Utc::now();
        let slot_id = question
            .source_slot_id
            .clone()
            .unwrap_or_else(|| format!("{}::{}", workflow_id, question_id));
        let slot_type = question.blocker_type.slot_type();

        // Try intelligent interpretation if interpreter is available
        let combined_context = build_interpreter_context(question, enriched_query);

        let (value, confidence, interpretation_result) = if let Some(ref interpreter) =
            self.answer_interpreter
        {
            match interpreter
                .interpret_answer_with_identity_and_telemetry(
                    &question.question_text,
                    user_response,
                    Some(&combined_context),
                    prompt_identity,
                    telemetry_scope,
                )
                .await
            {
                Ok(interpreted) => {
                    info!(
                        "[CLARIFIER] Interpreted answer as {:?} with confidence {:.2}, requires_replan={}",
                        interpreted.answer_type, interpreted.confidence, interpreted.requires_replan
                    );

                    // Log special cases
                    match interpreted.answer_type {
                        AnswerType::Correction => {
                            warn!(
                                "[CLARIFIER] User provided correction: {} -> {}",
                                interpreted
                                    .correction_target
                                    .as_ref()
                                    .unwrap_or(&"unknown".to_string()),
                                interpreted
                                    .correction_value
                                    .as_ref()
                                    .unwrap_or(&user_response.to_string())
                            );
                        },
                        AnswerType::Rejection => {
                            warn!(
                                "[CLARIFIER] User rejected question: {}",
                                interpreted
                                    .rejection_reason
                                    .as_ref()
                                    .unwrap_or(&"no reason given".to_string())
                            );
                        },
                        AnswerType::Ambiguous => {
                            warn!("[CLARIFIER] Ambiguous answer detected, may need reelicitation");
                        },
                        _ => {},
                    }

                    // Use correction value if available, otherwise raw response
                    let answer_value = interpreted
                        .correction_value
                        .as_ref()
                        .unwrap_or(&user_response.to_string())
                        .clone();

                    // Phase 5: Create interpretation summary for replanning
                    let interpretation = AnswerInterpretation {
                        answer_type: interpreted.answer_type,
                        requires_replan: interpreted.requires_replan,
                        confidence: interpreted.confidence,
                    };

                    (
                        serde_json::Value::String(answer_value),
                        interpreted.confidence,
                        Some(interpretation),
                    )
                },
                Err(e) => {
                    warn!(
                        "[CLARIFIER] Answer interpretation failed: {}, falling back to simple parsing",
                        e
                    );
                    (
                        serde_json::Value::String(user_response.trim().to_string()),
                        0.8,
                        None,
                    )
                },
            }
        } else {
            // No interpreter available, use simple string extraction
            debug!("[CLARIFIER] Using simple string extraction (no interpreter configured)");
            (
                serde_json::Value::String(user_response.trim().to_string()),
                0.8,
                None,
            )
        };

        // Log answer interpretation for monitoring
        if let Some(ref interp) = interpretation_result {
            debug!(
                "[CLARIFIER] Answer interpretation: {:?}, requires_replan={}",
                interp.answer_type, interp.requires_replan
            );
        }

        let slot = SlotRecord {
            id: slot_id,
            slot_type,
            value,
            confidence,
            provenance: vec![ProvenanceRecord {
                source: ProvenanceSource::UserReply,
                timestamp: now,
            }],
            evidence_links: Vec::new(),
            created_at: now,
            updated_at: now,
        };

        Ok(ClarifierResponsePayload {
            workflow_id: workflow_id.to_string(),
            stage: question.stage,
            blocker_type: question.blocker_type,
            slots: vec![slot],
            interpretation: interpretation_result,
        })
    }

    fn collect_context_snippets(
        &self,
        template: &ClarifierTemplate,
        workflow_context: &WorkflowContext,
    ) -> Vec<String> {
        let mut snippets = Vec::new();

        for requirement in &template.context_requirements {
            match requirement {
                ContextRequirement::RecentObservations(limit) => {
                    snippets.extend(self.recent_observation_snippets(workflow_context, *limit))
                },
                ContextRequirement::CompetingHypotheses => {
                    if !workflow_context.slot_graph.is_empty() {
                        let hypotheses = workflow_context
                            .slot_graph
                            .iter()
                            .map(|slot| format!("{} => {}", slot.id, slot.value))
                            .collect::<Vec<_>>()
                            .join("\n");
                        snippets.push(format!("Competing hypotheses:\n{}", hypotheses));
                    }
                },
                ContextRequirement::RelatedSlots => {
                    let related = workflow_context
                        .slot_graph
                        .iter()
                        .take(3)
                        .map(|slot| {
                            format!(
                                "{} ({:?}) → confidence {:.2}",
                                slot.id, slot.slot_type, slot.confidence
                            )
                        })
                        .collect::<Vec<_>>();
                    if !related.is_empty() {
                        snippets.push(format!("Related slots:\n{}", related.join("\n")));
                    }
                },
                ContextRequirement::PreviousAttempts => {
                    if !workflow_context.confidence_scores.is_empty() {
                        let summary = workflow_context
                            .confidence_scores
                            .iter()
                            .map(|(id, score)| format!("{id}: {score:.2}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        snippets.push(format!("Previous attempts confidence: {summary}"));
                    }
                },
            }
        }

        snippets
    }

    fn recent_observation_snippets(
        &self,
        workflow_context: &WorkflowContext,
        limit: usize,
    ) -> Vec<String> {
        workflow_context
            .recent_observations
            .iter()
            .rev()
            .take(limit.max(1))
            .map(|obs| {
                let label = obs
                    .metadata
                    .get("label")
                    .map(|s| s.as_str())
                    .unwrap_or_else(|| match obs.asset_type {
                        AssetType::UserMessage => "User message",
                        AssetType::ToolOutput => "Planning context",
                        AssetType::Transcript => "Transcript",
                        AssetType::DomSummary => "DOM summary",
                        AssetType::Screenshot => "Screenshot",
                    });

                match &obs.content {
                    AssetContent::Text(text) => {
                        format!("{}: {}", label, summarize_observation_text(text))
                    },
                    AssetContent::Json(json) => {
                        format!("{}: {}", label, truncate_text(&json.to_string(), 220))
                    },
                    AssetContent::Image(_) => format!("{}: <image attachment>", label),
                }
            })
            .collect()
    }

    fn render_prompt(
        &self,
        template: &ClarifierTemplate,
        context: &WorkflowContext,
        snippets: &[String],
        styling: &StageStyling,
    ) -> String {
        let mut prompt = template.prompt_template.clone();
        let summary = if context.slot_graph.is_empty() {
            "an in-progress automation task".to_string()
        } else {
            format!("{} slots resolved", context.slot_graph.len())
        };

        prompt = prompt.replace("{task_summary}", &summary);

        let entity_hint = template
            .blocker_type
            .entity_hint()
            .unwrap_or("the missing information");

        prompt = prompt.replace("{entity_type}", entity_hint);
        prompt = prompt.replace("{target}", entity_hint);

        let snippet_text = if snippets.is_empty() {
            "No additional context captured.".to_string()
        } else {
            snippets.join("\n\n")
        };

        prompt = prompt.replace("{recent_context}", &snippet_text);
        prompt = prompt.replace("{competing_options}", &snippet_text);
        let question_hint = context
            .question_hint
            .as_deref()
            .filter(|hint| !hint.trim().is_empty())
            .unwrap_or(entity_hint);
        prompt = prompt.replace("{question_hint}", question_hint);

        if !styling.tone_prefix.is_empty() {
            let trimmed = prompt.trim();
            prompt = format!("{}\n\n{}", styling.tone_prefix, trimmed);
        }

        prompt
    }

    fn urgency_from_context(&self, context: &WorkflowContext) -> f64 {
        if context.slot_graph.is_empty() {
            0.2
        } else {
            let avg_confidence = context
                .slot_graph
                .iter()
                .map(|slot| slot.confidence)
                .sum::<f64>()
                / (context.slot_graph.len() as f64);
            (0.5 - avg_confidence).max(0.0)
        }
    }
}

fn summarize_observation_text(text: &str) -> String {
    let collapsed = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let base = if collapsed.is_empty() {
        text.trim().to_string()
    } else {
        collapsed
    };

    truncate_text(&base, 220)
}

fn truncate_text(text: &str, max_len: usize) -> String {
    let mut chars = text.chars();
    let mut result = String::with_capacity(text.len().min(max_len) + 1);
    for _ in 0..max_len {
        if let Some(ch) = chars.next() {
            result.push(ch);
        } else {
            break;
        }
    }
    if chars.next().is_some() {
        result.push('…');
    }
    if result.is_empty() {
        text.to_string()
    } else {
        result
    }
}

pub fn build_slot_question_hint(slot: &SlotRecord) -> String {
    let label = humanize_slot_label(&slot.id);
    let value_summary = summarize_slot_value(&slot.value);
    let signal = if value_summary.is_empty() {
        "no signal yet".to_string()
    } else {
        value_summary
    };

    let hint = match slot.slot_type {
        SlotType::Entity => format!(
            "Which entity should I use for {label}? I currently have \"{signal}\"."
        ),
        SlotType::Status => format!(
            "What is the latest status for {label}? Signals so far suggest \"{signal}\"."
        ),
        SlotType::ToolSelection => format!(
            "Which approach should I use for {label}? Current pick is \"{signal}\" (API vs Browser, etc.)."
        ),
        SlotType::Action => format!(
            "What action should I take for {label}? I inferred \"{signal}\"."
        ),
        SlotType::Spatial => {
            format!("Which location should I target for {label}? I see \"{signal}\".")
        },
        SlotType::Temporal => format!(
            "What timing should I use for {label}? Current guess is \"{signal}\"."
        ),
        _ => format!(
            "Could you confirm {label}? Current value is \"{signal}\" (confidence {:.2}).",
            slot.confidence
        ),
    };

    truncate_text(&hint, 220)
}

fn humanize_slot_label(slot_id: &str) -> String {
    let cleaned: Vec<String> = slot_id
        .split([':', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect();

    if cleaned.is_empty() {
        slot_id.to_string()
    } else {
        cleaned.join(" ")
    }
}

fn summarize_slot_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(num) => num.to_string(),
        Value::String(text) => sanitize_signal(text),
        Value::Array(items) => {
            if items.is_empty() {
                return String::new();
            }
            let mut summaries: Vec<String> = items
                .iter()
                .map(summarize_slot_value)
                .filter(|s| !s.is_empty())
                .take(3)
                .collect();
            if summaries.is_empty() {
                String::new()
            } else {
                if items.len() > summaries.len() {
                    summaries.push("…".to_string());
                }
                summaries.join(", ")
            }
        },
        Value::Object(map) => {
            for key in [
                "description",
                "summary",
                "value",
                "name",
                "label",
                "text",
                "target",
            ] {
                if let Some(inner) = map.get(key) {
                    let candidate = summarize_slot_value(inner);
                    if !candidate.is_empty() {
                        return candidate;
                    }
                }
            }

            let mut entries: Vec<String> = map
                .iter()
                .filter_map(|(k, v)| {
                    let val = summarize_slot_value(v);
                    if val.is_empty() {
                        None
                    } else {
                        Some(format!("{k}: {val}"))
                    }
                })
                .take(2)
                .collect();

            if entries.is_empty() {
                String::new()
            } else {
                if map.len() > entries.len() {
                    entries.push("…".to_string());
                }
                entries.join(", ")
            }
        },
    }
}

fn sanitize_signal(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let collapsed = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    truncate_text(&collapsed, 160)
}

fn build_interpreter_context(question: &ClarifierQuestion, enriched_query: Option<&str>) -> String {
    let mut unique_snippets = Vec::new();
    let mut seen = HashSet::new();

    if let Some(enriched) = enriched_query {
        let trimmed = enriched.trim();
        if !trimmed.is_empty() {
            let entry = format!("Clarified task: {}", trimmed);
            if seen.insert(entry.clone()) {
                unique_snippets.push(entry);
            }
        }
    }

    for snippet in &question.context_snippets {
        let trimmed = snippet.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            unique_snippets.push(trimmed.to_string());
        }
    }

    unique_snippets.join("; ")
}

#[derive(Debug, Clone, Copy)]
struct StageStyling {
    tone_prefix: &'static str,
    urgency_adjustment: f64,
}

impl StageStyling {
    fn for_stage(stage: StageContext) -> Self {
        match stage {
            StageContext::PlanningBootstrap | StageContext::PlanningIteration => StageStyling {
                tone_prefix: "Planning mode — I'm mapping out our approach and need to confirm a detail before execution.",
                urgency_adjustment: -0.05,
            },
            StageContext::ExecutionCycle | StageContext::FollowUp => StageStyling {
                tone_prefix: "Execution mode — I'm mid-run and blocked until I hear from you.",
                urgency_adjustment: 0.1,
            },
            StageContext::Unknown => StageStyling {
                tone_prefix: "Workflow update — I need a quick clarification.",
                urgency_adjustment: 0.0,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClarifierResponsePayload {
    pub workflow_id: String,
    pub stage: StageContext,
    pub blocker_type: BlockerType,
    pub slots: Vec<SlotRecord>,
    /// Phase 5: Answer interpretation result (if interpreter is configured)
    pub interpretation: Option<AnswerInterpretation>,
}

/// Simplified answer interpretation for replanning decisions
#[derive(Debug, Clone)]
pub struct AnswerInterpretation {
    pub answer_type: AnswerType,
    pub requires_replan: bool,
    pub confidence: f64,
}

#[async_trait]
pub trait LlmService: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<String, ClarifierError>;

    async fn generate_with_telemetry(
        &self,
        prompt: &str,
        telemetry_scope: Option<&OperationLlmTelemetryScope>,
    ) -> Result<String, ClarifierError> {
        let _ = telemetry_scope;
        self.generate(prompt).await
    }
}

/// Deterministic fallback LLM that returns the prompt verbatim.
pub struct DeterministicClarifier;

#[async_trait]
impl LlmService for DeterministicClarifier {
    async fn generate(&self, prompt: &str) -> Result<String, ClarifierError> {
        Ok(prompt.to_string())
    }
}

/// Clarifier template definition.
#[derive(Debug, Clone)]
pub struct ClarifierTemplate {
    pub blocker_type: BlockerType,
    pub prompt_template: String,
    pub urgency_base: f64,
    pub preferred_channel: Channel,
    pub context_requirements: Vec<ContextRequirement>,
}

impl ClarifierTemplate {
    pub fn builder(blocker_type: BlockerType) -> ClarifierTemplateBuilder {
        ClarifierTemplateBuilder::new(blocker_type)
    }
}

/// Builder helper for clarifier templates.
pub struct ClarifierTemplateBuilder {
    blocker_type: BlockerType,
    prompt_template: String,
    urgency_base: f64,
    preferred_channel: Channel,
    context_requirements: Vec<ContextRequirement>,
}

impl ClarifierTemplateBuilder {
    pub fn new(blocker_type: BlockerType) -> Self {
        Self {
            blocker_type,
            prompt_template: String::new(),
            urgency_base: 0.5,
            preferred_channel: Channel::InApp,
            context_requirements: Vec::new(),
        }
    }

    pub fn prompt_template(mut self, template: impl Into<String>) -> Self {
        self.prompt_template = template.into();
        self
    }

    pub fn urgency_base(mut self, urgency: f64) -> Self {
        self.urgency_base = urgency.clamp(0.0, 1.0);
        self
    }

    pub fn preferred_channel(mut self, channel: Channel) -> Self {
        self.preferred_channel = channel;
        self
    }

    pub fn context_requirements(mut self, requirements: Vec<ContextRequirement>) -> Self {
        self.context_requirements = requirements;
        self
    }

    pub fn build(self) -> ClarifierTemplate {
        ClarifierTemplate {
            blocker_type: self.blocker_type,
            prompt_template: self.prompt_template,
            urgency_base: self.urgency_base,
            preferred_channel: self.preferred_channel,
            context_requirements: self.context_requirements,
        }
    }
}

/// Generate choice options for tool selection questions based on the service being accessed.
///
/// # Arguments
/// * `slot_id` - The slot ID in format "tool_selection:service_name" (e.g., "tool_selection:github_access")
///
/// # Returns
/// Vector of predefined options for the given service, or generic options if service is unknown
fn tool_selection_options(slot_id: &str) -> Vec<QuestionOption> {
    // Extract service name from slot_id (e.g., "tool_selection:github_access" → "github_access")
    let service_name = slot_id
        .strip_prefix("tool_selection:")
        .unwrap_or(slot_id)
        .to_lowercase();

    match service_name.as_str() {
        "github_access" | "github" => vec![
            QuestionOption {
                value: "api".to_string(),
                label: "GitHub API".to_string(),
                description: Some("Faster if you have a GitHub token configured".to_string()),
            },
            QuestionOption {
                value: "browser".to_string(),
                label: "Browser UI".to_string(),
                description: Some("No setup needed, login via web interface".to_string()),
            },
        ],
        "gmail_access" | "gmail" | "email_access" => vec![
            QuestionOption {
                value: "api".to_string(),
                label: "Gmail API".to_string(),
                description: Some("Faster if you have OAuth credentials configured".to_string()),
            },
            QuestionOption {
                value: "imap".to_string(),
                label: "IMAP".to_string(),
                description: Some("Standard email protocol, needs IMAP credentials".to_string()),
            },
            QuestionOption {
                value: "browser".to_string(),
                label: "Browser UI".to_string(),
                description: Some("Login via web, no credential setup needed".to_string()),
            },
        ],
        "dropbox_access"
        | "dropbox"
        | "google_drive_access"
        | "google_drive"
        | "onedrive_access"
        | "onedrive" => vec![
            QuestionOption {
                value: "api".to_string(),
                label: "API/SDK".to_string(),
                description: Some("Faster with OAuth tokens or API keys".to_string()),
            },
            QuestionOption {
                value: "browser".to_string(),
                label: "Browser UI".to_string(),
                description: Some("Interactive web interface, no API setup".to_string()),
            },
        ],
        "slack_access" | "slack" | "discord_access" | "discord" => vec![
            QuestionOption {
                value: "api".to_string(),
                label: "API/Webhook".to_string(),
                description: Some("Use API tokens or webhook URLs".to_string()),
            },
            QuestionOption {
                value: "browser".to_string(),
                label: "Browser UI".to_string(),
                description: Some("Interact via web interface".to_string()),
            },
        ],
        // Generic fallback for unknown services
        _ => vec![
            QuestionOption {
                value: "shell".to_string(),
                label: "Shell/API".to_string(),
                description: Some(
                    "Command-line or API access (faster, needs credentials)".to_string(),
                ),
            },
            QuestionOption {
                value: "browser".to_string(),
                label: "Browser UI".to_string(),
                description: Some("Interactive web interface (no credential setup)".to_string()),
            },
        ],
    }
}

fn default_templates() -> HashMap<BlockerType, ClarifierTemplate> {
    use Channel::{Email, InApp, PushNotification};

    let mut templates = HashMap::new();

    templates.insert(
        BlockerType::MissingEntity,
        ClarifierTemplate::builder(BlockerType::MissingEntity)
            .prompt_template(
                r#"
I'm working on: {task_summary}

Here's what I have so far:
{recent_context}

To keep moving I still need {entity_type}. Could you share that?"#,
            )
            .urgency_base(0.55)
            .preferred_channel(InApp)
            .context_requirements(vec![
                ContextRequirement::RecentObservations(3),
                ContextRequirement::RelatedSlots,
            ])
            .build(),
    );

    templates.insert(
        BlockerType::AmbiguousLocation,
        ClarifierTemplate::builder(BlockerType::AmbiguousLocation)
            .prompt_template(
                r#"
Task summary: {task_summary}

I found multiple location candidates:
{competing_options}

Which location should I use?"#,
            )
            .urgency_base(0.65)
            .preferred_channel(PushNotification)
            .context_requirements(vec![
                ContextRequirement::RecentObservations(2),
                ContextRequirement::CompetingHypotheses,
            ])
            .build(),
    );

    templates.insert(
        BlockerType::UncertainStatus,
        ClarifierTemplate::builder(BlockerType::UncertainStatus)
            .prompt_template(
                r#"
Current progress: {task_summary}

Observations so far:
{recent_context}

I'm unsure about {entity_type}. What is the latest status?"#,
            )
            .urgency_base(0.5)
            .preferred_channel(InApp)
            .context_requirements(vec![
                ContextRequirement::RecentObservations(3),
                ContextRequirement::PreviousAttempts,
            ])
            .build(),
    );

    templates.insert(
        BlockerType::ConflictingInformation,
        ClarifierTemplate::builder(BlockerType::ConflictingInformation)
            .prompt_template(
                r#"
While planning {task_summary} I noticed conflicting details:
{recent_context}

Which piece of information should I trust?"#,
            )
            .urgency_base(0.6)
            .preferred_channel(Email)
            .context_requirements(vec![
                ContextRequirement::CompetingHypotheses,
                ContextRequirement::PreviousAttempts,
            ])
            .build(),
    );

    templates.insert(
        BlockerType::LowConfidenceSlot,
        ClarifierTemplate::builder(BlockerType::LowConfidenceSlot)
            .prompt_template(
                r#"
I gathered the following details:
{recent_context}

I'm still not confident about {entity_type}. Could you confirm or correct it?"#,
            )
            .urgency_base(0.45)
            .preferred_channel(InApp)
            .context_requirements(vec![
                ContextRequirement::RecentObservations(2),
                ContextRequirement::RelatedSlots,
            ])
            .build(),
    );

    templates.insert(
        BlockerType::UnclearIntent,
        ClarifierTemplate::builder(BlockerType::UnclearIntent)
            .prompt_template(
                r#"
I'm piecing together {task_summary}.

From the recent context:
{recent_context}

What outcome are you hoping for so I can plan the right steps?"#,
            )
            .urgency_base(0.5)
            .preferred_channel(InApp)
            .context_requirements(vec![
                ContextRequirement::RecentObservations(2),
                ContextRequirement::PreviousAttempts,
            ])
            .build(),
    );

    templates.insert(
        BlockerType::ToolSelectionRequired,
        ClarifierTemplate::builder(BlockerType::ToolSelectionRequired)
            .prompt_template(
                r#"
I'm planning {task_summary} and detected that {entity_type} can be accessed in multiple ways.

Common options:
- Browser UI (no setup needed, login via web)
- API/Shell (faster if you have credentials configured)

Which approach should I use?"#,
            )
            .urgency_base(0.7)
            .preferred_channel(InApp)
            .context_requirements(vec![
                ContextRequirement::RecentObservations(1),
                ContextRequirement::CompetingHypotheses,
            ])
            .build(),
    );

    templates
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerType {
    MissingEntity,
    AmbiguousLocation,
    UncertainStatus,
    ConflictingInformation,
    LowConfidenceSlot,
    UnclearIntent,
    /// Tool selection required when multiple approaches exist (e.g., API vs Browser)
    ToolSelectionRequired,

    // Progressive elicitation blockers
    /// Inferred value is below confidence threshold
    LowInferenceConfidence,
    /// Multiple types match the parameter
    AmbiguousParameterType,
    /// Cannot infer without more context
    MissingInferenceContext,
    /// Parameter conflicts with another
    ParameterConflict,
}

impl BlockerType {
    fn slot_type(&self) -> SlotType {
        match self {
            BlockerType::MissingEntity => SlotType::Entity,
            BlockerType::AmbiguousLocation => SlotType::Spatial,
            BlockerType::UncertainStatus => SlotType::Status,
            BlockerType::ConflictingInformation => SlotType::Modifier,
            BlockerType::LowConfidenceSlot => SlotType::Modifier,
            BlockerType::UnclearIntent => SlotType::Action,
            BlockerType::ToolSelectionRequired => SlotType::ToolSelection,
            BlockerType::LowInferenceConfidence => SlotType::Modifier,
            BlockerType::AmbiguousParameterType => SlotType::Modifier,
            BlockerType::MissingInferenceContext => SlotType::Modifier,
            BlockerType::ParameterConflict => SlotType::Modifier,
        }
    }

    fn entity_hint(&self) -> Option<&'static str> {
        match self {
            BlockerType::MissingEntity => Some("the entity that is missing"),
            BlockerType::AmbiguousLocation => Some("the correct location"),
            BlockerType::UncertainStatus => Some("the current status"),
            BlockerType::ConflictingInformation => Some("the correct information"),
            BlockerType::LowConfidenceSlot => Some("the field we are unsure about"),
            BlockerType::UnclearIntent => Some("the exact goal"),
            BlockerType::ToolSelectionRequired => Some("which tool approach to use"),
            BlockerType::LowInferenceConfidence => Some("the parameter value we inferred"),
            BlockerType::AmbiguousParameterType => Some("which parameter type to use"),
            BlockerType::MissingInferenceContext => Some("additional context needed"),
            BlockerType::ParameterConflict => Some("how to resolve the parameter conflict"),
        }
    }
}

/// A choice option for questions that require selecting from predefined values
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    /// Value to use when this option is selected (e.g., "api", "browser")
    pub value: String,
    /// Human-readable label shown in UI
    pub label: String,
    /// Optional description providing more context
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifierQuestion {
    pub id: String,
    pub blocker_type: BlockerType,
    pub stage: StageContext,
    /// Original slot identifier that prompted this question (if available)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_slot_id: Option<String>,
    pub question_text: String,
    pub context_snippets: Vec<String>,
    pub urgency: f64,
    pub channel: Channel,
    pub created_at: DateTime<Utc>,
    /// Optional predefined choices for questions that require selection (e.g., tool selection)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<QuestionOption>>,

    /// Batch ID if this question is part of a batch
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,

    /// Total number of questions in the batch
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_total: Option<usize>,

    /// Optional confidence score for the source slot
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_confidence: Option<f32>,

    /// Optional list of related slot identifiers for additional context
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_slots: Vec<String>,
}

impl Default for ClarifierQuestion {
    fn default() -> Self {
        Self {
            id: String::new(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: None,
            question_text: String::new(),
            context_snippets: Vec::new(),
            urgency: 0.5,
            channel: Channel::InApp,
            created_at: chrono::Utc::now(),
            options: None,
            batch_id: None,
            batch_total: None,
            slot_confidence: None,
            related_slots: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkflowContext {
    pub workflow_id: String,
    pub stage_context: StageContext,
    pub recent_observations: Vec<ObservationAsset>,
    pub slot_graph: Vec<SlotRecord>,
    pub confidence_scores: HashMap<String, f64>,
    pub question_hint: Option<String>,
}

impl WorkflowContext {
    pub fn empty(workflow_id: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            stage_context: StageContext::default(),
            recent_observations: Vec::new(),
            slot_graph: Vec::new(),
            confidence_scores: HashMap::new(),
            question_hint: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ContextRequirement {
    RecentObservations(usize),
    CompetingHypotheses,
    RelatedSlots,
    PreviousAttempts,
}

#[derive(Debug, Error)]
pub enum ClarifierError {
    #[error("clarifier template missing for blocker type {:?}", 0)]
    TemplateMissing(BlockerType),
    #[error("clarifier question '{0}' not found")]
    UnknownQuestion(String),
    #[error("clarifier prompt error: {0}")]
    Prompt(String),
    #[error("llm error: {0}")]
    Llm(String),
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        execution::{PromptAgentKind, PromptIdentityContext},
        prompts::{Prompt, PromptCategory, PromptManager, PromptStore},
        state_tracker::{AssetType, ObservationAsset},
    };
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Mutex;

    struct EchoLlm;

    #[async_trait]
    impl LlmService for EchoLlm {
        async fn generate(&self, prompt: &str) -> Result<String, ClarifierError> {
            Ok(prompt.to_uppercase())
        }
    }

    struct InterpreterPromptStore;

    #[async_trait::async_trait]
    impl PromptStore for InterpreterPromptStore {
        async fn get_prompt(&self, name: &str, version: &str) -> anyhow::Result<Prompt> {
            let content = match name {
                crate::magician_v2::prompts::constants::names::ANSWER_INTERPRETATION_SYSTEM => {
                    "You are an answer interpreter.\n{identity_section}".to_string()
                },
                _ => "Question: {question}\nAnswer: {answer}\nContext: {context}".to_string(),
            };
            Ok(Prompt::new(
                name.to_string(),
                version.to_string(),
                content,
                PromptCategory::General,
                "test prompt".to_string(),
                "test".to_string(),
            ))
        }

        async fn list_versions(&self, _name: &str) -> anyhow::Result<Vec<String>> {
            Ok(vec!["v1.0.0".to_string()])
        }

        async fn list_prompt_names(&self) -> anyhow::Result<Vec<String>> {
            Ok(vec![
                crate::magician_v2::prompts::constants::names::ANSWER_INTERPRETATION.to_string(),
                crate::magician_v2::prompts::constants::names::ANSWER_INTERPRETATION_SYSTEM
                    .to_string(),
            ])
        }

        async fn save_prompt(&self, _prompt: &Prompt) -> anyhow::Result<()> {
            Ok(())
        }

        async fn prompt_exists(&self, _name: &str, _version: &str) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn latest_version(&self, _name: &str) -> anyhow::Result<String> {
            Ok("v1.0.0".to_string())
        }

        async fn delete_prompt(&self, _name: &str, _version: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn initialize(&self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    struct RecordingAnswerInterpreterLlm {
        system_prompts: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingAnswerInterpreterLlm {
        fn new() -> Self {
            Self {
                system_prompts: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn seen_system_prompts(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.system_prompts)
        }
    }

    #[async_trait::async_trait]
    impl AnswerInterpretationLLM for RecordingAnswerInterpreterLlm {
        async fn interpret(&self, _prompt: &str) -> Result<String, String> {
            Err("interpret() should not be used by answer interpreter".to_string())
        }

        async fn interpret_with_system(
            &self,
            system_prompt: Option<&str>,
            _prompt: &str,
        ) -> Result<String, String> {
            self.system_prompts.lock().unwrap().push(
                system_prompt
                    .map(str::to_string)
                    .unwrap_or_else(|| "<none>".to_string()),
            );
            Ok(r#"{
                "answer_type":"DirectAnswer",
                "slots":[],
                "correction_target":null,
                "correction_value":null,
                "clarification_text":null,
                "rejection_reason":null,
                "confidence":0.91,
                "requires_replan":false,
                "reasoning":"ok"
            }"#
            .to_string())
        }
    }

    fn library() -> ClarifierLibrary {
        ClarifierLibrary::with_default_templates(Arc::new(EchoLlm))
    }

    #[tokio::test]
    async fn generates_question_and_tracks_state() {
        let library = library();
        let context = WorkflowContext {
            workflow_id: "wf-1".to_string(),
            stage_context: StageContext::ExecutionCycle,
            recent_observations: vec![ObservationAsset {
                asset_id: "obs-1".to_string(),
                asset_type: AssetType::Transcript,
                content: AssetContent::Text("Need clarification".to_string()),
                metadata: HashMap::new(),
            }],
            slot_graph: Vec::new(),
            confidence_scores: HashMap::new(),
            question_hint: None,
        };

        let question = library
            .generate_question(BlockerType::MissingEntity, &context)
            .await
            .unwrap();
        assert_eq!(question.channel, Channel::InApp);
        assert_eq!(question.stage, StageContext::ExecutionCycle);
        assert!(
            question.question_text.contains("EXECUTION MODE"),
            "execution stage prefix should be applied"
        );
    }

    #[tokio::test]
    async fn parses_response_into_slot() {
        let library = library();
        let mut context = WorkflowContext::empty("wf-2");
        context.stage_context = StageContext::PlanningBootstrap;
        let question = library
            .generate_question(BlockerType::MissingEntity, &context)
            .await
            .unwrap();

        let response = library
            .parse_response(
                "wf-2",
                &question.id,
                &question,
                "The entity is BillingService",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.stage, StageContext::PlanningBootstrap);
        assert_eq!(response.slots.len(), 1);
        assert_eq!(response.slots[0].slot_type, SlotType::Entity);
    }

    #[tokio::test]
    async fn parses_response_reuses_source_slot_id() {
        let library = library();
        let question_id = "question-123".to_string();
        let slot_id = "workflow::slot-xyz".to_string();
        let question = ClarifierQuestion {
            id: question_id.clone(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: Some(slot_id.clone()),
            question_text: "Which service should we target?".to_string(),
            context_snippets: vec!["Existing slot captured from elicitation".to_string()],
            urgency: 0.4,
            channel: Channel::InApp,
            created_at: Utc::now(),
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        let response = library
            .parse_response(
                "workflow-xyz",
                &question_id,
                &question,
                "Use the billing service",
                None,
            )
            .await
            .unwrap();

        assert_eq!(response.slots.len(), 1);
        assert_eq!(response.slots[0].id, slot_id);
        assert_eq!(
            response.slots[0].value,
            serde_json::Value::String("Use the billing service".to_string())
        );
    }

    #[tokio::test]
    async fn parse_response_with_identity_injects_identity_into_interpreter_system_prompt() {
        let mut library = library();
        let interpreter_llm = RecordingAnswerInterpreterLlm::new();
        let seen_system_prompts = interpreter_llm.seen_system_prompts();
        let prompt_store: Arc<dyn PromptStore> = Arc::new(InterpreterPromptStore);
        let prompt_manager = Arc::new(PromptManager::new(prompt_store));
        let interpreter = Arc::new(AnswerInterpreter::new(
            Arc::new(interpreter_llm) as Arc<dyn AnswerInterpretationLLM>,
            prompt_manager,
        ));
        library.set_answer_interpreter(interpreter);

        let question = ClarifierQuestion {
            id: "q-identity".to_string(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: Some("slot:identity".to_string()),
            question_text: "Which service should be used?".to_string(),
            context_snippets: vec!["Context".to_string()],
            urgency: 0.5,
            channel: Channel::InApp,
            created_at: Utc::now(),
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        let identity = PromptIdentityContext {
            agent_kind: Some(PromptAgentKind::User),
            base_persona: Some("Be precise".to_string()),
            source_agent_id: Some("agent-42".to_string()),
            source_agent_name: Some("Assistant".to_string()),
            source_agent_aliases: Vec::new(),
            source_agent_persona: None,
            autonomous_controls: None,
        };

        let parsed = library
            .parse_response_with_identity(
                "wf-identity",
                &question.id,
                &question,
                "Use checkout-service",
                None,
                Some(&identity),
            )
            .await
            .expect("clarifier response should parse");
        assert_eq!(parsed.slots.len(), 1);

        let system_prompts = seen_system_prompts.lock().unwrap();
        assert_eq!(system_prompts.len(), 1);
        let system_prompt = &system_prompts[0];
        assert!(
            system_prompt.contains("AGENT IDENTITY CONTEXT"),
            "identity section should be injected into system prompt"
        );
        assert!(
            system_prompt.contains("Source agent id: agent-42"),
            "source agent id should be propagated"
        );
    }

    #[tokio::test]
    async fn default_templates_cover_all_blockers() {
        let library = library();
        let mut context = WorkflowContext::empty("wf-defaults");
        context.stage_context = StageContext::PlanningBootstrap;
        for blocker in [
            BlockerType::MissingEntity,
            BlockerType::AmbiguousLocation,
            BlockerType::UncertainStatus,
            BlockerType::ConflictingInformation,
            BlockerType::LowConfidenceSlot,
            BlockerType::UnclearIntent,
        ] {
            let question = library
                .generate_question(blocker, &context)
                .await
                .expect("question generated");
            assert_eq!(question.blocker_type, blocker);
            assert!(!question.question_text.is_empty());
        }
    }

    #[tokio::test]
    async fn stage_specific_voice_applied() {
        let library = library();
        let mut planning = WorkflowContext::empty("wf-planning");
        planning.stage_context = StageContext::PlanningBootstrap;
        let planning_question = library
            .generate_question(BlockerType::UnclearIntent, &planning)
            .await
            .unwrap();
        assert!(
            planning_question.question_text.contains("PLANNING MODE"),
            "planning stage prompt should mention planning mode"
        );

        let mut execution = WorkflowContext::empty("wf-execution");
        execution.stage_context = StageContext::ExecutionCycle;
        let execution_question = library
            .generate_question(BlockerType::UnclearIntent, &execution)
            .await
            .unwrap();
        assert!(
            execution_question.question_text.contains("EXECUTION MODE"),
            "execution stage prompt should mention execution mode"
        );
    }

    #[test]
    fn slot_question_hint_highlights_value() {
        let slot = SlotRecord {
            id: "status:sample_user".to_string(),
            slot_type: SlotType::Status,
            value: json!(
                "Sample user's current status (up/available/online) via presence indicators"
            ),
            confidence: 0.34,
            provenance: vec![],
            evidence_links: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let hint = build_slot_question_hint(&slot);
        assert!(
            hint.to_lowercase().contains("status"),
            "hint should mention slot label"
        );
        assert!(
            hint.contains("Sample"),
            "hint should preserve key entity details"
        );
        assert!(
            hint.contains("Signals"),
            "status hints should reference current signals"
        );
    }
}
