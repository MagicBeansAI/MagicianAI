//! Boundary B's live half: does a model answer better from a bounded working
//! set than from packed context?
//!
//! The deterministic suite (`artifact_v2::working_set_eval`) proves evidence
//! grounding — a planted fact is reachable, exactly attributed, and cheap to
//! ship — without a model. It cannot say whether a model, handed each lane's
//! bytes, actually *answers* the question. This lane hands it exactly those
//! bytes and asks.
//!
//! Same fixtures, same two lanes, same probes. For every probe the model is
//! asked the question twice: once over the context-packing lane's shipped text
//! (the corpus, packed to the budget, anonymous) and once over the working-set
//! lane's (one bounded search's excerpts plus the top-cited chunk, each named
//! by source). Correctness is the planted literal appearing in the answer —
//! deterministic, so the verdict cannot drift with a judge's mood — and an LLM
//! judge scores answer quality against the planted fact on top, because the
//! activation rule asks for judged quality and a literal match cannot tell a
//! confident wrong answer from a hedge. Latency and cost are the model's own.
//!
//! The gate this feeds is `content_acquisition.working_sets.activation.
//! enabled_lanes`, which stays empty until a lane has this evidence. This
//! binary writes the evidence; it does not open the lane.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use magician::{
    config::{load_default_magician_config, load_magician_config_from_path, MagicianConfig},
    magician_v2::{
        artifact_v2::{
            working_set_eval::{
                all_fixtures, EvalFixture, GroundTruthProbe, CONTEXT_PACK_BUDGET_BYTES,
            },
            workspace::ArtifactV2Workspace,
            WorkingSetStore,
        },
        query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter},
    },
};
use serde::Serialize;

const SCHEMA_VERSION: u32 = 1;
const GENERATED_BY: &str = "magician::working_set_live_eval";
const PRINCIPAL: &str = "working-set-eval";
const WORKSPACE: &str = "isolated";
const ANSWER_OPERATION: &str = "working_set_answer_eval";
const JUDGE_OPERATION: &str = "working_set_answer_judge";
const SEARCH_LIMIT: usize = 5;

/// What the lane must show before the routing gate may open for it. Chosen
/// so the deterministic suite's promise carries through to answers: the facts
/// packing cannot reach, the working set must answer; the facts both can
/// reach, the working set must not answer materially worse; and it must not
/// cost more to get there.
const MIN_WORKING_SET_CORRECT_BEYOND_BUDGET: f64 = 0.9;
const MAX_PARITY_LOSS_WITHIN_BUDGET: f64 = 0.1;

#[derive(Debug, Parser)]
#[command(about = "Live evaluation of the working-set lane against context packing")]
struct Args {
    /// Where to write report.json and report.html.
    #[arg(long, default_value = "coverage/evals/working-set/live/latest")]
    output_dir: PathBuf,
    /// Magician config; the active runtime config when omitted.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Ask each probe this many times per lane and pool the results.
    #[arg(long, default_value_t = 1)]
    repeat: usize,
    /// Print the plan and exit without a provider call.
    #[arg(long)]
    dry_run: bool,
    /// Print each lane's shipped context to stderr before asking. For reading
    /// a miss without a rebuild.
    #[arg(long)]
    show_context: bool,
    /// Provider-free: pin the report contract and the gate arithmetic.
    #[arg(long)]
    self_test: bool,
}

