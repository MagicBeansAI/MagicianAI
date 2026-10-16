//! HTTP client for the running Magician + Magicutor: auth, scoped tasks,
//! recipes, replay approvals, metrics, and the api-mining switch.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use serde_json::{json, Value};

pub const API_V2: &str = "/api/magician/v2";
pub const API_V3: &str = "/api/magician/v3";
pub const REPLAY_APPROVAL_REQUEST_TYPE: &str = "api_replay_approval";

/// The fixture login every session/write case uses. The task text states both,
/// so a well-behaved agent types them itself; when one instead escalates for
/// the password (agent variance, no human to answer), the harness supplies it
/// exactly as the real user would, so a stray pause cannot hang the run.
pub const FIXTURE_USERNAME: &str = "eval";
pub const FIXTURE_PASSWORD: &str = "eval-pass";

pub struct Magician {
    client: Client,
    base: String,
    bearer: String,
    pub principal: String,
    pub workspace: String,
    runtime_root: std::path::PathBuf,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BrowserSignals {
    pub browser_only_sequences: u64,
    pub router: Value,
    pub magicutor_tabs: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalOutcome {
    pub status: String,
    pub outcome_type: String,
    pub outcome_summary: String,
    pub details: Value,
}

// ---------------------------------------------------------------------------
// JSON helpers (pure; unit-tested)
// ---------------------------------------------------------------------------

pub fn first_string(value: &Value, key: &str) -> Option<String> {
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

pub fn first_u64(value: &Value, key: &str) -> Option<u64> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(Value::as_u64)
            .or_else(|| map.values().find_map(|value| first_u64(value, key))),
        Value::Array(items) => items.iter().find_map(|value| first_u64(value, key)),
        _ => None,
    }
}

/// Find the object that carries `execution_id == wanted` and return its
/// `status`.
pub fn find_execution_status(value: &Value, wanted: &str) -> Option<String> {
    match value {
        Value::Object(map) if map.get("execution_id").and_then(Value::as_str) == Some(wanted) => {
            map.get("status").and_then(Value::as_str).map(str::to_owned)
        },
        Value::Object(map) => map
            .values()
            .find_map(|value| find_execution_status(value, wanted)),
        Value::Array(items) => items
            .iter()
            .find_map(|value| find_execution_status(value, wanted)),
        _ => None,
    }
}

/// The outcome snapshot for a specific execution inside a task-details
/// document: prefer an object that names this execution id and carries an
/// `outcome_type`; fall back to the first `outcome_type` anywhere.
pub fn outcome_for_execution(details: &Value, execution_id: &str) -> (String, String) {
    fn walk(value: &Value, wanted: &str, out: &mut Option<(String, String)>) {
        if out.is_some() {
            return;
        }
        match value {
            Value::Object(map) => {
                let names_execution =
                    map.get("execution_id").and_then(Value::as_str) == Some(wanted);
                if names_execution {
                    if let Some(outcome_type) = map.get("outcome_type").and_then(Value::as_str) {
                        let summary = map
                            .get("outcome_summary")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or_else(|| first_string(value, "outcome_summary"))
                            .unwrap_or_default();
                        *out = Some((outcome_type.to_owned(), summary));
                        return;
                    }
                    if let Some(outcome) = map.get("outcome").filter(|outcome| outcome.is_object())
                    {
                        if let Some(outcome_type) =
                            outcome.get("outcome_type").and_then(Value::as_str)
                        {
                            let summary = outcome
                                .get("outcome_summary")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned();
                            *out = Some((outcome_type.to_owned(), summary));
                            return;
                        }
                    }
                }
                for child in map.values() {
                    walk(child, wanted, out);
                }
            },
            Value::Array(items) => {
                for item in items {
                    walk(item, wanted, out);
                }
            },
            _ => {},
        }
    }
    let mut found = None;
    walk(details, execution_id, &mut found);
    found.unwrap_or_else(|| {
        (
            first_string(details, "outcome_type").unwrap_or_default(),
            first_string(details, "outcome_summary")
                .or_else(|| first_string(details, "summary"))
                .unwrap_or_default(),
        )
    })
}

/// A HITL event's pause key — top-level or under `payload`, `correlation_id`
/// or the request `id`.
fn event_correlation(event: &Value) -> Option<&str> {
    for key in ["correlation_id", "id"] {
        if let Some(value) = event.get(key).and_then(Value::as_str) {
            return Some(value);
        }
        if let Some(value) = event
            .get("payload")
            .and_then(|payload| payload.get(key))
            .and_then(Value::as_str)
        {
            return Some(value);
        }
    }
    None
}

pub fn pending_replay_approvals(list: &Value) -> Vec<Value> {
    list.get("requests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|request| {
            request.get("request_type").and_then(Value::as_str)
                == Some(REPLAY_APPROVAL_REQUEST_TYPE)
        })
        .cloned()
        .collect()
}

