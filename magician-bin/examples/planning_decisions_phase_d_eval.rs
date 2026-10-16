//! Offline evaluation of the planning-decisions Phase D packs
//! (`answer_interpretation`, `progress_review`, `recipe_match`) against their
//! incumbent LLM calls.
//!
//! For each corpus case this runs, without the service:
//!
//! - the incumbent: the production LLM call with the store prompts
//!   (`AnswerInterpreter::interpret_with_llm`, `progress_review::llm_review`,
//!   `recipe_matcher::llm_match`) through the configured router;
//! - the engine: one shadow-mode decide over the adapter's production
//!   projection, on the engine at `decision.socket` (the live engine process,
//!   read-only `decide` calls; the operation must be bound there).
//!
//! It reports, per decision and head, agreement of the engine's top choice
//! with the LLM's label, how many answers clear the threshold (eligible) and
//! their agreement, how many cases a gate would resolve without the LLM (the
//! adapters' own resolution rules), latency and tokens. With `--repeat-llm`
//! the LLM runs twice per case: its own run-to-run agreement is the noise
//! floor any engine disagreement is read against.
//!
//! Writes `report.json` and prints a summary. Nothing is written to a scope.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use clap::Parser;
use decision_engine_contract::{batch::DecisionItem, request::Answer, DecideResponse};
use futures_util::{stream, StreamExt};
use magician::{
    config::{load_default_magician_config, load_magician_config_from_path, MagicianConfig},
    magician_v2::{
        api_mining::{
            recipe::{RecipeShape, TaskInput, TaskInputSchema, TaskInputSource, TaskRecipe},
            recipe_match_decisions,
            recipe_matcher::{llm_match, TaskShapeQuery},
        },
        ask_loop::{
            answer_interpretation_decisions as answers,
            answer_interpreter::{AnswerInterpretationLLM, AnswerInterpreter},
        },
        decision_host,
        decisions::runner,
        execution::agentic::progress_review::{self, ReviewCase},
        prompts::json_storage::{default_prompt_dir, JsonStorageConfig},
        query_analysis::operation_llm_router::{
            LLMOperation, OperationLlmRouter, SimplifiedLLMResponse,
        },
        JsonPromptStorage, PromptManager,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Parser)]
#[command(about = "Offline evaluation of the planning-decisions Phase D packs")]
struct Args {
    #[arg(
        long,
        default_value = "data/magician_v2/evals/planning_decisions_phase_d"
    )]
    corpus_dir: PathBuf,
    #[arg(
        long,
        default_value = "coverage/evals/planning-decisions-phase-d/latest"
    )]
    output_dir: PathBuf,
    /// Magician config; the active runtime config when omitted.
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    /// Only these decisions (comma-separated).
    #[arg(long)]
    only: Option<String>,
    /// Run the LLM twice per case (its own noise floor).
    #[arg(long)]
    repeat_llm: bool,
    /// Eligibility threshold the engine's answers are read at.
    #[arg(long, default_value_t = 0.8)]
    threshold: f64,
    /// The recipe matcher's confirm threshold (`api_mining.recipes.match_confirm_threshold`).
    #[arg(long, default_value_t = 0.85)]
    recipe_confirm_threshold: f32,
    #[arg(long, default_value_t = 3)]
    concurrency: usize,
}