#[derive(Debug, Clone, Serialize)]
struct LaneAnswer {
    lane: &'static str,
    bytes_shipped: u64,
    /// The planted literal appears in the answer.
    correct: bool,
    /// LLM-judged 0.0–1.0 against the planted fact; `None` when the judge
    /// call failed, which is recorded rather than scored as zero.
    judged_quality: Option<f64>,
    latency_ms: u64,
    provider: Option<String>,
    model: Option<String>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cost_usd: Option<f64>,
    /// The answer's first line, for the reader. Never the corpus.
    answer_head: String,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProbeResult {
    fixture: &'static str,
    question: &'static str,
    beyond_pack_budget: bool,
    context_pack: LaneAnswer,
    working_set: LaneAnswer,
}

#[derive(Debug, Clone, Serialize, Default)]
struct LaneSummary {
    probes: usize,
    correct: usize,
    correct_rate: f64,
    mean_judged_quality: Option<f64>,
    mean_latency_ms: f64,
    mean_bytes_shipped: f64,
    total_cost_usd: f64,
    priced_calls: usize,
}

#[derive(Debug, Clone, Serialize)]
struct Gate {
    name: &'static str,
    passed: bool,
    detail: String,
}

#[derive(Debug, Clone, Serialize)]
struct Report {
    schema_version: u32,
    generated_by: &'static str,
    generated_at: String,
    mode: &'static str,
    repeat: usize,
    context_pack_budget_bytes: usize,
    probes: Vec<ProbeResult>,
    beyond_budget: LaneComparison,
    within_budget: LaneComparison,
    all: LaneComparison,
    gates: Vec<Gate>,
    passed: bool,
    /// What a passing run licenses and what it does not.
    activation_note: String,
}

#[derive(Debug, Clone, Serialize, Default)]
struct LaneComparison {
    context_pack: LaneSummary,
    working_set: LaneSummary,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .compact()
        .init();
    let args = Args::parse();
    if args.repeat == 0 {
        anyhow::bail!("--repeat must be at least 1");
    }

    if args.self_test {
        let report = self_test_report(args.repeat);
        write_report(&args.output_dir, &report)?;
        print_summary(&report);
        return Ok(());
    }

    let fixtures = all_fixtures();
    let probe_count: usize = fixtures.iter().map(|fixture| fixture.probes.len()).sum();
    if args.dry_run {
        println!(
            "working-set live eval plan: {} fixtures, {} probes, {} answer calls + {} judge calls at repeat={}",
            fixtures.len(),
            probe_count,
            probe_count * 2 * args.repeat,
            probe_count * 2 * args.repeat,
            args.repeat
        );
        return Ok(());
    }

    load_runtime_env_files();
    // The router validates every chunked profile's adapter against the global
    // registry at construction and disables *all* operations when one is
    // missing — including the unchunked ones this eval makes. The runtime
    // registers the built-in adapters at boot; a standalone binary has to do
    // the same, which is why this example lives beside the runtime's boot
    // rather than in the library crate, whose examples cannot reach them.
    magician_chunking::register_builtin_chunk_adapters()
        .context("registering the built-in logical-context chunk adapters")?;
    let config = load_config(&args)?;
    let router_config = config
        .router_config()
        .cloned()
        .context("a configured LLM router is required for the live working-set eval")?;
    let router = Arc::new(OperationLlmRouter::new(Some(router_config)));

    // An isolated workspace: the fixtures are synthetic and must never land
    // in a real scope's working sets.
    let temp = tempfile::Builder::new()
        .prefix("magician-working-set-live-eval-")
        .tempdir()
        .context("creating the isolated eval workspace")?;
    let store = WorkingSetStore::new(ArtifactV2Workspace::new(temp.path()));

    let mut probes = Vec::with_capacity(probe_count * args.repeat);
    for fixture in &fixtures {
        store
            .create(PRINCIPAL, WORKSPACE, fixture.request.clone())
            .await
            .with_context(|| format!("creating the `{}` working set", fixture.name))?;
        for probe in &fixture.probes {
            for _ in 0..args.repeat {
                let packed = packed_context(fixture);
                let working = working_set_context(&store, fixture, probe).await;
                if args.show_context {
                    eprintln!(
                        "--- [{}] {} | working-set context ({} bytes) ---\n{}\n--- end ---",
                        fixture.name,
                        probe.question,
                        working.len(),
                        working
                    );
                }
                let context_pack = answer_lane("context_pack", &router, probe, &packed).await;
                let working_set = answer_lane("working_set", &router, probe, &working).await;
                eprintln!(
                    "[{}] {} | pack: correct={} {}ms | working-set: correct={} {}ms",
                    fixture.name,
                    probe.question,
                    context_pack.correct,
                    context_pack.latency_ms,
                    working_set.correct,
                    working_set.latency_ms
                );
                probes.push(ProbeResult {
                    fixture: fixture.name,
                    question: probe.question,
                    beyond_pack_budget: probe.beyond_pack_budget,
                    context_pack,
                    working_set,
                });
            }
        }
    }

