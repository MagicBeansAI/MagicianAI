use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tracing::{info, instrument, warn};

use super::{extraction::LlmCallTelemetry, SlotRecord};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    prompts::{constants, types::Prompt, PromptManager},
};

/// Configuration options controlling rewrite behaviour.
#[derive(Debug, Clone)]
pub struct RewriterConfig {
    /// Which model identifier the attached LLM service should use.
    pub model: String,
    /// Temperature parameter to forward along to the LLM.
    pub temperature: f32,
    /// Prompt version to load from the prompt manager.
    pub prompt_version: String,
    /// Minimum acceptable confidence before the conservative rewrite is forced.
    pub conservative_confidence_floor: f64,
}

impl Default for RewriterConfig {
    fn default() -> Self {
        Self {
            model: "gpt-5.6-terra".to_string(),
            temperature: 0.1,
            prompt_version: constants::versions::QUESTION_REWRITING.to_string(),
            conservative_confidence_floor: 0.55,
        }
    }
}

/// Strategy labels for rewrite attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RewriteStrategy {
    Conservative,
    Aggressive,
}

impl RewriteStrategy {
    fn label(self) -> &'static str {
        match self {
            RewriteStrategy::Conservative => "Conservative alignment pass",
            RewriteStrategy::Aggressive => "Aggressive reframing pass",
        }
    }

    fn instructions(self) -> &'static str {
        match self {
            RewriteStrategy::Conservative => {
                "Stay close to the original phrasing. Resolve only ambiguities that the slot \
                 graph explicitly supports. Surface gaps as open questions and avoid inventing \
                 missing details."
            },
            RewriteStrategy::Aggressive => {
                "Reframe the request for optimal planner execution. Merge evidence from the \
                 slot graph, infer concrete objectives, and propose explicit constraints. Fill \
                 reasonable defaults when the slot graph strongly supports them."
            },
        }
    }
}

impl fmt::Display for RewriteStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RewriteStrategy::Conservative => "conservative",
            RewriteStrategy::Aggressive => "aggressive",
        })
    }
}

/// Request payload sent to the rewrite model.
#[derive(Debug, Clone)]
pub struct RewriteModelRequest {
    pub prompt: String,
    pub strategy: RewriteStrategy,
    pub model: String,
    pub temperature: f32,
}

/// Response payload returned by the rewrite model.
#[derive(Debug, Clone)]
pub struct RewriteModelResponse {
    pub completion: String,
    pub telemetry: Option<LlmCallTelemetry>,
}

#[async_trait]
pub trait RewriteModel: Send + Sync {
    async fn generate(&self, request: RewriteModelRequest) -> Result<RewriteModelResponse>;

    async fn generate_scoped(
        &self,
        _scope: magicllm::LlmScope,
        request: RewriteModelRequest,
    ) -> Result<RewriteModelResponse> {
        self.generate(request).await
    }
}

/// Structured task definition emitted by the rewrite service.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClarifiedTask {
    pub clarified_task: String,
    pub constraints: Vec<String>,
    pub objectives: Vec<String>,
    pub resources: Vec<String>,
    pub confidence: f64,
    pub open_questions: Vec<ClarifiedOpenQuestion>,
    pub slot_graph_id: String,
    pub original_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewrite_strategy: Option<RewriteStrategy>,
}

/// Structured metadata for an outstanding clarification question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ClarifiedOpenQuestion {
    pub question_text: String,
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_confidence: Option<f32>,
    #[serde(default)]
    pub related_slots: Vec<String>,
}

impl ClarifiedOpenQuestion {
    pub fn from_text<T: Into<String>>(text: T) -> Self {
        Self {
            question_text: text.into(),
            ..Default::default()
        }
    }

    pub fn with_slot_id<T: Into<String>>(text: T, slot_id: String) -> Self {
        Self {
            question_text: text.into(),
            slot_id: Some(slot_id),
            ..Default::default()
        }
    }
}

