//! Live Task Recipes acceptance lane.
//!
//! This drives one cold browser run, then proves identical and variant task
//! shapes complete through `outcome_type=recipe_replay` without changing any
//! browser signal. It then forces answer-extractor drift in the scoped recipe,
//! verifies a browser handoff recompiles it, and proves the following run is
//! browserless again. Reports contain IDs, counters, and gate outcomes only.

use std::{
    fs,
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use clap::Parser;
use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Parser)]
#[command(about = "Prove cold -> warm -> variant -> drift Task Recipe behavior against Magician")]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:3002")]
    base_url: String,
    #[arg(long, default_value = "http://127.0.0.1:3003")]
    magicutor_base_url: String,
    #[arg(long)]
    bearer: Option<String>,
    #[arg(long, default_value = "anonymous")]
    principal: String,
    #[arg(long, default_value = "default")]
    workspace: String,
    #[arg(long)]
    runtime_root: Option<PathBuf>,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 900)]
    timeout_secs: u64,
    /// Provider-free smoke mode used by the aggregate live-suite self-test.
    #[arg(long)]
    self_test: bool,
}

struct ApiClient {
    client: Client,
    base: String,
    bearer: Option<String>,
    principal: String,
    workspace: String,
}

impl ApiClient {
    async fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut request = self
            .client
            .request(
                method,
                format!("{}{}", self.base.trim_end_matches('/'), path),
            )
            .header("X-Principal", &self.principal)
            .header("X-Workspace", &self.workspace);
        if let Some(bearer) = &self.bearer {
            request = request.bearer_auth(bearer);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("send Magician request")?;
        let status = response.status();
        let bytes = response.bytes().await.context("read Magician response")?;
        let payload: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            json!({"raw": String::from_utf8_lossy(&bytes).chars().take(512).collect::<String>()})
        });
        if !status.is_success() {
            bail!("{} returned HTTP {}: {}", path, status, payload);
        }
        Ok(payload)
    }

    async fn get(&self, path: &str) -> Result<Value> {
        self.json(Method::GET, path, None).await
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct BrowserSignals {
    browser_only_sequences: u64,
    router: Value,
    magicutor_tabs: usize,
}

#[derive(Debug, Serialize)]
struct Gate {
    id: &'static str,
    passed: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
struct LiveReport {
    schema_version: u32,
    generated_at: String,
    passed: bool,
    principal: String,
    workspace: String,
    cold_task_id: Option<String>,
    warm_task_id: Option<String>,
    variant_task_id: Option<String>,
    drift_task_id: Option<String>,
    healed_task_id: Option<String>,
    recipe_id: Option<String>,
    gates: Vec<Gate>,
    fatal_error: Option<String>,
}

#[derive(Default)]
struct RunIds {
    cold_task_id: Option<String>,
    warm_task_id: Option<String>,
    variant_task_id: Option<String>,
    drift_task_id: Option<String>,
    healed_task_id: Option<String>,
    recipe_id: Option<String>,
}

fn first_string(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| map.values().find_map(|value| first_string(value, key))),
        Value::Array(items) => items.iter().find_map(|value| first_string(value, key)),
        _ => None,
    }
}

fn first_u64(value: &Value, key: &str) -> Option<u64> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_u64)
            .or_else(|| map.values().find_map(|value| first_u64(value, key))),
        Value::Array(items) => items.iter().find_map(|value| first_u64(value, key)),
        _ => None,
    }
}

fn find_execution_status(value: &Value, execution_id: &str) -> Option<String> {
    match value {
        Value::Object(map)
            if map.get("execution_id").and_then(Value::as_str) == Some(execution_id) =>
        {
            map.get("status").and_then(Value::as_str).map(str::to_owned)
        },
        Value::Object(map) => map
            .values()
            .find_map(|value| find_execution_status(value, execution_id)),
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_execution_status(value, execution_id)),
        _ => None,
    }
}