    let report = assemble(probes, args.repeat, "live");
    write_report(&args.output_dir, &report)?;
    print_summary(&report);
    if report.passed {
        Ok(())
    } else {
        std::process::exit(1)
    }
}

/// The bytes context packing ships: the corpus, in order, cut at the budget.
/// Anonymous on purpose — that is what "put it all in the prompt" does.
fn packed_context(fixture: &EvalFixture) -> String {
    let mut packed = String::new();
    for source in &fixture.request.sources {
        packed.push_str(&source.document.text);
        packed.push('\n');
        if packed.len() >= CONTEXT_PACK_BUDGET_BYTES {
            break;
        }
    }
    let mut budget_end = CONTEXT_PACK_BUDGET_BYTES.min(packed.len());
    while budget_end > 0 && !packed.is_char_boundary(budget_end) {
        budget_end -= 1;
    }
    packed[..budget_end].to_string()
}

/// The bytes the working-set lane ships: one bounded search's excerpts and the
/// top-cited chunk, each labelled with its source — the `research-working-sets`
/// procedure, exactly as the deterministic lane measures it.
async fn working_set_context(
    store: &WorkingSetStore,
    fixture: &EvalFixture,
    probe: &GroundTruthProbe,
) -> String {
    let working_set_id = &fixture.request.working_set_id;
    let matches = match store
        .search(
            PRINCIPAL,
            WORKSPACE,
            working_set_id,
            probe.query,
            SEARCH_LIMIT,
        )
        .await
    {
        Ok(matches) => matches,
        Err(_) => return String::new(),
    };
    let mut context = String::new();
    for (index, search_match) in matches.iter().enumerate() {
        context.push_str(&format!(
            "[excerpt {} · source {} · \"{}\"]\n{}\n\n",
            index + 1,
            search_match.source_id,
            search_match.source_title,
            search_match.excerpt
        ));
    }
    if let Some(top) = matches.first() {
        if let Ok(chunk) = store
            .read_chunk(
                PRINCIPAL,
                WORKSPACE,
                working_set_id,
                &top.source_id,
                top.chunk_index,
            )
            .await
        {
            context.push_str(&format!(
                "[cited chunk · source {} · \"{}\" · chunk {}]\n{}\n",
                chunk.source_id, chunk.source_title, chunk.chunk_index, chunk.text
            ));
        }
    }
    context
}

async fn answer_lane(
    lane: &'static str,
    router: &OperationLlmRouter,
    probe: &GroundTruthProbe,
    context: &str,
) -> LaneAnswer {
    let system = "You answer a researcher's question using only the context you are given. \
                  Quote the exact value or identifier when the context contains one. If the \
                  context does not contain the answer, reply exactly: NOT IN CONTEXT.";
    let prompt = format!(
        "Context:\n{context}\n\nQuestion: {}\n\nAnswer in one or two sentences.",
        probe.question
    );
    let started = Instant::now();
    let response = router
        .generate_for_operation_with_system(
            &LLMOperation::Other(ANSWER_OPERATION.to_string()),
            Some(system),
            &prompt,
        )
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    let bytes_shipped = context.len() as u64;

    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return LaneAnswer {
                lane,
                bytes_shipped,
                correct: false,
                judged_quality: None,
                latency_ms,
                provider: None,
                model: None,
                input_tokens: None,
                output_tokens: None,
                cost_usd: None,
                answer_head: String::new(),
                error: Some(format!("{error:#}")),
            };
        },
    };

    let answer = response.content.trim().to_string();
    // The answer key, not the planted line: a model asked for one or two
    // sentences restates the value, it does not reproduce the corpus line, and
    // the first live run failed three answers the judge scored 1.0 on exactly
    // that. Case-insensitive because "500 Requests" is not a wrong answer.
    let correct = answer
        .to_lowercase()
        .contains(&probe.answer_key.to_lowercase());
    let (provider, model, input_tokens, output_tokens, cost_usd) = match &response.telemetry {
        Some(telemetry) if telemetry.usage_reported => {
            let kind = magicllm::LLMProviderKind::from_str(&telemetry.provider);
            let usage = magicllm::TokenUsage {
                prompt_tokens: Some(telemetry.input_tokens),
                completion_tokens: Some(telemetry.output_tokens),
                total_tokens: Some(telemetry.input_tokens + telemetry.output_tokens),
                reasoning_tokens: Some(telemetry.reasoning_tokens),
                ..Default::default()
            };
            let cost = magicllm::compute_cost(&kind, &telemetry.model, &usage);
            (
                Some(telemetry.provider.clone()),
                Some(telemetry.model.clone()),
                Some(telemetry.input_tokens),
                Some(telemetry.output_tokens),
                // Unknown is never zero: a model with no pricing row reports
                // no cost rather than a free one.
                (cost > 0.0).then_some(cost),
            )
        },
        Some(telemetry) => (
            Some(telemetry.provider.clone()),
            Some(telemetry.model.clone()),
            None,
            None,
            None,
        ),
        None => (None, None, None, None, None),
    };

    let judged_quality = judge(router, probe, &answer).await;

    LaneAnswer {
        lane,
        bytes_shipped,
        correct,
        judged_quality,
        latency_ms,
        provider,
        model,
        input_tokens,
        output_tokens,
        cost_usd,
        answer_head: answer
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect(),
        error: None,
    }
}

