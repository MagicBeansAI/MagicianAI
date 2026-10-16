//! Provider-free Task Recipes compile -> replay evaluator.
//!
//! Each corpus directory is one redacted browser run. The evaluator uses the
//! production compiler, matcher, runner, relevance classifier, grant store,
//! and origin policy while replacing only the network transport with recorded
//! responses. It writes both machine-readable and human-readable reports.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use clap::Parser;
use magician::magician_v2::api_mining::{
    noise_filter::NoiseFilter,
    origin_policy::OriginPolicyStore,
    recipe::TaskRecipe,
    recipe_compiler::{
        compile_task_recipe, llm_fallback::serialize_shape, values::collect_reported_values,
        CompileError, RecipeCompileInput,
    },
    recipe_matcher::{MatchKind, RecipeMatcher, TaskShapeQuery},
    recipe_runner::{
        FailureClass, RecipeRunInputs, RecipeRunner, StepTransport, TransportRequest,
        TransportResponse,
    },
    recipe_store::RecipeStore,
    relevance::{classify, Relevance},
    replay_grants::ReplayGrantStore,
    types::{NetworkTraceEvent, SessionContext},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Parser)]
#[command(about = "Evaluate Task Recipes against the redacted fixture corpus")]
struct Args {
    #[arg(long)]
    fixtures: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Deserialize)]
struct TaskFixture {
    task_id: String,
    title: String,
    #[serde(default)]
    description: String,
    agent_id: String,
    principal: String,
    workspace: String,
    #[serde(default)]
    typed_inputs: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ReportedFixture {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    artifact_previews: Vec<Value>,
    #[serde(default)]
    output_previews: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ExpectedFixture {
    compile: String,
    #[serde(default)]
    steps: Option<usize>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    data_flows: Option<usize>,
    #[serde(default)]
    write_steps: Option<usize>,
    #[serde(default)]
    verify_with_steps: Option<usize>,
    #[serde(default)]
    answer_fields: Vec<String>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    replay: Option<ReplayExpectation>,
    #[serde(default)]
    variants: Vec<ReplayExpectation>,
    #[serde(default)]
    title_variants: Vec<TitleExpectation>,
    #[serde(default)]
    must_not_contain: Vec<String>,
    #[serde(default)]
    known_failing: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RelevanceFixture {
    id: String,
    page_origin: String,
    #[serde(default)]
    parent_origin: Option<String>,
    expected: Relevance,
    trace: NetworkTraceEvent,
}

#[derive(Debug, Clone, Deserialize)]
struct ReplayExpectation {
    #[serde(default)]
    inputs: HashMap<String, String>,
    #[serde(default)]
    answer: Map<String, Value>,
    #[serde(default)]
    failure_class: Option<String>,
    #[serde(default)]
    response_status_override: Option<u16>,
    #[serde(default)]
    grant_writes: bool,
}

#[derive(Debug, Deserialize)]
struct TitleExpectation {
    title: String,
    expect: String,
    #[serde(default)]
    inputs: HashMap<String, String>,
}

#[derive(Clone)]
struct RecordedTransport {
    responses: Arc<HashMap<(String, String), TransportResponse>>,
    sends: Arc<Mutex<Vec<(String, String)>>>,
    status_override: Option<u16>,
}

impl RecordedTransport {
    fn new(traces: &[NetworkTraceEvent], status_override: Option<u16>) -> Self {
        let responses = traces
            .iter()
            .map(|trace| {
                (
                    (trace.method.to_ascii_uppercase(), trace.url.clone()),
                    TransportResponse {
                        status: trace.status,
                        headers: trace.response_headers.clone(),
                        body: trace.response_body.clone().unwrap_or_default(),
                    },
                )
            })
            .collect();
        Self {
            responses: Arc::new(responses),
            sends: Arc::new(Mutex::new(Vec::new())),
            status_override,
        }
    }
}

#[async_trait]
impl StepTransport for RecordedTransport {
    fn kind(&self) -> magician::magician_v2::api_mining::recipe::Transport {
        magician::magician_v2::api_mining::recipe::Transport::Reqwest
    }

    async fn send(
        &self,
        request: &TransportRequest,
    ) -> std::result::Result<TransportResponse, String> {
        let key = (request.method.to_ascii_uppercase(), request.url.clone());
        self.sends
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(key.clone());
        let mut response = self
            .responses
            .get(&key)
            .cloned()
            .ok_or_else(|| format!("fixture has no response for {} {}", key.0, key.1))?;
        if let Some(status) = self.status_override {
            response.status = status;
            if status == 403 {
                response.body =
                    "<html><title>Just a moment</title><body>cf-chl</body></html>".into();
            }
        }
        Ok(response)
    }
}

#[derive(Debug, Serialize)]
struct EvalReport {
    schema_version: u32,
    generated_at: String,
    fixture_root: String,
    cases: Vec<CaseReport>,
    metrics: Metrics,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct CaseReport {
    id: String,
    state: &'static str,
    assertions: usize,
    error: Option<String>,
    known_failing: Option<String>,
}

#[derive(Debug, Default, Serialize)]
struct Metrics {
    total_cases: usize,
    passed_cases: usize,
    known_failing_cases: usize,
    compile_success_rate: f64,
    replay_success_rate: f64,
    answer_exact_rate: f64,
    variant_success_rate: f64,
    title_match_accuracy: f64,
    secret_leak_count: usize,
    safety_violations: usize,
    telemetry_steps_in_recipes: usize,
    relevance_accuracy: f64,
}

#[derive(Default)]
struct Counters {
    compile_total: usize,
    compile_pass: usize,
    replay_total: usize,
    replay_pass: usize,
    answer_total: usize,
    answer_pass: usize,
    variant_total: usize,
    variant_pass: usize,
    title_total: usize,
    title_pass: usize,
    secret_leaks: usize,
    safety_violations: usize,
    telemetry_steps: usize,
    relevance_total: usize,
    relevance_pass: usize,
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

fn read_traces(path: &Path) -> Result<Vec<NetworkTraceEvent>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("parse {} line {}", path.display(), index + 1))
        })
        .collect()
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("parse {} line {}", path.display(), index + 1))
        })
        .collect()
}

