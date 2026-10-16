//! Runtime-backed live evaluator for progressive content retrieval.
//!
//! Unlike the standalone transport canary, this binary constructs the
//! production scoped capability/content resolver and invokes the compiled
//! `content_search` / `content_read` handler cores. Reports intentionally omit
//! URLs, queries, content, stable IDs, and browser/session fingerprints.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use magician::{
    config::{load_magician_config_from_path, magician_config_path, MagicianConfig},
    magician_v2::{
        artifact_v2::{workspace::default_storage_base_path, CapabilityWorkspaceManager},
        content_sources::{
            ContentAcquisitionResolver, RetrievalAttemptReceipt, RetrievalResult, RetrievalStatus,
        },
        execution::{
            compiled_dispatch::CompiledDispatchAuthority,
            compiled_handlers::{content_read, content_search},
            ExecutionConfig, MagicutorClient, ScopedCapabilityResolver,
        },
        resource_authority::scoped_authority::DiskBackedScopedResolver,
    },
};
use secrecy::SecretString;
use serde::Serialize;
use serde_json::{json, Value};
use url::Url;

const SCHEMA_VERSION: u32 = 1;
const GENERATED_BY: &str = "magician::content_retrieval_runtime_live_eval";

#[derive(Debug, Parser)]
#[command(about = "Evaluate the production progressive content-retrieval runtime")]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value = "tool-runtime-config.yaml")]
    tool_runtime_config: PathBuf,
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    #[arg(long, default_value = "AI")]
    query: String,
    #[arg(long, default_value = "hacker_news.discover")]
    discovery_action: String,
    #[arg(long, default_value = "https://news.ycombinator.com/")]
    public_url: String,
    #[arg(long, default_value_t = 25)]
    timeout_secs: u64,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    only_discovery: bool,
    #[arg(long)]
    self_test: bool,
}

#[derive(Debug, Serialize)]
struct EvalReport {
    schema_version: u32,
    generated_by: &'static str,
    generated_at: String,
    mode: &'static str,
    config_digest: String,
    scope_digest: String,
    configured_browser_engine: Option<String>,
    cases: Vec<EvalCase>,
    summary: EvalSummary,
}

#[derive(Debug, Serialize)]
struct EvalSummary {
    status: &'static str,
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
}

#[derive(Debug, Serialize)]
struct EvalCase {
    id: &'static str,
    state: &'static str,
    failure_codes: Vec<&'static str>,
    result: Option<SanitizedResult>,
}

/// Keeps evaluator-owned credential bookkeeping alive without ever opening the
/// operator's encrypted vault with the harness's ephemeral key. Installed
/// skill packages and their private legacy env bridges remain production-real;
/// only the canonical vault root is isolated.
struct EvalRuntime {
    resolver: Arc<ContentAcquisitionResolver>,
    _credential_root: tempfile::TempDir,
}

#[derive(Debug, Serialize)]
struct SanitizedResult {
    status: String,
    operation: String,
    candidate_count: usize,
    document_present: bool,
    handoff: Option<SanitizedHandoff>,
    attempts: Vec<SanitizedAttempt>,
    unavailable_optional_action_count: usize,
    unmet_goal_reason_count: usize,
    required_authority: Option<String>,
    total_cost_microunits: BTreeMap<String, u64>,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
struct SanitizedHandoff {
    kind: String,
    requested_mode: String,
    action_id: String,
    required_authority: String,
    requires_approval: bool,
}

#[derive(Debug, Serialize)]
struct SanitizedAttempt {
    rung_id: String,
    action_id: String,
    configured_order: usize,
    actual_order: usize,
    classification: String,
    authority: String,
    actual_transport: Option<String>,
    requested_browser_mode: Option<String>,
    resolved_browser_mode: Option<String>,
    browser_engine: Option<String>,
    session_outcome: Option<String>,
    render_ms: Option<u64>,
    extract_ms: Option<u64>,
    latency_ms: u64,
    returned_items: usize,
    quality_finding_count: usize,
}

#[derive(Clone, Copy)]
enum Scenario {
    Discovery,
    StaticRead,
    BrowserRead,
    ReplayFallback,
    PublicHandoff,
    AuthBoundary,
}

impl Scenario {
    fn id(self) -> &'static str {
        match self {
            Self::Discovery => "compiled_discovery",
            Self::StaticRead => "compiled_static_read",
            Self::BrowserRead => "configured_browser_read",
            Self::ReplayFallback => "replay_to_browser_fallback",
            Self::PublicHandoff => "public_browser_handoff",
            Self::AuthBoundary => "authenticated_approval_boundary",
        }
    }
}