/// 0.0–1.0 against the planted fact. The judge sees the reference answer and
/// the candidate, never the corpus, so it grades the answer and not the
/// retrieval twice.
async fn judge(router: &OperationLlmRouter, probe: &GroundTruthProbe, answer: &str) -> Option<f64> {
    let system = "You grade a short answer against a reference. Reply with only a number from \
                  0.0 to 1.0: 1.0 when the answer states the reference value correctly and \
                  without contradiction, 0.5 when it is partially right or hedged, 0.0 when it \
                  is wrong, missing, or says the information is not available.";
    // The judge is given the value a correct answer states, and the line it
    // was planted in for context. Given the line alone it marked a correct
    // "where is it defined" answer wrong because the line was a signature and
    // the answer was, rightly, a file.
    let prompt = format!(
        "Question: {}\nA correct answer states: {}\n(The source line this comes from reads: {})\nCandidate answer: {}\n\nScore:",
        probe.question, probe.answer_key, probe.fact, answer
    );
    let response = router
        .generate_for_operation_with_system(
            &LLMOperation::Other(JUDGE_OPERATION.to_string()),
            Some(system),
            &prompt,
        )
        .await
        .ok()?;
    let text = response.content.trim();
    let number: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    number
        .parse::<f64>()
        .ok()
        .map(|score| score.clamp(0.0, 1.0))
}

fn summarize<'a>(answers: impl Iterator<Item = &'a LaneAnswer>) -> LaneSummary {
    let answers: Vec<&LaneAnswer> = answers.collect();
    let probes = answers.len();
    if probes == 0 {
        return LaneSummary::default();
    }
    let correct = answers.iter().filter(|answer| answer.correct).count();
    let judged: Vec<f64> = answers
        .iter()
        .filter_map(|answer| answer.judged_quality)
        .collect();
    let priced: Vec<f64> = answers
        .iter()
        .filter_map(|answer| answer.cost_usd)
        .collect();
    LaneSummary {
        probes,
        correct,
        correct_rate: correct as f64 / probes as f64,
        mean_judged_quality: (!judged.is_empty())
            .then(|| judged.iter().sum::<f64>() / judged.len() as f64),
        mean_latency_ms: answers
            .iter()
            .map(|answer| answer.latency_ms as f64)
            .sum::<f64>()
            / probes as f64,
        mean_bytes_shipped: answers
            .iter()
            .map(|answer| answer.bytes_shipped as f64)
            .sum::<f64>()
            / probes as f64,
        total_cost_usd: priced.iter().sum(),
        priced_calls: priced.len(),
    }
}

