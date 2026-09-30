//! Scope-bound `/api/magician/v2/evals` — lanes, run, runs, spend, report.
//!
//! Modelled on [`crate::analytics_api`]: an `Option<web::Data<EvalsApi>>` so a
//! process that never wired the layer answers `503` instead of failing to
//! start, and [`resolve_required_scope`] on every data route so a missing scope
//! is a `400` rather than a silent read of somebody else's history.
//!
//! # What this layer owns, and what it refuses to own
//!
//! Everything here is a projection of a source that already owns the answer —
//! the Makefile owns the lane list, live probes own readiness, the task system
//! owns whether a lane is running, the LLM ledger owns money, and
//! [`EvalRunStore`] owns history. This module caches exactly one of them (the
//! parsed Makefile, keyed on its mtime and length) and derives the rest per
//! request. Nothing about a lane is stored here, so nothing about a lane can go
//! stale here.
//!
//! # Three contract details the UI depends on
//!
//! **`report_href` is a URL, not a path.** Lanes write their reports under
//! `COVERAGE_BASE_DIR`, which is not inside any static mount, so a filesystem
//! path in that field would render as a dead `<a href>`. [`report_handler`]
//! serves those files back over HTTP and [`EVAL_REPORT_HREF_PREFIX`] is the
//! prefix the run records are stamped with. The passthrough will only serve a
//! path that lies inside some lane's *declared* `report=` directory — the
//! Makefile is the allowlist, and the parser has already rejected `..`,
//! absolute paths and unexpanded `$(VAR)` in those values.
//!
//! **A spend range wider than the ledger's window is refused, not truncated.**
//! [`LEDGER_MAX_WINDOW_MS`] is a real bound on what can be answered. Answering
//! the part of the range that fits would return a number that looks exactly like
//! a total and is smaller than one, so [`spend_handler`] returns `400` naming
//! the limit instead.
//!
//! **Unknown cost carries no number.** [`CostValue`] serialises as
//! `{"kind":"known","usd":0.83}` or `{"kind":"unknown"}` — there is no numeric
//! field to read when the answer is unknown, which is what stops `—` becoming
//! `$0.00` one careless template away.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope;
use magician::magician_v2::analytics::llm_analytics_read_service::LlmAnalyticsReadService;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician_surfaces::evals::cost::{
    costs_for_runs, spend_in_range, CostValue, EvalCostLedger, LedgerCostRow, LedgerUnavailable,
    LlmLedgerCosts, LEDGER_MAX_WINDOW_MS,
};
use magician_surfaces::evals::readiness::{
    lane_readiness, probe_all, ProbeResult, ProbeSnapshot, ProbeTargets,
};
use magician_surfaces::evals::registry::{
    parse_eval_registry, EvalLane, EvalRegistry, EvalRequirement, OrphanedAnnotation,
};
use magician_surfaces::evals::run::EvalRun;
use magician_surfaces::evals::runner::{
    CoverageReportLocator, EvalExecutor, EvalRunError, EvalRunner, ReadinessSource, ReportLocator,
};
use magician_surfaces::evals::store::EvalRunStore;

/// The href prefix a lane's report is deep-linked under.
///
/// Run records are stamped with `{prefix}/{report_dir}` at the moment the run
/// ends, so this constant is baked into stored history: changing it orphans the
/// links on every run recorded before the change.
pub const EVAL_REPORT_HREF_PREFIX: &str = "/api/magician/v2/evals/report";

/// Runs returned when the caller does not say.
const DEFAULT_RUNS_LIMIT: usize = 50;
/// Ceiling on one page of runs. Each record carries a bounded log tail, so a
/// wide page is a real payload rather than a cheap one.
const MAX_RUNS_LIMIT: usize = 200;

/// Biggest report file served inline. Reports are HTML/JSON summaries; anything
/// past this is a raw artifact dump that belongs on disk, not in a response.
const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WEB_RESEARCH_QUESTION_CHARS: usize = 4_000;
const MAX_WEB_RESEARCH_ANSWER_CHARS: usize = 12_000;
const MAX_WEB_RESEARCH_SOURCES: usize = 8;
const MAX_WEB_RESEARCH_SOURCE_CHARS: usize = 40_000;
/// Default eval-only cost rail. Callers may select another configured named
/// profile for comparisons. Production evidence grading keeps its route.
const DEFAULT_WEB_RESEARCH_EVAL_LLM_PROFILE: &str = "gpt6luna-responses-toolsany";
const MAX_EVAL_LLM_PROFILE_CHARS: usize = 128;

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// Files a lane's report directory is checked for, in order, when the href
/// points at the directory rather than at one file.
const REPORT_INDEX_CANDIDATES: [&str; 5] = [
    "latest.html",
    "index.html",
    "report.html",
    "report.json",
    "summary.md",
];

/// Every requirement kind, so the page can render "down" and "never probed"
/// differently even for a requirement no lane in this Makefile declares.
const ALL_REQUIREMENTS: [EvalRequirement; 5] = [
    EvalRequirement::Ollama,
    EvalRequirement::Magician,
    EvalRequirement::MagicianBinary,
    EvalRequirement::Magicutor,
    EvalRequirement::ProviderKeys,
];

// ============================================================================
// Shared state
// ============================================================================

/// The parsed Makefile, and what it was parsed from.
struct CachedRegistry {
    modified: Option<SystemTime>,
    len: u64,
    registry: Arc<EvalRegistry>,
}

/// Shared state for the `/evals` endpoints.
pub struct EvalsApi {
    makefile_path: PathBuf,
    /// Conventionally `COVERAGE_BASE_DIR` — what the `report=` annotations are
    /// written relative to.
    reports_root: PathBuf,
    store: Arc<EvalRunStore>,
    readiness: Arc<dyn ReadinessSource>,
    reports: Arc<dyn ReportLocator>,
    /// `None` in a process with no execution runtime: every start refuses with
    /// `503` rather than pretending to have queued something.
    executor: Option<Arc<dyn EvalExecutor>>,
    /// `None` when analytics is not initialised. Cost then reads unknown for
    /// every run — which is true, and is emphatically not zero.
    llm_analytics: Option<Arc<LlmAnalyticsReadService>>,
    registry_cache: RwLock<Option<CachedRegistry>>,
}