fn enum_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn sanitize_attempt(attempt: &RetrievalAttemptReceipt) -> SanitizedAttempt {
    SanitizedAttempt {
        rung_id: attempt.rung_id.clone(),
        action_id: attempt.action_id.clone(),
        configured_order: attempt.configured_order,
        actual_order: attempt.actual_order,
        classification: enum_name(&attempt.classification),
        authority: enum_name(&attempt.authority),
        actual_transport: attempt.actual_transport.clone(),
        requested_browser_mode: attempt.requested_browser_mode.clone(),
        resolved_browser_mode: attempt.resolved_browser_mode.clone(),
        browser_engine: attempt.browser_engine.clone(),
        session_outcome: attempt.session_outcome.clone(),
        render_ms: attempt.render_ms,
        extract_ms: attempt.extract_ms,
        latency_ms: attempt.latency_ms,
        returned_items: attempt.returned_items,
        quality_finding_count: attempt.quality_findings.len(),
    }
}

fn sanitize_result(result: &RetrievalResult) -> SanitizedResult {
    SanitizedResult {
        status: enum_name(&result.status),
        operation: enum_name(&result.operation),
        candidate_count: result.candidates.len(),
        document_present: result.document.is_some(),
        handoff: result.handoff.as_ref().map(|handoff| SanitizedHandoff {
            kind: enum_name(&handoff.kind),
            requested_mode: handoff.requested_mode.clone(),
            action_id: handoff.action_id.clone(),
            required_authority: enum_name(&handoff.required_authority),
            requires_approval: handoff.requires_approval,
        }),
        attempts: result.attempts.iter().map(sanitize_attempt).collect(),
        unavailable_optional_action_count: result.unavailable_optional_actions.len(),
        unmet_goal_reason_count: result.unmet_goal_reasons.len(),
        required_authority: result.required_authority.as_ref().map(enum_name),
        total_cost_microunits: result.total_cost_microunits.clone(),
        duration_ms: result.duration_ms,
    }
}

fn attempt<'a>(
    result: &'a RetrievalResult,
    action_id: &str,
) -> Option<&'a RetrievalAttemptReceipt> {
    result
        .attempts
        .iter()
        .find(|attempt| attempt.action_id == action_id)
}