fn compare<'a>(probes: impl Iterator<Item = &'a ProbeResult> + Clone) -> LaneComparison {
    LaneComparison {
        context_pack: summarize(probes.clone().map(|probe| &probe.context_pack)),
        working_set: summarize(probes.map(|probe| &probe.working_set)),
    }
}

fn assemble(probes: Vec<ProbeResult>, repeat: usize, mode: &'static str) -> Report {
    let beyond_budget = compare(probes.iter().filter(|probe| probe.beyond_pack_budget));
    let within_budget = compare(probes.iter().filter(|probe| !probe.beyond_pack_budget));
    let all = compare(probes.iter());
    let errors = probes
        .iter()
        .flat_map(|probe| [&probe.context_pack, &probe.working_set])
        .filter(|answer| answer.error.is_some())
        .count();

    let gates = vec![
        Gate {
            name: "every_call_answered",
            passed: errors == 0,
            detail: format!("{errors} lane call(s) failed"),
        },
        Gate {
            name: "working_set_answers_beyond_the_budget",
            passed: beyond_budget.working_set.correct_rate >= MIN_WORKING_SET_CORRECT_BEYOND_BUDGET,
            detail: format!(
                "working-set correct {}/{} ({:.0}%) on facts packing cannot reach; floor {:.0}%",
                beyond_budget.working_set.correct,
                beyond_budget.working_set.probes,
                beyond_budget.working_set.correct_rate * 100.0,
                MIN_WORKING_SET_CORRECT_BEYOND_BUDGET * 100.0
            ),
        },
        Gate {
            name: "packing_loses_beyond_the_budget",
            passed: beyond_budget.context_pack.correct_rate
                < beyond_budget.working_set.correct_rate,
            detail: format!(
                "packing correct {}/{} vs working-set {}/{} beyond the budget — the premise of the boundary",
                beyond_budget.context_pack.correct,
                beyond_budget.context_pack.probes,
                beyond_budget.working_set.correct,
                beyond_budget.working_set.probes
            ),
        },
        Gate {
            name: "parity_within_the_budget",
            passed: within_budget.working_set.correct_rate
                >= within_budget.context_pack.correct_rate - MAX_PARITY_LOSS_WITHIN_BUDGET,
            detail: format!(
                "working-set {:.0}% vs packing {:.0}% on facts both can reach; at most {:.0} points worse",
                within_budget.working_set.correct_rate * 100.0,
                within_budget.context_pack.correct_rate * 100.0,
                MAX_PARITY_LOSS_WITHIN_BUDGET * 100.0
            ),
        },
        Gate {
            name: "working_set_is_not_dearer",
            passed: all.working_set.priced_calls == 0
                || all.context_pack.priced_calls == 0
                || all.working_set.total_cost_usd <= all.context_pack.total_cost_usd,
            detail: format!(
                "working-set ${:.4} over {} priced calls vs packing ${:.4} over {}",
                all.working_set.total_cost_usd,
                all.working_set.priced_calls,
                all.context_pack.total_cost_usd,
                all.context_pack.priced_calls
            ),
        },
    ];
    let passed = gates.iter().all(|gate| gate.passed);
    Report {
        schema_version: SCHEMA_VERSION,
        generated_by: GENERATED_BY,
        generated_at: Utc::now().to_rfc3339(),
        mode,
        repeat,
        context_pack_budget_bytes: CONTEXT_PACK_BUDGET_BYTES,
        probes,
        beyond_budget,
        within_budget,
        all,
        gates,
        passed,
        activation_note: if passed {
            "This run is the live evidence the routing gate asks for. Opening a lane is still an \
             owner's edit to `content_acquisition.working_sets.activation.enabled_lanes`, and \
             the runtime router that would consume it is not yet built."
                .to_string()
        } else {
            "A failing gate means the lane's promise did not survive contact with a model on \
             these fixtures; `enabled_lanes` stays empty."
                .to_string()
        },
    }
}