/// Output of the question-curation prompt.
#[derive(Debug, Clone, Deserialize)]
pub struct CuratedClarificationQuestion {
    pub question: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CuratedQuestionPayload {
    questions: Vec<CuratedClarificationQuestion>,
}

/// Question rewriter backed by an LLM plus slot graph context.
pub struct QuestionRewriter {
    rewrite_llm: Arc<dyn RewriteModel>,
    curation_llm: Arc<dyn RewriteModel>,
    prompt_manager: Arc<PromptManager>,
    config: RewriterConfig,
    prompt_template: OnceCell<Prompt>,
    curation_prompt_template: OnceCell<Prompt>,
}

impl QuestionRewriter {
    pub fn new(
        llm_service: Arc<dyn RewriteModel>,
        prompt_manager: Arc<PromptManager>,
        config: RewriterConfig,
    ) -> Self {
        Self::with_curation_model(
            Arc::clone(&llm_service),
            llm_service,
            prompt_manager,
            config,
        )
    }

    pub fn with_curation_model(
        rewrite_llm: Arc<dyn RewriteModel>,
        curation_llm: Arc<dyn RewriteModel>,
        prompt_manager: Arc<PromptManager>,
        config: RewriterConfig,
    ) -> Self {
        Self {
            rewrite_llm,
            curation_llm,
            prompt_manager,
            config,
            prompt_template: OnceCell::new(),
            curation_prompt_template: OnceCell::new(),
        }
    }

    /// Rewrite the user's request into a planner-ready ClarifiedTask.
    pub async fn rewrite_for_planner(
        &self,
        customer_query: &str,
        current_query: &str,
        slot_graph: &[SlotRecord],
        unresolved_slots: &[String],
    ) -> Result<ClarifiedTask> {
        self.rewrite_for_planner_with_telemetry(
            customer_query,
            current_query,
            slot_graph,
            unresolved_slots,
            None,
            OperationLlmCallAttribution::default(),
        )
        .await
    }