fn action_typed_inputs(case_dir: &Path, fallback: &[String]) -> Result<Vec<String>> {
    let path = case_dir.join("actions.jsonl");
    if !path.exists() {
        return Ok(fallback.to_vec());
    }
    let actions: Vec<Value> = read_jsonl(&path)?;
    let mut values: Vec<String> = actions
        .iter()
        .filter_map(|event| event.get("user_values").and_then(Value::as_array))
        .flatten()
        .filter_map(|value| match value {
            Value::String(value) => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        })
        .chain(fallback.iter().cloned())
        .collect();
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

fn collect_body_scalars(value: &Value, scalars: &mut HashSet<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_body_scalars(value, scalars);
            }
        },
        Value::Object(values) => {
            for value in values.values() {
                collect_body_scalars(value, scalars);
            }
        },
        Value::String(value) if value.len() >= 4 => {
            scalars.insert(value.clone());
        },
        Value::Number(value) => {
            let value = value.to_string();
            if value.len() >= 4 {
                scalars.insert(value);
            }
        },
        _ => {},
    }
}

/// Hosts, HTTP methods, and parameter names are shape. Header values, query
/// values, and request/response body scalars are private runtime material.
fn private_trace_scalars(traces: &[NetworkTraceEvent]) -> HashSet<String> {
    let mut scalars = HashSet::new();
    for trace in traces {
        for value in trace
            .request_headers
            .values()
            .chain(trace.response_headers.values())
        {
            if value.len() >= 4 {
                scalars.insert(value.clone());
            }
        }
        if let Ok(url) = url::Url::parse(&trace.url) {
            for (_, value) in url.query_pairs() {
                if value.len() >= 4 {
                    scalars.insert(value.into_owned());
                }
            }
        }
        for body in [
            trace.request_body.as_deref(),
            trace.response_body.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Ok(value) = serde_json::from_str::<Value>(body) {
                collect_body_scalars(&value, &mut scalars);
            } else if body.len() >= 4 {
                scalars.insert(body.to_owned());
            }
        }
    }
    scalars
}

fn evaluate_relevance_corpus(path: &Path, counters: &mut Counters) -> Result<usize> {
    let cases: Vec<RelevanceFixture> = read_jsonl(path)?;
    if cases.is_empty() {
        bail!("relevance corpus is empty");
    }
    let noise = NoiseFilter::seeded();
    for case in &cases {
        counters.relevance_total += 1;
        let actual = classify(
            &case.trace,
            &case.page_origin,
            case.parent_origin.as_deref(),
            &noise,
        )
        .relevance;
        if actual != case.expected {
            bail!(
                "relevance case {}: expected {}, got {}",
                case.id,
                case.expected.as_str(),
                actual.as_str()
            );
        }
        counters.relevance_pass += 1;
    }
    Ok(cases.len())
}

