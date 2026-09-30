//! Task Recipes fixture eval: local fixture sites + the real LLM browser agent
//! for the cold run, then browserless warm / variant / heal / write / drift
//! replay with fixture-side proof. Design:
//! `docs/plans/2026-09-11-task-recipes-fixture-eval-design.md`.

mod cases;
mod client;
mod driver;
mod evidence;
mod live;
mod report;
mod sites;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde_json::{json, Value};

use client::Magician;
use report::Report;
use sites::{board, catalog, notes, portal, RunningSite};

#[derive(Debug, Parser)]
#[command(about = "Prove browserless Task Recipe replay against local fixture sites")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:3002")]
    base_url: String,
    #[arg(long, default_value = "http://127.0.0.1:3003")]
    magicutor_base_url: String,
    /// Bearer bound to the eval workspace; else MAGICIAN_BEARER_TOKEN, else
    /// MAGICIAN_EVAL_USERNAME / MAGICIAN_EVAL_PASSWORD login.
    #[arg(long)]
    bearer: Option<String>,
    /// Workspace for this run. Default: a fresh `recipes-eval-<id>` per run,
    /// because agent memory persists per workspace and a second run in the
    /// same one lets the agent answer from memory without touching the site.
    /// A named workspace is never purged at the end of the run.
    #[arg(long)]
    workspace_slug: Option<String>,
    /// Kept for compatibility. Earlier harness workspaces are now always
    /// purged at the start of a run (`DELETE /workspaces/{id}?purge=true`,
    /// which removes the directory at the server's next start), and a passed
    /// run purges its own. `scripts/purge_task_recipes_eval_workspaces.sh`
    /// remains for sweeping while Magician is stopped.
    #[arg(long, hide = true)]
    purge_previous_workspaces: bool,
    #[arg(long)]
    runtime_root: Option<PathBuf>,
    /// The running Magician's log (the supervisor tees it here).
    #[arg(long, default_value = "magician.log")]
    magician_log: PathBuf,
    /// The Magician binary used for the per-scope operator step
    /// `seal-stateless-loop-cutover` (the stateless driver refuses an unsealed
    /// scope; a fresh workspace is unsealed until an operator — here, the
    /// harness — seals it once).
    #[arg(long, default_value = "magician.bin")]
    magician_bin: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 600)]
    timeout_secs: u64,
    /// Comma-separated case ids (c1..c6); default all.
    #[arg(long)]
    cases: Option<String>,
    /// Keep the scope's learned data after the run (and skip the purge before
    /// it when the previous report passed).
    #[arg(long)]
    keep_scope: bool,
    /// Provider-free smoke: start the sites, exercise them over HTTP, write a
    /// report, exit 0. Used by the aggregate live-suite self-test.
    #[arg(long)]
    self_test: bool,
}

fn start_sites() -> Result<HashMap<&'static str, RunningSite>> {
    let running = sites::serve_all(vec![
        ("catalog", json!({}), catalog::configure as sites::Configure),
        ("portal", json!({}), portal::configure as sites::Configure),
        (
            "notes",
            notes::initial_data(),
            notes::configure as sites::Configure,
        ),
        ("board", json!({}), board::configure as sites::Configure),
    ])?;
    Ok(running.into_iter().map(|site| (site.name, site)).collect())
}

fn site_origins(sites: &HashMap<&'static str, RunningSite>) -> BTreeMap<String, String> {
    sites
        .iter()
        .map(|(name, site)| ((*name).to_owned(), site.origin.clone()))
        .collect()
}

fn resolve_runtime_root(args: &Args) -> Result<PathBuf> {
    args.runtime_root
        .clone()
        .or_else(|| std::env::var_os("MAGICIAN_ROOT_DIR").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join("MagicianNotes")))
        .context(
            "could not resolve the runtime root (pass --runtime-root or set MAGICIAN_ROOT_DIR)",
        )
}