/// The engine's answer to one head.
#[derive(Debug, Clone, Serialize)]
struct EngineHead {
    choice: String,
    confidence: f64,
    eligible: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
struct Cost {
    latency_ms: u64,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct EngineResult {
    /// The model whose answer the engine returned (the route's primary, or
    /// its backup when the primary was below threshold).
    model: Option<String>,
    heads: BTreeMap<String, EngineHead>,
    /// Whether the gated adapter would skip the LLM with these eligible answers.
    gate_resolves: bool,
    gate_reason: String,
    cost: Cost,
}

#[derive(Debug, Clone, Default, Serialize)]
struct LlmResult {
    /// Head -> label in the pack's option ids; empty when the reply did not parse.
    labels: BTreeMap<String, String>,
    cost: Cost,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CaseResult {
    decision: &'static str,
    id: String,
    expected: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<EngineResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_error: Option<String>,
    llm: LlmResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    llm_repeat: Option<LlmResult>,
}

#[derive(Debug, Clone, Deserialize)]
struct AnswerCase {
    id: String,
    question: String,
    answer: String,
    #[serde(default)]
    context: String,
    expected_type: Option<String>,
    expected_replan: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewCaseFile {
    id: String,
    goal: String,
    success_criteria: String,
    iterations: usize,
    streak: usize,
    work: Vec<String>,
    #[serde(default)]
    artifacts: Vec<String>,
    expected: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RecipeFixture {
    template: String,
    description_template: Option<String>,
    inputs: Vec<RecipeInputFixture>,
}

#[derive(Debug, Clone, Deserialize)]
struct RecipeInputFixture {
    name: String,
    schema: TaskInputSchema,
}

#[derive(Debug, Clone, Deserialize)]
struct RecipeCase {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    candidates: Vec<String>,
    expected: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RecipeCorpus {
    recipes: BTreeMap<String, RecipeFixture>,
    cases: Vec<RecipeCase>,
}

/// The production clarifier's adapter shape: the `answer_interpretation`
/// operation through the router, keeping the call's response for its usage.
struct RouterAnswerLlm {
    router: Arc<OperationLlmRouter>,
    last: Arc<Mutex<Option<SimplifiedLLMResponse>>>,
}

#[async_trait]
impl AnswerInterpretationLLM for RouterAnswerLlm {
    async fn interpret(&self, prompt: &str) -> std::result::Result<String, String> {
        self.interpret_with_system(None, prompt).await
    }

    async fn interpret_with_system(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> std::result::Result<String, String> {
        let response = self
            .router
            .generate_for_operation_with_system(
                &LLMOperation::Other("answer_interpretation".to_owned()),
                system_prompt,
                prompt,
            )
            .await
            .map_err(|e| e.to_string())?;
        let content = response.content.clone();
        if let Ok(mut last) = self.last.lock() {
            *last = Some(response);
        }
        Ok(content)
    }
}

struct Ctx {
    router: Arc<OperationLlmRouter>,
    prompt_manager: Arc<PromptManager>,
    principal: String,
    workspace: String,
    threshold: f64,
    recipe_threshold: f32,
}

type Job = std::pin::Pin<Box<dyn std::future::Future<Output = CaseResult> + Send>>;

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
    let prompt_manager = Arc::new(PromptManager::new(Arc::new(JsonPromptStorage::new(
        JsonStorageConfig {
            storage_dir: default_prompt_dir(),
            enable_cache: true,
            max_cache_entries: 64,
        },
    )?)));
    // The progress reviewer renders through the global prompt store.
    magician::magician_v2::prompts::set_global_prompt_manager(prompt_manager.clone());

    let only: Option<Vec<String>> = args
        .only
        .as_ref()
        .map(|o| o.split(',').map(|s| s.trim().to_owned()).collect());
    let wanted = |name: &str| only.as_ref().is_none_or(|o| o.iter().any(|x| x == name));
    let mut primed = BTreeMap::new();
    for op in [
        answers::DECISION_ID,
        progress_review::DECISION_ID,
        recipe_match_decisions::DECISION_ID,
    ] {
        if wanted(op) {
            let ok =
                decision_host::prime_classification_for_eval(op, &args.principal, &args.workspace)
                    .await;
            println!("{op} policy discovered: {ok}");
            primed.insert(op, ok);
        }
    }

    let ctx = Arc::new(Ctx {
        router,
        prompt_manager,
        principal: args.principal.clone(),
        workspace: args.workspace.clone(),
        threshold: args.threshold,
        recipe_threshold: args.recipe_confirm_threshold,
    });
    let repeat = args.repeat_llm;
    let started = Instant::now();
    let mut jobs: Vec<Job> = Vec::new();

    if wanted(answers::DECISION_ID) {
        let corpus = read_json(&args.corpus_dir.join("answer_interpretation.json"))?;
        let cases: Vec<AnswerCase> = serde_json::from_value(corpus["cases"].clone())?;
        for case in cases {
            let ctx = ctx.clone();
            jobs.push(Box::pin(
                async move { answer_case(&ctx, case, repeat).await },
            ));
        }
    }
    if wanted(progress_review::DECISION_ID) {
        let corpus = read_json(&args.corpus_dir.join("progress_review.json"))?;
        let cases: Vec<ReviewCaseFile> = serde_json::from_value(corpus["cases"].clone())?;
        for case in cases {
            let ctx = ctx.clone();
            jobs.push(Box::pin(
                async move { review_case(&ctx, case, repeat).await },
            ));
        }
    }
    if wanted(recipe_match_decisions::DECISION_ID) {
        let corpus: RecipeCorpus =
            serde_json::from_value(read_json(&args.corpus_dir.join("recipe_match.json"))?)?;
        let recipes: Arc<BTreeMap<String, TaskRecipe>> = Arc::new(
            corpus
                .recipes
                .iter()
                .map(|(id, fixture)| (id.clone(), recipe(id, fixture)))
                .collect(),
        );
        for case in corpus.cases {
            let ctx = ctx.clone();
            let recipes = recipes.clone();
            jobs.push(Box::pin(async move {
                recipe_case(&ctx, &recipes, case, repeat).await
            }));
        }
    }
    println!("{} case(s)", jobs.len());
    let mut results: Vec<CaseResult> = stream::iter(jobs)
        .buffer_unordered(args.concurrency.max(1))
        .inspect(|r| println!("  done {}/{}", r.decision, r.id))
        .collect()
        .await;
    results.sort_by(|a, b| (a.decision, &a.id).cmp(&(b.decision, &b.id)));

    let summary = summarize(&results);
    let report = json!({
        "generated_by": "magician::planning_decisions_phase_d_eval",
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "elapsed_s": started.elapsed().as_secs(),
        "threshold": args.threshold,
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

async fn probe(
    ctx: &Ctx,
    op: &str,
    projection: &str,
    item: DecisionItem,
) -> Result<DecideResponse> {
    let scope = magicllm::LlmTraceContext::new(
        magicllm::LlmScope::new(ctx.principal.clone(), ctx.workspace.clone()),
        magicllm::LlmWorkloadClass::Ambient,
    );
    runner::probe(op, projection, vec![item], scope, Duration::from_secs(60))
        .await
        .map_err(anyhow::Error::msg)
}

/// Every answer for the single item with eligibility at the threshold, and
/// the eligible answers as the adapter's gate would see them.
fn read_engine(
    ctx: &Ctx,
    reply: &DecideResponse,
) -> (BTreeMap<String, EngineHead>, BTreeMap<String, Answer>, Cost) {
    let mut heads = BTreeMap::new();
    let mut eligible = BTreeMap::new();
    if let Some(response) = reply.batch.items.first().and_then(|i| i.response.as_ref()) {
        for (question, answer) in &response.answers {
            if let Answer::Choice {
                choice, confidence, ..
            } = answer
            {
                let ok = *confidence >= ctx.threshold;
                heads.insert(
                    question.as_str().to_owned(),
                    EngineHead {
                        choice: choice.as_str().to_owned(),
                        confidence: *confidence,
                        eligible: ok,
                    },
                );
                if ok {
                    eligible.insert(question.as_str().to_owned(), answer.clone());
                }
            }
        }
    }
    let usage = serde_json::to_value(&reply.model_calls).unwrap_or(Value::Null);
    let sum = |key: &str| {
        usage
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| c.get(key).and_then(Value::as_u64))
                    .sum::<u64>()
            })
            .unwrap_or(0)
    };
    (
        heads,
        eligible,
        Cost {
            latency_ms: reply.latency_ms,
            input_tokens: Some(sum("input_tokens")),
            output_tokens: Some(sum("output_tokens")),
        },
    )
}

fn cost_of(response: Option<&SimplifiedLLMResponse>, latency_ms: u64) -> Cost {
    let usage = response.and_then(|r| r.usage.as_ref());
    Cost {
        latency_ms,
        input_tokens: usage.map(|u| u64::from(u.prompt_tokens)),
        output_tokens: usage.map(|u| u64::from(u.completion_tokens)),
    }
}

fn engine_result(
    ctx: &Ctx,
    reply: Result<DecideResponse>,
    gate: impl FnOnce(&BTreeMap<String, Answer>) -> std::result::Result<(), &'static str>,
) -> (Option<EngineResult>, Option<String>) {
    match reply {
        Ok(reply) => {
            let (heads, eligible, cost) = read_engine(ctx, &reply);
            let model = reply
                .batch
                .items
                .first()
                .and_then(|i| i.response.as_ref())
                .map(|r| r.model.model.clone());
            let gate = gate(&eligible);
            (
                Some(EngineResult {
                    model,
                    heads,
                    gate_resolves: gate.is_ok(),
                    gate_reason: gate.err().unwrap_or("resolved").to_owned(),
                    cost,
                }),
                None,
            )
        },
        Err(error) => (None, Some(error.to_string())),
    }
}

async fn answer_llm(ctx: &Ctx, case: &AnswerCase) -> LlmResult {
    let last = Arc::new(Mutex::new(None));
    let interpreter = AnswerInterpreter::new(
        RouterAnswerLlm {
            router: ctx.router.clone(),
            last: last.clone(),
        },
        ctx.prompt_manager.clone(),
    );
    let context = (!case.context.is_empty()).then_some(case.context.as_str());
    let result = interpreter
        .interpret_with_llm(&case.question, &case.answer, context, None, None)
        .await;
    let response = last.lock().ok().and_then(|mut l| l.take());
    match result {
        Ok(call) => {
            let mut labels = BTreeMap::new();
            if call.structured {
                labels.insert(
                    "answer_type".to_owned(),
                    answers::answer_type_id(call.answer.answer_type).to_owned(),
                );
                labels.insert(
                    "requires_replan".to_owned(),
                    answers::replan_id(call.answer.requires_replan).to_owned(),
                );
            }
            LlmResult {
                labels,
                cost: cost_of(response.as_ref(), call.latency_ms),
                error: (!call.structured).then(|| "reply did not parse".to_owned()),
            }
        },
        Err(error) => LlmResult {
            error: Some(error.to_string()),
            ..LlmResult::default()
        },
    }
}

async fn answer_case(ctx: &Ctx, case: AnswerCase, repeat: bool) -> CaseResult {
    let context = (!case.context.is_empty()).then_some(case.context.as_str());
    let item = answers::project(&case.question, &case.answer, context);
    let reply = probe(ctx, answers::DECISION_ID, answers::PROJECTION, item).await;
    let (engine, engine_error) = engine_result(ctx, reply, |eligible| {
        answers::resolve_without_llm(eligible).map(|_| ())
    });
    let llm = answer_llm(ctx, &case).await;
    let llm_repeat = if repeat {
        Some(answer_llm(ctx, &case).await)
    } else {
        None
    };
    let mut expected = BTreeMap::new();
    if let Some(v) = &case.expected_type {
        expected.insert("answer_type".to_owned(), v.clone());
    }
    if let Some(v) = &case.expected_replan {
        expected.insert("requires_replan".to_owned(), v.clone());
    }
    CaseResult {
        decision: answers::DECISION_ID,
        id: case.id,
        expected,
        engine,
        engine_error,
        llm,
        llm_repeat,
    }
}

/// The production work-summary shape (`decision.rs::summarize_reviewer_work`).
fn work_summary(case: &ReviewCaseFile) -> String {
    let first = case.iterations.saturating_sub(case.work.len()) + 1;
    let lines: Vec<String> = case
        .work
        .iter()
        .enumerate()
        .map(|(i, sig)| format!("- iter {}: {}", first + i, sig))
        .collect();
    let mut out = format!(
        "Recent actions ({} total iterations):\n{}",
        case.iterations,
        lines.join("\n")
    );
    if !case.artifacts.is_empty() {
        out.push_str("\n\nArtifacts produced so far: ");
        out.push_str(&case.artifacts.join(", "));
    }
    out
}

async fn review_llm(ctx: &Ctx, review: &ReviewCase) -> LlmResult {
    match progress_review::llm_review(&ctx.router, review).await {
        Ok(call) => LlmResult {
            labels: match &call.verdict {
                Ok(verdict) => BTreeMap::from([(
                    "decision".to_owned(),
                    progress_review::decision_id(verdict.decision).to_owned(),
                )]),
                Err(_) => BTreeMap::new(),
            },
            cost: cost_of(Some(&call.response), call.latency_ms),
            error: call.verdict.err().map(|e| e.to_string()),
        },
        Err(error) => LlmResult {
            error: Some(error.to_string()),
            ..LlmResult::default()
        },
    }
}

async fn review_case(ctx: &Ctx, case: ReviewCaseFile, repeat: bool) -> CaseResult {
    let review = ReviewCase {
        goal: case.goal.clone(),
        success_criteria: case.success_criteria.clone(),
        iterations: case.iterations,
        streak: case.streak,
        cap: 20,
        work_summary: work_summary(&case),
    };
    let item = progress_review::project(&review);
    let reply = probe(
        ctx,
        progress_review::DECISION_ID,
        progress_review::PROJECTION,
        item,
    )
    .await;
    let (engine, engine_error) = engine_result(ctx, reply, |eligible| {
        progress_review::resolve_without_llm(eligible, String::new()).map(|_| ())
    });
    let llm = review_llm(ctx, &review).await;
    let llm_repeat = if repeat {
        Some(review_llm(ctx, &review).await)
    } else {
        None
    };
    CaseResult {
        decision: progress_review::DECISION_ID,
        id: case.id,
        expected: case
            .expected
            .map(|v| BTreeMap::from([("decision".to_owned(), v)]))
            .unwrap_or_default(),
        engine,
        engine_error,
        llm,
        llm_repeat,
    }
}

fn recipe(id: &str, fixture: &RecipeFixture) -> TaskRecipe {
    TaskRecipe {
        id: id.to_owned(),
        scope_principal: "eval".into(),
        scope_workspace: "eval".into(),
        agent_id: "eval".into(),
        shape: RecipeShape {
            template: fixture.template.clone(),
            description_template: fixture.description_template.clone(),
            fingerprint: id.to_owned(),
            inputs: fixture
                .inputs
                .iter()
                .map(|input| TaskInput {
                    name: input.name.clone(),
                    schema: input.schema,
                    example_value: String::new(),
                    source: TaskInputSource::TaskText,
                })
                .collect(),
        },
        current_version: 1,
        versions: Vec::new(),
    }
}

async fn recipe_llm(
    ctx: &Ctx,
    query: &TaskShapeQuery<'_>,
    candidates: &[&TaskRecipe],
) -> LlmResult {
    match llm_match(&ctx.router, &ctx.prompt_manager, query, candidates).await {
        Some(call) => LlmResult {
            labels: call
                .choice(candidates, ctx.recipe_threshold)
                .map(|choice| BTreeMap::from([("recipe".to_owned(), choice)]))
                .unwrap_or_default(),
            cost: cost_of(Some(&call.response), call.latency_ms),
            error: call
                .reply
                .is_none()
                .then(|| "reply did not parse".to_owned()),
        },
        None => LlmResult {
            error: Some("LLM call failed".to_owned()),
            ..LlmResult::default()
        },
    }
}

async fn recipe_case(
    ctx: &Ctx,
    recipes: &BTreeMap<String, TaskRecipe>,
    case: RecipeCase,
    repeat: bool,
) -> CaseResult {
    let candidates: Vec<&TaskRecipe> = case
        .candidates
        .iter()
        .filter_map(|id| recipes.get(id))
        .collect();
    let item = recipe_match_decisions::project(&case.title, &case.description, &candidates);
    let reply = probe(
        ctx,
        recipe_match_decisions::DECISION_ID,
        recipe_match_decisions::PROJECTION,
        item,
    )
    .await;
    let (engine, engine_error) = engine_result(ctx, reply, |eligible| {
        if recipe_match_decisions::engine_says_none(eligible) {
            Ok(())
        } else {
            Err("match_needs_llm")
        }
    });
    let query = TaskShapeQuery {
        task_id: "offline",
        title: &case.title,
        description: &case.description,
        agent_id: "offline",
        principal: &ctx.principal,
        workspace: &ctx.workspace,
    };
    let llm = recipe_llm(ctx, &query, &candidates).await;
    let llm_repeat = if repeat {
        Some(recipe_llm(ctx, &query, &candidates).await)
    } else {
        None
    };
    CaseResult {
        decision: recipe_match_decisions::DECISION_ID,
        id: case.id,
        expected: case
            .expected
            .map(|v| BTreeMap::from([("recipe".to_owned(), v)]))
            .unwrap_or_default(),
        engine,
        engine_error,
        llm,
        llm_repeat,
    }
}

fn mean(values: &[u64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<u64>() as f64 / values.len() as f64)
}

fn summarize(results: &[CaseResult]) -> Value {
    let mut by_decision: BTreeMap<&str, Vec<&CaseResult>> = BTreeMap::new();
    for r in results {
        by_decision.entry(r.decision).or_default().push(r);
    }
    let mut out = serde_json::Map::new();
    for (decision, cases) in by_decision {
        let mut heads: BTreeMap<String, Value> = BTreeMap::new();
        let head_names: std::collections::BTreeSet<String> = cases
            .iter()
            .flat_map(|c| c.llm.labels.keys().cloned())
            .collect();
        for head in head_names {
            let (mut n, mut agree, mut eligible, mut eligible_agree) = (0, 0, 0, 0);
            let (mut gold_n, mut gold_llm, mut gold_engine) = (0, 0, 0);
            let (mut rep_n, mut rep_agree) = (0, 0);
            let mut disagreements = Vec::new();
            for c in &cases {
                let (Some(engine), Some(label)) = (&c.engine, c.llm.labels.get(&head)) else {
                    continue;
                };
                let Some(answer) = engine.heads.get(&head) else {
                    continue;
                };
                n += 1;
                let same = &answer.choice == label;
                agree += usize::from(same);
                if answer.eligible {
                    eligible += 1;
                    eligible_agree += usize::from(same);
                }
                if !same {
                    disagreements.push(json!({
                        "id": c.id, "llm": label, "engine": answer.choice,
                        "confidence": answer.confidence, "expected": c.expected.get(&head),
                    }));
                }
                if let Some(gold) = c.expected.get(&head) {
                    gold_n += 1;
                    gold_llm += usize::from(label == gold);
                    gold_engine += usize::from(&answer.choice == gold);
                }
                if let Some(rep) = c.llm_repeat.as_ref().and_then(|r| r.labels.get(&head)) {
                    rep_n += 1;
                    rep_agree += usize::from(rep == label);
                }
            }
            heads.insert(
                head,
                json!({
                    "compared": n,
                    "agree": agree,
                    "eligible": eligible,
                    "eligible_agree": eligible_agree,
                    "llm_vs_llm_repeat": if rep_n > 0 { json!(format!("{rep_agree}/{rep_n}")) } else { Value::Null },
                    "hand_label": {"cases": gold_n, "llm_correct": gold_llm, "engine_correct": gold_engine},
                    "disagreements": disagreements,
                }),
            );
        }
        let engine_latency: Vec<u64> = cases
            .iter()
            .filter_map(|c| c.engine.as_ref().map(|e| e.cost.latency_ms))
            .collect();
        let llm_latency: Vec<u64> = cases
            .iter()
            .filter(|c| c.llm.error.is_none())
            .map(|c| c.llm.cost.latency_ms)
            .collect();
        let engine_in: Vec<u64> = cases
            .iter()
            .filter_map(|c| c.engine.as_ref().and_then(|e| e.cost.input_tokens))
            .collect();
        let llm_in: Vec<u64> = cases
            .iter()
            .filter_map(|c| c.llm.cost.input_tokens)
            .collect();
        let llm_out: Vec<u64> = cases
            .iter()
            .filter_map(|c| c.llm.cost.output_tokens)
            .collect();
        let mut gate_reasons: BTreeMap<String, usize> = BTreeMap::new();
        let mut models: BTreeMap<String, usize> = BTreeMap::new();
        for c in &cases {
            if let Some(e) = &c.engine {
                *gate_reasons.entry(e.gate_reason.clone()).or_default() += 1;
                *models
                    .entry(e.model.clone().unwrap_or_else(|| "none".to_owned()))
                    .or_default() += 1;
            }
        }
        out.insert(
            decision.to_owned(),
            json!({
                "cases": cases.len(),
                "engine_errors": cases.iter().filter(|c| c.engine_error.is_some()).count(),
                "llm_errors": cases.iter().filter(|c| c.llm.error.is_some()).count(),
                "heads": heads,
                "gate_reasons": gate_reasons,
                "answered_by": models,
                "latency_ms_mean": {"engine": mean(&engine_latency), "llm": mean(&llm_latency)},
                "tokens_mean": {"engine_input": mean(&engine_in), "llm_input": mean(&llm_in), "llm_output": mean(&llm_out)},
            }),
        );
    }
    Value::Object(out)
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
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