fn compile_error_name(error: &CompileError) -> &'static str {
    match error {
        CompileError::NoTraces => "no_traces",
        CompileError::NoAnswerBearingResponse => "no_answer_bearing_response",
        CompileError::IncompleteAnswerCoverage => "incomplete_answer_coverage",
        CompileError::NoResolvableSteps => "no_resolvable_steps",
        CompileError::AnswerFieldNotReported { .. } => "answer_field_not_reported",
        CompileError::CaptureMissingRequestBody { .. } => "capture_missing_request_body",
    }
}

fn rate(pass: usize, total: usize) -> f64 {
    if total == 0 {
        1.0
    } else {
        pass as f64 / total as f64
    }
}

fn failure_name(class: &FailureClass) -> &'static str {
    match class {
        FailureClass::Auth => "auth",
        FailureClass::AntiBot => "anti_bot",
        FailureClass::SchemaDrift => "schema_drift",
        FailureClass::Http => "http",
        FailureClass::Network => "network",
        FailureClass::PolicyBlocked => "policy_blocked",
        FailureClass::InputMissing => "input_missing",
    }
}

async fn replay_once(
    recipe: &TaskRecipe,
    traces: &[NetworkTraceEvent],
    expectation: &ReplayExpectation,
    temp: &Path,
) -> Result<(bool, bool, usize)> {
    let grants = ReplayGrantStore::open(temp);
    let policy = OriginPolicyStore::open(temp);
    let lookup = |_: &str, _: &str| Some(SessionContext::default());
    let mut recipe = recipe.clone();
    let inputs = RecipeRunInputs {
        inputs: expectation.inputs.clone(),
        timeout_ms: Some(Duration::from_secs(5).as_millis() as u64),
        approved_write_steps: HashSet::new(),
    };
    let transport = RecordedTransport::new(traces, expectation.response_status_override);
    let sends = Arc::clone(&transport.sends);
    let retry_transport = transport.clone();
    let mut result = RecipeRunner {
        can_continue: None,
        transports: vec![Box::new(transport)],
        grants: &grants,
        origin_policy: &policy,
        session_lookup: &lookup,
        auth_healer: None,
        max_auth_heals: 0,
        step_feedback: None,
        observer: None,
    }
    .run(&mut recipe, &inputs)
    .await;

    if let Some(pending) = result.pending_approval.clone() {
        if !expectation.grant_writes {
            let send_count = sends.lock().unwrap_or_else(|p| p.into_inner()).len();
            return Ok((false, send_count == 0, send_count));
        }
        grants
            .grant_for_url(&pending.grant_key, &pending.url_template, Some("eval"))
            .map_err(anyhow::Error::msg)?;
        result = RecipeRunner {
            can_continue: None,
            transports: vec![Box::new(retry_transport)],
            grants: &grants,
            origin_policy: &policy,
            session_lookup: &lookup,
            auth_healer: None,
            max_auth_heals: 0,
            step_feedback: None,
            observer: None,
        }
        .run(&mut recipe, &inputs)
        .await;
    }

    let expected_failure = expectation.failure_class.as_deref();
    let failure_matches = match expected_failure {
        None => result.success,
        Some(expected) => result
            .fallback
            .as_ref()
            .map(|fallback| failure_name(&fallback.class) == expected)
            .or_else(|| {
                result
                    .failure
                    .as_ref()
                    .map(|failure| failure_name(&failure.class) == expected)
            })
            .unwrap_or(false),
    };
    let answer_matches = expected_failure.is_some() || result.answer == expectation.answer;
    let send_count = sends.lock().unwrap_or_else(|p| p.into_inner()).len();
    Ok((failure_matches, answer_matches, send_count))
}