fn selected_cases(args: &Args) -> Result<Vec<String>> {
    let Some(list) = &args.cases else {
        return Ok(cases::ALL_CASES
            .iter()
            .map(|case| (*case).to_owned())
            .collect());
    };
    let mut selected = Vec::new();
    for raw in list.split(',') {
        let case = raw.trim().to_ascii_lowercase();
        if case.is_empty() {
            continue;
        }
        if cases::site_for(&case).is_none() && !live::is_live_case(&case) {
            bail!(
                "unknown case `{case}` (fixture: {}; public sites: {})",
                cases::ALL_CASES.join(", "),
                live::ALL_CASES.join(", ")
            );
        }
        selected.push(case);
    }
    if selected.is_empty() {
        bail!("--cases selected nothing");
    }
    Ok(selected)
}

/// Exercise every site over real HTTP and check the request log classifies
/// what it saw. No Magician involved.
async fn self_test(sites: &HashMap<&'static str, RunningSite>) -> Result<()> {
    // This crate's reqwest has no cookie store; carry the fixture session by hand.
    fn sid_cookie(response: &reqwest::Response) -> String {
        response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or_default()
            .to_owned()
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let catalog = &sites["catalog"];
    let search: Value = client
        .get(format!("{}/api/search?q=rust", catalog.origin))
        .header("X-Page-Nonce", "self-test")
        .send()
        .await?
        .json()
        .await?;
    if search["hits"][0]["id"] != 7 {
        bail!("catalog search did not rank the rust anchor first: {search}");
    }
    let about = client
        .get(format!("{}/about", catalog.origin))
        .send()
        .await?
        .text()
        .await?;
    if !about.contains("1987") {
        bail!("catalog /about lost its founding year");
    }
    client
        .get(format!("{}/px.gif?e=view", catalog.origin))
        .send()
        .await?;

    let portal = &sites["portal"];
    let login = client
        .post(format!("{}/api/session", portal.origin))
        .json(&json!({"username": "eval", "password": "eval-pass"}))
        .send()
        .await?;
    if !login.status().is_success() {
        bail!("portal login refused the fixture credentials");
    }
    let portal_cookie = sid_cookie(&login);
    let orders: Value = client
        .get(format!("{}/api/me/orders", portal.origin))
        .header("Cookie", &portal_cookie)
        .send()
        .await?
        .json()
        .await?;
    if orders["orders"][0]["id"] != "ORD-8841" {
        bail!("portal orders did not return the anchor order: {orders}");
    }
    client
        .post(format!("{}/__eval/knobs", portal.origin))
        .json(&json!({"session_ttl_secs": 0}))
        .send()
        .await?;
    let expired = client
        .get(format!("{}/api/me/orders", portal.origin))
        .header("Cookie", &portal_cookie)
        .send()
        .await?;
    if expired.status() != 401 {
        bail!("portal did not expire the session under the knob");
    }
    client
        .post(format!("{}/__eval/knobs", portal.origin))
        .json(&json!({"session_ttl_secs": null}))
        .send()
        .await?;

    let notes = &sites["notes"];
    let login = client
        .post(format!("{}/api/session", notes.origin))
        .json(&json!({"username": "eval", "password": "eval-pass"}))
        .send()
        .await?;
    let notes_cookie = sid_cookie(&login);
    let login: Value = login.json().await?;
    let csrf = login["csrf_token"].as_str().unwrap_or_default().to_owned();
    let created = client
        .post(format!("{}/api/notes", notes.origin))
        .header("Cookie", &notes_cookie)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({"text": "self test"}))
        .send()
        .await?;
    if created.status() != 201 {
        bail!(
            "notes POST with a valid CSRF token was refused: {}",
            created.status()
        );
    }
    let refused = client
        .post(format!("{}/api/notes", notes.origin))
        .header("Cookie", &notes_cookie)
        .json(&json!({"text": "no csrf"}))
        .send()
        .await?;
    if refused.status() != 403 {
        bail!("notes POST without CSRF was accepted");
    }

    let board = &sites["board"];
    let graphql: Value = client
        .post(format!("{}/graphql", board.origin))
        .json(&json!({"query": "{ board(id: \"alpha\") { title score } }", "variables": {}}))
        .send()
        .await?
        .json()
        .await?;
    if graphql["data"]["board"]["score"] != 7331 {
        bail!("board graphql lost the alpha score: {graphql}");
    }

    let log: Value = client
        .get(format!("{}/__eval/requests", catalog.origin))
        .send()
        .await?
        .json()
        .await?;
    let kinds: Vec<&str> = log["requests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|record| record["kind"].as_str())
        .collect();
    if kinds != ["api", "document", "beacon"] {
        bail!("catalog request log misclassified the self-test traffic: {kinds:?}");
    }
    if log["requests"][0]["page_nonce"] != "self-test" {
        bail!("catalog request log lost the page nonce");
    }
    Ok(())
}

struct Preflight {
    magician_version: String,
}

/// Install the skill set into the eval scope. This is the symlink layer of
/// the operator target `make -C skillshub install-scope` without its `build`
/// prerequisite: that step cargo-builds skill CLIs behind the shared build
/// lock (tens of minutes on a busy machine) and the bins are already built
/// for the default scope. A fresh workspace has no packs, and the browser
/// decision path requires the `browser` pack.
async fn install_scope_skills(
    runtime_root: &std::path::Path,
    principal: &str,
    workspace: &str,
) -> Result<bool> {
    let skills = runtime_root
        .join("scopes")
        .join(principal)
        .join(workspace)
        .join("skills");
    if skills.join("browser").exists() {
        return Ok(false);
    }
    let output = tokio::process::Command::new("python3")
        .args([
            "skillshub/scripts/install_skill_layer.py",
            "--layer",
            "all",
            "--dest",
            &skills.display().to_string(),
        ])
        .output()
        .await
        .context("run skillshub/scripts/install_skill_layer.py")?;
    if !output.status.success() {
        bail!(
            "install_skill_layer.py failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or_default()
        );
    }
    if !skills.join("browser").exists() {
        bail!(
            "install_skill_layer.py ran but {} still has no `browser` pack",
            skills.display()
        );
    }
    Ok(true)
}

/// Seal the eval scope's legacy-writer cutover with the documented operator
/// CLI. Idempotent: a sealed scope reports `already_sealed: true`.
async fn seal_scope_cutover(
    bin: &std::path::Path,
    principal: &str,
    workspace: &str,
) -> Result<bool> {
    // A bare file name would resolve through PATH; pin it to the cwd.
    let bin = std::fs::canonicalize(bin)
        .with_context(|| format!("magician binary not found at {}", bin.display()))?;
    let output = tokio::process::Command::new(&bin)
        .args([
            "seal-stateless-loop-cutover",
            "--principal",
            principal,
            "--workspace",
            workspace,
            "--deployment-id",
            "task-recipes-fixture-eval",
            "--confirm-legacy-writers-drained",
        ])
        .output()
        .await
        .with_context(|| format!("run {} seal-stateless-loop-cutover", bin.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sealed = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|row| row.get("legacy_writers_retired").and_then(Value::as_bool) == Some(true));
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A seal published under another deployment id is still a seal.
    if !output.status.success() && stderr.contains("already retired by deployment") {
        return Ok(true);
    }
    match sealed {
        Some(row) if output.status.success() => Ok(row.get("already_sealed").and_then(Value::as_bool).unwrap_or(false)),
        _ => bail!(
            "seal-stateless-loop-cutover for {principal}/{workspace} did not publish a seal (exit {}): {}",
            output.status,
            stderr.lines().last().unwrap_or_default()
        ),
    }
}

async fn preflight(
    args: &Args,
    magician: &Magician,
    runtime_root: &std::path::Path,
) -> Result<Preflight> {
    let health = magician.health().await?;
    let status = health
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "ok" && status != "healthy" {
        bail!("magician /health is not ok: {health}");
    }
    magician
        .magicutor_ready(&args.magicutor_base_url)
        .await
        .context("magicutor is not serving CDP discovery (is the extension bridge up?)")?;
    let settings = magician.api_mining_settings().await?;
    let effective = settings
        .get("effective")
        .and_then(Value::as_bool)
        .or_else(|| settings.get("enabled").and_then(Value::as_bool))
        .unwrap_or(false);
    if !effective {
        magician
            .set_api_mining(true)
            .await
            .context("enable api mining for the eval scope")?;
    }
    let recipes_enabled = settings
        .get("recipes")
        .and_then(|recipes| recipes.get("enabled"))
        .and_then(Value::as_bool)
        .or_else(|| settings.get("recipes_enabled").and_then(Value::as_bool))
        .unwrap_or(true);
    if !recipes_enabled {
        bail!("api_mining.recipes.enabled is off in the runtime config; enable it and restart");
    }
    if install_scope_skills(runtime_root, &magician.principal, &magician.workspace).await? {
        eprintln!(
            "task_recipes_fixture_eval: installed skills into {}/{}",
            magician.principal, magician.workspace
        );
    }
    let already_sealed =
        seal_scope_cutover(&args.magician_bin, &magician.principal, &magician.workspace).await?;
    if !already_sealed {
        eprintln!(
            "task_recipes_fixture_eval: sealed the stateless-loop cutover for {}/{}",
            magician.principal, magician.workspace
        );
    }
    Ok(Preflight {
        magician_version: health
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
    })
}

const WORKSPACE_PREFIX: &str = "recipes-eval";
const HARNESS_MARKER: &str = ".task-recipes-fixture-eval";

fn fresh_workspace_slug() -> String {
    let id = ulid::Ulid::new().to_string().to_ascii_lowercase();
    format!("{WORKSPACE_PREFIX}-{}", &id[id.len() - 8..])
}

/// Mark the workspace as harness-created so a later purge only ever touches
/// directories this eval made.
fn mark_workspace(runtime_root: &std::path::Path, principal: &str, workspace: &str) -> Result<()> {
    let dir = runtime_root.join("scopes").join(principal).join(workspace);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(HARNESS_MARKER),
        b"created by task_recipes_fixture_eval\n",
    )?;
    Ok(())
}

/// Is this one of the harness's own workspaces? The bare prefix counts too —
/// an explicit `--workspace-slug recipes-eval` — which a `recipes-eval-` prefix
/// test never matched, so that one survived every cleanup.
fn is_harness_workspace(workspace: &str) -> bool {
    workspace == WORKSPACE_PREFIX || workspace.starts_with(&format!("{WORKSPACE_PREFIX}-"))
}

/// Purge earlier harness workspaces, keeping at most the current one.
///
/// Every run mints a fresh workspace (agent memory is per workspace), so
/// without this they accumulate. Only a workspace whose directory is already
/// gone or carries the harness marker is touched: one an operator named and
/// populated by hand is left alone. The purge is `?purge=true`, which removes
/// the registry row now and the directory at the server's next start, so it is
/// safe while the service is running.
async fn purge_previous_workspaces(
    magician: &Magician,
    runtime_root: &std::path::Path,
    current: &str,
) -> Result<Vec<String>> {
    let principal_dir = runtime_root.join("scopes").join(&magician.principal);
    let mut purged = Vec::new();
    for workspace in magician.list_workspaces().await? {
        if workspace == current || !is_harness_workspace(&workspace) {
            continue;
        }
        let dir = principal_dir.join(&workspace);
        if dir.exists() && !dir.join(HARNESS_MARKER).exists() {
            eprintln!("task_recipes_fixture_eval: kept {workspace} (no harness marker)");
            continue;
        }
        match magician.purge_workspace(&workspace).await {
            Ok(()) => purged.push(workspace),
            Err(error) => eprintln!("task_recipes_fixture_eval: {workspace} not purged: {error:#}"),
        }
    }
    Ok(purged)
}

fn previous_run_passed(output: &std::path::Path) -> bool {
    std::fs::read(output.join("report.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|report| report.get("passed").and_then(Value::as_bool))
        .unwrap_or(false)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let sites = start_sites()?;
    let workspace_slug = args
        .workspace_slug
        .clone()
        .unwrap_or_else(fresh_workspace_slug);
    let mut report = Report {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        passed: false,
        principal: String::new(),
        workspace: workspace_slug.clone(),
        magician_version: String::new(),
        sites: site_origins(&sites),
        cases: Vec::new(),
        fatal_error: None,
    };

    if args.self_test {
        let result = self_test(&sites).await;
        for site in sites.values() {
            site.stop().await;
        }
        report.fatal_error = result.as_ref().err().map(|error| format!("{error:#}"));
        report.passed = result.is_ok();
        report::write(&report, &args.output)?;
        return result;
    }

    let outcome: Result<()> = async {
        let selected = selected_cases(&args)?;
        let runtime_root = resolve_runtime_root(&args)?;
        let magician = Magician::connect(
            &args.base_url,
            &workspace_slug,
            args.bearer.clone(),
            &runtime_root,
        )
        .await?;
        report.principal = magician.principal.clone();
        report.workspace = magician.workspace.clone();
        mark_workspace(&runtime_root, &magician.principal, &magician.workspace)?;
        let purged =
            purge_previous_workspaces(&magician, &runtime_root, &magician.workspace).await?;
        if !purged.is_empty() {
            eprintln!(
                "task_recipes_fixture_eval: purged earlier eval workspaces {} (data removed at the next server start)",
                purged.join(", ")
            );
        }
        let checks = preflight(&args, &magician, &runtime_root).await?;
        report.magician_version = checks.magician_version;
        if !(args.keep_scope && previous_run_passed(&args.output)) {
            magician
                .purge_api_mining()
                .await
                .context("purge the eval scope before the run")?;
        }
        eprintln!(
            "task_recipes_fixture_eval: scope {}/{} · magician {} · cases {}",
            magician.principal,
            magician.workspace,
            report.magician_version,
            selected.join(",")
        );
        for (name, site) in &sites {
            eprintln!("  site {name} = {}", site.origin);
        }
        let ctx = cases::CaseCtx {
            magician: &magician,
            magicutor_base: &args.magicutor_base_url,
            sites: &sites,
            runtime_root: &runtime_root,
            log_path: &args.magician_log,
            timeout: Duration::from_secs(args.timeout_secs),
        };
        let live_ctx = live::LiveCtx {
            magician: &magician,
            magicutor_base: &args.magicutor_base_url,
            runtime_root: &runtime_root,
            log_path: &args.magician_log,
            timeout: Duration::from_secs(args.timeout_secs),
            http: live::http_client()?,
        };
        for case in &selected {
            eprintln!("▶ case {case}");
            let result = if live::is_live_case(case) {
                live::run_case(case, &live_ctx).await
            } else {
                cases::run_case(case, &ctx).await
            };
            // The verdict is computed here rather than at each return. A case
            // that forgets carries the default verdict instead of its gates',
            // and the live cases have a dozen early exits: a run whose 41 gates
            // were all green still reported failure, because no live case ever
            // called this. Computing it once, for both families, is the only
            // shape that cannot silently disagree with the gates.
            let result = result.finish();
            for phase in &result.phases {
                let failed: Vec<&str> = phase
                    .gates
                    .iter()
                    .filter(|gate| !gate.passed)
                    .map(|gate| gate.id.as_str())
                    .collect();
                eprintln!(
                    "   {} {:<22} {} · {} · {}",
                    if phase.passed() { "pass" } else { "FAIL" },
                    phase.id,
                    phase.outcome_type,
                    phase.summary_excerpt.chars().take(80).collect::<String>(),
                    if failed.is_empty() {
                        String::new()
                    } else {
                        format!("failed: {}", failed.join(", "))
                    }
                );
                if let Some(error) = &phase.error {
                    eprintln!("   error: {error}");
                }
            }
            report.cases.push(result);
            report::write(&report.clone().finalize(), &args.output)?;
        }
        // A failed run keeps its learned data on disk for diagnosis; the next
        // run purges before it starts.
        let all_passed = !report.cases.is_empty() && report.cases.iter().all(|case| case.passed);
        if !args.keep_scope && all_passed {
            magician
                .purge_api_mining()
                .await
                .context("purge the eval scope after the run")?;
            // A workspace this run minted has no further use once it passed.
            // One the operator named is theirs to keep.
            if args.workspace_slug.is_none() {
                magician
                    .purge_workspace(&magician.workspace)
                    .await
                    .context("purge the eval workspace after the run")?;
            }
        }
        Ok(())
    }
    .await;

    for site in sites.values() {
        site.stop().await;
    }
    report.fatal_error = outcome.as_ref().err().map(|error| format!("{error:#}"));
    let report = report.finalize();
    report::write(&report, &args.output)?;
    eprintln!(
        "task_recipes_fixture_eval: {} · report {}",
        if report.passed { "PASSED" } else { "FAILED" },
        args.output.join("report.json").display()
    );
    if let Err(error) = outcome {
        return Err(error);
    }
    if !report.passed {
        bail!("one or more Task Recipes fixture gates failed");
    }
    Ok(())
}