    #[instrument(
        skip(self, slot_graph, unresolved_slots),
        fields(slot_count = slot_graph.len(), unresolved = unresolved_slots.len())
    )]
    pub async fn rewrite_for_planner_with_telemetry(
        &self,
        customer_query: &str,
        current_query: &str,
        slot_graph: &[SlotRecord],
        unresolved_slots: &[String],
        llm_telemetry: Option<&OperationLlmTelemetryContext>,
        attribution: OperationLlmCallAttribution,
    ) -> Result<ClarifiedTask> {
        // LOCATION 1: Entry logging - log all inputs
        info!(
            "[MAGICIAN-QUERY-REWRITER] 🚀 === REWRITER ENTRY ===\n\
             - Customer query (original): '{}'\n\
             - Query for rewrite (current): '{}'\n\
             - Query length: {} chars\n\
             - Slot graph size: {} slots\n\
             - Slot details: {:?}\n\
             - Unresolved slots: {} slots\n\
             - Unresolved details: {:?}",
            customer_query,
            current_query,
            current_query.len(),
            slot_graph.len(),
            slot_graph
                .iter()
                .map(|s| format!("{} = {:?} (conf: {:.2})", s.id, s.value, s.confidence))
                .collect::<Vec<_>>(),
            unresolved_slots.len(),
            unresolved_slots
        );

        if current_query.trim().is_empty() {
            return Err(anyhow!("current query is empty; nothing to rewrite"));
        }

        let slot_graph_id = compute_slot_graph_id(slot_graph)?;
        let slot_summary = summarise_slot_graph(slot_graph)?;
        let unresolved_summary = summarise_unresolved(unresolved_slots);

        // LOCATION 2: Slot processing logging - log computed ID and summaries
        info!(
            "[MAGICIAN-QUERY-REWRITER] 📊 SLOT PROCESSING COMPLETE\n\
             - Slot graph ID: {}\n\
             - Slot summary length: {} chars\n\
             - Slot summary:\n{}\n\
             - Unresolved summary:\n{}",
            slot_graph_id,
            slot_summary.len(),
            slot_summary,
            unresolved_summary
        );

        let template = self.prompt().await?;
        let mut best_candidate: Option<Candidate> = None;

        for (attempt, strategy) in [RewriteStrategy::Conservative, RewriteStrategy::Aggressive]
            .into_iter()
            .enumerate()
        {
            // LOCATION 3a: Strategy loop start - log strategy attempt
            info!(
                "[MAGICIAN-QUERY-REWRITER] 🎯 TRYING STRATEGY: {}\n\
                 - Strategy label: {}\n\
                 - Instructions: {}",
                strategy,
                strategy.label(),
                strategy.instructions()
            );

            let sanitized_query = sanitize_prompt_input(current_query);
            let prompt = render_prompt(
                template,
                &sanitized_query,
                &slot_summary,
                &unresolved_summary,
                strategy,
            )?;

            // LOCATION 3b: Before LLM call - log request details
            info!(
                "[MAGICIAN-QUERY-REWRITER] 🤖 CALLING LLM for strategy {}\n\
                 - Model: {}\n\
                 - Temperature: {}\n\
                 - Prompt length: {} chars",
                strategy,
                self.config.model,
                self.config.temperature,
                prompt.len()
            );

            let started_at = std::time::Instant::now();
            let request = RewriteModelRequest {
                prompt,
                strategy,
                model: self.config.model.clone(),
                temperature: self.config.temperature,
            };
            let llm_call = match llm_telemetry {
                Some(telemetry) => self.rewrite_llm.generate_scoped(telemetry.scope(), request),
                None => self.rewrite_llm.generate(request),
            };
            let response = match llm_call.await {
                Ok(response) => {
                    // LOCATION 3c: LLM response received
                    info!(
                        "[MAGICIAN-QUERY-REWRITER] ✅ LLM RESPONSE RECEIVED for strategy {}\n\
                         - Response length: {} chars\n\
                         - Response preview: {}",
                        strategy,
                        response.completion.len(),
                        response.completion.chars().take(200).collect::<String>()
                    );
                    response
                },
                Err(err) => {
                    warn!(
                        "[MAGICIAN-QUERY-REWRITER] ❌ LLM CALL FAILED for strategy {}: {err:?}",
                        strategy
                    );
                    continue;
                },
            };

            let parsed = parse_candidate(
                &response.completion,
                &slot_graph_id,
                unresolved_slots,
                strategy,
                self.config.conservative_confidence_floor,
                customer_query,
            );
            if let (Some(context), Some(usage)) = (llm_telemetry, response.telemetry.as_ref()) {
                let mut call_attribution = attribution.clone();
                call_attribution.attempt = Some((attempt + 1) as u32);
                let latency_ms = started_at.elapsed().as_millis() as u64;
                match parsed.as_ref() {
                    Ok(_) => context.emit_usage_validated_success(
                        "question_rewriting",
                        usage,
                        latency_ms,
                        call_attribution,
                        "question_rewriting_json",
                    ),
                    Err(error) => context.emit_usage_validation_failure(
                        "question_rewriting",
                        usage,
                        latency_ms,
                        call_attribution,
                        "question_rewriting_json",
                        &error.to_string(),
                    ),
                }
            }

            match parsed {
                Ok(candidate) => {
                    // LOCATION 3d: Candidate parsed successfully - log details
                    info!(
                        "[MAGICIAN-QUERY-REWRITER] ✅ CANDIDATE PARSED for strategy {}\n\
                         - Score: {:.3}\n\
                         - Clarified task: '{}'\n\
                         - Confidence: {:.2}\n\
                         - Constraints: {} items\n\
                         - Objectives: {} items\n\
                         - Resources: {} items\n\
                         - Open questions: {} items\n\
                         - Current best score: {:.3}",
                        strategy,
                        candidate.score,
                        candidate.task.clarified_task,
                        candidate.task.confidence,
                        candidate.task.constraints.len(),
                        candidate.task.objectives.len(),
                        candidate.task.resources.len(),
                        candidate.task.open_questions.len(),
                        best_candidate.as_ref().map(|c| c.score).unwrap_or(0.0)
                    );

                    let is_new_best = best_candidate
                        .as_ref()
                        .map(|current| candidate.score > current.score)
                        .unwrap_or(true);
                    if is_new_best {
                        info!(
                            "[MAGICIAN-QUERY-REWRITER] 🏆 NEW BEST CANDIDATE from strategy {}\n\
                             - New score: {:.3}\n\
                             - Previous best: {:.3}",
                            strategy,
                            candidate.score,
                            best_candidate.as_ref().map(|c| c.score).unwrap_or(0.0)
                        );
                    }

                    best_candidate = match best_candidate {
                        None => Some(candidate),
                        Some(ref current) if candidate.score > current.score => Some(candidate),
                        _ => best_candidate,
                    };
                },
                Err(err) => {
                    warn!(
                        "[MAGICIAN-QUERY-REWRITER] ❌ CANDIDATE PARSING FAILED for strategy {}: {err:?}",
                        strategy
                    );
                },
            }
        }

        // LOCATION 4: Exit logging - log final result or failure
        match best_candidate {
            Some(ref candidate) => {
                info!(
                    "[MAGICIAN-QUERY-REWRITER] ✅ === REWRITER SUCCESS ===\n\
                     - Final score: {:.3}\n\
                     - Winning strategy: {:?}\n\
                     - Clarified task: '{}'\n\
                     - Confidence: {:.2}\n\
                     - Constraints: {:?}\n\
                     - Objectives: {:?}\n\
                     - Resources: {:?}\n\
                     - Open questions: {:?}\n\
                     - Slot graph ID: {}",
                    candidate.score,
                    candidate.task.rewrite_strategy,
                    candidate.task.clarified_task,
                    candidate.task.confidence,
                    candidate.task.constraints,
                    candidate.task.objectives,
                    candidate.task.resources,
                    candidate.task.open_questions,
                    candidate.task.slot_graph_id
                );
            },
            None => {
                warn!(
                    "[MAGICIAN-QUERY-REWRITER] ❌ === REWRITER FAILURE ===\n\
                     - No valid candidate produced\n\
                     - Customer query: '{}'\n\
                     - Current query: '{}'\n\
                     - Slot graph size: {} slots\n\
                     - Unresolved slots: {} slots",
                    customer_query,
                    current_query,
                    slot_graph.len(),
                    unresolved_slots.len()
                );
            },
        }

        best_candidate
            .map(|candidate| candidate.task)
            .ok_or_else(|| anyhow!("failed to produce a valid ClarifiedTask candidate"))
    }

    /// Filter and rephrase open questions to only those required for planning.
    pub async fn curate_clarification_questions(
        &self,
        clarified_task: &ClarifiedTask,
        max_questions: usize,
    ) -> Result<Vec<CuratedClarificationQuestion>> {
        self.curate_clarification_questions_with_telemetry(
            clarified_task,
            max_questions,
            None,
            OperationLlmCallAttribution::default(),
        )
        .await
    }

    #[instrument(
        skip(self, clarified_task),
        fields(open_questions = clarified_task.open_questions.len(), max_questions = max_questions)
    )]
    pub async fn curate_clarification_questions_with_telemetry(
        &self,
        clarified_task: &ClarifiedTask,
        max_questions: usize,
        llm_telemetry: Option<&OperationLlmTelemetryContext>,
        attribution: OperationLlmCallAttribution,
    ) -> Result<Vec<CuratedClarificationQuestion>> {
        if clarified_task.open_questions.is_empty() {
            return Ok(Vec::new());
        }

        let prompt = self
            .build_curation_prompt(clarified_task, max_questions)
            .await?;
        let request = RewriteModelRequest {
            prompt,
            strategy: RewriteStrategy::Conservative,
            model: self.config.model.clone(),
            temperature: self.config.temperature,
        };

        let started_at = std::time::Instant::now();
        let response = match llm_telemetry {
            Some(telemetry) => {
                self.curation_llm
                    .generate_scoped(telemetry.scope(), request)
                    .await
            },
            None => self.curation_llm.generate(request).await,
        }?;
        let parsed = Self::parse_curated_questions(&response.completion);
        if let (Some(context), Some(usage)) = (llm_telemetry, response.telemetry.as_ref()) {
            let latency_ms = started_at.elapsed().as_millis() as u64;
            match parsed.as_ref() {
                Ok(_) => context.emit_usage_validated_success(
                    "question_curation",
                    usage,
                    latency_ms,
                    attribution,
                    "question_curation_json",
                ),
                Err(error) => context.emit_usage_validation_failure(
                    "question_curation",
                    usage,
                    latency_ms,
                    attribution,
                    "question_curation_json",
                    &error.to_string(),
                ),
            }
        }
        let payload = parsed?;
        Ok(payload.questions)
    }

    async fn prompt(&self) -> Result<&Prompt> {
        self.prompt_template
            .get_or_try_init(|| async {
                self.prompt_manager
                    .get_prompt(
                        constants::names::QUESTION_REWRITING,
                        &self.config.prompt_version,
                    )
                    .await
                    .context("failed to load question rewriting prompt")
            })
            .await
    }

    async fn curation_prompt(&self) -> Result<&Prompt> {
        self.curation_prompt_template
            .get_or_try_init(|| async {
                self.prompt_manager
                    .get_prompt(
                        constants::names::QUESTION_CURATION,
                        constants::versions::QUESTION_CURATION,
                    )
                    .await
                    .context("failed to load question curation prompt")
            })
            .await
    }
}