fn metric(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

async fn browser_signals(api: &ApiClient, magicutor_base: &str) -> Result<BrowserSignals> {
    let sequence = api
        .get("/api/magician/v2/api-mining/sequence-metrics")
        .await?;
    let router = api
        .get("/api/magician/v2/api-mining/router-metrics")
        .await?;
    let response = api
        .client
        .get(format!(
            "{}/json/list",
            magicutor_base.trim_end_matches('/')
        ))
        .send()
        .await
        .context("query Magicutor tab list")?;
    if !response.status().is_success() {
        bail!("Magicutor /json/list returned {}", response.status());
    }
    let tabs: Value = response.json().await.context("decode Magicutor tab list")?;
    Ok(BrowserSignals {
        browser_only_sequences: first_u64(&sequence, "with_browser_only_steps").unwrap_or(0),
        router,
        magicutor_tabs: tabs.as_array().map_or(0, Vec::len),
    })
}

async fn create_and_execute(
    api: &ApiClient,
    title: &str,
    description: &str,
) -> Result<(String, String)> {
    let created = api
        .json(
            Method::POST,
            "/api/magician/v3/tasks",
            Some(json!({
                "title": title,
                "description": description,
                "agent_id": "personal-assistant",
                "ui_thread_id": "task-recipes-live-eval",
                "created_by": "user",
                "approved": true
            })),
        )
        .await?;
    let task_id = first_string(&created, "task_id").context("create response omitted task_id")?;
    let accepted = api
        .json(
            Method::POST,
            &format!("/api/magician/v3/tasks/{task_id}/execute"),
            Some(json!({})),
        )
        .await?;
    let execution_id =
        first_string(&accepted, "execution_id").context("execute response omitted execution_id")?;
    Ok((task_id, execution_id))
}

async fn wait_terminal(
    api: &ApiClient,
    task_id: &str,
    execution_id: &str,
    timeout: Duration,
) -> Result<Value> {
    let started = Instant::now();
    loop {
        let executions = api
            .get(&format!("/api/magician/v3/tasks/{task_id}/executions"))
            .await?;
        if let Some(status) = find_execution_status(&executions, execution_id) {
            let status = status.to_ascii_lowercase();
            if status == "completed" {
                return api
                    .get(&format!("/api/magician/v3/tasks/{task_id}/details"))
                    .await;
            }
            if matches!(status.as_str(), "failed" | "cancelled" | "canceled") {
                bail!("task {task_id} became terminal with status {status}");
            }
        }
        if started.elapsed() >= timeout {
            bail!(
                "task {task_id} did not complete within {}s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_recipe(
    api: &ApiClient,
    task_id: &str,
    timeout: Duration,
) -> Result<(String, Value)> {
    let started = Instant::now();
    loop {
        let list = api.get("/api/magician/v2/api-mining/recipes").await?;
        for row in list.as_array().into_iter().flatten() {
            let Some(id) = row.get("id").and_then(Value::as_str) else {
                continue;
            };
            let detail = api
                .get(&format!("/api/magician/v2/api-mining/recipes/{id}"))
                .await?;
            if first_string(&detail, "task_id").as_deref() == Some(task_id) {
                return Ok((id.to_owned(), detail));
            }
        }
        if started.elapsed() >= timeout {
            bail!(
                "no recipe compiled from task {task_id} within {}s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn wait_for_recipe_version(
    api: &ApiClient,
    recipe_id: &str,
    previous_versions: usize,
    timeout: Duration,
) -> Result<Value> {
    let started = Instant::now();
    loop {
        let detail = api
            .get(&format!("/api/magician/v2/api-mining/recipes/{recipe_id}"))
            .await?;
        if version_count(&detail) > previous_versions {
            return Ok(detail);
        }
        if started.elapsed() >= timeout {
            bail!(
                "recipe {recipe_id} did not advance beyond version count {previous_versions} within {}s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn version_count(recipe: &Value) -> usize {
    recipe
        .get("versions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn safe_scope_segment(value: &str) -> Result<&str> {
    if !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        Ok(value)
    } else {
        bail!("scope segment is not safe for a local eval path")
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("json.eval-{}-{nonce}", std::process::id()));
    let permissions = fs::metadata(path)?.permissions();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(&temp, permissions)?;
    fs::rename(&temp, path)?;
    Ok(())
}

fn force_drift(
    runtime_root: &Path,
    principal: &str,
    workspace: &str,
    recipe_id: &str,
) -> Result<(PathBuf, Vec<u8>)> {
    let path = runtime_root
        .join("scopes")
        .join(safe_scope_segment(principal)?)
        .join(safe_scope_segment(workspace)?)
        .join("api_mining")
        .join("recipes")
        .join(format!("{recipe_id}.json"));
    let original =
        fs::read(&path).with_context(|| format!("read recipe for drift at {}", path.display()))?;
    let mut recipe: Value = serde_json::from_slice(&original)?;
    let current = recipe
        .get("current_version")
        .and_then(Value::as_u64)
        .context("recipe omitted current_version")?;
    let versions = recipe
        .get_mut("versions")
        .and_then(Value::as_array_mut)
        .context("recipe omitted versions")?;
    let version = versions
        .iter_mut()
        .find(|version| version.get("version").and_then(Value::as_u64) == Some(current))
        .context("current recipe version not found")?;
    let answer = version
        .get_mut("answer_spec")
        .and_then(Value::as_array_mut)
        .and_then(|answers| answers.first_mut())
        .context("recipe has no answer extractor to drift")?;
    let extractor = answer
        .get_mut("extractor")
        .and_then(Value::as_object_mut)
        .context("answer extractor is not an object")?;
    if !extractor.contains_key("path") {
        bail!("live drift lane currently requires a JSON-path answer extractor");
    }
    extractor.insert(
        "path".into(),
        Value::String("$.__forced_task_recipe_drift__".into()),
    );
    atomic_write(&path, &serde_json::to_vec_pretty(&recipe)?)?;
    Ok((path, original))
}

async fn drive(
    api: &ApiClient,
    args: &Args,
    gates: &mut Vec<Gate>,
    ids: &mut RunIds,
) -> Result<()> {
    let timeout = Duration::from_secs(args.timeout_secs);
    let cold_title = "First HN story about rust";
    let cold_description = "API-mining live evaluation. Use the browser tool, not web_search or web_fetch. Open https://hn.algolia.com/?q=rust and report the title, URL, points, and comments for the first result. Do not log in.";
    let metrics_before = api
        .get("/api/magician/v2/api-mining/recipe-metrics")
        .await?;
    let (cold_task, cold_execution) = create_and_execute(api, cold_title, cold_description).await?;
    ids.cold_task_id = Some(cold_task.clone());
    let cold_details = wait_terminal(api, &cold_task, &cold_execution, timeout).await?;
    let cold_summary = first_string(&cold_details, "outcome_summary")
        .or_else(|| first_string(&cold_details, "summary"))
        .unwrap_or_default();
    let (recipe_id, recipe) = wait_for_recipe(api, &cold_task, Duration::from_secs(60)).await?;
    ids.recipe_id = Some(recipe_id.clone());
    gates.push(Gate {
        id: "cold.compiled_after_one_run",
        passed: true,
        detail: format!("task={cold_task}; versions={}", version_count(&recipe)),
    });

    let warm_before = browser_signals(api, &args.magicutor_base_url).await?;
    let (warm_task, warm_execution) = create_and_execute(api, cold_title, cold_description).await?;
    ids.warm_task_id = Some(warm_task.clone());
    let warm_details = wait_terminal(api, &warm_task, &warm_execution, timeout).await?;
    let warm_after = browser_signals(api, &args.magicutor_base_url).await?;
    let warm_outcome = first_string(&warm_details, "outcome_type").unwrap_or_default();
    let warm_summary = first_string(&warm_details, "outcome_summary")
        .or_else(|| first_string(&warm_details, "summary"))
        .unwrap_or_default();
    gates.push(Gate {
        id: "warm.outcome_recipe_replay",
        passed: warm_outcome == "recipe_replay",
        detail: warm_outcome,
    });
    gates.push(Gate {
        id: "warm.no_browser_signal_moved",
        passed: warm_before == warm_after,
        detail: format!("before={warm_before:?}; after={warm_after:?}"),
    });
    gates.push(Gate {
        id: "warm.answer_preserved",
        passed: !warm_summary.is_empty()
            && (cold_summary.is_empty()
                || warm_summary.contains(cold_summary.lines().next().unwrap_or_default())),
        detail: format!(
            "cold_bytes={}; warm_bytes={}",
            cold_summary.len(),
            warm_summary.len()
        ),
    });

    let variant_before = browser_signals(api, &args.magicutor_base_url).await?;
    let (variant_task, variant_execution) = create_and_execute(
        api,
        "First HN story about golang",
        &cold_description.replace("rust", "golang"),
    )
    .await?;
    ids.variant_task_id = Some(variant_task.clone());
    let variant_details = wait_terminal(api, &variant_task, &variant_execution, timeout).await?;
    let variant_after = browser_signals(api, &args.magicutor_base_url).await?;
    let variant_outcome = first_string(&variant_details, "outcome_type").unwrap_or_default();
    let variant_summary = first_string(&variant_details, "outcome_summary")
        .or_else(|| first_string(&variant_details, "summary"))
        .unwrap_or_default();
    gates.push(Gate {
        id: "variant.outcome_recipe_replay",
        passed: variant_outcome == "recipe_replay",
        detail: variant_outcome,
    });
    gates.push(Gate {
        id: "variant.no_browser_signal_moved",
        passed: variant_before == variant_after,
        detail: format!("before={variant_before:?}; after={variant_after:?}"),
    });
    gates.push(Gate {
        id: "variant.input_changed_answer",
        passed: !variant_summary.is_empty() && variant_summary != warm_summary,
        detail: format!(
            "warm_bytes={}; variant_bytes={}",
            warm_summary.len(),
            variant_summary.len()
        ),
    });

    let runtime_root = args
        .runtime_root
        .clone()
        .or_else(|| std::env::var_os("MAGICIAN_ROOT_DIR").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|home| home.join("MagicianNotes")))
        .context("could not resolve runtime root for drift lane")?;
    let before_versions = version_count(&recipe);
    let (recipe_path, original) =
        force_drift(&runtime_root, &args.principal, &args.workspace, &recipe_id)?;
    let drift_run = async {
        let (task, execution) = create_and_execute(api, cold_title, cold_description).await?;
        ids.drift_task_id = Some(task.clone());
        let details = wait_terminal(api, &task, &execution, timeout).await?;
        let outcome = first_string(&details, "outcome_type").unwrap_or_default();
        let healed_recipe =
            wait_for_recipe_version(api, &recipe_id, before_versions, Duration::from_secs(60))
                .await?;
        Ok::<_, anyhow::Error>((outcome, healed_recipe))
    }
    .await;
    let (drift_outcome, healed_recipe) = match drift_run {
        Ok(value) => value,
        Err(error) => {
            atomic_write(&recipe_path, &original)
                .context("restore recipe after failed drift lane")?;
            return Err(error);
        },
    };
    let after_versions = version_count(&healed_recipe);
    gates.push(Gate {
        id: "drift.browser_handoff_completed",
        passed: drift_outcome != "recipe_replay",
        detail: drift_outcome,
    });
    gates.push(Gate {
        id: "drift.recompiled_new_version",
        passed: after_versions > before_versions,
        detail: format!("before={before_versions}; after={after_versions}"),
    });

    let healed_before = browser_signals(api, &args.magicutor_base_url).await?;
    let (healed_task, healed_execution) =
        create_and_execute(api, cold_title, cold_description).await?;
    ids.healed_task_id = Some(healed_task.clone());
    let healed_details = wait_terminal(api, &healed_task, &healed_execution, timeout).await?;
    let healed_after = browser_signals(api, &args.magicutor_base_url).await?;
    let healed_outcome = first_string(&healed_details, "outcome_type").unwrap_or_default();
    gates.push(Gate {
        id: "drift.next_run_recipe_replay",
        passed: healed_outcome == "recipe_replay",
        detail: healed_outcome,
    });
    gates.push(Gate {
        id: "drift.next_run_no_browser",
        passed: healed_before == healed_after,
        detail: format!("before={healed_before:?}; after={healed_after:?}"),
    });

    let metrics_after = api
        .get("/api/magician/v2/api-mining/recipe-metrics")
        .await?;
    let lookup_delta =
        metric(&metrics_after, "lookup_hit").saturating_sub(metric(&metrics_before, "lookup_hit"));
    let replay_delta = metric(&metrics_after, "replay_succeeded")
        .saturating_sub(metric(&metrics_before, "replay_succeeded"));
    let fallback_delta = metric(&metrics_after, "fallback_handoffs")
        .saturating_sub(metric(&metrics_before, "fallback_handoffs"));
    gates.push(Gate {
        id: "metrics.lookup_hits",
        passed: lookup_delta >= 4,
        detail: format!("delta={lookup_delta}"),
    });
    gates.push(Gate {
        id: "metrics.replay_successes",
        passed: replay_delta >= 3,
        detail: format!("delta={replay_delta}"),
    });
    gates.push(Gate {
        id: "metrics.fallback_handoff",
        passed: fallback_delta >= 1,
        detail: format!("delta={fallback_delta}"),
    });

    let runs = api
        .get(&format!(
            "/api/magician/v2/api-mining/recipes/{recipe_id}/runs?limit=10"
        ))
        .await?;
    let run_text = runs.to_string();
    gates.push(Gate {
        id: "ledger.records_both_rails",
        passed: run_text.contains("\"api\"") && run_text.contains("api_then_browser"),
        detail: format!("bytes={}", run_text.len()),
    });
    Ok(())
}

fn render_html(report: &LiveReport) -> String {
    let rows = report
        .gates
        .iter()
        .map(|gate| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                gate.id,
                if gate.passed { "pass" } else { "FAIL" },
                gate.detail
            )
        })
        .collect::<String>();
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Task Recipes live eval</title><style>body{{font:14px system-ui;max-width:1100px;margin:40px auto}}table{{width:100%;border-collapse:collapse}}td,th{{padding:8px;border-bottom:1px solid #ddd;text-align:left}}</style></head><body><h1>Task Recipes live eval</h1><p>Passed: <strong>{}</strong></p><table><thead><tr><th>Gate</th><th>State</th><th>Detail</th></tr></thead><tbody>{}</tbody></table></body></html>"#,
        report.passed, rows
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.self_test {
        let sample = json!({"task":{"manifest":{"task_id":"task-self"}},"execution":{"execution_id":"exec-self","status":"completed"}});
        if first_string(&sample, "task_id").as_deref() != Some("task-self")
            || find_execution_status(&sample, "exec-self").as_deref() != Some("completed")
        {
            bail!("live evaluator envelope self-test failed");
        }
        return Ok(());
    }
    let bearer = args
        .bearer
        .clone()
        .or_else(|| std::env::var("MAGICIAN_BEARER").ok())
        .or_else(|| std::env::var("MAGICIAN_BEARER_TOKEN").ok());
    let api = ApiClient {
        client: Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?,
        base: args.base_url.clone(),
        bearer,
        principal: args.principal.clone(),
        workspace: args.workspace.clone(),
    };
    let mut gates = Vec::new();
    let mut ids = RunIds::default();
    let result = drive(&api, &args, &mut gates, &mut ids).await;
    let passed = result.is_ok() && gates.iter().all(|gate| gate.passed);
    let report = LiveReport {
        schema_version: 1,
        generated_at: chrono::Utc::now().to_rfc3339(),
        passed,
        principal: args.principal,
        workspace: args.workspace,
        cold_task_id: ids.cold_task_id,
        warm_task_id: ids.warm_task_id,
        variant_task_id: ids.variant_task_id,
        drift_task_id: ids.drift_task_id,
        healed_task_id: ids.healed_task_id,
        recipe_id: ids.recipe_id,
        fatal_error: result.as_ref().err().map(ToString::to_string),
        gates,
    };
    fs::create_dir_all(&args.output)?;
    fs::write(
        args.output.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    fs::write(args.output.join("latest.html"), render_html(&report))?;
    if let Err(error) = result {
        return Err(error);
    }
    if !report.passed {
        bail!("one or more Task Recipes live gates failed");
    }
    Ok(())
}