fn gate_result(
    scenario: Scenario,
    result: &RetrievalResult,
    expected_engine: Option<&str>,
    discovery_action: &str,
) -> Vec<&'static str> {
    let mut failures = Vec::new();
    match scenario {
        Scenario::Discovery => {
            let action = if discovery_action.trim().is_empty() {
                result.attempts.first()
            } else {
                attempt(result, discovery_action)
            };
            if result.status != RetrievalStatus::Complete {
                failures.push("discovery_not_complete");
            }
            if result.candidates.is_empty() {
                failures.push("discovery_returned_no_candidates");
            }
            if action.is_none_or(|attempt| enum_name(&attempt.classification) != "sufficient") {
                failures.push("discovery_action_not_sufficient");
            }
        },
        Scenario::StaticRead => {
            let action = attempt(result, "static_http.read");
            if result.status != RetrievalStatus::Complete || result.document.is_none() {
                failures.push("static_read_not_complete");
            }
            if action.is_none_or(|attempt| enum_name(&attempt.classification) != "sufficient") {
                failures.push("static_action_not_sufficient");
            }
        },
        Scenario::BrowserRead => {
            let action = attempt(result, "browser.headless.read");
            if result.status != RetrievalStatus::Complete || result.document.is_none() {
                failures.push("browser_read_not_complete");
            }
            if action.is_none_or(|attempt| {
                enum_name(&attempt.classification) != "sufficient"
                    || attempt.actual_transport.as_deref() != Some("browser")
                    || attempt.requested_browser_mode.as_deref() != Some("public_headless_read")
                    || attempt.resolved_browser_mode.as_deref() != Some("public_headless_read")
                    || attempt.session_outcome.as_deref() != Some("closed")
            }) {
                failures.push("browser_transport_receipt_invalid");
            }
            if let Some(expected_engine) = expected_engine {
                if action.and_then(|attempt| attempt.browser_engine.as_deref())
                    != Some(expected_engine)
                {
                    failures.push("configured_browser_engine_not_used");
                }
            }
        },
        Scenario::ReplayFallback => {
            if attempt(result, "api_replay.read").is_none() {
                failures.push("replay_attempt_missing");
            }
            let browser = attempt(result, "browser.headless.read");
            if result.status != RetrievalStatus::Complete || result.document.is_none() {
                failures.push("replay_fallback_not_complete");
            }
            if browser.is_none_or(|attempt| {
                enum_name(&attempt.classification) != "sufficient"
                    || attempt.actual_transport.as_deref() != Some("browser")
            }) {
                failures.push("replay_did_not_fall_back_to_browser");
            }
        },
        Scenario::PublicHandoff => {
            let handoff = result.handoff.as_ref();
            if result.status != RetrievalStatus::HandoffRequired {
                failures.push("public_handoff_status_invalid");
            }
            if handoff.is_none_or(|handoff| {
                handoff.action_id != "browser.headless.discover_handoff"
                    || handoff.requires_approval
                    || handoff.requested_mode != "headless"
            }) {
                failures.push("public_handoff_contract_invalid");
            }
        },
        Scenario::AuthBoundary => {
            let handoff = result.handoff.as_ref();
            if result.status != RetrievalStatus::ApprovalRequired {
                failures.push("authenticated_read_did_not_require_approval");
            }
            if handoff.is_none_or(|handoff| {
                handoff.action_id != "browser.cdp.read"
                    || !handoff.requires_approval
                    || handoff.requested_mode != "cdp"
            }) {
                failures.push("authenticated_handoff_contract_invalid");
            }
            if result
                .attempts
                .iter()
                .any(|attempt| attempt.actual_transport.as_deref() == Some("browser"))
            {
                failures.push("authenticated_browser_executed_without_grant");
            }
        },
    }
    failures
}

fn validate_public_url(raw: &str) -> Result<String> {
    let parsed = Url::parse(raw).context("parsing public eval URL")?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none()
    {
        bail!("runtime live eval requires a credential-free HTTPS URL");
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        bail!("runtime live eval refuses local browser targets");
    }
    Ok(parsed.to_string())
}

fn digest(value: impl AsRef<[u8]>) -> String {
    blake3::hash(value.as_ref()).to_hex()[..16].to_string()
}

fn install_extra_roots(path: &Path) {
    let roots = tool_runtime_core::config::Config::load(path, None, None)
        .ok()
        .map(|config| {
            config
                .registry
                .resolved_paths(path.parent())
                .into_iter()
                .filter(|path| path.exists())
                .collect()
        })
        .unwrap_or_default();
    magician::magician_v2::config_extras::set_extra_system_roots(roots);
}

fn magicutor_client(config: &MagicianConfig) -> Result<MagicutorClient> {
    let mut execution = ExecutionConfig::new(
        Url::parse(&config.execution.magicutor_base_url)
            .context("parsing configured Magicutor URL")?,
    );
    execution.request_timeout = Duration::from_secs(config.execution.request_timeout_secs);
    execution.api_key = config
        .execution
        .magicutor_api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
        .map(SecretString::new);
    MagicutorClient::new(execution).context("constructing Magicutor client")
}