impl QuestionRewriter {
    async fn build_curation_prompt(
        &self,
        clarified_task: &ClarifiedTask,
        max_questions: usize,
    ) -> Result<String> {
        let candidate_list = clarified_task
            .open_questions
            .iter()
            .enumerate()
            .map(|(idx, q)| format!("{}. {}", idx + 1, q.question_text.trim()))
            .collect::<Vec<_>>()
            .join("\n");

        let constraint_block = if clarified_task.constraints.is_empty() {
            "None recorded.".to_string()
        } else {
            clarified_task
                .constraints
                .iter()
                .enumerate()
                .map(|(idx, c)| format!("{}. {}", idx + 1, c.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let template = self.curation_prompt().await?;
        let mut variables = HashMap::new();
        variables.insert(
            "clarified_task".to_string(),
            clarified_task.clarified_task.clone(),
        );
        variables.insert("constraints".to_string(), constraint_block);
        variables.insert("candidate_questions".to_string(), candidate_list);
        variables.insert("max_questions".to_string(), max_questions.to_string());

        template
            .render(&variables)
            .with_context(|| "failed to render question curation prompt")
    }

    fn parse_curated_questions(raw: &str) -> Result<CuratedQuestionPayload> {
        let trimmed = raw.trim();
        if let Ok(payload) = serde_json::from_str::<CuratedQuestionPayload>(trimmed) {
            return Ok(payload);
        }

        if let Some(json_fragment) = extract_json_object(trimmed) {
            serde_json::from_str(json_fragment).map_err(|err| {
                anyhow!(
                    "failed to parse curated question payload from fragment: {}",
                    err
                )
            })
        } else {
            Err(anyhow!(
                "model response did not contain a valid JSON payload: {}",
                trimmed
            ))
        }
    }
}

struct Candidate {
    task: ClarifiedTask,
    score: f64,
}

fn render_prompt(
    template: &Prompt,
    current_query: &str,
    slot_summary: &str,
    unresolved_summary: &str,
    strategy: RewriteStrategy,
) -> Result<String> {
    let mut variables = HashMap::new();
    variables.insert("strategy_name".to_string(), strategy.label().to_string());
    variables.insert(
        "strategy_instructions".to_string(),
        strategy.instructions().to_string(),
    );
    variables.insert(
        "original_query".to_string(),
        format!("\"{}\"", current_query.trim()),
    );
    variables.insert("slot_summary".to_string(), slot_summary.to_string());
    variables.insert(
        "unresolved_slots".to_string(),
        unresolved_summary.to_string(),
    );

    template
        .render(&variables)
        .context("failed to render question rewriting prompt")
}

fn summarise_slot_graph(slot_graph: &[SlotRecord]) -> Result<String> {
    if slot_graph.is_empty() {
        return Ok("(No slots captured yet.)".to_string());
    }

    let mut slots: Vec<&SlotRecord> = slot_graph.iter().collect();
    slots.sort_by(|a, b| a.id.cmp(&b.id));

    let mut lines = Vec::with_capacity(slots.len());
    for slot in slots {
        let value = serde_json::to_string(&slot.value)
            .unwrap_or_else(|_| "\"<unserializable>\"".to_string());
        let trimmed_value = truncate(&value, 220);
        let provenance = slot
            .provenance
            .iter()
            .map(|p| format!("{:?}", p.source))
            .collect::<Vec<_>>();
        let mut line = format!(
            "- [{:?}] {}: {} (confidence {:.2})",
            slot.slot_type, slot.id, trimmed_value, slot.confidence
        );
        if !slot.evidence_links.is_empty() {
            line.push_str(&format!(" evidence_links={}", slot.evidence_links.len()));
        }
        if !provenance.is_empty() {
            line.push_str(&format!(" provenance={}", provenance.join("|")));
        }
        lines.push(line);
    }

    Ok(lines.join("\n"))
}

fn summarise_unresolved(unresolved_slots: &[String]) -> String {
    if unresolved_slots.is_empty() {
        "(No unresolved slots tracked.)".to_string()
    } else {
        unresolved_slots
            .iter()
            .filter_map(|slot| {
                let trimmed = slot.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(format!("- {}", trimmed))
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn sanitize_prompt_input(input: &str) -> String {
    input.trim().to_string()
}

fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        value.to_string()
    } else {
        format!("{}…", &value[..limit])
    }
}

fn compute_slot_graph_id(slot_graph: &[SlotRecord]) -> Result<String> {
    let mut slots: Vec<&SlotRecord> = slot_graph.iter().collect();
    slots.sort_by(|a, b| a.id.cmp(&b.id));

    let mut hasher = blake3::Hasher::new();
    for slot in slots {
        hasher.update(slot.id.as_bytes());
        hasher.update(
            serde_json::to_string(&slot.slot_type)
                .context("failed to serialize slot type")?
                .as_bytes(),
        );
        hasher.update(
            serde_json::to_string(&slot.value)
                .context("failed to serialize slot value")?
                .as_bytes(),
        );
        hasher.update(&slot.confidence.to_le_bytes());
        hasher.update(
            serde_json::to_string(&slot.provenance)
                .context("failed to serialize slot provenance")?
                .as_bytes(),
        );
        hasher.update(
            serde_json::to_string(&slot.evidence_links)
                .context("failed to serialize evidence links")?
                .as_bytes(),
        );
    }

    Ok(hasher.finalize().to_hex().to_string())
}

fn parse_candidate(
    raw: &str,
    slot_graph_id: &str,
    unresolved_slots: &[String],
    strategy: RewriteStrategy,
    conservative_floor: f64,
    customer_query: &str,
) -> Result<Candidate> {
    let sanitized = raw.trim();
    if sanitized.is_empty() {
        return Err(anyhow!("rewrite response empty"));
    }

    let payload: ClarifiedTaskPayload = serde_json::from_str(sanitized).or_else(|_| {
        // Attempt to recover from stray pre/post text by extracting JSON substring.
        extract_json_object(sanitized)
            .ok_or_else(|| anyhow!("unable to locate JSON object in rewrite response"))
            .and_then(|json| serde_json::from_str(json).context("failed to parse extracted JSON"))
    })?;

    let clarified_task = payload
        .clarified_task
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| anyhow!("clarified_task missing or empty"))?;

    let mut constraints = normalise_list(payload.constraints);
    let objectives = normalise_list(payload.objectives);
    let resources = normalise_list(payload.resources);
    let mut open_questions = normalise_open_questions(payload.open_questions);

    inject_unresolved(unresolved_slots, &mut constraints, &mut open_questions);

    let mut confidence = payload.confidence.unwrap_or(0.5);
    if !confidence.is_finite() {
        confidence = 0.5;
    }
    confidence = confidence.clamp(0.0, 1.0);

    if matches!(strategy, RewriteStrategy::Conservative) && confidence < conservative_floor {
        confidence = conservative_floor;
    }

    let task = ClarifiedTask {
        clarified_task,
        constraints,
        objectives,
        resources,
        confidence,
        open_questions,
        slot_graph_id: slot_graph_id.to_string(),
        original_message: customer_query.to_string(),
        rewrite_strategy: Some(strategy),
    };

    let score = score_candidate(&task, strategy);

    Ok(Candidate { task, score })
}

fn score_candidate(task: &ClarifiedTask, strategy: RewriteStrategy) -> f64 {
    let base = task.confidence;
    let enrichment = (task.constraints.len() as f64 * 0.04)
        + (task.objectives.len() as f64 * 0.03)
        + (task.resources.len() as f64 * 0.02)
        - (task.open_questions.len() as f64 * 0.015);
    let strategy_bias = match strategy {
        RewriteStrategy::Aggressive => 0.02,
        RewriteStrategy::Conservative => 0.0,
    };
    (base + enrichment + strategy_bias).max(0.0)
}

fn normalise_list(items: Option<Vec<String>>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalised = Vec::new();
    if let Some(items) = items {
        for item in items {
            let trimmed = item.trim();
            if trimmed.is_empty() {
                continue;
            }
            let canonical = trimmed.to_string();
            if seen.insert(canonical.clone()) {
                normalised.push(canonical);
            }
        }
    }
    normalised
}

/// Whether a clarification question's whole (trimmed) text is a null-ish
/// sentinel — e.g. an upstream elicitor that serialized a `null` open-question
/// as the literal string "None"/"null". These are never real questions;
/// surfacing one produces a bogus planning HITL clarification (row title
/// "None", empty modal, "Clarified task:" leaked into the description) that
/// cannot round-trip on respond. Matched case-insensitively against the WHOLE
/// trimmed value only, so a legitimate question that merely *contains* "none"
/// (e.g. "Is none of these correct?") is unaffected.
pub fn is_sentinel_question_text(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "none" | "null" | "nil" | "undefined" | "nan" | "n/a"
    )
}

fn normalise_open_questions(items: Option<Vec<RawOpenQuestion>>) -> Vec<ClarifiedOpenQuestion> {
    let mut seen = HashSet::new();
    let mut normalised = Vec::new();
    if let Some(items) = items {
        for item in items {
            let mut question = ClarifiedOpenQuestion::from(item);
            let trimmed = question.question_text.trim().to_string();
            // Drop empty AND null-ish sentinel questions ("None"/"null"/…) at the
            // single choke-point that feeds `plan.pending_questions`, so a
            // stringified-null never becomes a surfaced clarification.
            if trimmed.is_empty() || is_sentinel_question_text(&trimmed) {
                continue;
            }
            let canonical = trimmed.to_lowercase();
            if seen.insert(canonical) {
                question.question_text = trimmed;
                normalised.push(question);
            }
        }
    }
    normalised
}

fn inject_unresolved(
    unresolved_slots: &[String],
    constraints: &mut Vec<String>,
    open_questions: &mut Vec<ClarifiedOpenQuestion>,
) {
    if unresolved_slots.is_empty() {
        return;
    }

    let existing_constraints: HashSet<String> =
        constraints.iter().map(|c| c.to_lowercase()).collect();
    let mut existing_questions: HashSet<String> = open_questions
        .iter()
        .map(|q| q.question_text.to_lowercase())
        .collect();

    for slot in unresolved_slots {
        let trimmed = slot.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_lowercase();
        if existing_constraints.contains(&lower) || existing_questions.contains(&lower) {
            continue;
        }

        let question_text = format!("Need to capture value for `{}`", trimmed);
        let mut question =
            ClarifiedOpenQuestion::with_slot_id(question_text.clone(), trimmed.to_string());
        question
            .context
            .push(format!("Pending slot `{}` is missing a value.", trimmed));
        open_questions.push(question);
        existing_questions.insert(lower);
    }
}

fn extract_json_object(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start = None;
    for (idx, &byte) in bytes.iter().enumerate() {
        match byte {
            b'{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            },
            b'}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(begin) = start {
                            return text.get(begin..=idx);
                        }
                    }
                }
            },
            _ => {},
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct ClarifiedTaskPayload {
    clarified_task: Option<String>,
    constraints: Option<Vec<String>>,
    objectives: Option<Vec<String>>,
    resources: Option<Vec<String>>,
    open_questions: Option<Vec<RawOpenQuestion>>,
    confidence: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawOpenQuestion {
    Text(String),
    Rich {
        question_text: String,
        #[serde(default)]
        context: Vec<String>,
        #[serde(default)]
        slot_id: Option<String>,
        #[serde(default)]
        slot_confidence: Option<f32>,
        #[serde(default)]
        related_slots: Vec<String>,
    },
}

impl From<RawOpenQuestion> for ClarifiedOpenQuestion {
    fn from(value: RawOpenQuestion) -> Self {
        match value {
            RawOpenQuestion::Text(text) => ClarifiedOpenQuestion::from_text(text),
            RawOpenQuestion::Rich {
                question_text,
                context,
                slot_id,
                slot_confidence,
                related_slots,
            } => ClarifiedOpenQuestion {
                question_text,
                context,
                slot_id,
                slot_confidence,
                related_slots,
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod sentinel_tests {
    use super::*;

    #[test]
    fn sentinel_question_text_matches_null_ish_whole_values_only() {
        for s in ["None", "none", " NULL ", "nil", "undefined", "NaN", "n/a"] {
            assert!(is_sentinel_question_text(s), "{s:?} should be a sentinel");
        }
        // Real questions — including ones that merely CONTAIN a sentinel word —
        // must survive (substring safety).
        for s in [
            "Which WhatsApp account should I use?",
            "Is none of these correct?",
            "Should the report be null-safe?",
        ] {
            assert!(
                !is_sentinel_question_text(s),
                "{s:?} should NOT be a sentinel"
            );
        }
    }

    #[test]
    fn normalise_open_questions_drops_empty_and_sentinel_questions() {
        let items = Some(vec![
            RawOpenQuestion::Text("None".to_string()),
            RawOpenQuestion::Text("   ".to_string()),
            RawOpenQuestion::Text("null".to_string()),
            RawOpenQuestion::Text("Which account should I use?".to_string()),
            // case-insensitive duplicate of the real one → deduped
            RawOpenQuestion::Text("which account should i use?".to_string()),
        ]);
        let out = normalise_open_questions(items);
        assert_eq!(out.len(), 1, "only the single real question should survive");
        assert_eq!(out[0].question_text, "Which account should I use?");
    }
}
