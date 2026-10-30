//! Final-answer live eval: replays a finished run's finalization (execution
//! output synthesis, then the task-user projection with its claim check,
//! repair and caveat) with the live model router, and checks what the owner
//! would be shown.
//!
//! Each case names a real execution in a runtime root. Its task directory is
//! copied into the output directory and finalized there, so the runtime is
//! only read. A case passes when the published answer contains every
//! `must_contain` entry (a string, or a list of which any one will do) and
//! none of `must_not_contain`, ignoring case.
//!
//! A negative case plants an error with `alter_answer` (replacements in the
//! agent's own answer, never in tool results) and lists the planted text in
//! `must_flag`: it passes only when each is left out of the published answer
//! or the answer carries Magician's "not fully verified" caveat.
//!
//! Cases: `data/magician_v2/evals/final_answer/cases.json`. Every case dir
//! keeps the rendered synthesis bundles, both outputs, the claim check and
//! each claim-judge prompt and response (`judge/`).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use magician::{
    config::{load_default_magician_config, load_magician_config_from_path, MagicianConfig},
    magician_v2::{
        artifact_v2::{
            service::finalize_replay::{replay_finalization, FinalizeReplayRequest},
            synthesis::SynthesisDependencies,
            workspace::runtime_config_path,
        },
        prompts::{
            json_storage::{default_prompt_dir, JsonStorageConfig},
            JsonPromptStorage, PromptManager, PromptStore,
        },
        query_analysis::operation_llm_router::OperationLlmRouter,
    },
};
use serde::{Deserialize, Serialize};

const SCHEMA_VERSION: u32 = 1;
const GENERATED_BY: &str = "magician::final_answer_live_eval";

#[derive(Parser)]
struct Args {
    /// Cases file.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/magician_v2/evals/final_answer/cases.json"))]
    cases: PathBuf,
    /// Only this case id.
    #[arg(long)]
    case: Option<String>,
    /// Runtime root the cases ran under; the active one when omitted.
    #[arg(long)]
    runtime_root: Option<PathBuf>,
    /// Magician config; the active runtime config when omitted.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Report directory; a fresh run directory is created inside it.
    #[arg(long)]
    output_dir: PathBuf,
    /// Replays per case (synthesis is not deterministic).
    #[arg(long, default_value_t = 1)]
    repeat: usize,
}

#[derive(Deserialize)]
struct CaseFile {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    #[serde(default)]
    note: String,
    principal: String,
    workspace: String,
    task_id: String,
    execution_id: String,
    #[serde(default)]
    must_contain: Vec<Needle>,
    #[serde(default)]
    must_not_contain: Vec<String>,
    #[serde(default)]
    alter_answer: Vec<Alteration>,
    #[serde(default)]
    must_flag: Vec<String>,
}

/// Text the answer must contain: one string, or alternatives of which any
/// one will do (wording that may legitimately vary).
#[derive(Deserialize)]
#[serde(untagged)]
enum Needle {
    One(String),
    AnyOf(Vec<String>),
}

impl Needle {
    fn found_in(&self, lowered_answer: &str) -> bool {
        let found = |text: &String| lowered_answer.contains(&text.to_lowercase());
        match self {
            Self::One(text) => found(text),
            Self::AnyOf(options) => options.iter().any(found),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::One(text) => text.clone(),
            Self::AnyOf(options) => options.join(" | "),
        }
    }
}

#[derive(Deserialize)]
struct Alteration {
    from: String,
    to: String,
}

/// Magician's caveat on an answer it could not fully confirm: the HTML
/// `aside` marker, or the Markdown banner's words.
const CAVEAT_MARKERS: [&str; 2] = ["data-magician-caveat", "**Not fully verified.**"];

/// Where the claim judge writes its prompts and responses
/// (`answer_verification::dump_judge_exchange`).
const JUDGE_DUMP_DIR_ENV: &str = "MAGICIAN_CLAIM_JUDGE_DUMP_DIR";

#[derive(Serialize)]
struct CaseRun {
    case: String,
    run: usize,
    passed: bool,
    missing: Vec<String>,
    forbidden_present: Vec<String>,
    /// Planted errors published as if verified.
    unflagged: Vec<String>,
    error: Option<String>,
    caveated: bool,
    unconfirmed_claims: usize,
    repaired_claims: usize,
    removed_claims: usize,
    directory: String,
}

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    generated_by: &'static str,
    generated_at: String,
    passed: usize,
    total: usize,
    runs: Vec<CaseRun>,
}

fn load_config(args: &Args) -> Result<MagicianConfig> {
    match args.config.as_deref() {
        Some(path) => load_magician_config_from_path(path)
            .with_context(|| format!("loading config {}", path.display())),
        None => load_default_magician_config().context("loading the active magician config"),
    }
}