impl EvalsApi {
    /// `repo_root` is where the Makefile lives; `reports_root` is where the
    /// lanes write (`COVERAGE_BASE_DIR`).
    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        repo_root: impl Into<PathBuf>,
        reports_root: impl Into<PathBuf>,
    ) -> Self {
        let reports_root = reports_root.into();
        Self {
            makefile_path: repo_root.into().join("Makefile"),
            reports: Arc::new(CoverageReportLocator::new(
                reports_root.clone(),
                EVAL_REPORT_HREF_PREFIX,
            )),
            reports_root,
            store: Arc::new(EvalRunStore::new(workspace_layout)),
            // Fail closed by default: a snapshot with nothing in it proves
            // nothing, so every lane that declares a requirement blocks until
            // a real probe source is wired.
            readiness: Arc::new(NoProbes),
            executor: None,
            llm_analytics: None,
            registry_cache: RwLock::new(None),
        }
    }

    #[must_use]
    pub fn with_probe_targets(mut self, targets: ProbeTargets) -> Self {
        self.readiness = Arc::new(LiveReadiness { targets });
        self
    }

    #[must_use]
    pub fn with_readiness_source(mut self, readiness: Arc<dyn ReadinessSource>) -> Self {
        self.readiness = readiness;
        self
    }

    #[must_use]
    pub fn with_executor(mut self, executor: Arc<dyn EvalExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    #[must_use]
    pub fn with_report_locator(mut self, reports: Arc<dyn ReportLocator>) -> Self {
        self.reports = reports;
        self
    }

    #[must_use]
    pub fn with_llm_analytics_read_service(
        mut self,
        service: Arc<LlmAnalyticsReadService>,
    ) -> Self {
        self.llm_analytics = Some(service);
        self
    }

    /// The lanes the Makefile declares, re-parsed only when the file changed.
    ///
    /// Caching lives here rather than in [`magician_surfaces::evals::runner`]
    /// on purpose: the runner takes the lane list as an argument so it stays
    /// pure, and this is the layer that knows what a request costs.
    fn registry(&self) -> Result<Arc<EvalRegistry>, String> {
        let metadata = std::fs::metadata(&self.makefile_path).map_err(|error| {
            format!(
                "the eval registry could not be read from `{}`: {error}",
                self.makefile_path.display()
            )
        })?;
        let stamp = (metadata.modified().ok(), metadata.len());

        if let Ok(cache) = self.registry_cache.read() {
            if let Some(cached) = cache.as_ref() {
                if cached.modified == stamp.0 && cached.len == stamp.1 {
                    return Ok(Arc::clone(&cached.registry));
                }
            }
        }

        let text = std::fs::read_to_string(&self.makefile_path).map_err(|error| {
            format!(
                "the eval registry could not be read from `{}`: {error}",
                self.makefile_path.display()
            )
        })?;
        let registry = Arc::new(parse_eval_registry(&text));
        if let Ok(mut cache) = self.registry_cache.write() {
            *cache = Some(CachedRegistry {
                modified: stamp.0,
                len: stamp.1,
                registry: Arc::clone(&registry),
            });
        }
        Ok(registry)
    }

    /// The money side of the join for one scope.
    ///
    /// A process with no analytics gets [`UnavailableLedger`], whose `Err`
    /// becomes [`CostValue::Unknown`] for every run. That is the whole point:
    /// there is no path on which a missing ledger becomes a page of free lanes.
    fn ledger(&self, principal: &str, workspace: &str) -> Box<dyn EvalCostLedger> {
        match self.llm_analytics.as_ref() {
            Some(service) => Box::new(LlmLedgerCosts::new(
                Arc::clone(service),
                principal,
                workspace,
            )),
            None => Box::new(UnavailableLedger),
        }
    }
}

/// Probes nothing, proves nothing — every requirement reads `Unknown` and
/// therefore blocks. The default, so a half-wired process refuses expensive
/// lanes rather than offering them.
struct NoProbes;

#[async_trait]
impl ReadinessSource for NoProbes {
    async fn probe(&self) -> ProbeSnapshot {
        ProbeSnapshot::default()
    }
}

/// The live probes, aimed at whatever the lanes themselves aim at.
struct LiveReadiness {
    targets: ProbeTargets,
}

#[async_trait]
impl ReadinessSource for LiveReadiness {
    async fn probe(&self) -> ProbeSnapshot {
        probe_all(&self.targets).await
    }
}

/// The ledger in a process that has none. Always `Err`, never empty rows: an
/// empty answer would fold to `Known(0.0)`, and "we cannot ask" is not "it was
/// free".
struct UnavailableLedger;

impl EvalCostLedger for UnavailableLedger {
    fn cost_rows_for_tasks(
        &self,
        _task_ids: &[&str],
        _from_ms: i64,
        _to_ms: i64,
    ) -> Result<Vec<LedgerCostRow>, LedgerUnavailable> {
        Err(LedgerUnavailable::new(
            "the LLM ledger is not wired into this process, so eval cost is unknown (not zero)",
        ))
    }
}

// ============================================================================
// Wire shapes
// ============================================================================

/// The error body every `/evals` route returns.
#[derive(Debug, Clone, Serialize)]
pub struct EvalsError {
    /// Stable machine code — the UI branches on this, never on `message`.
    pub error: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// Every unsatisfied requirement, in the same tokens the Makefile uses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<EvalRequirement>,
    /// The run already in flight, so the caller can link to it instead of only
    /// being told "no".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl EvalsError {
    fn simple(error: &str, message: impl Into<String>) -> Self {
        Self {
            error: error.to_string(),
            message: message.into(),
            lane: None,
            missing: Vec::new(),
            task_id: None,
        }
    }

    fn for_lane(error: &str, lane: String, message: impl Into<String>) -> Self {
        Self {
            lane: Some(lane),
            ..Self::simple(error, message)
        }
    }
}

/// One run, with its cost joined on.
#[derive(Debug, Clone, Serialize)]
pub struct RunView {
    #[serde(flatten)]
    pub run: EvalRun,
    /// `{"kind":"known","usd":…}` or `{"kind":"unknown"}`. Never a bare number,
    /// never `null`.
    pub cost: CostValue,
}