fn self_test_report(repeat: usize) -> Report {
    fn answer(lane: &'static str, correct: bool, bytes: u64, cost: f64) -> LaneAnswer {
        LaneAnswer {
            lane,
            bytes_shipped: bytes,
            correct,
            judged_quality: Some(if correct { 1.0 } else { 0.0 }),
            latency_ms: 800,
            provider: Some("self-test".to_string()),
            model: Some("self-test".to_string()),
            input_tokens: Some(100),
            output_tokens: Some(20),
            cost_usd: Some(cost),
            answer_head: if correct { "planted" } else { "NOT IN CONTEXT" }.to_string(),
            error: None,
        }
    }
    let probes = vec![
        ProbeResult {
            fixture: "self-test",
            question: "a fact beyond the budget",
            beyond_pack_budget: true,
            context_pack: answer("context_pack", false, 131_072, 0.02),
            working_set: answer("working_set", true, 2_048, 0.001),
        },
        ProbeResult {
            fixture: "self-test",
            question: "a fact within the budget",
            beyond_pack_budget: false,
            context_pack: answer("context_pack", true, 131_072, 0.02),
            working_set: answer("working_set", true, 2_048, 0.001),
        },
    ];
    let report = assemble(probes, repeat, "self-test");
    assert!(
        report.passed,
        "the self-test fixture must pass its own gates: {:?}",
        report.gates
    );
    report
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

fn write_report(output_dir: &Path, report: &Report) -> Result<()> {
    fs::create_dir_all(output_dir).with_context(|| format!("creating {}", output_dir.display()))?;
    fs::write(
        output_dir.join("report.json"),
        serde_json::to_vec_pretty(report)?,
    )?;
    fs::write(output_dir.join("report.html"), render_html(report))?;
    Ok(())
}

fn print_summary(report: &Report) {
    for gate in &report.gates {
        println!(
            "{} {}: {}",
            if gate.passed { "PASS" } else { "FAIL" },
            gate.name,
            gate.detail
        );
    }
    let lane = |name: &str, summary: &LaneSummary| {
        println!(
            "  {name:<13} correct {}/{} · judged {} · {:.0} ms · {:.0} B · ${:.4}",
            summary.correct,
            summary.probes,
            summary
                .mean_judged_quality
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "n/a".to_string()),
            summary.mean_latency_ms,
            summary.mean_bytes_shipped,
            summary.total_cost_usd
        );
    };
    println!("beyond the pack budget:");
    lane("context_pack", &report.beyond_budget.context_pack);
    lane("working_set", &report.beyond_budget.working_set);
    println!("within the pack budget:");
    lane("context_pack", &report.within_budget.context_pack);
    lane("working_set", &report.within_budget.working_set);
    println!(
        "{}: {}",
        if report.passed { "PASSED" } else { "FAILED" },
        report.activation_note
    );
}

fn render_html(report: &Report) -> String {
    fn esc(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
    let mut rows = String::new();
    for probe in &report.probes {
        for answer in [&probe.context_pack, &probe.working_set] {
            rows.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(probe.fixture),
                esc(probe.question),
                if probe.beyond_pack_budget { "beyond" } else { "within" },
                answer.lane,
                if answer.correct { "✓" } else { "✗" },
                answer
                    .judged_quality
                    .map(|value| format!("{value:.2}"))
                    .unwrap_or_else(|| "–".to_string()),
                answer.latency_ms,
                answer.bytes_shipped,
                esc(&answer.answer_head)
            ));
        }
    }
    let gates: String = report
        .gates
        .iter()
        .map(|gate| {
            format!(
                "<li><strong>{}</strong> {} — {}</li>",
                if gate.passed { "PASS" } else { "FAIL" },
                gate.name,
                esc(&gate.detail)
            )
        })
        .collect();
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Working-set live eval</title>\
         <style>body{{font:14px/1.4 system-ui;margin:2rem}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:4px 8px;text-align:left}}</style>\
         <h1>Working-set lane vs context packing — {} ({})</h1>\
         <p>{}</p><ul>{}</ul>\
         <table><tr><th>fixture</th><th>question</th><th>fact</th><th>lane</th><th>correct</th><th>judged</th><th>ms</th><th>bytes</th><th>answer</th></tr>{}</table>",
        if report.passed { "PASSED" } else { "FAILED" },
        esc(report.mode),
        esc(&report.activation_note),
        gates,
        rows
    )
}