fn claim_count(check: Option<&serde_json::Value>, key: &str) -> usize {
    check
        .and_then(|check| check.get(key))
        .and_then(serde_json::Value::as_array)
        .map_or(0, Vec::len)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cases: CaseFile = serde_json::from_slice(
        &fs::read(&args.cases).with_context(|| format!("reading {}", args.cases.display()))?,
    )
    .context("parsing the cases file")?;
    let cases: Vec<Case> = cases
        .cases
        .into_iter()
        .filter(|case| args.case.as_deref().is_none_or(|id| id == case.id))
        .collect();
    if cases.is_empty() {
        bail!("no case matches");
    }

    for file_name in [".env.development", ".env"] {
        let _ = dotenvy::from_path(runtime_config_path(file_name, file_name));
    }
    magician_chunking::register_builtin_chunk_adapters()
        .context("registering the built-in logical-context chunk adapters")?;
    let config = load_config(&args)?;
    let runtime_root = match args.runtime_root.clone() {
        Some(root) => root,
        None => runtime_config_path(".env", ".env")
            .parent()
            .map(Path::to_path_buf)
            .context("the active runtime root")?,
    };
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        &runtime_root,
    );
    let router = OperationLlmRouter::new(Some(
        config
            .router_config()
            .cloned()
            .context("a configured LLM router is required")?,
    ));
    let configured = router
        .shared_configured_router()
        .context("the LLM router did not initialize")?;
    router.set_dispatch_queue(magicllm::LlmDispatchQueue::start(
        configured,
        Arc::new(magicllm::dispatch::NoopTaskStateView),
        Arc::new(magicllm::dispatch::NoopTaskLedgerSink),
        config.llm.dispatch.clone(),
    ));
    let prompt_store: Arc<dyn PromptStore> = Arc::new(
        JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: default_prompt_dir(),
            enable_cache: true,
            max_cache_entries: 50,
        })
        .context("prompt storage")?,
    );
    let dependencies = SynthesisDependencies {
        prompt_manager: Some(Arc::new(PromptManager::new(prompt_store))),
        operation_llm_router: Some(Arc::new(router)),
        event_broadcaster: None,
    };

    let run_dir = args
        .output_dir
        .join(Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
    fs::create_dir_all(&run_dir)?;
    let mut runs = Vec::new();
    for case in &cases {
        for run in 1..=args.repeat.max(1) {
            let directory = run_dir.join(format!("{}-{run}", case.id));
            println!("== {} (run {run}) {}", case.id, case.note);
            let alter_answer: Vec<(String, String)> = case
                .alter_answer
                .iter()
                .map(|alteration| (alteration.from.clone(), alteration.to.clone()))
                .collect();
            // Every claim-judge prompt and response of this replay.
            std::env::set_var(JUDGE_DUMP_DIR_ENV, directory.join("judge"));
            let result = replay_finalization(FinalizeReplayRequest {
                runtime_root: &runtime_root,
                principal: &case.principal,
                workspace: &case.workspace,
                task_id: &case.task_id,
                execution_id: &case.execution_id,
                output_dir: &directory,
                dependencies: dependencies.clone(),
                alter_answer: &alter_answer,
            })
            .await;
            let record = match result {
                Ok(result) => {
                    fs::write(
                        directory.join("execution_output.md"),
                        &result.execution_output,
                    )?;
                    fs::write(directory.join("answer.txt"), &result.user_output)?;
                    fs::write(
                        directory.join("result.json"),
                        serde_json::to_vec_pretty(&result)?,
                    )?;
                    let lowered = result.user_output.to_lowercase();
                    let missing: Vec<String> = case
                        .must_contain
                        .iter()
                        .filter(|needle| !needle.found_in(&lowered))
                        .map(Needle::describe)
                        .collect();
                    let forbidden_present: Vec<String> = case
                        .must_not_contain
                        .iter()
                        .filter(|needle| lowered.contains(&needle.to_lowercase()))
                        .cloned()
                        .collect();
                    let caveated_answer = CAVEAT_MARKERS
                        .iter()
                        .any(|marker| result.user_output.contains(marker));
                    let unflagged: Vec<String> = case
                        .must_flag
                        .iter()
                        .filter(|planted| {
                            result.user_output.contains(planted.as_str()) && !caveated_answer
                        })
                        .cloned()
                        .collect();
                    println!("{}\n", result.user_output);
                    let check = result.claim_check.as_ref();
                    CaseRun {
                        case: case.id.clone(),
                        run,
                        passed: missing.is_empty()
                            && forbidden_present.is_empty()
                            && unflagged.is_empty(),
                        missing,
                        forbidden_present,
                        unflagged,
                        error: None,
                        caveated: claim_count(check, "unconfirmed") > 0,
                        unconfirmed_claims: claim_count(check, "unconfirmed"),
                        repaired_claims: claim_count(check, "repaired"),
                        removed_claims: claim_count(check, "removed"),
                        directory: directory.display().to_string(),
                    }
                },
                Err(error) => CaseRun {
                    case: case.id.clone(),
                    run,
                    passed: false,
                    missing: Vec::new(),
                    forbidden_present: Vec::new(),
                    unflagged: Vec::new(),
                    error: Some(error.to_string()),
                    caveated: false,
                    unconfirmed_claims: 0,
                    repaired_claims: 0,
                    removed_claims: 0,
                    directory: directory.display().to_string(),
                },
            };
            println!(
                "-> {} missing={:?} forbidden={:?} unflagged={:?} repaired={} unconfirmed={} removed={} {}",
                if record.passed { "PASS" } else { "FAIL" },
                record.missing,
                record.forbidden_present,
                record.unflagged,
                record.repaired_claims,
                record.unconfirmed_claims,
                record.removed_claims,
                record.error.as_deref().unwrap_or("")
            );
            runs.push(record);
        }
    }
    let report = Report {
        schema_version: SCHEMA_VERSION,
        generated_by: GENERATED_BY,
        generated_at: Utc::now().to_rfc3339(),
        passed: runs.iter().filter(|run| run.passed).count(),
        total: runs.len(),
        runs,
    };
    fs::write(
        run_dir.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let latest = args.output_dir.join("latest");
    let _ = fs::remove_file(&latest);
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&run_dir, &latest);
    println!(
        "final-answer live eval: {}/{} passed; report {}",
        report.passed,
        report.total,
        run_dir.join("report.json").display()
    );
    if report.passed != report.total {
        std::process::exit(1);
    }
    Ok(())
}