/// One lane, with everything derived about it right now.
#[derive(Debug, Clone, Serialize)]
pub struct LaneView {
    pub run_options: Option<&'static str>,
    #[serde(flatten)]
    pub lane: EvalLane,
    pub ready: bool,
    /// Empty when ready. Named so the page can say what to start rather than
    /// only that something is missing.
    pub missing: Vec<EvalRequirement>,
    /// Whether a Run button may be offered at all — a lane whose annotation
    /// never parsed, or whose `kind` nobody declared, has no runnable form.
    pub runnable: bool,
    pub last_run: Option<RunView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LanesResponse {
    pub lanes: Vec<LaneView>,
    /// Annotations that never became lanes.
    ///
    /// Returned rather than dropped because an annotation binding to nothing is
    /// an eval that has silently vanished from the grid — the exact failure this
    /// page exists to prevent, and the only one that is invisible if omitted.
    pub orphaned: Vec<OrphanedAnnotation>,
    /// Requirement -> what the probe found, so the page can distinguish "Ollama
    /// is down" (a fact) from "we could not tell" (not one).
    pub probes: BTreeMap<String, ProbeResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunsResponse {
    pub runs: Vec<RunView>,
    /// Runs matching the filter, not the number returned — the page pages
    /// server-side, so it needs to know what it is paging through.
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartedRunResponse {
    pub task_id: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunsQuery {
    pub lane: Option<String>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpendQuery {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebResearchJudgeSource {
    pub url: String,
    pub final_url: Option<String>,
    pub title: Option<String>,
    pub excerpt: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebResearchJudgeRequest {
    pub question: String,
    pub answer: String,
    pub sources: Vec<WebResearchJudgeSource>,
    #[serde(default)]
    pub llm_profile: Option<String>,
    /// `web_research_answer` (default) grades the answer's assertions for
    /// support. `declared_open_items` grades a partial's declared-open items
    /// for honesty against the agent's OWN opened evidence: unfaithful only if
    /// that evidence resolves an item the answer said it could not.
    #[serde(default)]
    pub evidence_kind: Option<String>,
    /// The run's real tool inventory (`name xN (M failed)` lines). A claim
    /// about what the agent DID is judged against this, not against page
    /// text; without it every process statement in an answer reads as an
    /// invented claim.
    #[serde(default)]
    pub observed_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebResearchJudgeResponse {
    pub supported: bool,
    pub reason: String,
    /// `contradicted` | `absent` when `supported` is false and the judge said which.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_kind: Option<String>,
    /// Limitation sentences the answer declared about itself; not scored.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub declared_open: Vec<String>,
}

fn valid_eval_llm_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile.chars().count() <= MAX_EVAL_LLM_PROFILE_CHARS
        && profile.chars().enumerate().all(|(index, ch)| {
            ch.is_ascii_alphanumeric() || (index > 0 && matches!(ch, '.' | '_' | ':' | '-'))
        })
}

fn web_research_eval_routing_overrides(
    profile: &str,
) -> magician::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides {
    use magician::magician_v2::query_analysis::operation_llm_router::{
        OperationRoutingEndpoint, OperationRoutingOverrides,
    };

    let mut operations = BTreeMap::new();
    operations.insert(
        "evidence_precision_judge".to_string(),
        OperationRoutingEndpoint::for_profile(profile)
            .expect("the validated eval profile name is non-empty"),
    );
    OperationRoutingOverrides {
        operations,
        ..OperationRoutingOverrides::default()
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// POST `/api/magician/v2/evals/web-researcher/judge`
///
/// Evaluation-only semantic gate. It compares the answer with bounded text
/// fetched from the answer's cited pages through the existing governed
/// evidence-precision judge. It does not participate in the research task and
/// therefore cannot repair, steer, or mask the agent's answer.
pub async fn judge_web_research_answer_handler(
    req: HttpRequest,
    body: web::Json<WebResearchJudgeRequest>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    let question = body.question.trim();
    let answer = body.answer.trim();
    let llm_profile = body
        .llm_profile
        .as_deref()
        .unwrap_or(DEFAULT_WEB_RESEARCH_EVAL_LLM_PROFILE)
        .trim();
    if !valid_eval_llm_profile(llm_profile) {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_judge_profile",
            "llm_profile must be a configured profile name using only letters, numbers, '.', '_', ':', or '-' (maximum 128 characters)",
        ));
    }
    if question.is_empty()
        || question.chars().count() > MAX_WEB_RESEARCH_QUESTION_CHARS
        || answer.is_empty()
        || answer.chars().count() > MAX_WEB_RESEARCH_ANSWER_CHARS
    {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_judge_input",
            "question and answer must be non-empty and within the eval judge size limits",
        ));
    }
    if body.sources.is_empty() || body.sources.len() > MAX_WEB_RESEARCH_SOURCES {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_judge_sources",
            format!("the eval judge requires 1-{MAX_WEB_RESEARCH_SOURCES} opened citation sources"),
        ));
    }
    let source_chars = body
        .sources
        .iter()
        .map(|source| source.excerpt.chars().count())
        .sum::<usize>();
    if source_chars == 0 || source_chars > MAX_WEB_RESEARCH_SOURCE_CHARS {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_judge_sources",
            format!(
                "opened source excerpts must contain 1-{MAX_WEB_RESEARCH_SOURCE_CHARS} characters in total"
            ),
        ));
    }

    let mut source_excerpt = format!(
        "Question being answered:\n{question}\n\nOpened cited pages (search snippets are not evidence):"
    );
    for (index, source) in body.sources.iter().enumerate() {
        source_excerpt.push_str(&format!(
            "\n\n[Source {}]\nRequested URL: {}\nFinal URL: {}\nTitle: {}\nExcerpt:\n{}",
            index + 1,
            source.url.trim(),
            source.final_url.as_deref().unwrap_or(&source.url).trim(),
            source.title.as_deref().unwrap_or("<unknown>").trim(),
            source.excerpt.trim(),
        ));
    }

    let Some(router) =
        magician::magician_v2::query_analysis::operation_llm_router::global_operation_router()
    else {
        return HttpResponse::ServiceUnavailable().json(EvalsError::simple(
            "judge_unavailable",
            "the governed LLM router is not initialized",
        ));
    };
    let Some(prompt_manager) = magician::magician_v2::prompts::global_prompt_manager() else {
        return HttpResponse::ServiceUnavailable().json(EvalsError::simple(
            "judge_unavailable",
            "the managed prompt store is not initialized",
        ));
    };
    let scoped_router = router
        .with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)))
        .with_routing_overrides(Some(web_research_eval_routing_overrides(llm_profile)));
    let evidence_kind = match body.evidence_kind.as_deref().map(str::trim) {
        None | Some("") | Some("web_research_answer") => "web_research_answer",
        Some("declared_open_items") => "declared_open_items",
        Some(other) => {
            return HttpResponse::BadRequest().json(EvalsError::simple(
                "invalid_judge_kind",
                format!("unsupported evidence_kind `{other}`"),
            ));
        },
    };
    let mut observed_actions = body
        .observed_actions
        .iter()
        .map(|action| action.trim())
        .filter(|action| !action.is_empty())
        .take(64)
        .map(|action| action.chars().take(200).collect::<String>())
        .collect::<Vec<_>>();
    observed_actions.push("opened cited public pages".to_string());
    match magician::magician_v2::evidence::grade_summary_precision(
        "web-research-answer",
        &source_excerpt,
        evidence_kind,
        &observed_actions,
        answer,
        &scoped_router,
        &prompt_manager,
    )
    .await
    {
        Ok(verdict) => HttpResponse::Ok().json(WebResearchJudgeResponse {
            supported: verdict.faithful,
            reason: verdict.reason,
            unsupported_kind: verdict.unsupported_kind.map(|kind| {
                serde_json::to_value(kind)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_default()
            }),
            declared_open: verdict.declared_open,
        }),
        Err(error) => HttpResponse::BadGateway().json(EvalsError::simple(
            "judge_failed",
            format!("the evidence judge failed: {error}"),
        )),
    }
}

