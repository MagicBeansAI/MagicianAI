//! Offline replay of the planning analysis paths (planning-decisions Phase C
//! steps 3–4): does taking the unified query-analysis LLM call off the
//! planning hot path change what planning sees?
//!
//! For each first-turn planning message (the committed corpus, plus — with
//! `--historical-root` — the planning turns already recorded in a scope's task
//! pipeline stores) this runs the production code of both paths, without the
//! running service:
//!
//! - analysis: `UnifiedQueryAnalyzer::analyze_query_with_context_traced` on
//!   the legacy path, which runs the unified LLM call and the gated
//!   `query_intent` engine decision together and reports what the new path
//!   would have resolved from the engine alone (and why it would fall back);
//! - extraction: the legacy slot extraction (seeded with the analysis'
//!   entities) and the assessed one (`SLOT_EXTRACTION_ASSESSED`, no seed);
//! - elicitation: the production `ElicitationService` on each side's analysis
//!   and slots, so the clarification decision includes the rewriter's open
//!   questions and their curation.
//!
//! The decision engine is the one at `decision.socket` (the live engine
//! process, read-only `decide` calls); model calls go through the configured
//! router. `--repeat-legacy` runs the legacy side a second time to measure
//! its own run-to-run noise, the floor any disagreement is read against.
//!
//! Writes `report.json` and prints a summary. Nothing is written to a scope.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::Parser;
use futures_util::{stream, StreamExt};
use magician::{
    config::{load_default_magician_config, load_magician_config_from_path, MagicianConfig},
    magician_v2::{
        analytics::operation_llm_telemetry::OperationLlmCallAttribution,
        decision_host,
        pipeline::system_agents::slot_extractor::{extracted_entities_from, legacy_seed_context},
        prompts::{
            constants::versions,
            json_storage::{default_prompt_dir, JsonStorageConfig},
        },
        query_analysis::{
            analysis_path::{AnalysisPath, CallCost, EXTRACTION_GATE},
            operation_llm_router::{LLMOperation, OperationLLMWrapper, OperationLlmRouter},
            unified_analyzer::AnalysisTrace,
            ConversationContext, UnifiedQueryAnalysis, UnifiedQueryAnalyzer,
        },
        slot_graph::{
            extraction::SlotExtraction, ElicitationConfig, ElicitationService, EnrichmentPipeline,
            ExtractionConfig, MultiLlmExtractionAdapter, MultiLlmRewriteModel, ProvisionalSlot,
            QuestionRewriter, RewriterConfig, SlotExtractor, SlotGraphRepository, SlotRecord,
        },
        storage::{TurnDirection, V2Turn, WaitingState},
        ConfidenceService, JsonPromptStorage, PromptManager,
    },
};
use runtime_core::{ExecutionContext, ToolCatalog, ToolInfo};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Parser)]
#[command(about = "Offline replay of the legacy and new planning analysis paths")]
struct Args {
    /// Corpus of first-turn planning messages.
    #[arg(
        long,
        default_value = "data/magician_v2/evals/planning_analysis_path/corpus.json"
    )]
    corpus: PathBuf,
    /// A scope's `tasks/` directory: also replay its recorded planning turns
    /// (pipeline stores) and compare against what was recorded.
    #[arg(long)]
    historical_root: Option<PathBuf>,
    /// Where to write report.json.
    #[arg(long, default_value = "coverage/evals/planning-analysis-path/latest")]
    output_dir: PathBuf,
    /// Magician config; the active runtime config when omitted.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Scope the engine decision and model calls are attributed to.
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    /// Run the legacy side twice per case (its own noise floor).
    #[arg(long)]
    repeat_legacy: bool,
    /// Skip elicitation (analysis and extraction only).
    #[arg(long)]
    no_elicitation: bool,
    /// Only these case ids (comma-separated).
    #[arg(long)]
    only: Option<String>,
    /// Cases in flight at once.
    #[arg(long, default_value_t = 3)]
    concurrency: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct Corpus {
    cases: Vec<CorpusCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct CorpusCase {
    id: String,
    query: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    expect_clarification: Option<bool>,
}

/// What a recorded planning turn's pipeline store holds.
#[derive(Debug, Clone, Serialize)]
struct Recorded {
    intent: Option<String>,
    task_clarity: Option<String>,
    complexity_score: Option<f64>,
    extraction_confidence: Option<f64>,
    entity_values: Vec<String>,
    slot_count: usize,
    needs_clarification: Option<bool>,
    question_count: usize,
}

#[derive(Debug, Clone)]
struct Case {
    id: String,
    source: &'static str,
    kind: String,
    query: String,
    expect_clarification: Option<bool>,
    recorded: Option<Recorded>,
}

#[derive(Debug, Clone, Serialize)]
struct ExtractionResult {
    extraction_confidence: Option<f32>,
    extraction_reasoning: Option<String>,
    gate_triggered: bool,
    slots: Vec<Value>,
    cost: CallCost,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ElicitationOutcome {
    needs_clarification: bool,
    question_count: usize,
    open_questions: Vec<String>,
    clarified_task: String,
    slot_overall_confidence: f64,
    unresolved_slots: usize,
    latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Side {
    extraction: ExtractionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    elicitation: Option<ElicitationOutcome>,
}

#[derive(Debug, Clone, Serialize)]
struct CaseResult {
    id: String,
    source: &'static str,
    kind: String,
    query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expect_clarification: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recorded: Option<Recorded>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<AnalysisTrace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy: Option<Side>,
    #[serde(skip_serializing_if = "Option::is_none")]
    legacy_repeat: Option<Side>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<Side>,
    legacy_entity_values: Vec<String>,
    legacy_entities_covered: f64,
    missing_legacy_entities: Vec<String>,
}

struct NullToolCatalog;

#[async_trait]
impl ToolCatalog for NullToolCatalog {
    async fn list_tool_names(&self, _: &ExecutionContext) -> Vec<String> {
        Vec::new()
    }
    async fn available_categories(&self, _: &ExecutionContext) -> Vec<String> {
        Vec::new()
    }
    async fn get_tool_metadata(
        &self,
        _: &str,
        _: &ExecutionContext,
    ) -> Option<std::collections::HashMap<String, Value>> {
        None
    }
    async fn filtered_tools_by_categories(
        &self,
        _: &[String],
        _: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>> {
        Ok(Vec::new())
    }
    async fn all_tools(&self, _: &ExecutionContext) -> Result<Vec<ToolInfo>> {
        Ok(Vec::new())
    }
    async fn category_tool_counts(
        &self,
        _: &ExecutionContext,
    ) -> Result<std::collections::HashMap<String, usize>> {
        Ok(std::collections::HashMap::new())
    }
}

struct DiscardSlots;

#[async_trait]
impl SlotGraphRepository for DiscardSlots {
    async fn create_slot(&self, _: &SlotRecord) -> Result<()> {
        Ok(())
    }
}

struct Lanes {
    analyzer: UnifiedQueryAnalyzer,
    legacy_extractor: Arc<SlotExtractor>,
    assessed_extractor: Arc<SlotExtractor>,
    elicitation: Option<Arc<ElicitationService>>,
    context: ExecutionContext,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    load_runtime_env_files();
    magician_chunking::register_builtin_chunk_adapters()
        .context("registering the built-in logical-context chunk adapters")?;
    let config = load_config(&args)?;
    let router_config = config
        .router_config()
        .cloned()
        .context("a configured LLM router is required")?;
    let router = Arc::new(OperationLlmRouter::new(Some(router_config)));

    decision_host::set_global_decision_locality(
        config.privacy.processing.mode == magicllm::ProcessingLocality::Local,
    );
    decision_host::configure(&config.decision);
    let primed = decision_host::prime_classification_for_eval(
        "query_intent",
        &args.principal,
        &args.workspace,
    )
    .await;
    println!(
        "query_intent policy discovered: {primed} (false = engine unavailable; every case falls back)"
    );

    let prompt_manager = Arc::new(PromptManager::new(Arc::new(JsonPromptStorage::new(
        JsonStorageConfig {
            storage_dir: default_prompt_dir(),
            enable_cache: true,
            max_cache_entries: 64,
        },
    )?)));
    let tools: Arc<dyn ToolCatalog> = Arc::new(NullToolCatalog);
    let analyzer = UnifiedQueryAnalyzer::new(
        Arc::new(OperationLLMWrapper::new(
            router.clone(),
            LLMOperation::QueryAnalysis,
        )),
        tools,
        prompt_manager.clone(),
    );
    let legacy_extractor = Arc::new(SlotExtractor::new(
        Arc::new(MultiLlmExtractionAdapter::new(router.clone())),
        prompt_manager.clone(),
        ExtractionConfig::default(),
    ));
    let assessed_extractor =
        Arc::new(legacy_extractor.with_prompt_version(versions::SLOT_EXTRACTION_ASSESSED));
    let elicitation = (!args.no_elicitation).then(|| {
        Arc::new(ElicitationService::new(
            legacy_extractor.clone(),
            Arc::new(EnrichmentPipeline::default()),
            Arc::new(QuestionRewriter::with_curation_model(
                Arc::new(MultiLlmRewriteModel::new(
                    router.clone(),
                    LLMOperation::QuestionRewriting,
                )),
                Arc::new(MultiLlmRewriteModel::new(
                    router.clone(),
                    LLMOperation::QuestionCuration,
                )),
                prompt_manager.clone(),
                RewriterConfig::default(),
            )),
            Arc::new(ConfidenceService::default()),
            Arc::new(DiscardSlots),
            None,
            None,
            None,
            ElicitationConfig::default(),
        ))
    });
    let mut context = ExecutionContext::default();
    context.principal = args.principal.clone();
    context.workspace = args.workspace.clone();
    let lanes = Arc::new(Lanes {
        analyzer,
        legacy_extractor,
        assessed_extractor,
        elicitation,
        context,
    });

    let mut cases = load_corpus(&args.corpus)?;
    if let Some(root) = &args.historical_root {
        cases.extend(load_historical(root)?);
    }
    if let Some(only) = &args.only {
        let keep: Vec<&str> = only.split(',').map(str::trim).collect();
        cases.retain(|case| keep.contains(&case.id.as_str()));
    }
    println!("{} case(s)", cases.len());

    let repeat = args.repeat_legacy;
    let started = Instant::now();
    let mut results: Vec<CaseResult> = stream::iter(cases)
        .map(|case| {
            let lanes = lanes.clone();
            async move {
                let result = run_case(&lanes, case, repeat).await;
                println!("  done {}", result.id);
                result
            }
        })
        .buffer_unordered(args.concurrency.max(1))
        .collect()
        .await;
    results.sort_by(|a, b| (a.source, &a.id).cmp(&(b.source, &b.id)));

    let summary = summarize(&results);
    let report = json!({
        "generated_by": "magician::planning_analysis_path_eval",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "elapsed_s": started.elapsed().as_secs(),
        "engine_policy_discovered": primed,
        "summary": summary,
        "cases": results,
    });
    std::fs::create_dir_all(&args.output_dir)?;
    let path = args.output_dir.join("report.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report["summary"])?);
    println!("report: {}", path.display());
    Ok(())
}

async fn run_case(lanes: &Lanes, case: Case, repeat: bool) -> CaseResult {
    let workflow = format!("replay-{}", case.id);
    let conversation = ConversationContext {
        execution_status: WaitingState::Planning,
        pending_slots: Vec::new(),
        recent_turns: vec![V2Turn {
            id: format!("turn-{}", case.id),
            execution_id: workflow.clone(),
            direction: TurnDirection::Inbound,
            text: case.query.clone(),
            in_reply_to_slot_id: None,
            created_at: chrono::Utc::now().timestamp_millis(),
            query_analysis: None,
            analysis_metadata: None,
            strategy_attempts: Vec::new(),
            processing_metadata: None,
            recommended_questions: None,
            enriched_query: None,
        }],
    };
    let mut result = CaseResult {
        id: case.id.clone(),
        source: case.source,
        kind: case.kind.clone(),
        query: case.query.clone(),
        expect_clarification: case.expect_clarification,
        recorded: case.recorded.clone(),
        analysis: None,
        analysis_error: None,
        legacy: None,
        legacy_repeat: None,
        new: None,
        legacy_entity_values: Vec::new(),
        legacy_entities_covered: 1.0,
        missing_legacy_entities: Vec::new(),
    };

    // Legacy analysis: the LLM and the gated engine together; the trace says
    // what the new path would have resolved from the engine alone.
    let analysis = lanes
        .analyzer
        .analyze_query_with_context_traced(
            AnalysisPath::Legacy,
            &case.query,
            &lanes.context,
            None,
            conversation,
            Some(&workflow),
            Some(&workflow),
        )
        .await;
    let (legacy_analysis, trace) = match analysis {
        Ok(pair) => pair,
        Err(error) => {
            result.analysis_error = Some(format!("{error:#}"));
            return result;
        },
    };
    result.legacy_entity_values = legacy_analysis
        .extracted_entities
        .entities
        .values()
        .cloned()
        .collect();
    result.legacy_entity_values.sort();

    // The new path's analysis: engine heads when resolved; on fallback the
    // legacy analysis (the LLM runs exactly as on legacy). Extraction is the
    // slot extractor's either way.
    let mut new_analysis = legacy_analysis.clone();
    if let Some(heads) = trace
        .new_path
        .heads
        .as_ref()
        .filter(|_| trace.new_path.resolved)
    {
        new_analysis.complexity.score = heads.complexity_score;
        new_analysis.categories.categories.clear();
        new_analysis.dependencies.is_multi_step = false;
        new_analysis.dependencies.workflow_steps.clear();
        new_analysis.dependencies.required_capabilities.clear();
        new_analysis.slot_match = None;
    }
    result.analysis = Some(trace);

    let legacy = run_legacy_side(lanes, &case, &workflow, &legacy_analysis).await;
    if repeat {
        let workflow = format!("{workflow}-r");
        result.legacy_repeat =
            Some(run_legacy_side(lanes, &case, &workflow, &legacy_analysis).await);
    }
    let (new_side, new_slots) = run_new_side(lanes, &case, &workflow, new_analysis).await;
    let (coverage, missing) = entity_coverage(&result.legacy_entity_values, &new_slots);
    result.legacy_entities_covered = coverage;
    result.missing_legacy_entities = missing;
    result.legacy = Some(legacy);
    result.new = Some(new_side);
    result
}

async fn run_legacy_side(
    lanes: &Lanes,
    case: &Case,
    workflow: &str,
    analysis: &UnifiedQueryAnalysis,
) -> Side {
    let seed = legacy_seed_context(analysis);
    let extraction = extract(
        &lanes.legacy_extractor,
        &case.query,
        seed.as_ref(),
        workflow,
    )
    .await;
    let (slots, mut extraction) = match extraction {
        Ok(extraction) => {
            let slots = extraction.slots.clone();
            let mut view = extraction_view(&extraction, false);
            // The legacy gate reads the analysis' extraction confidence.
            view.extraction_confidence = Some(analysis.extracted_entities.extraction_confidence);
            view.extraction_reasoning =
                Some(analysis.extracted_entities.extraction_reasoning.clone());
            (slots, view)
        },
        Err(error) => (Vec::new(), extraction_error(error)),
    };
    extraction.gate_triggered = analysis.extracted_entities.extraction_confidence < EXTRACTION_GATE;
    let elicitation = elicit(lanes, case, workflow, analysis, &slots).await;
    Side {
        extraction,
        elicitation,
    }
}

async fn run_new_side(
    lanes: &Lanes,
    case: &Case,
    workflow: &str,
    mut analysis: UnifiedQueryAnalysis,
) -> (Side, Vec<ProvisionalSlot>) {
    let workflow = format!("{workflow}-new");
    match extract(&lanes.assessed_extractor, &case.query, None, &workflow).await {
        Ok(extraction) => {
            analysis.extracted_entities = extracted_entities_from(&extraction);
            let slots = extraction.slots.clone();
            let view = extraction_view(&extraction, true);
            let elicitation = elicit(lanes, case, &workflow, &analysis, &slots).await;
            (
                Side {
                    extraction: view,
                    elicitation,
                },
                slots,
            )
        },
        Err(error) => (
            Side {
                extraction: extraction_error(error),
                elicitation: None,
            },
            Vec::new(),
        ),
    }
}

async fn extract(
    extractor: &SlotExtractor,
    query: &str,
    seed: Option<&magician::magician_v2::slot_graph::ConversationContext>,
    workflow: &str,
) -> Result<SlotExtraction> {
    extractor
        .extract_with_scope(
            query,
            seed,
            Some(workflow),
            Some(workflow),
            None,
            None,
            OperationLlmCallAttribution::default(),
        )
        .await
}

fn extraction_view(extraction: &SlotExtraction, assessed: bool) -> ExtractionResult {
    let confidence = extraction.extraction_confidence.filter(|_| assessed);
    ExtractionResult {
        extraction_confidence: confidence,
        extraction_reasoning: extraction.extraction_reasoning.clone().filter(|_| assessed),
        gate_triggered: confidence.unwrap_or(0.0) < EXTRACTION_GATE,
        slots: extraction
            .slots
            .iter()
            .map(|slot| {
                json!({
                    "type": slot.slot_type,
                    "value": slot.value,
                    "confidence": slot.confidence,
                })
            })
            .collect(),
        cost: CallCost::from_telemetry(extraction.latency_ms, extraction.telemetry.as_ref()),
        error: None,
    }
}

fn extraction_error(error: anyhow::Error) -> ExtractionResult {
    ExtractionResult {
        extraction_confidence: None,
        extraction_reasoning: None,
        gate_triggered: false,
        slots: Vec::new(),
        cost: CallCost::default(),
        error: Some(format!("{error:#}")),
    }
}

async fn elicit(
    lanes: &Lanes,
    case: &Case,
    workflow: &str,
    analysis: &UnifiedQueryAnalysis,
    slots: &[ProvisionalSlot],
) -> Option<ElicitationOutcome> {
    let service = lanes.elicitation.as_ref()?;
    let records: Vec<SlotRecord> = slots
        .iter()
        .cloned()
        .map(|slot| SlotRecord::from_provisional(workflow, slot))
        .collect();
    let started = Instant::now();
    let outcome = service
        .elicit_and_rewrite_with_scope(
            workflow,
            &case.query,
            Some(analysis),
            None,
            Some(records),
            None,
            None,
            None,
            OperationLlmCallAttribution::default(),
        )
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    Some(match outcome {
        Ok(result) => ElicitationOutcome {
            needs_clarification: result.needs_clarification,
            question_count: result.recommended_questions.len(),
            open_questions: result
                .clarified_task
                .open_questions
                .iter()
                .map(|q| q.question_text.clone())
                .collect(),
            clarified_task: result.clarified_task.clarified_task.clone(),
            slot_overall_confidence: result.confidence_summary.overall,
            unresolved_slots: result.confidence_summary.unresolved_slots.len(),
            latency_ms,
            error: None,
        },
        Err(error) => ElicitationOutcome {
            needs_clarification: false,
            question_count: 0,
            open_questions: Vec::new(),
            clarified_task: String::new(),
            slot_overall_confidence: 0.0,
            unresolved_slots: 0,
            latency_ms,
            error: Some(format!("{error:#}")),
        },
    })
}

fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(normalize(text)),
        Value::Number(number) => out.push(number.to_string()),
        Value::Array(items) => items.iter().for_each(|item| leaves(item, out)),
        Value::Object(map) => map
            .iter()
            .filter(|(key, _)| key.as_str() != "type")
            .for_each(|(_, item)| leaves(item, out)),
        _ => {},
    }
}

fn entity_coverage(values: &[String], slots: &[ProvisionalSlot]) -> (f64, Vec<String>) {
    if values.is_empty() {
        return (1.0, Vec::new());
    }
    let mut new_leaves = Vec::new();
    for slot in slots {
        leaves(&slot.value, &mut new_leaves);
    }
    let missing: Vec<String> = values
        .iter()
        .filter(|value| {
            let value = normalize(value);
            !value.is_empty()
                && !new_leaves.iter().any(|leaf| {
                    !leaf.is_empty() && (leaf.contains(&value) || value.contains(leaf.as_str()))
                })
        })
        .cloned()
        .collect();
    (
        (values.len() - missing.len()) as f64 / values.len() as f64,
        missing,
    )
}

fn summarize(results: &[CaseResult]) -> Value {
    let mut counts: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut bump = |key: &'static str, agree: Option<bool>| {
        if let Some(agree) = agree {
            let entry = counts.entry(key).or_default();
            entry.1 += 1;
            if agree {
                entry.0 += 1;
            }
        }
    };
    let mut resolved = 0;
    let mut fallbacks: BTreeMap<&str, usize> = BTreeMap::new();
    let mut legacy_analysis_ms = Vec::new();
    let mut legacy_analysis_in = Vec::new();
    let mut engine_ms = Vec::new();
    let mut legacy_slot_ms = Vec::new();
    let mut new_slot_ms = Vec::new();
    let mut legacy_slot_in = Vec::new();
    let mut new_slot_in = Vec::new();
    let mut coverage = Vec::new();
    let mut disagreements = Vec::new();
    for result in results {
        let Some(trace) = &result.analysis else {
            continue;
        };
        if trace.new_path.resolved {
            resolved += 1;
        } else if let Some(reason) = trace.new_path.fallback_reason {
            *fallbacks.entry(reason).or_default() += 1;
        }
        if let (Some(heads), Some(llm)) = (&trace.new_path.heads, &trace.llm) {
            bump(
                "intent: engine vs LLM",
                Some(heads.intent == llm.heads.intent),
            );
            bump(
                "task_clarity: engine vs LLM",
                Some(heads.task_clarity == llm.heads.task_clarity),
            );
            bump(
                "complexity bucket: engine vs LLM",
                Some(heads.complexity_bucket == llm.heads.complexity_bucket),
            );
            let mut legacy = trace.result.clone();
            legacy.complexity_score = heads.complexity_score;
            bump("heads: new vs legacy result", Some(*heads == legacy));
        }
        if let Some(llm) = &trace.llm {
            legacy_analysis_ms.push(llm.cost.latency_ms as f64);
            if let Some(tokens) = llm.cost.input_tokens {
                legacy_analysis_in.push(f64::from(tokens));
            }
        }
        if let Some(ms) = trace.engine_latency_ms {
            engine_ms.push(ms as f64);
        }
        coverage.push(result.legacy_entities_covered);
        let (Some(legacy), Some(new)) = (&result.legacy, &result.new) else {
            continue;
        };
        legacy_slot_ms.push(legacy.extraction.cost.latency_ms as f64);
        new_slot_ms.push(new.extraction.cost.latency_ms as f64);
        if let Some(tokens) = legacy.extraction.cost.input_tokens {
            legacy_slot_in.push(f64::from(tokens));
        }
        if let Some(tokens) = new.extraction.cost.input_tokens {
            new_slot_in.push(f64::from(tokens));
        }
        let gate_agree = legacy.extraction.gate_triggered == new.extraction.gate_triggered;
        bump("0.7 extraction gate: new vs legacy", Some(gate_agree));
        let clarify = match (&legacy.elicitation, &new.elicitation) {
            (Some(l), Some(n)) if l.error.is_none() && n.error.is_none() => {
                Some(l.needs_clarification == n.needs_clarification)
            },
            _ => None,
        };
        bump("needs_clarification: new vs legacy", clarify);
        if let Some(repeat) = &result.legacy_repeat {
            bump(
                "0.7 extraction gate: legacy vs legacy repeat",
                Some(legacy.extraction.gate_triggered == repeat.extraction.gate_triggered),
            );
            let noise = match (&legacy.elicitation, &repeat.elicitation) {
                (Some(l), Some(r)) if l.error.is_none() && r.error.is_none() => {
                    Some(l.needs_clarification == r.needs_clarification)
                },
                _ => None,
            };
            bump("needs_clarification: legacy vs legacy repeat", noise);
        }
        if let Some(recorded) = &result.recorded {
            let recorded_gate = recorded
                .extraction_confidence
                .map(|confidence| confidence < f64::from(EXTRACTION_GATE));
            bump(
                "0.7 extraction gate: new vs recorded turn",
                recorded_gate.map(|gate| gate == new.extraction.gate_triggered),
            );
            let new_clarify = new
                .elicitation
                .as_ref()
                .filter(|n| n.error.is_none())
                .map(|n| n.needs_clarification);
            bump(
                "needs_clarification: new vs recorded turn",
                recorded
                    .needs_clarification
                    .zip(new_clarify)
                    .map(|(recorded, new)| recorded == new),
            );
            let legacy_clarify = legacy
                .elicitation
                .as_ref()
                .filter(|l| l.error.is_none())
                .map(|l| l.needs_clarification);
            bump(
                "needs_clarification: legacy rerun vs recorded turn",
                recorded
                    .needs_clarification
                    .zip(legacy_clarify)
                    .map(|(recorded, legacy)| recorded == legacy),
            );
        }
        if !gate_agree || clarify == Some(false) {
            disagreements.push(result.id.clone());
        }
    }
    let mean = |values: &[f64]| {
        if values.is_empty() {
            Value::Null
        } else {
            json!((values.iter().sum::<f64>() / values.len() as f64).round())
        }
    };
    json!({
        "cases": results.len(),
        "analysis_errors": results.iter().filter(|r| r.analysis_error.is_some()).count(),
        "new_path_resolved_without_llm": resolved,
        "new_path_fallbacks": fallbacks,
        "agreement": counts
            .into_iter()
            .map(|(key, (agree, total))| (key.to_string(), json!(format!("{agree}/{total}"))))
            .collect::<serde_json::Map<_, _>>(),
        "mean_legacy_entities_covered_by_new_slots": mean(&coverage),
        "latency_ms": {
            "legacy_query_analysis_llm": mean(&legacy_analysis_ms),
            "engine_query_intent": mean(&engine_ms),
            "legacy_slot_extraction": mean(&legacy_slot_ms),
            "assessed_slot_extraction": mean(&new_slot_ms),
        },
        "input_tokens": {
            "legacy_query_analysis_llm": mean(&legacy_analysis_in),
            "legacy_slot_extraction": mean(&legacy_slot_in),
            "assessed_slot_extraction": mean(&new_slot_in),
        },
        "clarification_or_gate_disagreements": disagreements,
    })
}

fn load_corpus(path: &Path) -> Result<Vec<Case>> {
    let corpus: Corpus = serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
    )?;
    Ok(corpus
        .cases
        .into_iter()
        .map(|case| Case {
            id: case.id,
            source: "corpus",
            kind: case.kind,
            query: case.query,
            expect_clarification: case.expect_clarification,
            recorded: None,
        })
        .collect())
}

/// Planning turns recorded in `<tasks>/<task>/executions/<exec>/pipeline/store.json`.
fn load_historical(tasks: &Path) -> Result<Vec<Case>> {
    let mut cases = Vec::new();
    for task in std::fs::read_dir(tasks)? {
        let executions = task?.path().join("executions");
        let Ok(entries) = std::fs::read_dir(&executions) else {
            continue;
        };
        for execution in entries {
            let execution = execution?.path();
            let store = execution.join("pipeline").join("store.json");
            let Ok(bytes) = std::fs::read(&store) else {
                continue;
            };
            let Ok(store) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if let Some(case) = historical_case(&execution, &store) {
                cases.push(case);
            }
        }
    }
    Ok(cases)
}

fn historical_case(execution: &Path, store: &Value) -> Option<Case> {
    let artifacts = store.get("artifacts")?.as_object()?;
    let latest = |kind: &str| {
        artifacts
            .values()
            .filter(|a| a.get("artifact_type").and_then(Value::as_str) == Some(kind))
            .max_by_key(|a| {
                a.get("produced_at")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .and_then(|a| a.get("content"))
    };
    let qa = latest("query_analysis")?;
    let query = qa.get("original_query")?.as_str()?.trim().to_string();
    if query.is_empty() {
        return None;
    }
    let entities = qa.pointer("/extracted_entities/entities");
    let elicitation = latest("elicitation_result");
    let id = execution.file_name()?.to_string_lossy().to_string();
    Some(Case {
        id: format!("hist-{id}"),
        source: "historical",
        kind: "recorded".to_string(),
        query,
        expect_clarification: None,
        recorded: Some(Recorded {
            intent: qa.get("intent").and_then(Value::as_str).map(str::to_owned),
            task_clarity: qa
                .get("task_clarity")
                .and_then(Value::as_str)
                .map(str::to_owned),
            complexity_score: qa.pointer("/complexity/score").and_then(Value::as_f64),
            extraction_confidence: qa
                .pointer("/extracted_entities/extraction_confidence")
                .and_then(Value::as_f64),
            entity_values: entities
                .and_then(Value::as_object)
                .map(|map| {
                    map.values()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            slot_count: latest("slot_graph")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
            needs_clarification: elicitation
                .and_then(|e| e.get("needs_clarification"))
                .and_then(Value::as_bool),
            question_count: elicitation
                .and_then(|e| e.get("recommended_questions"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        }),
    })
}

fn load_config(args: &Args) -> Result<MagicianConfig> {
    match args.config.as_deref() {
        Some(path) => load_magician_config_from_path(path)
            .with_context(|| format!("loading config {}", path.display())),
        None => load_default_magician_config().context("loading the active magician config"),
    }
}

fn load_runtime_env_files() {
    for file_name in [".env.development", ".env"] {
        let path = magician::magician_v2::artifact_v2::workspace::runtime_config_path(
            file_name, file_name,
        );
        let _ = dotenvy::from_path(path);
    }
}