async fn write_preflight_is_safe(
    recipe: &TaskRecipe,
    traces: &[NetworkTraceEvent],
    temp: &Path,
) -> Result<bool> {
    if !recipe.has_write_steps() {
        return Ok(true);
    }
    let grants = ReplayGrantStore::open(temp);
    let policy = OriginPolicyStore::open(temp);
    let lookup = |_: &str, _: &str| Some(SessionContext::default());
    let transport = RecordedTransport::new(traces, None);
    let sends = Arc::clone(&transport.sends);
    let inputs = recipe
        .shape
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.example_value.clone()))
        .collect();
    let mut candidate = recipe.clone();
    let result = RecipeRunner {
        can_continue: None,
        transports: vec![Box::new(transport)],
        grants: &grants,
        origin_policy: &policy,
        session_lookup: &lookup,
        auth_healer: None,
        max_auth_heals: 0,
        step_feedback: None,
        observer: None,
    }
    .run(
        &mut candidate,
        &RecipeRunInputs {
            inputs,
            timeout_ms: Some(Duration::from_secs(5).as_millis() as u64),
            approved_write_steps: HashSet::new(),
        },
    )
    .await;
    let send_count = sends
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len();
    Ok(send_count == 0 && result.pending_approval.is_some() && result.fallback.is_none())
}

async fn evaluate_case(case_dir: &Path, counters: &mut Counters) -> Result<usize> {
    let task: TaskFixture = read_json(&case_dir.join("task.json"))?;
    let reported: ReportedFixture = read_json(&case_dir.join("reported.json"))?;
    let expected: ExpectedFixture = read_json(&case_dir.join("expected.json"))?;
    let traces = read_traces(&case_dir.join("traces.jsonl"))?;
    let previews: Vec<&str> = reported
        .output_previews
        .iter()
        .map(String::as_str)
        .collect();
    let values = collect_reported_values(&reported.summary, &reported.artifact_previews, &previews);
    let input = RecipeCompileInput {
        task_id: task.task_id.clone(),
        execution_id: format!("exec_{}", task.task_id),
        monitor_revision: None,
        agent_id: task.agent_id.clone(),
        principal: task.principal.clone(),
        workspace: task.workspace.clone(),
        task_title: task.title.clone(),
        task_text: task.description.clone(),
        reported: values,
        traces: traces.clone(),
        typed_inputs: action_typed_inputs(case_dir, &task.typed_inputs)?,
        trace_files: vec!["traces.jsonl".into()],
        sequences: Vec::new(),
    };
    counters.compile_total += 1;
    let compiled = compile_task_recipe(&input);
    if expected.compile != "ok" {
        let error = compiled.err().context("fixture unexpectedly compiled")?;
        if compile_error_name(&error) != expected.compile {
            bail!(
                "expected compile {}, got {}",
                expected.compile,
                compile_error_name(&error)
            );
        }
        counters.compile_pass += 1;
        return Ok(1);
    }
    let recipe = compiled.map_err(anyhow::Error::msg)?;
    counters.compile_pass += 1;
    let version = recipe
        .current()
        .context("compiled recipe has no current version")?;
    let mut assertions = 1;
    macro_rules! expect_eq {
        ($actual:expr, $expected:expr, $label:literal) => {{
            assertions += 1;
            if $actual != $expected {
                bail!("{}: expected {:?}, got {:?}", $label, $expected, $actual);
            }
        }};
    }
    if let Some(count) = expected.steps {
        expect_eq!(version.steps.len(), count, "step count");
    }
    if let Some(count) = expected.data_flows {
        expect_eq!(version.data_flows.len(), count, "data-flow count");
    }
    if let Some(count) = expected.write_steps {
        expect_eq!(
            version
                .steps
                .iter()
                .filter(|step| step.side_effects
                    == magician::magician_v2::api_mining::capability::SideEffects::Write)
                .count(),
            count,
            "write-step count"
        );
    }
    if let Some(count) = expected.verify_with_steps {
        expect_eq!(
            version
                .steps
                .iter()
                .filter(|step| step.verify_with.is_some())
                .count(),
            count,
            "verify-with count"
        );
    }
    let mut inputs: Vec<_> = recipe
        .shape
        .inputs
        .iter()
        .map(|input| input.name.clone())
        .collect();
    inputs.sort();
    let mut expected_inputs = expected.inputs.clone();
    expected_inputs.sort();
    expect_eq!(inputs, expected_inputs, "input names");
    if let Some(template) = expected.template.as_deref() {
        expect_eq!(recipe.shape.template.as_str(), template, "template");
    }
    let mut fields: Vec<_> = version
        .answer_spec
        .iter()
        .map(|field| field.field.clone())
        .collect();
    fields.sort();
    let mut expected_fields = expected.answer_fields.clone();
    expected_fields.sort();
    expect_eq!(fields, expected_fields, "answer fields");

    let recipe_json = serde_json::to_string(&recipe)?;
    let shape_json = serialize_shape(&recipe, &task.title);
    for forbidden in &expected.must_not_contain {
        assertions += 1;
        if recipe_json.contains(forbidden) || shape_json.contains(forbidden) {
            counters.secret_leaks += 1;
            bail!("secret/specific value leaked into recipe or shape payload");
        }
    }
    for private_value in private_trace_scalars(&traces) {
        assertions += 1;
        if shape_json.contains(&private_value) {
            counters.secret_leaks += 1;
            bail!("trace scalar leaked into shape-only LLM payload");
        }
    }

    // Evaluation is read-only with respect to the checked-in corpus. A
    // persisted noise filter belongs to a runtime scope, never a fixture dir.
    let noise = NoiseFilter::seeded();
    for step in &version.steps {
        if let Some(trace) = traces.iter().find(|trace| {
            trace.method.eq_ignore_ascii_case(&step.method)
                && trace.url.starts_with(step.origin.as_str())
        }) {
            let page_origin =
                magician::magician_v2::api_mining::relevance::page_origin_for_trace(trace);
            if classify(trace, &page_origin, None, &noise).relevance == Relevance::Telemetry {
                counters.telemetry_steps += 1;
                bail!("telemetry endpoint entered recipe steps");
            }
        }
    }

    let temp = tempfile::tempdir()?;
    assertions += 1;
    if !write_preflight_is_safe(&recipe, &traces, temp.path()).await? {
        counters.safety_violations += 1;
        bail!("write recipe sent a request or failed to stop at approval preflight");
    }
    if let Some(replay) = &expected.replay {
        counters.replay_total += 1;
        counters.answer_total += 1;
        let (replay_ok, answer_ok, sends) =
            replay_once(&recipe, &traces, replay, temp.path()).await?;
        assertions += 2;
        if replay_ok {
            counters.replay_pass += 1;
        } else {
            bail!("replay outcome mismatch after {sends} send(s)");
        }
        if answer_ok {
            counters.answer_pass += 1;
        } else {
            bail!("replay answer differed from expected answer");
        }
    }
    for variant in &expected.variants {
        counters.variant_total += 1;
        let (replay_ok, answer_ok, sends) =
            replay_once(&recipe, &traces, variant, temp.path()).await?;
        assertions += 2;
        if replay_ok && answer_ok {
            counters.variant_pass += 1;
        } else {
            bail!("variant replay mismatch after {sends} send(s)");
        }
    }

    let store = RecipeStore::new(temp.path().join("matcher"));
    store.save(&recipe)?;
    let matcher = RecipeMatcher::deterministic(&store);
    for (index, title) in expected.title_variants.iter().enumerate() {
        counters.title_total += 1;
        let variant_task_id = format!("variant_{}_{}", task.task_id, index);
        let query = TaskShapeQuery {
            task_id: &variant_task_id,
            title: &title.title,
            description: &task.description,
            agent_id: &task.agent_id,
            principal: &task.principal,
            workspace: &task.workspace,
        };
        let found = matcher.find(&query).await;
        let matches_kind = match (title.expect.as_str(), &found) {
            ("none", None) => true,
            ("template", Some(found)) => {
                matches!(found.kind, MatchKind::Template) && found.inputs == title.inputs
            },
            _ => false,
        };
        assertions += 1;
        if matches_kind {
            counters.title_pass += 1;
        } else {
            bail!("title matcher result differed for {:?}", title.title);
        }
    }
    Ok(assertions)
}