/// GET `/api/magician/v2/evals/lanes`
pub async fn list_lanes_handler(
    req: HttpRequest,
    api: Option<web::Data<EvalsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return layer_unavailable();
    };
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let registry = match api.registry() {
        Ok(registry) => registry,
        Err(message) => {
            return HttpResponse::InternalServerError()
                .json(EvalsError::simple("registry_unreadable", message))
        },
    };

    let snapshot = api.readiness.probe().await;
    let runs = match api.store.list(&principal, &workspace, None) {
        Ok(runs) => runs,
        Err(error) => {
            return HttpResponse::InternalServerError().json(EvalsError::simple(
                "run_history_unreadable",
                format!("eval run history could not be read: {error}"),
            ))
        },
    };

    // `list` is newest-first, so the first sighting of a lane is its last run.
    let mut seen: HashSet<String> = HashSet::new();
    let last_runs: Vec<EvalRun> = runs
        .into_iter()
        .filter(|run| seen.insert(run.lane_id.clone()))
        .collect();
    // One ledger query for the whole grid, not one per lane.
    let costs = costs_for_runs(&last_runs, api.ledger(&principal, &workspace).as_ref());
    let mut last_by_lane: HashMap<String, RunView> = HashMap::new();
    for (run, cost) in last_runs.into_iter().zip(costs.into_iter()) {
        last_by_lane.insert(
            run.lane_id.clone(),
            RunView {
                run,
                cost: cost.cost,
            },
        );
    }

    let lanes = registry
        .lanes
        .iter()
        .map(|lane| {
            let readiness = lane_readiness(&lane.requires, &snapshot);
            LaneView {
                run_options: (lane.id == magician_surfaces::evals::options::LIFECYCLE_LIVE)
                    .then_some("memory_lifecycle"),
                ready: readiness.ready,
                missing: readiness.missing,
                runnable: lane.parse_error.is_none() && lane.kind.runnable().is_some(),
                last_run: last_by_lane.get(&lane.id).cloned(),
                lane: lane.clone(),
            }
        })
        .collect::<Vec<_>>();

    HttpResponse::Ok().json(LanesResponse {
        lanes,
        orphaned: registry.orphaned.clone(),
        probes: probe_map(&snapshot),
    })
}

/// POST `/api/magician/v2/evals/{lane}/run`
pub async fn run_lane_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Bytes,
    api: Option<web::Data<EvalsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return layer_unavailable();
    };
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let Some(executor) = api.executor.clone() else {
        return HttpResponse::ServiceUnavailable().json(EvalsError::simple(
            "runner_unavailable",
            "the eval runner is not wired into this process, so no lane can be started here",
        ));
    };
    let registry = match api.registry() {
        Ok(registry) => registry,
        Err(message) => {
            return HttpResponse::InternalServerError()
                .json(EvalsError::simple("registry_unreadable", message))
        },
    };

    let lane_id = path.into_inner();
    if body.len() > 4096 {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_run_options",
            "run options exceed 4096 bytes",
        ));
    }
    let options = if body.is_empty() {
        magician_surfaces::evals::options::EvalRunOptions::default()
    } else {
        match serde_json::from_slice::<magician_surfaces::evals::options::EvalRunOptions>(&body) {
            Ok(options) => options,
            Err(error) => {
                return HttpResponse::BadRequest()
                    .json(EvalsError::simple("invalid_run_options", error.to_string()))
            },
        }
    };
    if let Err(error) = options.validate(&lane_id) {
        return HttpResponse::BadRequest().json(EvalsError::simple("invalid_run_options", error));
    }
    let runner = EvalRunner::new(
        executor,
        Arc::clone(&api.store),
        Arc::clone(&api.readiness),
        Arc::clone(&api.reports),
    );
    match runner
        .start_lane_with_options(&principal, &workspace, &registry.lanes, &lane_id, &options)
        .await
    {
        Ok(handle) => HttpResponse::Accepted().json(StartedRunResponse {
            task_id: handle.task_id,
            run_id: handle.run_id,
        }),
        Err(error) => run_error_response(error),
    }
}

/// GET `/api/magician/v2/evals/runs`
pub async fn list_runs_handler(
    req: HttpRequest,
    query: web::Query<RunsQuery>,
    api: Option<web::Data<EvalsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return layer_unavailable();
    };
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let from_ms = query.from_ms.unwrap_or(i64::MIN);
    let to_ms = query.to_ms.unwrap_or(i64::MAX);
    if to_ms <= from_ms {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_range",
            format!("`from_ms` must be before `to_ms`, got [{from_ms}, {to_ms})"),
        ));
    }

    let runs = match api
        .store
        .list(&principal, &workspace, query.lane.as_deref())
    {
        Ok(runs) => runs,
        Err(error) => {
            return HttpResponse::InternalServerError().json(EvalsError::simple(
                "run_history_unreadable",
                format!("eval run history could not be read: {error}"),
            ))
        },
    };
    // Filtering on start (not on overlap) so consecutive ranges partition
    // history rather than double-counting the runs that straddle a boundary.
    let matching: Vec<EvalRun> = runs
        .into_iter()
        .filter(|run| run.started_at_ms >= from_ms && run.started_at_ms < to_ms)
        .collect();

    let total = matching.len();
    let limit = query
        .limit
        .unwrap_or(DEFAULT_RUNS_LIMIT)
        .clamp(1, MAX_RUNS_LIMIT);
    let offset = query.offset.unwrap_or(0);
    let page: Vec<EvalRun> = matching.into_iter().skip(offset).take(limit).collect();

    let costs = costs_for_runs(&page, api.ledger(&principal, &workspace).as_ref());
    let runs = page
        .into_iter()
        .zip(costs.into_iter())
        .map(|(run, cost)| RunView {
            run,
            cost: cost.cost,
        })
        .collect::<Vec<_>>();

    HttpResponse::Ok().json(RunsResponse {
        runs,
        total,
        limit,
        offset,
    })
}

/// GET `/api/magician/v2/evals/spend`
///
/// Refuses a range the ledger cannot answer instead of summing the part of it
/// that fits: a partial answer is a smaller bill wearing the clothes of a
/// total, and nothing downstream could tell the difference.
pub async fn spend_handler(
    req: HttpRequest,
    query: web::Query<SpendQuery>,
    api: Option<web::Data<EvalsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return layer_unavailable();
    };
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let (Some(from_ms), Some(to_ms)) = (query.from_ms, query.to_ms) else {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "range_required",
            "eval spend needs an explicit `from_ms` and `to_ms`; a total over an unstated \
             range is not a total",
        ));
    };
    if from_ms < 0 || to_ms <= from_ms {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "invalid_range",
            format!("eval spend needs a positive half-open window, got [{from_ms}, {to_ms})"),
        ));
    }
    let width_ms = to_ms.saturating_sub(from_ms);
    if width_ms > LEDGER_MAX_WINDOW_MS {
        return HttpResponse::BadRequest().json(EvalsError::simple(
            "range_too_wide",
            format!(
                "the LLM ledger answers at most {} days at a time and this range is {} days; \
                 narrow it rather than reading a partial total",
                LEDGER_MAX_WINDOW_MS / DAY_MS,
                width_ms.div_euclid(DAY_MS) + i64::from(width_ms.rem_euclid(DAY_MS) > 0),
            ),
        ));
    }

    let runs = match api.store.list(&principal, &workspace, None) {
        Ok(runs) => runs,
        Err(error) => {
            return HttpResponse::InternalServerError().json(EvalsError::simple(
                "run_history_unreadable",
                format!("eval run history could not be read: {error}"),
            ))
        },
    };
    let report = spend_in_range(
        &runs,
        api.ledger(&principal, &workspace).as_ref(),
        from_ms,
        to_ms,
    );
    HttpResponse::Ok().json(report)
}