fn build_resolver(config: &MagicianConfig, repo_root: &Path) -> Result<EvalRuntime> {
    let storage_workspace = magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(
        default_storage_base_path(),
    );
    let manager = Arc::new(CapabilityWorkspaceManager::new(
        storage_workspace.clone(),
        repo_root,
    ));
    manager.ensure_seeded_for_existing_scopes()?;

    let mut file_sandbox = config.execution.file_sandbox.clone();
    file_sandbox.augment_with_scopes_root(storage_workspace.base_root());
    let capability_resolver = Arc::new(ScopedCapabilityResolver::new(
        Arc::clone(&manager),
        Arc::new(magicutor_client(config)?),
        file_sandbox,
        config.execution.shell_sandbox.clone(),
        None,
        None,
        None,
    ));
    let authority = CompiledDispatchAuthority::from_resolver(Arc::new(
        DiskBackedScopedResolver::new(config.resource_authority.clone(), storage_workspace.clone()),
    ));
    let (key_provider, secret_capabilities, _) =
        magician::magician_v2::secrets::in_memory_secret_runtime_bootstrap().into_parts();
    let credential_root =
        tempfile::tempdir().context("creating isolated live-eval credential bookkeeping root")?;
    let secret_store_resolver = Arc::new(
        magician::magician_v2::secrets::SecretStoreResolver::new_with_capabilities(
            key_provider,
            credential_root.path().to_path_buf(),
            secret_capabilities,
        ),
    );
    Ok(EvalRuntime {
        resolver: Arc::new(ContentAcquisitionResolver::new(
            capability_resolver,
            manager,
            authority,
            secret_store_resolver,
            config.content_acquisition.clone(),
            config.api_mining.clone(),
        )),
        _credential_root: credential_root,
    })
}

fn scoped_args(args: &Args) -> Value {
    json!({
        "__principal": args.principal,
        "__workspace": args.workspace,
        "deadline_ms": args.timeout_secs.saturating_mul(1_000),
    })
}

async fn run_scenario(
    scenario: Scenario,
    args: &Args,
    resolver: Arc<ContentAcquisitionResolver>,
    config: &MagicianConfig,
    public_url: &str,
) -> EvalCase {
    let mut input = scoped_args(args);
    let object = input.as_object_mut().expect("scoped args are an object");
    let mut replay_url = Url::parse(public_url).expect("validated public URL remains valid");
    replay_url.query_pairs_mut().append_pair(
        "magician_runtime_live_eval",
        &uuid::Uuid::new_v4().simple().to_string(),
    );
    match scenario {
        Scenario::Discovery => {
            object.insert("query".into(), json!(args.query));
            object.insert("limit".into(), json!(3));
            object.insert("min_candidates".into(), json!(1));
            object.insert("min_sources".into(), json!(1));
            if !args.discovery_action.trim().is_empty() {
                object.insert("allowed_actions".into(), json!([args.discovery_action]));
            }
        },
        Scenario::StaticRead => {
            object.insert("url".into(), json!(public_url));
            object.insert("min_chars".into(), json!(32));
            object.insert("allowed_actions".into(), json!(["static_http.read"]));
        },
        Scenario::BrowserRead => {
            object.insert("url".into(), json!(public_url));
            object.insert("min_chars".into(), json!(32));
            object.insert("allowed_actions".into(), json!(["browser.headless.read"]));
        },
        Scenario::ReplayFallback => {
            object.insert("url".into(), json!(replay_url.as_str()));
            object.insert("min_chars".into(), json!(16));
            object.insert(
                "allowed_actions".into(),
                json!(["api_replay.read", "browser.headless.read"]),
            );
        },
        Scenario::PublicHandoff => {
            object.insert("query".into(), json!(args.query));
            object.insert("limit".into(), json!(1));
            object.insert("min_candidates".into(), json!(1));
            object.insert(
                "allowed_actions".into(),
                json!(["browser.headless.discover_handoff"]),
            );
        },
        Scenario::AuthBoundary => {
            object.insert("url".into(), json!(public_url));
            object.insert("maximum_authority".into(), json!("authenticated_read"));
            object.insert("allowed_actions".into(), json!(["browser.cdp.read"]));
        },
    }

    let settings = config.content_acquisition.progressive_retrieval.clone();
    let raw = match scenario {
        Scenario::Discovery | Scenario::PublicHandoff => {
            content_search::execute_with_runtime(resolver, settings, input).await
        },
        _ => content_read::execute_with_runtime(resolver, settings, input).await,
    };
    match raw {
        Ok(value) => match serde_json::from_value::<RetrievalResult>(value) {
            Ok(result) => {
                let expected_engine = config
                    .content_acquisition
                    .browser
                    .engine
                    .as_deref()
                    .or(Some("bundled_chrome"));
                let failure_codes =
                    gate_result(scenario, &result, expected_engine, &args.discovery_action);
                EvalCase {
                    id: scenario.id(),
                    state: if failure_codes.is_empty() {
                        "passed"
                    } else {
                        "failed"
                    },
                    failure_codes,
                    result: Some(sanitize_result(&result)),
                }
            },
            Err(_) => EvalCase {
                id: scenario.id(),
                state: "failed",
                failure_codes: vec!["runtime_result_deserialization_failed"],
                result: None,
            },
        },
        Err(_) => EvalCase {
            id: scenario.id(),
            state: "failed",
            failure_codes: vec!["compiled_handler_execution_failed"],
            result: None,
        },
    }
}