fn render_html(report: &EvalReport) -> String {
    let rows = report
        .cases
        .iter()
        .map(|case| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                case.id,
                case.state,
                case.assertions,
                case.error
                    .as_deref()
                    .or(case.known_failing.as_deref())
                    .unwrap_or("")
            )
        })
        .collect::<String>();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Task Recipes eval</title><style>body{{font:14px system-ui;max-width:1100px;margin:40px auto;color:#17202a}}table{{width:100%;border-collapse:collapse}}td,th{{padding:8px;border-bottom:1px solid #ddd;text-align:left}}code{{background:#f4f6f7;padding:2px 4px}}</style></head><body><h1>Task Recipes offline eval</h1><p>Passed: <strong>{}</strong> · cases: {}/{} · secret leaks: {} · safety violations: {} · telemetry steps: {}</p><p>Compile {:.1}% · replay {:.1}% · exact answer {:.1}% · variants {:.1}% · title matching {:.1}% · relevance {:.1}%</p><table><thead><tr><th>Case</th><th>State</th><th>Assertions</th><th>Detail</th></tr></thead><tbody>{}</tbody></table></body></html>"#,
        report.passed,
        report.metrics.passed_cases,
        report.metrics.total_cases,
        report.metrics.secret_leak_count,
        report.metrics.safety_violations,
        report.metrics.telemetry_steps_in_recipes,
        report.metrics.compile_success_rate * 100.0,
        report.metrics.replay_success_rate * 100.0,
        report.metrics.answer_exact_rate * 100.0,
        report.metrics.variant_success_rate * 100.0,
        report.metrics.title_match_accuracy * 100.0,
        report.metrics.relevance_accuracy * 100.0,
        rows
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let mut dirs: Vec<_> = fs::read_dir(&args.fixtures)
        .with_context(|| format!("read fixture root {}", args.fixtures.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir()) && entry.file_name() != "relevance"
        })
        .collect();
    dirs.sort_by_key(|entry| entry.file_name());
    if dirs.is_empty() {
        bail!("Task Recipes fixture corpus is empty");
    }

    let mut counters = Counters::default();
    let mut cases = Vec::with_capacity(dirs.len());
    for entry in dirs {
        let id = entry.file_name().to_string_lossy().into_owned();
        let expected: ExpectedFixture = read_json(&entry.path().join("expected.json"))?;
        match evaluate_case(&entry.path(), &mut counters).await {
            Ok(assertions) => cases.push(CaseReport {
                id,
                state: "passed",
                assertions,
                error: None,
                known_failing: expected.known_failing,
            }),
            Err(error) if expected.known_failing.is_some() => cases.push(CaseReport {
                id,
                state: "known_failing",
                assertions: 0,
                error: Some(error.to_string()),
                known_failing: expected.known_failing,
            }),
            Err(error) => cases.push(CaseReport {
                id,
                state: "failed",
                assertions: 0,
                error: Some(error.to_string()),
                known_failing: None,
            }),
        }
    }
    let relevance_path = args.fixtures.join("relevance").join("cases.jsonl");
    match evaluate_relevance_corpus(&relevance_path, &mut counters) {
        Ok(assertions) => cases.push(CaseReport {
            id: "relevance/cases".into(),
            state: "passed",
            assertions,
            error: None,
            known_failing: None,
        }),
        Err(error) => cases.push(CaseReport {
            id: "relevance/cases".into(),
            state: "failed",
            assertions: 0,
            error: Some(error.to_string()),
            known_failing: None,
        }),
    }
    let failed = cases.iter().any(|case| case.state == "failed");
    let metrics = Metrics {
        total_cases: cases.len(),
        passed_cases: cases.iter().filter(|case| case.state == "passed").count(),
        known_failing_cases: cases
            .iter()
            .filter(|case| case.state == "known_failing")
            .count(),
        compile_success_rate: rate(counters.compile_pass, counters.compile_total),
        replay_success_rate: rate(counters.replay_pass, counters.replay_total),
        answer_exact_rate: rate(counters.answer_pass, counters.answer_total),
        variant_success_rate: rate(counters.variant_pass, counters.variant_total),
        title_match_accuracy: rate(counters.title_pass, counters.title_total),
        secret_leak_count: counters.secret_leaks,
        safety_violations: counters.safety_violations,
        telemetry_steps_in_recipes: counters.telemetry_steps,
        relevance_accuracy: rate(counters.relevance_pass, counters.relevance_total),
    };
    let report = EvalReport {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        fixture_root: args.fixtures.display().to_string(),
        passed: !failed
            && metrics.known_failing_cases == 0
            && metrics.compile_success_rate == 1.0
            && metrics.replay_success_rate == 1.0
            && metrics.answer_exact_rate == 1.0
            && metrics.variant_success_rate == 1.0
            && metrics.title_match_accuracy == 1.0
            && metrics.secret_leak_count == 0
            && metrics.safety_violations == 0
            && metrics.telemetry_steps_in_recipes == 0
            && metrics.relevance_accuracy == 1.0,
        cases,
        metrics,
    };
    fs::create_dir_all(&args.output)?;
    fs::write(
        args.output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(args.output.join("latest.html"), render_html(&report))?;
    if !report.passed {
        bail!("Task Recipes offline eval gates failed");
    }
    Ok(())
}