/// GET `/api/magician/v2/evals/report/{tail}`
///
/// Serves a lane's own (non-uniform) report so `report_href` can be a working
/// link. Deliberately NOT scope-bound: these files are repo-level build output
/// under `COVERAGE_BASE_DIR`, and the link is followed by a browser navigation
/// that carries no `X-Principal` header. What gates it instead is the Makefile:
/// the requested path must lie inside some lane's declared `report=` directory,
/// and those values were validated at parse time to be repo-relative with no
/// `..` in them.
pub async fn report_handler(
    path: web::Path<String>,
    api: Option<web::Data<EvalsApi>>,
) -> HttpResponse {
    let Some(api) = api else {
        return layer_unavailable();
    };
    let requested = path.into_inner();
    let registry = match api.registry() {
        Ok(registry) => registry,
        Err(message) => {
            return HttpResponse::InternalServerError()
                .json(EvalsError::simple("registry_unreadable", message))
        },
    };

    let not_found = || {
        HttpResponse::NotFound().json(EvalsError::simple(
            "report_not_found",
            format!("no eval report is served at `{requested}`"),
        ))
    };
    if !path_is_inside_a_declared_report_dir(&registry, &requested) {
        return not_found();
    }
    let Some(file) = resolve_report_file(&api.reports_root.join(&requested)) else {
        return not_found();
    };
    // Belt and braces against a symlink inside a report directory pointing out
    // of the reports root: the path check above is textual, this one is not.
    let inside = match (file.canonicalize(), api.reports_root.canonicalize()) {
        (Ok(file), Ok(root)) => file.starts_with(root),
        _ => false,
    };
    if !inside {
        return not_found();
    }

    match std::fs::metadata(&file) {
        Ok(metadata) if metadata.len() > MAX_REPORT_BYTES => {
            return HttpResponse::BadRequest().json(EvalsError::simple(
                "report_too_large",
                format!(
                    "`{requested}` is {} bytes; reports are served inline only up to {MAX_REPORT_BYTES}",
                    metadata.len()
                ),
            ))
        },
        Ok(_) => {},
        Err(_) => return not_found(),
    }

    match tokio::fs::read(&file).await {
        Ok(bytes) => HttpResponse::Ok()
            // The files are build output, not trusted markup: never let a
            // browser sniff a different type than the extension declares.
            .insert_header(("X-Content-Type-Options", "nosniff"))
            .content_type(report_content_type(&file))
            .body(bytes),
        Err(error) => HttpResponse::InternalServerError().json(EvalsError::simple(
            "report_unreadable",
            format!("eval report `{requested}` could not be read: {error}"),
        )),
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn layer_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(EvalsError::simple(
        "evals_unavailable",
        "the evals layer is not initialized in this process",
    ))
}

/// Every refusal to start a lane, as a status the page can act on.
///
/// The two `409`s are the ones that matter: they are the difference between
/// "we stopped you before anything was spent" and a run that dies halfway
/// through, or a lane billed twice.
fn run_error_response(error: EvalRunError) -> HttpResponse {
    match error {
        EvalRunError::UnknownLane(lane) => HttpResponse::NotFound().json(EvalsError::for_lane(
            "unknown_lane",
            lane.clone(),
            format!(
                "no eval lane named `{lane}`; the Makefile is the registry, so it was renamed \
                 or its `## eval:` annotation was lost"
            ),
        )),
        EvalRunError::NotRunnable { lane } => {
            HttpResponse::UnprocessableEntity().json(EvalsError::for_lane(
                "lane_not_runnable",
                lane.clone(),
                format!("lane `{lane}` never declared a `kind`, so there is no way to run it"),
            ))
        },
        EvalRunError::Unparseable { lane, problem } => {
            HttpResponse::UnprocessableEntity().json(EvalsError::for_lane(
                "lane_annotation_malformed",
                lane.clone(),
                format!(
                    "lane `{lane}` has a malformed annotation, so what it declares cannot be \
                     trusted: {problem}"
                ),
            ))
        },
        EvalRunError::NotReady { lane, missing } => {
            let named = missing
                .iter()
                .copied()
                .map(requirement_key)
                .collect::<Vec<_>>()
                .join(", ");
            let verb = if missing.len() == 1 { "is" } else { "are" };
            HttpResponse::Conflict().json(EvalsError {
                missing,
                ..EvalsError::for_lane(
                    "lane_not_ready",
                    lane.clone(),
                    format!("lane `{lane}` was not started: {named} {verb} not proven up"),
                )
            })
        },
        EvalRunError::AlreadyRunning { lane, task_id } => {
            HttpResponse::Conflict().json(EvalsError {
                task_id: Some(task_id.clone()),
                ..EvalsError::for_lane(
                    "lane_already_running",
                    lane.clone(),
                    format!("lane `{lane}` is already running as task `{task_id}`"),
                )
            })
        },
        EvalRunError::Backend(message) => {
            HttpResponse::InternalServerError().json(EvalsError::simple(
                "task_system_unavailable",
                format!("the task system could not run this lane: {message}"),
            ))
        },
    }
}

/// The requirement's wire token (`magician_binary`, not `MagicianBinary`), so a
/// message names it exactly as the Makefile's `requires=` does.
fn requirement_key(requirement: EvalRequirement) -> String {
    serde_json::to_value(requirement)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{requirement:?}"))
}

/// Every requirement kind and what the probe found, including the ones no lane
/// declares — a page that only lists what was asked about cannot say that a
/// probe was skipped.
fn probe_map(snapshot: &ProbeSnapshot) -> BTreeMap<String, ProbeResult> {
    ALL_REQUIREMENTS
        .iter()
        .copied()
        .map(|requirement| (requirement_key(requirement), snapshot.result(requirement)))
        .collect()
}

/// Whether `requested` names something inside a lane's declared `report=`
/// directory.
///
/// The Makefile is the allowlist. `requested` itself is caller-supplied, so it
/// is re-checked for traversal here even though the annotation it must match
/// was already validated: the caller controls what comes *after* the declared
/// prefix.
fn path_is_inside_a_declared_report_dir(registry: &EvalRegistry, requested: &str) -> bool {
    let requested = requested.trim_matches('/');
    if requested.is_empty() || !path_has_only_normal_components(requested) {
        return false;
    }
    registry.lanes.iter().any(|lane| {
        lane.report_dir.as_deref().is_some_and(|dir| {
            let dir = dir.trim_matches('/');
            !dir.is_empty()
                && (requested == dir
                    || requested
                        .strip_prefix(dir)
                        .is_some_and(|rest| rest.starts_with('/')))
        })
    })
}

/// Rejects anything that is not a plain relative path: `..`, a root, a Windows
/// prefix, and the `.` that would let a prefix check be dodged.
fn path_has_only_normal_components(value: &str) -> bool {
    Path::new(value)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// The file a report href resolves to: the path itself when it is a file, or
/// the first recognisable index inside it when it is a directory.
fn resolve_report_file(target: &Path) -> Option<PathBuf> {
    let metadata = std::fs::metadata(target).ok()?;
    if metadata.is_file() {
        return Some(target.to_path_buf());
    }
    if !metadata.is_dir() {
        return None;
    }
    REPORT_INDEX_CANDIDATES
        .iter()
        .map(|candidate| target.join(candidate))
        .find(|candidate| candidate.is_file())
}

fn report_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        "md" | "txt" | "log" | "text" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;

    use actix_web::{http::StatusCode, test as actix_test, App};
    use serde_json::Value;
    use tempfile::TempDir;

    use magician_surfaces::evals::run::{EvalRun, EvalRunStatus};
    use magician_surfaces::evals::runner::{
        EvalCommandOutcome, EvalExecutionSpec, StartedExecution,
    };

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";

    /// Four lanes covering every shape the page has to draw, plus one
    /// annotation that binds to nothing.
    const MAKEFILE: &str = concat!(
        "## eval: kind=harness\n",
        "eval-harness-ok:\n",
        "\t@echo ok\n",
        "\n",
        "## eval: kind=live requires=ollama report=evals/needs-ollama\n",
        "eval-needs-ollama:\n",
        "\t@echo live\n",
        "\n",
        "## eval: kind=liv\n",
        "eval-bad-kind:\n",
        "\t@echo bad\n",
        "\n",
        "## eval: kind=harness\n",
        "\n",
        "eval-orphaned-annotation:\n",
        "\t@echo orphan\n",
    );

    struct FixedReadiness(ProbeSnapshot);

    #[async_trait]
    impl ReadinessSource for FixedReadiness {
        async fn probe(&self) -> ProbeSnapshot {
            self.0.clone()
        }
    }

    struct NoReports;

    impl ReportLocator for NoReports {
        fn report_href(&self, _lane: &EvalLane, _started_at_ms: i64) -> Option<String> {
            None
        }
    }

    /// Answers the one question the runner asks before spending anything, and
    /// refuses to actually start: every test here is about a refusal, and a
    /// fake that "started" would need a runtime to supervise the run.
    struct FakeExecutor {
        inflight: Result<Option<String>, String>,
    }

    impl FakeExecutor {
        fn idle() -> Arc<Self> {
            Arc::new(Self { inflight: Ok(None) })
        }

        fn already_running(task_id: &str) -> Arc<Self> {
            Arc::new(Self {
                inflight: Ok(Some(task_id.to_string())),
            })
        }
    }

    #[async_trait]
    impl EvalExecutor for FakeExecutor {
        async fn running_task_for_lane(
            &self,
            _principal: &str,
            _workspace: &str,
            _lane_id: &str,
        ) -> Result<Option<String>, String> {
            self.inflight.clone()
        }

        async fn start(&self, _spec: EvalExecutionSpec) -> Result<StartedExecution, String> {
            Err("this test never starts a real execution".to_string())
        }

        async fn wait(&self, _execution_id: &str) -> Result<EvalCommandOutcome, String> {
            Err("nothing was started".to_string())
        }
    }

    fn everything_down() -> Arc<dyn ReadinessSource> {
        Arc::new(FixedReadiness(
            ProbeSnapshot::default().with(EvalRequirement::Ollama, ProbeResult::Down),
        ))
    }

    fn api_with(
        readiness: Arc<dyn ReadinessSource>,
        executor: Arc<dyn EvalExecutor>,
    ) -> (TempDir, TempDir, web::Data<EvalsApi>) {
        let repo = TempDir::new().expect("repo tempdir");
        let scope = TempDir::new().expect("scope tempdir");
        std::fs::write(repo.path().join("Makefile"), MAKEFILE).expect("fixture Makefile");

        let api = EvalsApi::new(
            ArtifactV2Workspace::new(scope.path()),
            repo.path(),
            repo.path().join("coverage"),
        )
        .with_readiness_source(readiness)
        .with_report_locator(Arc::new(NoReports))
        .with_executor(executor);
        (repo, scope, web::Data::new(api))
    }

    fn default_api() -> (TempDir, TempDir, web::Data<EvalsApi>) {
        api_with(everything_down(), FakeExecutor::idle())
    }

    /// Exactly the routes `bin/magician.rs` registers, in the same order — an
    /// ordering bug there would otherwise be invisible here.
    macro_rules! build_app {
        ($api:expr) => {
            actix_test::init_service(
                App::new()
                    .app_data($api.clone())
                    .route("/evals/lanes", web::get().to(list_lanes_handler))
                    .route("/evals/runs", web::get().to(list_runs_handler))
                    .route("/evals/spend", web::get().to(spend_handler))
                    .route("/evals/report/{tail:.*}", web::get().to(report_handler))
                    .route(
                        "/evals/web-researcher/judge",
                        web::post().to(judge_web_research_answer_handler),
                    )
                    .route("/evals/{lane}/run", web::post().to(run_lane_handler)),
            )
            .await
        };
    }

    #[actix_web::test]
    async fn web_research_judge_rejects_missing_opened_sources_before_dispatch() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/evals/web-researcher/judge")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .set_json(serde_json::json!({
                    "question": "What is the current price?",
                    "answer": "It costs $1.",
                    "sources": []
                }))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "invalid_judge_sources");
    }

    #[test]
    fn web_research_eval_judge_routes_only_its_operation_to_selected_profile() {
        let overrides = web_research_eval_routing_overrides("candidate-profile");
        assert_eq!(
            overrides
                .operations
                .get("evidence_precision_judge")
                .and_then(|endpoint| endpoint.profile_name()),
            Some("candidate-profile")
        );
        assert!(overrides.planning.is_none());
        assert!(overrides.evaluation.is_none());
        assert!(overrides.correction_extraction.is_none());
        assert!(overrides.memory_consolidation.is_none());
    }

    #[test]
    fn web_research_eval_profile_validation_accepts_config_names_only() {
        assert!(valid_eval_llm_profile(
            DEFAULT_WEB_RESEARCH_EVAL_LLM_PROFILE
        ));
        assert!(valid_eval_llm_profile("candidate.profile:v2_tools-any"));
        assert!(!valid_eval_llm_profile(""));
        assert!(!valid_eval_llm_profile(" profile"));
        assert!(!valid_eval_llm_profile("profile/name"));
        assert!(!valid_eval_llm_profile(&"x".repeat(129)));
    }

    fn store_run(api: &EvalsApi, principal: &str, lane: &str, run_id: &str, started: i64) {
        let mut run = EvalRun::new(run_id, lane, principal, WORKSPACE, started);
        run.status = EvalRunStatus::Passed;
        run.exit_code = Some(0);
        run.duration_ms = 1_000;
        api.store.append(&run).expect("the run records");
    }

    /// A read with no scope must not quietly answer for someone. The scope
    /// helper owns the 400; this asserts the route actually goes through it.
    #[actix_web::test]
    async fn lanes_requires_scope() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/evals/lanes")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// An annotation that binds to nothing is an eval that has silently
    /// vanished. Dropping it from the response is the one failure the page
    /// cannot draw.
    #[actix_web::test]
    async fn lanes_returns_orphaned_annotations() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/evals/lanes")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let body: Value = actix_test::read_body_json(response).await;
        let lanes = body["lanes"].as_array().expect("lanes");
        assert_eq!(lanes.len(), 3, "the three rules that carry an annotation");

        let orphaned = body["orphaned"].as_array().expect("orphaned");
        assert_eq!(orphaned.len(), 1, "the annotation ended by a blank line");
        assert!(
            orphaned[0]["reason"]
                .as_str()
                .expect("a reason")
                .contains("blank line"),
            "the diagnostic must say what got in the way: {orphaned:?}"
        );

        // Readiness is per-lane and fail-closed: `requires=ollama` is Down here.
        let live = lanes
            .iter()
            .find(|lane| lane["id"] == "eval-needs-ollama")
            .expect("the live lane");
        assert_eq!(live["ready"], false);
        assert_eq!(live["missing"][0], "ollama");
        // …and a lane with no requirements is not gated behind a service it
        // never touches.
        let harness = lanes
            .iter()
            .find(|lane| lane["id"] == "eval-harness-ok")
            .expect("the harness lane");
        assert_eq!(harness["ready"], true);
        assert_eq!(body["probes"]["ollama"], "down");
        assert_eq!(
            body["probes"]["magicutor"], "unknown",
            "a requirement nobody probed is unproven, not fine"
        );
    }

    #[actix_web::test]
    async fn lifecycle_run_button_records_history_and_serves_its_exact_report() {
        struct CompletingExecutor {
            reports: PathBuf,
            goals: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl EvalExecutor for CompletingExecutor {
            async fn running_task_for_lane(
                &self,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<Option<String>, String> {
                Ok(None)
            }
            async fn start(&self, spec: EvalExecutionSpec) -> Result<StartedExecution, String> {
                let run_id = spec
                    .goal
                    .split_whitespace()
                    .find_map(|s| s.strip_prefix("EVAL_RUN_ID="))
                    .unwrap();
                let directory = self
                    .reports
                    .join("evals/memory-lifecycle/live/runs")
                    .join(run_id);
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(directory.join("report.html"), "lifecycle retained failure")
                    .unwrap();
                self.goals.lock().unwrap().push(spec.goal);
                Ok(StartedExecution {
                    task_id: "lifecycle-task".into(),
                    execution_id: "lifecycle-exec".into(),
                })
            }
            async fn wait(&self, _: &str) -> Result<EvalCommandOutcome, String> {
                Ok(EvalCommandOutcome::new(1, "51/57"))
            }
        }
        let repo = TempDir::new().unwrap();
        let scope = TempDir::new().unwrap();
        std::fs::write(repo.path().join("Makefile"), "## eval: kind=live requires=provider_keys report=evals/memory-lifecycle/live\ntest-memory-lifecycle-live-eval:\n\t@echo fixture\n").unwrap();
        let executor = Arc::new(CompletingExecutor {
            reports: repo.path().join("coverage"),
            goals: Default::default(),
        });
        let api = web::Data::new(
            EvalsApi::new(
                ArtifactV2Workspace::new(scope.path()),
                repo.path(),
                &executor.reports,
            )
            .with_readiness_source(Arc::new(FixedReadiness(
                ProbeSnapshot::default().with(EvalRequirement::ProviderKeys, ProbeResult::Up),
            )))
            .with_executor(executor.clone()),
        );
        let store = Arc::clone(&api.store);
        let app = build_app!(api);
        let request = actix_test::TestRequest::post()
            .uri("/evals/test-memory-lifecycle-live-eval/run")
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .set_json(serde_json::json!({"profiles":["candidate"],"repeats":1}))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let started: Value = actix_test::read_body_json(response).await;
        for _ in 0..200 {
            if !store.list(PRINCIPAL, WORKSPACE, None).unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let request = actix_test::TestRequest::get()
            .uri("/evals/runs?lane=test-memory-lifecycle-live-eval")
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_request();
        let history: Value = actix_test::call_and_read_body_json(&app, request).await;
        let run = &history["runs"][0];
        assert_eq!(run["run_id"], started["run_id"]);
        assert_eq!(run["status"], "failed");
        assert_eq!(run["options"]["profiles"][0], "candidate");
        assert_eq!(run["cost"]["kind"], "unknown");
        let href = run["report_href"].as_str().unwrap();
        assert!(href.contains(started["run_id"].as_str().unwrap()));
        let request = actix_test::TestRequest::get()
            .uri(&href.replacen("/api/magician/v2", "", 1))
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            actix_test::read_body(response).await.as_ref(),
            b"lifecycle retained failure"
        );
        assert!(
            executor.goals.lock().unwrap()[0].contains("MEMORY_LIFECYCLE_EVAL_PROFILES=candidate")
        );
    }

    #[actix_web::test]
    async fn lifecycle_lane_discovery_and_options_validation() {
        let (repo, _scope, api) = default_api();
        std::fs::write(repo.path().join("Makefile"), "## eval: kind=live requires=provider_keys report=evals/memory-lifecycle/live\ntest-memory-lifecycle-live-eval:\n\t@echo fixture\n").unwrap();
        let app = build_app!(api);
        let get = actix_test::TestRequest::get()
            .uri("/evals/lanes")
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .to_request();
        let body: Value = actix_test::call_and_read_body_json(&app, get).await;
        assert_eq!(body["lanes"][0]["run_options"], "memory_lifecycle");
        assert_eq!(body["lanes"][0]["ready"], false);
        for options in [
            serde_json::json!({"repeats":0}),
            serde_json::json!({"profiles":["bad;command"]}),
            serde_json::json!({"unknown":true}),
        ] {
            let request = actix_test::TestRequest::post()
                .uri("/evals/test-memory-lifecycle-live-eval/run")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .set_json(options)
                .to_request();
            let response = actix_test::call_service(&app, request).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body: Value = actix_test::read_body_json(response).await;
            assert_eq!(body["error"], "invalid_run_options");
        }
        let request = actix_test::TestRequest::post()
            .uri("/evals/test-memory-lifecycle-live-eval/run")
            .insert_header(("X-Principal", PRINCIPAL))
            .insert_header(("X-Workspace", WORKSPACE))
            .set_json(
                serde_json::json!({"profiles":["candidate"],"repeats":1,"partition":"validation"}),
            )
            .to_request();
        assert_eq!(
            actix_test::call_service(&app, request).await.status(),
            StatusCode::CONFLICT
        );
    }

    #[actix_web::test]
    async fn running_an_unknown_lane_is_404() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/evals/eval-does-not-exist/run")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "unknown_lane");
    }

    /// The check that stops an expensive run BEFORE it starts. Naming the
    /// services is the whole value: "not ready" alone sends the operator
    /// looking, and a second reload to discover the next missing one.
    #[actix_web::test]
    async fn running_an_unready_lane_is_409_naming_the_missing_services() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/evals/eval-needs-ollama/run")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "lane_not_ready");
        assert_eq!(body["missing"][0], "ollama");
        assert!(
            body["message"]
                .as_str()
                .expect("a message")
                .contains("ollama"),
            "the human-readable half must name the service too: {body}"
        );
    }

    /// Refusing here is what stops one lane being billed twice, so the answer
    /// carries the in-flight task rather than only saying no.
    #[actix_web::test]
    async fn running_an_already_running_lane_is_409_naming_the_task() {
        let (_repo, _scope, api) = api_with(
            everything_down(),
            FakeExecutor::already_running("task_inflight_7"),
        );
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            // The harness lane: ready, so the ONLY thing that can refuse it is
            // the in-flight check.
            actix_test::TestRequest::post()
                .uri("/evals/eval-harness-ok/run")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "lane_already_running");
        assert_eq!(body["task_id"], "task_inflight_7");
    }

    /// `kind=liv` parses into a lane carrying a defect rather than vanishing —
    /// and a lane whose declaration cannot be trusted must not be startable.
    #[actix_web::test]
    async fn a_lane_with_unknown_kind_is_422() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::post()
                .uri("/evals/eval-bad-kind/run")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let body: Value = actix_test::read_body_json(response).await;
        let error = body["error"].as_str().expect("an error code");
        assert!(
            error == "lane_annotation_malformed" || error == "lane_not_runnable",
            "a lane nobody understood must not be runnable, got `{error}`"
        );
    }

    #[actix_web::test]
    async fn runs_are_scope_isolated() {
        let (_repo, _scope, api) = default_api();
        store_run(
            &api,
            PRINCIPAL,
            "eval-harness-ok",
            "evr_mine",
            1_700_000_000_000,
        );
        store_run(
            &api,
            "someone-else",
            "eval-harness-ok",
            "evr_theirs",
            1_700_000_001_000,
        );

        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/evals/runs")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["runs"][0]["run_id"], "evr_mine");
        assert_eq!(body["runs"][0]["principal"], PRINCIPAL);
    }

    /// The ledger cannot answer a range this wide. Summing the part it *would*
    /// answer returns a smaller number in the shape of a total, and nothing
    /// downstream could tell the difference — so it is refused instead.
    #[actix_web::test]
    async fn a_spend_range_wider_than_the_ledger_window_is_rejected() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let from_ms = 1_700_000_000_000i64;
        let too_wide = from_ms + LEDGER_MAX_WINDOW_MS + 1;

        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri(&format!("/evals/spend?from_ms={from_ms}&to_ms={too_wide}"))
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body: Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "range_too_wide");
        assert!(
            body["message"].as_str().expect("a message").contains("31"),
            "the refusal must name the limit so the caller can narrow the range: {body}"
        );

        // Exactly at the bound is answerable, so the refusal is a bound and not
        // an off-by-one that quietly costs a day of history.
        let at_bound = from_ms + LEDGER_MAX_WINDOW_MS;
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri(&format!("/evals/spend?from_ms={from_ms}&to_ms={at_bound}"))
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// `—` and `$0.00` are one careless template apart. The wire shape is what
    /// keeps them apart: when the cost is unknown there is no number to read.
    #[actix_web::test]
    async fn unknown_cost_serializes_without_a_numeric_field() {
        let (_repo, _scope, api) = default_api();
        // No `task_id`, so spend cannot be attributed to this run at all.
        store_run(
            &api,
            PRINCIPAL,
            "eval-harness-ok",
            "evr_1",
            1_700_000_000_000,
        );

        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/evals/runs")
                .insert_header(("X-Principal", PRINCIPAL))
                .insert_header(("X-Workspace", WORKSPACE))
                .to_request(),
        )
        .await;
        let body: Value = actix_test::read_body_json(response).await;

        let cost = &body["runs"][0]["cost"];
        assert_eq!(cost["kind"], "unknown");
        assert!(
            cost.get("usd").is_none(),
            "an unknown cost must carry no number for anything to render as zero: {cost}"
        );
        assert!(
            cost.as_object().expect("a tagged object").len() == 1,
            "the unknown shape is exactly {{kind: unknown}}: {cost}"
        );
    }

    /// The href the run record is stamped with has to be a URL the browser can
    /// follow, not a path on the box that produced it.
    #[test]
    fn the_report_href_prefix_is_a_url_path() {
        assert!(EVAL_REPORT_HREF_PREFIX.starts_with('/'));
        assert!(!EVAL_REPORT_HREF_PREFIX.ends_with('/'));
    }

    /// The Makefile is the allowlist for the report passthrough, so a path
    /// outside every declared `report=` directory is not served — and neither
    /// is one that tries to climb out of a declared one.
    #[test]
    fn only_paths_inside_a_declared_report_dir_are_served() {
        let registry = parse_eval_registry(MAKEFILE);
        for (path, allowed) in [
            ("evals/needs-ollama", true),
            ("evals/needs-ollama/latest.html", true),
            ("evals/needs-ollama-elsewhere", false),
            ("evals", false),
            ("evals/needs-ollama/../../etc/passwd", false),
            ("/etc/passwd", false),
            ("", false),
        ] {
            assert_eq!(
                path_is_inside_a_declared_report_dir(&registry, path),
                allowed,
                "path: {path}"
            );
        }
    }

    #[actix_web::test]
    async fn an_unknown_report_path_is_404_rather_than_a_disk_read() {
        let (_repo, _scope, api) = default_api();
        let app = build_app!(api);
        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/evals/report/etc/passwd")
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// A requirement missing from this list would be invisible on the page.
    /// The match below is exhaustive on purpose: adding an `EvalRequirement`
    /// variant makes this test fail to compile, which is the reminder.
    #[test]
    fn every_requirement_kind_is_reported_to_the_page() {
        for requirement in ALL_REQUIREMENTS {
            let known = match requirement {
                EvalRequirement::Ollama
                | EvalRequirement::Magician
                | EvalRequirement::MagicianBinary
                | EvalRequirement::Magicutor
                | EvalRequirement::ProviderKeys => true,
            };
            assert!(known);
        }
        assert_eq!(
            probe_map(&ProbeSnapshot::default()).len(),
            ALL_REQUIREMENTS.len()
        );
    }
}