pub fn version_count(recipe: &Value) -> usize {
    recipe
        .get("versions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

impl Magician {
    /// `MAGICIAN_BEARER_TOKEN` wins when set; otherwise log in with
    /// `MAGICIAN_EVAL_USERNAME` / `MAGICIAN_EVAL_PASSWORD`, ensure the eval
    /// workspace exists, and rotate the session onto it.
    pub async fn connect(
        base: &str,
        workspace_slug: &str,
        bearer_override: Option<String>,
        runtime_root: &std::path::Path,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let base = base.trim_end_matches('/').to_owned();
        let bearer = match bearer_override.or_else(|| std::env::var("MAGICIAN_BEARER_TOKEN").ok()) {
            Some(token) if !token.trim().is_empty() => token.trim().to_owned(),
            _ => {
                let username = eval_credential("MAGICIAN_EVAL_USERNAME", runtime_root);
                let password = eval_credential("MAGICIAN_EVAL_PASSWORD", runtime_root);
                let (Some(username), Some(password)) = (username, password) else {
                    bail!(
                        "no credentials: set MAGICIAN_BEARER_TOKEN (a token bound to workspace `{workspace_slug}`), \
                         or MAGICIAN_EVAL_USERNAME + MAGICIAN_EVAL_PASSWORD in the environment or in \
                         $MAGICIAN_ROOT_DIR/.env.development (or .env)"
                    );
                };
                login_and_scope(&client, &base, &username, &password, workspace_slug).await?
            },
        };
        let mut magician = Self {
            client,
            base,
            bearer,
            principal: String::new(),
            workspace: String::new(),
            runtime_root: runtime_root.to_path_buf(),
        };
        let session = magician.session().await.context("GET /auth/session")?;
        magician.principal = first_string(&session, "principal").unwrap_or_default();
        magician.workspace = first_string(&session, "workspace").unwrap_or_default();
        if magician.workspace != workspace_slug {
            bail!(
                "bearer is bound to workspace `{}`, expected `{workspace_slug}`",
                magician.workspace
            );
        }
        Ok(magician)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.bearer);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("send {path}"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("read {path}"))?;
        let payload: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            json!({"raw": String::from_utf8_lossy(&bytes).chars().take(512).collect::<String>()})
        });
        Ok((status, payload))
    }

    async fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, payload) = self.request(method, path, body).await?;
        if !status.is_success() {
            bail!("{path} returned HTTP {status}: {payload}");
        }
        Ok(payload)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.json(Method::GET, path, None).await
    }

    pub async fn session(&self) -> Result<Value> {
        self.get(&format!("{API_V2}/auth/session")).await
    }

    pub async fn health(&self) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}/health", self.base))
            .send()
            .await
            .context("GET /health")?;
        Ok(response.json().await.unwrap_or(Value::Null))
    }

    pub async fn api_mining_settings(&self) -> Result<Value> {
        self.get(&format!("{API_V2}/api-mining/settings")).await
    }

    pub async fn set_api_mining(&self, enabled: bool) -> Result<Value> {
        self.json(
            Method::PUT,
            &format!("{API_V2}/api-mining/settings"),
            Some(json!({"enabled": enabled, "layer": "scope"})),
        )
        .await
    }

    /// Wipe the scope's learned data, then switch mining back on.
    pub async fn purge_api_mining(&self) -> Result<Value> {
        let purge = self
            .json(
                Method::POST,
                &format!("{API_V2}/api-mining/settings/disable-and-purge"),
                Some(json!({"confirm": "delete learned data"})),
            )
            .await?;
        self.set_api_mining(true).await?;
        Ok(purge)
    }

    pub async fn create_and_execute(
        &self,
        title: &str,
        description: &str,
        ui_thread_id: &str,
    ) -> Result<(String, String)> {
        let created = self
            .json(
                Method::POST,
                &format!("{API_V3}/tasks"),
                Some(json!({
                    "title": title,
                    "description": description,
                    "agent_id": "personal-assistant",
                    "ui_thread_id": ui_thread_id,
                    "created_by": "user",
                    "approved": true
                })),
            )
            .await?;
        let task_id =
            first_string(&created, "task_id").context("create response omitted task_id")?;
        let accepted = self
            .json(
                Method::POST,
                &format!("{API_V3}/tasks/{task_id}/execute"),
                Some(json!({})),
            )
            .await?;
        let execution_id = first_string(&accepted, "execution_id")
            .context("execute response omitted execution_id")?;
        Ok((task_id, execution_id))
    }

    pub async fn task_details(&self, task_id: &str) -> Result<Value> {
        self.get(&format!("{API_V3}/tasks/{task_id}/details")).await
    }

    /// Poll until the execution is terminal. `on_pending` sees the current
    /// pending replay approvals on every poll and may answer one by returning
    /// `(request_id, option_id)`.
    pub async fn wait_terminal(
        &self,
        task_id: &str,
        execution_id: &str,
        timeout: Duration,
        on_pending: &mut dyn FnMut(&[Value]) -> Option<(String, String)>,
    ) -> Result<TerminalOutcome> {
        let started = Instant::now();
        let mut answered_inputs: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        loop {
            let pending = self.pending_replay_approvals().await.unwrap_or_default();
            if let Some((request_id, option)) = on_pending(&pending) {
                self.respond_approval(&request_id, &option).await?;
            }
            // A cold run that escalates for the fixture password would otherwise
            // hang until timeout. Answer it as the user would; harmless when the
            // agent never pauses (no unanswered credential HITL to find).
            self.answer_pending_credential_pause(task_id, execution_id, &mut answered_inputs)
                .await;
            let executions = self
                .get(&format!("{API_V3}/tasks/{task_id}/executions"))
                .await?;
            if let Some(status) = find_execution_status(&executions, execution_id) {
                let status = status.to_ascii_lowercase();
                if matches!(
                    status.as_str(),
                    "completed" | "failed" | "cancelled" | "canceled"
                ) {
                    let details = self.task_details(task_id).await?;
                    let (outcome_type, outcome_summary) =
                        outcome_for_execution(&details, execution_id);
                    return Ok(TerminalOutcome {
                        status,
                        outcome_type,
                        outcome_summary,
                        details,
                    });
                }
            }
            if started.elapsed() >= timeout {
                bail!(
                    "task {task_id} did not reach a terminal state within {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    pub async fn list_recipes(&self) -> Result<Value> {
        self.get(&format!("{API_V2}/api-mining/recipes")).await
    }

    pub async fn recipe(&self, id: &str) -> Result<Value> {
        self.get(&format!("{API_V2}/api-mining/recipes/{id}")).await
    }

    /// Poll the recipe list until one is bound to `task_id`.
    pub async fn wait_for_recipe_bound_to(
        &self,
        task_id: &str,
        timeout: Duration,
    ) -> Result<(String, Value)> {
        let started = Instant::now();
        loop {
            let list = self.list_recipes().await?;
            let rows = list
                .as_array()
                .cloned()
                .or_else(|| list.get("recipes").and_then(Value::as_array).cloned())
                .unwrap_or_default();
            for row in rows {
                let Some(id) = row.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let detail = self.recipe(id).await?;
                let bound = first_string(&detail, "task_id").as_deref() == Some(task_id)
                    || detail
                        .get("task_ids")
                        .and_then(Value::as_array)
                        .is_some_and(|ids| ids.iter().any(|value| value.as_str() == Some(task_id)));
                if bound {
                    return Ok((id.to_owned(), detail));
                }
            }
            if started.elapsed() >= timeout {
                bail!(
                    "no recipe bound to task {task_id} within {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn wait_for_recipe_version(
        &self,
        recipe_id: &str,
        previous: usize,
        timeout: Duration,
    ) -> Result<Value> {
        let started = Instant::now();
        loop {
            let detail = self.recipe(recipe_id).await?;
            if version_count(&detail) > previous {
                return Ok(detail);
            }
            if started.elapsed() >= timeout {
                bail!(
                    "recipe {recipe_id} stayed at {previous} version(s) for {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    pub async fn replay_grants(&self) -> Result<Value> {
        self.get(&format!("{API_V2}/api-mining/replay-grants"))
            .await
    }

    pub async fn list_workspaces(&self) -> Result<Vec<String>> {
        let list = self.get(&format!("{API_V2}/workspaces")).await?;
        Ok(list
            .as_array()
            .cloned()
            .or_else(|| list.get("workspaces").and_then(Value::as_array).cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|workspace| {
                workspace
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect())
    }

    /// Delete a workspace and its data: the registry row goes now, the
    /// directory at the server's next start (`DELETE …?purge=true`). Safe
    /// against a live service — nothing is removed from under it.
    pub async fn purge_workspace(&self, slug: &str) -> Result<()> {
        self.json(
            Method::DELETE,
            &format!("{API_V2}/workspaces/{slug}?purge=true"),
            None,
        )
        .await?;
        Ok(())
    }

    pub async fn pending_replay_approvals(&self) -> Result<Vec<Value>> {
        let list = self.get(&format!("{API_V2}/user-requests")).await?;
        Ok(pending_replay_approvals(&list))
    }

    /// Opt an origin into replay. Every origin defaults to replay-refused until
    /// an operator flips this, so this is the explicit audit step standing
    /// between "a signed-in surface was captured" and "it is replayed
    /// unattended".
    pub async fn allow_origin_replay(&self, origin_url: &str) -> Result<Value> {
        let key = origin_url
            .replace("://", "___")
            .replace(['.', '/', ':'], "_");
        self.json(
            Method::POST,
            &format!("{API_V2}/api-mining/origins/{key}/allow-replay"),
            Some(json!({"origin_url": origin_url, "allow_replay": true})),
        )
        .await
    }

    pub async fn respond_approval(&self, request_id: &str, option: &str) -> Result<()> {
        self.json(
            Method::POST,
            &format!("{API_V2}/hitl/{request_id}/respond"),
            Some(json!({
                "source": "user_request",
                "value": {"type": "choice", "selected_id": option},
                "channel": "web"
            })),
        )
        .await?;
        Ok(())
    }

    /// Resume an agentic input pause (`need_user_input`) — the runtime source,
    /// not the user-request/approval one. `correlation_id` is the pause's
    /// `pause_state_id` from the `hitl.requested` event.
    pub async fn respond_agentic_input(
        &self,
        correlation_id: &str,
        execution_id: &str,
        value_type: &str,
        value: &str,
    ) -> Result<()> {
        self.json(
            Method::POST,
            &format!("{API_V2}/hitl/{correlation_id}/respond"),
            Some(json!({
                "source": "agentic",
                "execution_id": execution_id,
                "input_type": value_type,
                "value": {"type": value_type, "value": value},
                "channel": "web"
            })),
        )
        .await?;
        Ok(())
    }

    /// If the execution is paused on an unresolved credential prompt, answer it
    /// with the fixture login. Reads the on-disk timeline (the pause is a
    /// runtime HITL, not a user-request), matches `hitl.requested` rows with no
    /// later resolution, and answers a `password`/`text` prompt once each.
    async fn answer_pending_credential_pause(
        &self,
        task_id: &str,
        execution_id: &str,
        answered: &mut std::collections::HashSet<String>,
    ) {
        let events = crate::evidence::execution_events(
            &self.runtime_root,
            &self.principal,
            &self.workspace,
            task_id,
            execution_id,
        )
        .unwrap_or_default();
        // Correlations that already carry a resolution are done; never re-answer.
        let resolved: std::collections::HashSet<&str> = events
            .iter()
            .filter(|event| {
                let kind = crate::evidence::event_type(event);
                kind == "hitl.resolved" || kind == "input.resolved" || kind == "execution.resumed"
            })
            .filter_map(|event| event_correlation(event))
            .collect();
        for event in &events {
            if crate::evidence::event_type(event) != "hitl.requested" {
                continue;
            }
            let Some(correlation) = event_correlation(event) else {
                continue;
            };
            if resolved.contains(correlation) || answered.contains(correlation) {
                continue;
            }
            let input_type = event
                .get("input_type")
                .or_else(|| event.get("payload").and_then(|p| p.get("input_type")))
                .and_then(Value::as_str)
                .unwrap_or("");
            let prompt = event
                .get("prompt")
                .or_else(|| event.get("payload").and_then(|p| p.get("prompt")))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let (value_type, value) = match input_type {
                "password" => ("password", FIXTURE_PASSWORD),
                "text" if prompt.contains("user") => ("text", FIXTURE_USERNAME),
                "text" if prompt.contains("pass") => ("password", FIXTURE_PASSWORD),
                _ => continue,
            };
            if self
                .respond_agentic_input(correlation, execution_id, value_type, value)
                .await
                .is_ok()
            {
                answered.insert(correlation.to_owned());
            }
        }
    }

    pub async fn browser_signals(&self, magicutor_base: &str) -> Result<BrowserSignals> {
        let sequence = self
            .get(&format!("{API_V2}/api-mining/sequence-metrics"))
            .await?;
        let router = self
            .get(&format!("{API_V2}/api-mining/router-metrics"))
            .await?;
        let tabs: Value = self
            .client
            .get(format!(
                "{}/json/list",
                magicutor_base.trim_end_matches('/')
            ))
            .send()
            .await
            .context("Magicutor /json/list")?
            .json()
            .await
            .unwrap_or(Value::Null);
        Ok(BrowserSignals {
            browser_only_sequences: first_u64(&sequence, "with_browser_only_steps").unwrap_or(0),
            router,
            magicutor_tabs: tabs.as_array().map_or(0, Vec::len),
        })
    }

    pub async fn magicutor_ready(&self, magicutor_base: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!(
                "{}/json/version",
                magicutor_base.trim_end_matches('/')
            ))
            .send()
            .await
            .context("Magicutor /json/version")?;
        if !response.status().is_success() {
            bail!("Magicutor /json/version returned {}", response.status());
        }
        Ok(response.json().await.unwrap_or(Value::Null))
    }
}

/// A credential from the environment, else from the live runtime env files
/// (`$MAGICIAN_ROOT_DIR/.env.development`, then `.env`) — the sanctioned home
/// for runtime credentials, so the owner sets them once.
fn eval_credential(key: &str, runtime_root: &std::path::Path) -> Option<String> {
    if let Some(value) = std::env::var(key)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
    {
        return Some(value);
    }
    for file in [".env.development", ".env"] {
        let Ok(text) = std::fs::read_to_string(runtime_root.join(file)) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if name.trim() != key {
                continue;
            }
            let value = value.trim().trim_matches('"').trim_matches('\'').to_owned();
            if !value.is_empty() {
                return Some(value);
            }
        }
    }
    None
}

async fn login_and_scope(
    client: &Client,
    base: &str,
    username: &str,
    password: &str,
    workspace_slug: &str,
) -> Result<String> {
    let login: Value = client
        .post(format!("{base}{API_V2}/auth/login"))
        .json(&json!({"username": username, "password": password}))
        .send()
        .await
        .context("POST /auth/login")?
        .error_for_status()
        .context("login rejected")?
        .json()
        .await?;
    let session_token = first_string(&login, "token").context("login omitted token")?;
    let workspaces: Value = client
        .get(format!("{base}{API_V2}/workspaces"))
        .bearer_auth(&session_token)
        .send()
        .await
        .context("GET /workspaces")?
        .error_for_status()?
        .json()
        .await?;
    let exists = workspaces
        .as_array()
        .cloned()
        .or_else(|| {
            workspaces
                .get("workspaces")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .any(|workspace| workspace.get("id").and_then(Value::as_str) == Some(workspace_slug));
    if !exists {
        client
            .post(format!("{base}{API_V2}/workspaces"))
            .bearer_auth(&session_token)
            .json(&json!({
                "slug": workspace_slug,
                "display_name": "Task Recipes eval",
                "description": "Disposable scope for the Task Recipes fixture eval"
            }))
            .send()
            .await
            .context("POST /workspaces")?
            .error_for_status()
            .context("create eval workspace")?;
    }
    let rotated: Value = client
        .post(format!("{base}{API_V2}/auth/session/scope"))
        .bearer_auth(&session_token)
        .json(&json!({"workspace": workspace_slug}))
        .send()
        .await
        .context("POST /auth/session/scope")?
        .error_for_status()
        .context("rotate session onto eval workspace")?
        .json()
        .await?;
    first_string(&rotated, "token").context("rotated session omitted token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_for_execution_prefers_matching_execution() {
        let details = json!({
            "task": {"task_id": "task_1"},
            "executions": [
                {"execution_id": "exec_old", "status": "completed", "outcome_type": "agentic", "outcome_summary": "old answer"},
                {"execution_id": "exec_new", "status": "completed", "outcome": {"outcome_type": "recipe_replay", "outcome_summary": "3119 points"}}
            ]
        });
        assert_eq!(
            outcome_for_execution(&details, "exec_new"),
            ("recipe_replay".to_owned(), "3119 points".to_owned())
        );
        assert_eq!(
            outcome_for_execution(&details, "exec_old"),
            ("agentic".to_owned(), "old answer".to_owned())
        );
        // Unknown execution falls back to the first outcome anywhere.
        assert_eq!(outcome_for_execution(&details, "exec_missing").0, "agentic");
    }

    #[test]
    fn pending_replay_approval_filters_by_request_type() {
        let list = json!({"requests": [
            {"id": "r1", "request_type": "clarification"},
            {"id": "r2", "request_type": "api_replay_approval", "options": [{"id": "approve_once"}]}
        ]});
        let pending = pending_replay_approvals(&list);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["id"], "r2");
        assert!(pending_replay_approvals(&json!({})).is_empty());
    }

    #[test]
    fn eval_credential_reads_env_files_without_values_in_env() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".env.development"),
            "# comment\nexport MAGICIAN_EVAL_USERNAME=\"eval-user\"\nMAGICIAN_EVAL_PASSWORD=\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join(".env"),
            "MAGICIAN_EVAL_PASSWORD='from-dot-env'\n",
        )
        .unwrap();
        assert_eq!(
            eval_credential("MAGICIAN_EVAL_USERNAME", temp.path()).as_deref(),
            Some("eval-user")
        );
        assert_eq!(
            eval_credential("MAGICIAN_EVAL_PASSWORD", temp.path()).as_deref(),
            Some("from-dot-env")
        );
        assert_eq!(
            eval_credential("MAGICIAN_EVAL_MISSING_KEY_XYZ", temp.path()),
            None
        );
    }

    #[test]
    fn execution_status_is_found_by_id() {
        let executions = json!({"executions": [
            {"execution_id": "a", "status": "running"},
            {"execution_id": "b", "status": "completed"}
        ]});
        assert_eq!(
            find_execution_status(&executions, "b").as_deref(),
            Some("completed")
        );
        assert_eq!(find_execution_status(&executions, "zzz"), None);
    }
}