fn skipped_case(scenario: Scenario) -> EvalCase {
    EvalCase {
        id: scenario.id(),
        state: "skipped",
        failure_codes: Vec::new(),
        result: None,
    }
}

fn summarize(cases: &[EvalCase]) -> EvalSummary {
    let passed = cases.iter().filter(|case| case.state == "passed").count();
    let failed = cases.iter().filter(|case| case.state == "failed").count();
    let skipped = cases.iter().filter(|case| case.state == "skipped").count();
    EvalSummary {
        status: if failed == 0 { "passed" } else { "failed" },
        total: cases.len(),
        passed,
        failed,
        skipped,
    }
}

fn write_report(report: &EvalReport, output: Option<&Path>) -> Result<()> {
    let rendered = serde_json::to_string_pretty(report)?;
    if let Some(output) = output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating report directory {}", parent.display()))?;
        }
        let temporary = output.with_extension("json.tmp");
        fs::write(&temporary, rendered.as_bytes())
            .with_context(|| format!("writing temporary report {}", temporary.display()))?;
        fs::rename(&temporary, output)
            .with_context(|| format!("publishing report {}", output.display()))?;
    }
    println!("{rendered}");
    Ok(())
}

fn self_test() -> Result<()> {
    let raw = json!({
        "trace_id": "secret-trace",
        "status": "approval_required",
        "operation": "read",
        "candidates": [],
        "handoff": {
            "id": "secret-handoff",
            "kind": "authenticated_read_approval",
            "browser_session_id": "secret-session",
            "requested_mode": "authenticated_cdp_read",
            "action_id": "browser.cdp.read",
            "required_authority": "authenticated_read",
            "principal": "secret-principal",
            "workspace": "secret-workspace",
            "target_url": "https://private.invalid/secret",
            "query": "secret-query",
            "requires_approval": true,
            "expires_at_ms": 1
        },
        "attempts": [],
        "unavailable_optional_actions": [],
        "unmet_goal_reasons": ["secret-reason"],
        "required_authority": "authenticated_read",
        "total_cost_microunits": {},
        "duration_ms": 1
    });
    let result: RetrievalResult = serde_json::from_value(raw)?;
    let rendered = serde_json::to_string(&sanitize_result(&result))?;
    for forbidden in [
        "secret-trace",
        "secret-handoff",
        "secret-session",
        "secret-principal",
        "secret-workspace",
        "private.invalid",
        "secret-query",
        "secret-reason",
        "trace_id",
        "browser_session_id",
        "target_url",
    ] {
        if rendered.contains(forbidden) {
            bail!("sanitized report retained forbidden value or field `{forbidden}`");
        }
    }
    println!("content retrieval runtime live evaluator self-test passed");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.self_test {
        return self_test();
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
    let config_path = args.config.clone().unwrap_or_else(magician_config_path);
    let config_bytes = fs::read(&config_path)
        .with_context(|| format!("reading config {}", config_path.display()))?;
    let config = load_magician_config_from_path(&config_path)?;
    let public_url = validate_public_url(&args.public_url)?;
    let scope_digest = digest(format!("{}\0{}", args.principal, args.workspace));
    let configured_browser_engine = config.content_acquisition.browser.engine.clone();

    if args.dry_run {
        let scenarios = [
            Scenario::Discovery,
            Scenario::StaticRead,
            Scenario::BrowserRead,
            Scenario::ReplayFallback,
            Scenario::PublicHandoff,
            Scenario::AuthBoundary,
        ];
        let cases = scenarios
            .into_iter()
            .filter(|scenario| !args.only_discovery || matches!(scenario, Scenario::Discovery))
            .map(skipped_case)
            .collect::<Vec<_>>();
        let report = EvalReport {
            schema_version: SCHEMA_VERSION,
            generated_by: GENERATED_BY,
            generated_at: chrono::Utc::now().to_rfc3339(),
            mode: "dry_run",
            config_digest: digest(config_bytes),
            scope_digest,
            configured_browser_engine,
            summary: summarize(&cases),
            cases,
        };
        return write_report(&report, args.output.as_deref());
    }

    install_extra_roots(&args.tool_runtime_config);
    let repo_root = fs::canonicalize(&args.repo_root)
        .with_context(|| format!("resolving repo root {}", args.repo_root.display()))?;
    let runtime = build_resolver(&config, &repo_root)?;
    let resolver = Arc::clone(&runtime.resolver);
    let browser = &config.content_acquisition.browser;
    let mut cases = Vec::new();
    for scenario in [
        Scenario::Discovery,
        Scenario::StaticRead,
        Scenario::BrowserRead,
        Scenario::ReplayFallback,
        Scenario::PublicHandoff,
        Scenario::AuthBoundary,
    ] {
        if args.only_discovery && !matches!(scenario, Scenario::Discovery) {
            continue;
        }
        let enabled = match scenario {
            Scenario::Discovery | Scenario::StaticRead => true,
            Scenario::BrowserRead => browser.enabled && browser.public_headless_reads,
            Scenario::ReplayFallback => {
                browser.enabled
                    && browser.public_headless_reads
                    && browser.verified_api_replay
                    && config.api_mining.enable_replay
            },
            Scenario::PublicHandoff => browser.enabled && browser.public_handoffs,
            Scenario::AuthBoundary => browser.enabled && browser.authenticated_cdp,
        };
        if enabled {
            cases.push(
                run_scenario(scenario, &args, Arc::clone(&resolver), &config, &public_url).await,
            );
        } else {
            cases.push(skipped_case(scenario));
        }
    }
    let summary = summarize(&cases);
    let failed = summary.failed > 0;
    let report = EvalReport {
        schema_version: SCHEMA_VERSION,
        generated_by: GENERATED_BY,
        generated_at: chrono::Utc::now().to_rfc3339(),
        mode: "live",
        config_digest: digest(config_bytes),
        scope_digest,
        configured_browser_engine,
        cases,
        summary,
    };
    write_report(&report, args.output.as_deref())?;
    if failed {
        bail!("runtime content retrieval live evaluation failed");
    }
    Ok(())
}
