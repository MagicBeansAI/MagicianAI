//! Core types for agentic execution.
//!
//! This module defines the data structures for the observe-decide-execute loop,
//! including environment state capture, execution history, and outcomes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::OnFailureMode;
use crate::magician_v2::agents::memory_tiers::MemoryTierDefinition;
use crate::magician_v2::agents::{types::ApprovalRule, TrustPolicyEnforcer};
use crate::magician_v2::artifact_v2::models::TaskOutputMode;
use crate::magician_v2::execution::actions::{ExecutableAction, FileAction, HttpMethod};
use crate::magician_v2::execution::types::{ActionVerification, PageState, VerificationMatch};

// ============================================================================
// History Formatting Constants
// ============================================================================

/// Max reasoning chars for the most recent iteration (full context preferred)
const REASONING_LIMIT_MOST_RECENT: usize = 1200;
/// Max reasoning chars for older iterations (middle-truncated)
const REASONING_LIMIT_OLDER: usize = 700;
/// Max assistant-visible text chars retained in prompt history.
const ASSISTANT_TURN_TEXT_LIMIT: usize = 900;
/// Max assistant tool-call argument chars retained in prompt history.
const ASSISTANT_TURN_ARGUMENT_LIMIT: usize = 900;
/// Hard cap for the combined outer execution history block sent to the LLM.
const OUTER_HISTORY_CONTEXT_LIMIT: usize = 60_000;
/// Hard cap for the recent-action-only block used in task/runtime sections.
const OUTER_RECENT_HISTORY_CONTEXT_LIMIT: usize = 45_000;

/// A hard execution token budget failure reported by the shared task-local meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionTokenBudgetError {
    pub used: u64,
    pub limit: u64,
    pub missing_usage: bool,
}

impl std::fmt::Display for ExecutionTokenBudgetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.missing_usage {
            write!(
                formatter,
                "LLM response omitted complete token usage under a hard execution budget; budget exhausted (used {}, limit {})",
                self.used, self.limit
            )
        } else {
            write!(
                formatter,
                "execution token budget exhausted (used {}, limit {})",
                self.used, self.limit
            )
        }
    }
}

impl std::error::Error for ExecutionTokenBudgetError {}

#[derive(Debug)]
struct ExecutionTokenMeter {
    used: AtomicU64,
    limit: u64,
}

impl ExecutionTokenMeter {
    fn new(used: u64, limit: u64) -> Self {
        Self {
            used: AtomicU64::new(used),
            limit,
        }
    }

    fn charge(&self, tokens: u64) -> u64 {
        let mut current = self.used.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(tokens);
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }

    fn snapshot(&self) -> (u64, u64) {
        (self.used.load(Ordering::Relaxed), self.limit)
    }
}

tokio::task_local! {
    /// Shared by every inline-awaited LLM consumer in one agentic execution.
    /// Inline recursive segments reuse the active meter. Spawned delegated
    /// children do not inherit Tokio task-locals and install their own meter.
    static EXECUTION_TOKEN_METER: Arc<ExecutionTokenMeter>;
}

/// Explicit carrier for scheduler/runtime boundaries that remain part of the
/// same logical execution. Tokio task-locals intentionally do not propagate to
/// spawned tasks; capturing the meter prevents an executor ownership handoff
/// from accidentally resetting the tree-wide token ledger.
#[derive(Clone, Default)]
pub struct CapturedExecutionTokenMeter(Option<Arc<ExecutionTokenMeter>>);

impl CapturedExecutionTokenMeter {
    pub fn current() -> Self {
        Self(EXECUTION_TOKEN_METER.try_with(Arc::clone).ok())
    }

    pub async fn scope<F>(self, future: F) -> F::Output
    where
        F: std::future::Future,
    {
        match self.0 {
            Some(meter) => EXECUTION_TOKEN_METER.scope(meter, future).await,
            None => future.await,
        }
    }
}

/// The run-scoped task-locals a lane or runtime hop carries to another task —
/// the coding-run flag, the run's parent engine, the run's launch pin, and
/// the secret scope — as
/// one value, so a hop cannot re-scope some of them and drop the rest. Tokio
/// task-locals do not cross a spawn; a hop captures these where the run's
/// task still has them and re-scopes them on the far side with
/// [`Self::scope`]. The token meter travels separately (see
/// [`CapturedExecutionTokenMeter`]): the lane worker carries it itself. The
/// coding run's real repo path (`CODING_RUN_REAL_REPO`) is deliberately not
/// carried: `run_coding_task` scopes it on the action task around the Pi
/// turn, once the repo binding is resolved, and no hop above that has one.
#[derive(Clone, Debug)]
pub struct CapturedRunTaskLocals {
    coding_context_active: bool,
    parent_engine: Option<String>,
    /// The pin a run launched from this task inherits.
    run_engine_pin: Option<crate::magician_v2::execution::plane::RunEnginePin>,
    secret_scope: Option<(String, String)>,
}

impl CapturedRunTaskLocals {
    /// Capture from the ambient task: the coding flag and the parent engine
    /// as this task sees them, the secret scope from the caller's principal
    /// and workspace. For hops with no run context in hand.
    pub fn current(principal: Option<String>, workspace: Option<String>) -> Self {
        use crate::magician_v2::execution::coding_engine::coding_context_active;
        use crate::magician_v2::query_analysis::parent_engine::current_parent_engine;
        Self {
            coding_context_active: coding_context_active(),
            parent_engine: current_parent_engine(),
            run_engine_pin: crate::magician_v2::execution::plane::current_launching_run_engine_pin(
            ),
            secret_scope: principal.zip(workspace),
        }
    }

    /// Capture for a run's context: the parent is the run's own engine, the
    /// launch pin the run's own pin, and the secret scope the run's
    /// principal and workspace; the coding flag is the ambient one, which
    /// the run seam set from the agent's tools. A chat grant's context
    /// carries the chat's pin, so a run the chat launches inherits the chat.
    pub fn for_context(ctx: &AgenticContext) -> Self {
        Self {
            parent_engine: crate::magician_v2::execution::plane::run_parent_engine(ctx),
            run_engine_pin: ctx.run_engine_pin.clone(),
            ..Self::current(ctx.principal.clone(), ctx.workspace.clone())
        }
    }

    /// Run `future` under every captured task-local, on whichever task polls
    /// it. Owns its captures, so the future can be boxed and spawned.
    pub async fn scope<F>(self, future: F) -> F::Output
    where
        F: std::future::Future,
    {
        let scoped = crate::magician_v2::execution::coding_engine::with_coding_context(
            self.coding_context_active,
            future,
        );
        let scoped = crate::magician_v2::query_analysis::parent_engine::with_parent_engine(
            self.parent_engine.as_deref(),
            scoped,
        );
        let scoped = crate::magician_v2::execution::plane::with_launching_run_engine_pin(
            self.run_engine_pin,
            scoped,
        );
        match self.secret_scope.as_ref() {
            Some((principal, workspace)) => {
                crate::magician_v2::secrets::with_secret_scope(principal, workspace, scoped).await
            },
            None => scoped.await,
        }
    }
}

/// Run an execution under its hard token budget, reusing an active inline meter.
pub async fn with_execution_token_meter<F>(used: u64, limit: u64, future: F) -> F::Output
where
    F: std::future::Future,
{
    if EXECUTION_TOKEN_METER.try_with(|_| ()).is_ok() {
        return future.await;
    }
    EXECUTION_TOKEN_METER
        .scope(Arc::new(ExecutionTokenMeter::new(used, limit)), future)
        .await
}

/// Spawn a child in a structured task set while preserving the current
/// execution's exact shared token meter. Dropping the set aborts its children,
/// so cancelled execution segments cannot leave paid agent work detached.
pub fn spawn_with_execution_token_meter_in_set<F, T>(tasks: &mut tokio::task::JoinSet<T>, future: F)
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let active_meter = EXECUTION_TOKEN_METER.try_with(Arc::clone).ok();
    let task: Pin<Box<dyn std::future::Future<Output = T> + Send>> = Box::pin(async move {
        if let Some(meter) = active_meter {
            EXECUTION_TOKEN_METER.scope(meter, future).await
        } else {
            future.await
        }
    });
    tasks.spawn(task);
}

/// Return `(used, limit)` for the current execution, if a hard budget is active.
pub fn execution_token_budget_snapshot() -> Option<(u64, u64)> {
    EXECUTION_TOKEN_METER
        .try_with(|meter| meter.snapshot())
        .ok()
}

/// Reject a new LLM call once the cumulative execution usage reaches its limit.
pub fn preflight_execution_token_budget() -> Result<(), ExecutionTokenBudgetError> {
    let Some((used, limit)) = execution_token_budget_snapshot() else {
        return Ok(());
    };
    if used >= limit {
        return Err(ExecutionTokenBudgetError {
            used,
            limit,
            missing_usage: false,
        });
    }
    Ok(())
}

/// Charge known usage and reject a response that pushes the execution over its limit.
pub fn account_execution_tokens(tokens: u64) -> Result<(), ExecutionTokenBudgetError> {
    let Some(meter) = EXECUTION_TOKEN_METER.try_with(Arc::clone).ok() else {
        return Ok(());
    };
    let used = meter.charge(tokens);
    if used > meter.limit {
        return Err(ExecutionTokenBudgetError {
            used,
            limit: meter.limit,
            missing_usage: false,
        });
    }
    Ok(())
}

/// Fail closed when an LLM response does not expose complete usage metadata.
pub fn exhaust_execution_token_budget_for_missing_usage() -> Result<(), ExecutionTokenBudgetError> {
    let Some(meter) = EXECUTION_TOKEN_METER.try_with(Arc::clone).ok() else {
        return Ok(());
    };
    let used = meter
        .used
        .fetch_max(meter.limit, Ordering::Relaxed)
        .max(meter.limit);
    Err(ExecutionTokenBudgetError {
        used,
        limit: meter.limit,
        missing_usage: true,
    })
}

// ============================================================================
// UTF-8 Safe String Truncation
// ============================================================================

/// Safely truncate a string at a character boundary.
/// Returns a slice of at most `max_bytes` bytes, but always ending at a valid UTF-8 char boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Find the largest valid char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate from the middle, keeping head (context) and tail (intent/plan).
/// LLM reasoning typically puts setup/context first and the decision/next-step
/// intent at the end — so preserving both ends is more useful than head-only.
// ============================================================================
// Redaction constants — used for passwords, secrets, tokens, CVVs, OTPs, PINs.
// The placeholder includes the parameter ID so the executor can map it back
// to the correct resolved_input at action time.
// ============================================================================

fn process_runtime_root() -> PathBuf {
    crate::magician_v2::process_storage::runtime_root()
}

/// Prefix for redacted secret placeholders: `[REDACTED:<param_id>]`
pub const REDACTED_PREFIX: &str = "[REDACTED:";
/// Suffix for redacted secret placeholders
pub const REDACTED_SUFFIX: &str = "]";

/// Build a redacted placeholder that carries the parameter ID.
/// Example: `[REDACTED:input-2]`
pub fn redacted_placeholder(param_id: &str) -> String {
    format!("{}{}{}", REDACTED_PREFIX, param_id, REDACTED_SUFFIX)
}

/// Extract the parameter ID from a redacted placeholder, if the text matches.
/// Returns `Some("input-2")` for `[REDACTED:input-2]`, `None` otherwise.
pub fn parse_redacted_placeholder(text: &str) -> Option<&str> {
    text.strip_prefix(REDACTED_PREFIX)
        .and_then(|rest| rest.strip_suffix(REDACTED_SUFFIX))
}

/// Returns true if the parameter name looks like a secret (password, token, etc.)
/// Uses word-boundary matching for short terms like "pin" to avoid false positives
/// on words like "shipping", "spinning", "pinned".
pub use crate::magician_v2::secrets::classify::is_secret_param_name;

fn truncate_middle(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // 40% head, 60% tail — tail usually has the decision/plan
    let head_budget = max_bytes * 2 / 5;
    let separator = " <...redacted for brevity> ";
    // Guard against underflow when max_bytes is too small for the split
    let min_viable = head_budget + separator.len() + 1;
    if max_bytes < min_viable {
        return truncate_utf8(s, max_bytes).to_string();
    }
    let tail_budget = max_bytes - head_budget - separator.len();

    let head = truncate_utf8(s, head_budget);
    // Find a valid char boundary for the tail start
    let tail_start_candidate = s.len().saturating_sub(tail_budget);
    let mut tail_start = tail_start_candidate;
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let tail = if tail_start < s.len() {
        &s[tail_start..]
    } else {
        ""
    };

    format!("{}{}{}", head, separator, tail)
}

fn bound_outer_prompt_context(section_name: &str, content: String, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content;
    }
    let original_len = content.len();
    let bounded = truncate_middle(&content, max_bytes);
    format!(
        "{bounded}\n\n[{} truncated: original_bytes={}, retained_bytes={}]",
        section_name,
        original_len,
        bounded.len()
    )
}

fn summarize_primitive_payload_for_prompt(output: &str) -> Option<String> {
    let raw_json = output.strip_prefix("Browser result: ")?;
    let payload = serde_json::from_str::<Value>(raw_json).ok()?;
    let terminal_decision = payload.get("terminal_decision").and_then(Value::as_str)?;
    let iterations = payload.get("inner_iterations")?;

    let success = payload
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(terminal_decision == "goal_reached");
    let objective = payload.get("inner_objective");
    let capability = objective
        .and_then(|value| value.get("capability"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let objective_id = objective
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let evidence = payload
        .get("evidence_ledger")
        .and_then(|value| value.get("terminal_evidence"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let summary = payload
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let final_url = payload
        .get("evidence_ledger")
        .and_then(|value| value.get("final_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let prefix = if terminal_decision == "goal_reached" {
        "Inner-loop objective completed"
    } else {
        "Inner-loop objective terminal"
    };
    let mut parts = vec![
        format!("{prefix}: capability={capability}"),
        format!("objective_id={objective_id}"),
        format!("terminal_decision={terminal_decision}"),
        format!("success={success}"),
        format!("iterations={iterations}"),
    ];
    if let Some(value) = evidence {
        parts.push(format!("evidence={}", truncate_utf8(value, 800)));
    }
    if let Some(value) = final_url {
        parts.push(format!("final_url={}", truncate_utf8(value, 500)));
    }
    if let Some(value) = summary {
        parts.push(format!("summary={}", truncate_utf8(value, 1000)));
    }

    Some(parts.join("; "))
}

pub fn summarize_action_output_for_prompt(_action: &ExecutableAction, output: &str) -> String {
    if let Some(summary) = summarize_primitive_payload_for_prompt(output) {
        return summary;
    }

    output.to_string()
}

// ============================================================================
// Environment State Types
// ============================================================================

/// Observed state from any environment type.
/// Each variant captures the relevant state for its context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvironmentState {
    /// No environment initialized yet. The LLM decides the first action
    /// based on goal + available tools. The environment is created lazily
    /// when the first action executes.
    Uninitialized,

    /// Browser environment (web page) - uses PageState for Merkle tree support
    Browser(PageState),

    /// Filesystem environment
    Filesystem(FilesystemState),

    /// HTTP/API environment
    Http(HttpState),

    /// Shell/terminal environment
    Shell(ShellState),
}

impl EnvironmentState {
    /// Get a human-readable type name
    pub fn type_name(&self) -> &'static str {
        match self {
            EnvironmentState::Uninitialized => "uninitialized",
            EnvironmentState::Browser(_) => "browser",
            EnvironmentState::Filesystem(_) => "filesystem",
            EnvironmentState::Http(_) => "http",
            EnvironmentState::Shell(_) => "shell",
        }
    }

    /// Format state for LLM prompt.
    pub fn format_for_llm(&self) -> String {
        self.format_for_llm_with_replayed_result(false)
    }

    /// Format state for the decision prompt. `result_replayed` says the last
    /// action's result already reaches the provider as a native tool-result
    /// message in the same request; a shell/pack body is then referenced,
    /// not pasted again under a byte cap — the cut copy cost tokens and its
    /// "(truncated)" read as rows lost when the whole result sat one message
    /// up. Browser, filesystem and http states are the observation itself and
    /// render as before.
    pub fn format_for_llm_with_replayed_result(&self, result_replayed: bool) -> String {
        match self {
            EnvironmentState::Uninitialized => {
                "No environment initialized. Decide your first action based on the goal and available tools.".to_string()
            },
            EnvironmentState::Browser(state) => {
                let mut parts = Vec::new();

                let url = state.url.as_deref().unwrap_or("(unknown)");
                let title = state.title.as_deref().unwrap_or("(no title)");
                parts.push(format!("URL: {}", url));
                parts.push(format!("Title: {}", title));

                // Add page stage if known
                if state.current_stage != crate::magician_v2::execution::types::PageStage::Unknown {
                    parts.push(format!("Page Stage: {:?}", state.current_stage));
                }

                if let Some(text) = &state.visible_text {
                    let truncated = if text.len() > 2000 {
                        format!("{}... (truncated)", truncate_utf8(text, 2000))
                    } else {
                        text.clone()
                    };
                    parts.push(format!("Visible Text:\n{}", truncated));
                }
                if let Some(snapshot) = state.accessibility_snapshot_for_ai.as_deref() {
                    let truncated = if snapshot.len() > 3000 {
                        format!("{}... (truncated)", truncate_utf8(snapshot, 3000))
                    } else {
                        snapshot.to_string()
                    };
                    parts.push(format!("Accessibility Tree:\n{}", truncated));
                } else if let Some(tree) = &state.accessibility_tree {
                    let tree_str = tree.to_string();
                    let truncated = if tree_str.len() > 3000 {
                        format!("{}... (truncated)", truncate_utf8(&tree_str, 3000))
                    } else {
                        tree_str
                    };
                    parts.push(format!("Accessibility Tree:\n{}", truncated));
                }

                parts.join("\n\n")
            },
            EnvironmentState::Filesystem(state) => {
                let mut parts = vec![format!(
                    "Working Directory: {}",
                    state.current_dir.display()
                )];
                if let Some(op) = &state.last_operation {
                    parts.push(format!("Last Operation: {}", op));
                }
                if let Some(result) = &state.last_result {
                    let truncated = if result.len() > 2000 {
                        format!("{}... (truncated)", truncate_utf8(result, 2000))
                    } else {
                        result.clone()
                    };
                    parts.push(format!("Result:\n{}", truncated));
                }
                if let Some(err) = &state.error {
                    parts.push(format!("Error: {}", err));
                }
                parts.join("\n")
            },
            EnvironmentState::Http(state) => {
                let mut parts = vec![];
                if let Some(url) = &state.last_url {
                    parts.push(format!("Last URL: {}", url));
                }
                if let Some(status) = state.last_status {
                    parts.push(format!("Status: {}", status));
                }
                if let Some(response) = &state.last_response {
                    let truncated = if response.len() > 2000 {
                        format!("{}... (truncated)", truncate_utf8(response, 2000))
                    } else {
                        response.clone()
                    };
                    parts.push(format!("Response:\n{}", truncated));
                }
                if let Some(err) = &state.error {
                    parts.push(format!("Error: {}", err));
                }
                if let Some(browser_url) = &state.browser_url_hint {
                    parts.push(format!(
                        "Note: Data retrieved via API replay. Browser is still on: {}",
                        browser_url
                    ));
                }
                if parts.is_empty() {
                    "No HTTP activity yet".to_string()
                } else {
                    parts.join("\n")
                }
            },
            EnvironmentState::Shell(state) => {
                let mut parts = vec![format!(
                    "Working Directory: {}",
                    state.working_dir.display()
                )];
                if let Some(cmd) = &state.last_command {
                    parts.push(format!("Last Command: {}", cmd));
                }
                if let Some(code) = state.last_exit_code {
                    parts.push(format!("Exit Code: {}", code));
                }
                if let Some(stdout) = &state.last_stdout {
                    if result_replayed && !stdout.is_empty() {
                        parts.push(format!(
                            "Stdout: delivered whole as the tool result above ({} bytes); nothing \
                             is cut there.",
                            stdout.len()
                        ));
                    } else {
                        let truncated = if stdout.len() > 1500 {
                            format!("{}... (truncated)", truncate_utf8(stdout, 1500))
                        } else {
                            stdout.clone()
                        };
                        parts.push(format!("Stdout:\n{}", truncated));
                    }
                }
                if let Some(stderr) = &state.last_stderr {
                    if !stderr.is_empty() {
                        let truncated = if stderr.len() > 500 {
                            format!("{}... (truncated)", truncate_utf8(stderr, 500))
                        } else {
                            stderr.clone()
                        };
                        parts.push(format!("Stderr:\n{}", truncated));
                    }
                }
                parts.join("\n")
            },
        }
    }
}

// NOTE: BrowserState has been removed - use PageState from execution/types.rs instead.
// PageState includes Merkle tree support for O(1) change detection.

/// Filesystem environment state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemState {
    /// Current working directory
    pub current_dir: PathBuf,

    /// Last operation performed (e.g., "Read", "Write")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_operation: Option<String>,

    /// Result of last operation (file content, listing, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,

    /// Error from last operation, if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Default for FilesystemState {
    fn default() -> Self {
        Self {
            current_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            last_operation: None,
            last_result: None,
            error: None,
        }
    }
}

/// HTTP/API environment state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HttpState {
    /// Last requested URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_url: Option<String>,

    /// HTTP status code of last response
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_status: Option<u16>,

    /// Response body (truncated if large)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_response: Option<String>,

    /// Error from last request, if any
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// When set, indicates this state came from an API replay and the browser
    /// is still on this URL. To interact with UI elements, Navigate there first.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_url_hint: Option<String>,
}

/// Shell/terminal environment state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellState {
    /// Current working directory
    pub working_dir: PathBuf,

    /// Last command executed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_command: Option<String>,

    /// Stdout from last command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_stdout: Option<String>,

    /// Stderr from last command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_stderr: Option<String>,

    /// Exit code from last command
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_exit_code: Option<i32>,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
            last_command: None,
            last_stdout: None,
            last_stderr: None,
            last_exit_code: None,
        }
    }
}

// ============================================================================
// Pending Input Types (for Planning → Execution Handoff)
// ============================================================================

/// Source of a pending input - tracks where the input requirement originated.
///
/// This helps with debugging and allows the UI to show appropriate context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputSource {
    /// Deferred from planning phase (JIT questions)
    /// These are questions that were identified during planning but marked
    /// for later asking to avoid overwhelming the user upfront.
    DeferredFromPlanning,

    /// Discovered during execution (e.g., placeholder in action)
    /// These are parameters like `{{email}}` that weren't resolved during planning.
    DiscoveredDuringExecution,

    /// LLM decided it needs more info during agentic loop
    /// The agentic decision LLM determined it cannot proceed without user input.
    AgenticDecision,
}

/// A pending input that needs to be resolved before or during execution.
///
/// This represents the execution-phase format for inputs that need user values.
/// It's converted from `UnresolvedInput` (planning format) at execution start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInput {
    /// Unique identifier for this input (matches UnresolvedInput.id)
    pub id: String,

    /// Logical parameter name (e.g., "email", "password")
    pub parameter: String,

    /// Step ID that requires this input (for per-step filtering)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,

    /// Human-readable description of what's needed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Where this input requirement came from
    pub source: PendingInputSource,

    /// The resolved value once user provides it (None until resolved)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_value: Option<Value>,
}

impl PendingInput {
    /// Create a new pending input from planning phase
    pub fn from_planning(
        id: impl Into<String>,
        parameter: impl Into<String>,
        step_id: Option<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            parameter: parameter.into(),
            step_id,
            description,
            source: PendingInputSource::DeferredFromPlanning,
            resolved_value: None,
        }
    }

    /// Create a new pending input discovered during execution
    pub fn from_execution(
        id: impl Into<String>,
        parameter: impl Into<String>,
        step_id: Option<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            parameter: parameter.into(),
            step_id,
            description,
            source: PendingInputSource::DiscoveredDuringExecution,
            resolved_value: None,
        }
    }

    /// Create a new pending input from agentic decision
    pub fn from_agentic_decision(
        id: impl Into<String>,
        parameter: impl Into<String>,
        step_id: Option<String>,
        description: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            parameter: parameter.into(),
            step_id,
            description,
            source: PendingInputSource::AgenticDecision,
            resolved_value: None,
        }
    }

    /// Convert from UnresolvedInput (planning format) to PendingInput (execution format)
    ///
    /// This is the handoff point from planning → execution. Preserves auto_fill values
    /// if they exist, otherwise leaves resolved_value as None for user to provide.
    pub fn from_unresolved(
        unresolved: &crate::magician_v2::strategy::plan::UnresolvedInput,
    ) -> Self {
        Self {
            id: unresolved.id.clone(),
            parameter: unresolved.parameter.clone(),
            step_id: unresolved.step_id.clone(),
            description: if !unresolved.prompt.is_empty() {
                Some(unresolved.prompt.clone())
            } else if !unresolved.display_name.is_empty() {
                Some(unresolved.display_name.clone())
            } else {
                None
            },
            source: PendingInputSource::DeferredFromPlanning,
            resolved_value: unresolved.auto_fill.clone(),
        }
    }

    /// Check if this input has been resolved
    pub fn is_resolved(&self) -> bool {
        self.resolved_value.is_some()
    }

    /// Mark this input as resolved with a value
    pub fn resolve(&mut self, value: Value) {
        self.resolved_value = Some(value);
    }
}

// ============================================================================
// Agentic Context
// ============================================================================

/// Agent-classification for prompt identity scoping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptAgentKind {
    User,
    System,
}

/// Bounded autonomous style controls (not policy overrides).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutonomousPromptControls {
    #[serde(default)]
    pub traits: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creative_latitude: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

/// Prompt identity stack used by decision system prompts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromptIdentityContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<PromptAgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    /// User-facing display name resolved from the active `AgentDefinition`.
    ///
    /// Keep this separate from the backend service identity: prompt consumers
    /// may present the active agent name, but must never infer an assistant
    /// name from a process, crate, API-path, or service key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_name: Option<String>,
    /// Bounded discovery aliases for the active agent. These are identity
    /// metadata, not wake-word or routing policy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_agent_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomous_controls: Option<AutonomousPromptControls>,
}

/// Ownership snapshot bound to an execution-owned runtime moment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnerSnapshot {
    pub active_owner_agent_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_stack: Vec<String>,
}

/// One-time approval replay bound to the owner that received the approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovedConfirmationAction {
    pub action_json: String,
    pub owner_snapshot: OwnerSnapshot,
}

/// Why a suspended parent execution frame exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgenticContinuationKind {
    SameOwnerSubGoal,
    InContextDelegation,
}

/// Durable parent frame for nested work that may pause.
///
/// Nested agentic loops run on the same execution id. If the nested loop asks
/// for input, its pause must retain enough of the parent to return to it after
/// the nested work terminates; otherwise resume incorrectly promotes the child
/// goal to the execution root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticContinuationFrame {
    pub kind: AgenticContinuationKind,
    pub goal: String,
    pub success_criteria: String,
    pub remaining_iterations: usize,
    pub max_repeated_actions: usize,
    pub iteration_offset: usize,
    pub depth: usize,
    /// Parent active-work budget restored after this nested frame terminates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_budget_secs: Option<u64>,
    /// Active work already charged to the parent when nesting began.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub work_budget_consumed_ms: u64,
    /// Child counter value at nesting time. Subtracting this from the child's
    /// terminal counter yields only nested work, without double-counting work
    /// inherited from the parent.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub nested_work_budget_baseline_ms: u64,
    /// The parent's live loop-protection counters at the instant nested work
    /// begins. The child carries its own bundle in the ordinary pause state;
    /// this copy is restored only when the child terminal unwinds this frame.
    /// Without it a child pause/resume resets the parent's loop detector,
    /// retry/rejection budgets, failure fingerprints, and cumulative cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_protective_state: Option<LoopProtectiveState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_inputs: Vec<PendingInput>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resolved_inputs: HashMap<String, Value>,
    /// See `AgenticContext::resolved_input_sensitivity`; absent in frames
    /// written before the contract existed.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resolved_input_sensitivity:
        HashMap<String, crate::magician_v2::user_requests::SensitiveKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking_for_parameter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_environment_knowledge: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action_history_summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_confirmation_actions: Vec<ApprovedConfirmationAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_owner_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_stack: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_invocation_stack: Vec<Option<crate::magician_v2::agents::AgentInvocationContext>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context_override: Option<crate::magician_v2::agents::AgentInvocationContext>,
}

/// Exact source/target relationship that must still exist before a delegated
/// or handed-over pause can resume.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransitionAuthorizationBinding {
    pub source_agent_id: String,
    pub target_agent_id: String,
    pub surface: crate::magician_v2::agents::InvocationSurface,
}

/// Optional overrides for scoped agentic execution (autonomous cycles).
///
/// Passed into `execute_agentic_direct_with_outcome()` to inject fields into
/// the internally-built `AgenticContext`. Existing callers pass `None` for
/// zero behavior change.
/// Plane-delegated launch attenuation (plane Task 6b): what a `run_task`
/// caller's grant pins onto the execution it starts. Persisted beside the
/// execution's routing overrides before dispatch, and applied to the run's
/// context at composition — so the denial, the narrowed catalog, and the
/// inherited harness survive restarts, recovery, and pause/resume.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlaneDelegationAttenuation {
    /// Capability families this run (and its spawned children) can never
    /// reach, whatever the agent's profile grants.
    pub denied: Vec<String>,
    /// `Some` narrows this run's catalog to the intersection with these
    /// names; `None` keeps the agent's surface (minus denied).
    pub allowed: Option<Vec<String>>,
    /// The harness this run thinks with. `Some("magician")` is a deliberate
    /// pin; `None` leaves the engine to the process snapshot.
    pub harness_engine: Option<String>,
    /// Grant ceiling: the most USD this run may spend (lowers the run's
    /// effective cost cap; it can never raise one).
    pub max_usd: Option<f64>,
    /// Grant ceiling: wall-clock seconds for this run (lowers the run's work
    /// budget; it can never raise one).
    pub max_wall_clock_secs: Option<u64>,
}

/// A secret a launch seeds into a run: collected by a surface that owns no
/// run of its own (a chat turn's `need_user_input`) and handed to the run
/// that will use it. It travels raw only inside this process, is vaulted at
/// loop entry (`vault_flagged_resolved_inputs`) under the run's scope, and
/// the context retains a reference from then on. `Debug` never prints the
/// value.
#[derive(Clone)]
pub struct SeededSensitiveInput {
    /// The resolved-input key, and so the placeholder `[REF:<key>]` the model
    /// was told to use.
    pub key: String,
    pub kind: crate::magician_v2::user_requests::SensitiveKind,
    pub value: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for SeededSensitiveInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SeededSensitiveInput")
            .field("key", &self.key)
            .field("kind", &self.kind)
            .field("value", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgenticContextOverrides {
    /// Secrets the launching surface already collected for this run (P3,
    /// chat). Applied as flagged resolved inputs and vaulted at loop entry.
    pub seeded_sensitive_inputs: Vec<SeededSensitiveInput>,
    /// Plane-delegated launch attenuation (Task 6b). `None` for every
    /// non-plane launch.
    pub plane_attenuation: Option<PlaneDelegationAttenuation>,
    /// The launch pin saved with this execution, loaded from its durable
    /// record. `None` when the launch path keeps none; composition then pins
    /// the run from its launching run or the Settings choice.
    pub run_engine_pin: Option<crate::magician_v2::execution::plane::RunEnginePin>,
    /// A one-shot launch of a delegated `Runnable` shell created by this
    /// server process. Canonical roots instead use cutover-gated durable
    /// `Planning` dormancy. This is deliberately distinct from interrupted
    /// recovery: the final lifecycle gate consumes a matching process-local
    /// nonce and proves the exact still-Runnable generation has no earlier
    /// loop or pause authority.
    pub server_owned_fresh_launch: bool,
    pub server_owned_fresh_launch_nonce: Option<String>,
    pub server_owned_fresh_launch_updated_at: Option<i64>,
    /// This launch is re-admitting an already-persisted nonterminal runtime
    /// after process loss. The orchestrator must re-check the durable exact
    /// segment owner immediately before it publishes controls or mutates the
    /// runtime row; ordinary fresh launches leave this false.
    pub interrupted_runtime_recovery: bool,
    /// Exact committed source selected by typed base-execution recovery. This
    /// is copied into `stateless_resume_source_segment` only by the canonical
    /// Artifact recovery path.
    pub interrupted_runtime_recovery_source_segment: Option<String>,
    pub interrupted_runtime_recovery_source_revision: Option<u64>,
    /// Runtime generation for the narrow no-snapshot crash window. The final
    /// launch boundary rechecks both this value and indexed absence before it
    /// may publish controls for a pre-seed recovery.
    pub interrupted_runtime_preseed_updated_at: Option<i64>,
    /// Opaque durable admission paired with the pre-seed runtime generation.
    pub interrupted_runtime_preseed_token: Option<String>,
    /// Restrict built-in action types (e.g. only `["browser", "bash"]`).
    pub allowed_action_types: Option<Vec<String>>,
    /// Capability "env" to scope this execution to a single tool/pack (debug
    /// page / SOTA tests). When set (e.g. `"browser"`), the orchestrator keeps
    /// only that tool in `merged_agent_tools` and derives the matching
    /// `allowed_action_types`, so the agent runs that tool directly with no
    /// delegation. `None` = no scoping (full agent tool set).
    pub env_mode: Option<String>,
    /// Max tasks the agent can spawn in one cycle.
    pub max_spawned_tasks: Option<u32>,
    /// Principal for task tenancy.
    pub principal: Option<String>,
    /// Workspace for task tenancy.
    pub workspace: Option<String>,
    /// Server-authenticated invocation installed by a product sub-run,
    /// delegated child, or owner transition. This is never deserialized from
    /// task/API input; ordinary tasks leave it empty and derive `task`.
    pub invocation_context_override: Option<crate::magician_v2::agents::AgentInvocationContext>,
    /// Work authority ref, sealed like `invocation_context_override`:
    /// set only by server-side spawn code (delegated-child creation, from the
    /// parent's durable record), never deserialized from task/API/model
    /// input — a caller that could name its own work could grant
    /// itself one (§4.2c row 5).
    ///
    /// Generic over every arm of
    /// [`crate::magician_v2::work_context::WorkContextKind`] because the
    /// durable `ExecutionRun` it is seeded from already is. Narrowing it to the
    /// engagement form here is what used to make a program-scoped run arrive at
    /// dispatch carrying nothing.
    pub work_authority: Option<crate::magician_v2::work_context::WorkAuthorityRef>,
    /// Task ID — surfaced in the prompt so the agent can call task management APIs.
    pub task_id: Option<String>,
    /// Execution ID — the current execution record for audit.
    pub execution_id: Option<String>,
    /// Exact already-committed stateless segment authorized by a claimed
    /// placement-retry timer. Server-only; ordinary launches leave it empty.
    pub stateless_resume_source_segment: Option<String>,

    /// Immutable timer generation paired with
    /// `stateless_resume_source_segment`. Both values are server-only and must
    /// match the durable placement continuation before an exact retry may
    /// reactivate the execution.
    pub stateless_retry_due_at: Option<DateTime<Utc>>,
    /// Root execution ID for rerun-level task output accumulation.
    pub root_execution_id: Option<String>,
    /// Task output projection mode for this run.
    pub task_output_mode: Option<TaskOutputMode>,
    /// API port for the magician HTTP server — used to construct API URLs in prompts.
    pub api_port: Option<u16>,
    /// Agent ID — used to populate delegation targets when no trust context is provided.
    pub agent_id: Option<String>,
    /// Override success criteria for this run.
    pub success_criteria: Option<String>,
    /// Exact or lane-based per-run LLM routing overrides.
    pub llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,
    /// Immutable execution-local routing overlay, kept separate from the
    /// active owner's refreshable defaults. Eval/diagnostic routes populate
    /// this field so owner hydration, handover, and hot reload cannot silently
    /// send later calls back to production routing.
    pub execution_llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,
    /// Runtime-only provider/disclosure fence for one server-admitted app
    /// workflow. It is rebuilt from current registry authority on every
    /// launch/resume and is never written into pause or task state.
    pub app_disclosure_guard: Option<magicllm::LlmDisclosureGuard>,
    /// Owner-minted provenance for the sole exact callable-agent result
    /// declaration. It is runtime-only and never deserialized from task or API
    /// state; ordinary protected app workflows leave it empty.
    pub app_agent_tool_result_declaration:
        Option<crate::magician_v2::apps::agent_capability::AppAgentToolResultDeclarationPermit>,
    /// Initial browser URL to seed before the first observation.
    pub initial_url: Option<String>,
    /// Pre-resolved merged agent tools (own + delegate, with blacklist applied).
    /// Authoritative source for what tools the planner should know about.
    pub merged_agent_tools: Vec<runtime_core::ToolInfo>,
    /// Preserve the caller's deliberately narrowed initial direct-tool set
    /// across canonical owner-profile hydration. Used by one-pack chat runs;
    /// ordinary direct/task executions leave this false.
    pub preserve_initial_tool_scope: bool,
    /// Per-tool parameter prefix deny patterns.
    /// Outer key = tool name, inner key = parameter name, values = denied prefixes.
    pub denied_tool_params:
        std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    /// Expected artifact declarations from the plan step or delegation request.
    /// When non-empty, these override the default declarations on the `AgenticContext`.
    /// When empty (default), the orchestrator falls back to
    /// `default_expected_artifact_declarations()`.
    pub expected_artifact_declarations: Vec<crate::magician_v2::agents::types::ArtifactDeclaration>,
    /// Spend token IDs propagated into this execution.
    pub spend_token_ids: Vec<String>,
    /// Active execution owner for execution-owned runtime.
    pub active_owner_agent_id: Option<String>,
    /// Suspended owners that authorize the current active owner.
    pub owner_stack: Vec<String>,

    /// True when this direct execution is serving a chat turn.
    ///
    /// Chat-inline executions may answer with assistant text directly when no
    /// runtime tool is needed. Automation/task executions keep strict tool-call
    /// behavior so they cannot silently stop with prose.
    pub chat_inline: bool,

    /// Optional active-work budget supplied by the caller.
    ///
    /// This is a soft loop-boundary budget, not a cancellation timeout: once
    /// elapsed, the current provider/tool operation may finish, the executor
    /// starts no subsequent iteration, and normal result synthesis continues.
    pub work_budget_secs: Option<u64>,

    /// Developer-Mode plan-mode gate. The orchestrator sets this from
    /// the thread's `plan_mode` column so the executor gates every
    /// non-read-only action behind an approval prompt. Defaults to
    /// false; existing chat-mode threads pay zero overhead.
    pub plan_mode_enabled: bool,

    /// Accept-in-scope permission overlay. In-tree file edits skip
    /// tool-authorization HITL. Destructive, sandbox, and out-of-tree
    /// paths still prompt.
    pub accept_in_scope_enabled: bool,

    /// agent-browser `--session` id override. When set, this
    /// execution's headed browser attaches to this existing session
    /// instead of spawning a fresh Chrome window keyed on
    /// `execution_id`. Plumbed from `GoalTaskOptions.browser_session_id_override`
    /// via `trigger_goal_awaitable_with_scope_and_overrides`, then
    /// forwarded to `PrimitiveExecCtx.browser_session_id_override`
    /// which `primitive/dispatch.rs` reads at
    /// `AgentBrowserSession::new` time. `None` preserves the legacy
    /// per-execution session derivation.
    pub browser_session_id_override: Option<String>,

    /// Per-execution prompt identity (agent kind + base persona +
    /// source-agent provenance). When set, this overrides any
    /// `prompt_identity` already on the built `AgenticContext` from
    /// `trust_context`.
    ///
    /// **Why this exists:** the chat-inline delegate path runs through
    /// `execute_agentic_direct_with_outcome` with `trust_context: None`,
    /// which defaults the context's `prompt_identity` to `None`. Without
    /// this override, the delegated worker (mac-operator, etc.) runs
    /// without its persona — only the generic "agentic executor" system
    /// prompt reaches the LLM, and the agent's DECISION-FIRST guidance
    /// (e.g. "system control → osascript") is silently dropped.
    ///
    /// Populated by `build_direct_agent_overrides` from the loaded
    /// `AgentDefinition.persona`, applied in
    /// `execute_agentic_direct_with_outcome` after the override block.
    pub prompt_identity: Option<crate::magician_v2::prompt_identity::PromptIdentityContext>,
    /// STEP 3 — single-execution-context pipeline roster. When `Some` and
    /// non-empty, `execute_agentic_direct_with_outcome` runs these stages
    /// SEQUENTIALLY within the ONE built `ExecutionRun` (swapping the active
    /// owner/persona/trust per stage via `execute_pipeline_single_context`)
    /// instead of a single agentic cycle — no spawned child per stage, no
    /// reconcile round-trip. `None` (the default) preserves the single-cycle
    /// behavior for every existing caller.
    pub pipeline_stages: Option<Vec<crate::magician_v2::execution::PipelineStage>>,

    /// Option B — run single-target delegations IN-CONTEXT (no child spawn) for
    /// this execution. Set `true` by the service for chat `orchestrate_pipeline`
    /// tasks when `pipeline_single_context_enabled`. `false` (default) preserves
    /// the spawn-child path for every existing caller.
    pub delegate_single_in_context: bool,

    /// RCA fix #5 — VibeDev coding-coordinator signal. `true` when this run is a
    /// VibeDev *Build* run (the `vibedev` thread/tag, NOT a `plan`/Discuss run)
    /// that MUST engage the coding pipeline. Derived server-side from the manifest
    /// (`is_vibedev_coding_build_run`) at run creation, then copied onto the built
    /// `AgenticContext` so the executor (#2 sandbox arming),
    /// `refinement_gaps_if_warranted` (#3), `CatalogBuildContext` (#4), and the
    /// artifact_v2 terminal verdict (#1) can read it without re-loading the task.
    /// `false` (default, via `#[derive(Default)]`) for every other caller.
    pub coding_coordinator_run: bool,
}

/// Shared-runtime identity and authority ceiling for the current autonomous
/// owner frame. The mutable family set itself stays in the bounded shared L3
/// store; this handle contains no credentials or prompt context.
#[derive(Debug, Clone)]
pub struct AutonomousSurfaceRuntimeBinding {
    pub key: crate::magician_v2::execution::flat_loop::SurfaceWorkingSetKey,
    pub authority_revision: String,
    pub authorized_tool_names: std::collections::HashSet<String>,
}

/// Configuration for an agentic execution run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineLoopStateSegment {
    /// Zero-based position in the orchestrated pipeline.
    pub stage_index: usize,
    /// One-based attempt number for this stage.
    pub attempt: usize,
}

#[derive(Debug, Clone)]
pub struct AgenticContext {
    /// The goal to achieve (from step.task)
    pub goal: String,

    /// Success criteria (from step.expected_output)
    pub success_criteria: String,

    /// Hint action from planner (optional starting point)
    pub hint_action: Option<ExecutableAction>,

    /// Maximum iterations before giving up
    pub max_iterations: usize,

    /// Maximum provider-reported LLM tokens for this execution.
    pub max_tokens_per_cycle: Option<u64>,

    /// Optional active-work budget for this logical run.
    ///
    /// Checked only between agentic operations. It never cancels an in-flight
    /// provider/tool call, survives pause/resume, and does not bound terminal
    /// collation/finalization.
    pub work_budget_secs: Option<u64>,

    /// Active work charged by earlier pause/resume segments of this run.
    pub work_budget_consumed_ms: u64,

    /// Monotonic start for the current in-memory active-work segment. Runtime
    /// only: durable pauses persist the accumulated milliseconds above.
    pub work_budget_segment_started_at: Option<std::time::Instant>,

    /// Cumulative charged tokens, including usage restored after a pause.
    pub llm_tokens_used: u64,

    /// Number of iterations completed before this execution segment began.
    ///
    /// Pause/resume starts a fresh in-memory loop, but prompt/debug artifacts
    /// should keep stable cumulative numbering for the same execution.
    pub iteration_offset: usize,

    /// Runtime-only loop-state address discriminator for one pipeline attempt.
    ///
    /// Pipeline stages intentionally retain the root `execution_id` so events,
    /// artifacts, screenshots, and execution-directory writes stay attached to
    /// the registered root run. The stateless journal cannot use that same bare
    /// id for every stage and retry, however: a completed attempt leaves a
    /// terminal record that the next attempt must never adopt. This field is
    /// folded only into `loop_state_address`; it is not a routing identity.
    pub pipeline_loop_state_segment: Option<PipelineLoopStateSegment>,

    /// One-shot exact segment selected by a stateless recovery pause.
    ///
    /// Ordinary user-input resumes intentionally start a new generation. An
    /// indeterminate-effect resume is different: the unresolved effect ledger
    /// and Apply cursor live on the segment that asked the question, so the
    /// resumed worker must reclaim that exact segment before it can honour an
    /// operator's adopt/refire decision. Runtime-only and consumed when the
    /// stateless arm is constructed.
    pub stateless_resume_source_segment: Option<String>,

    /// Generic boot recovery mode. Runtime ownership contention in this mode
    /// is a typed no-projection deferral, never a placement sleep or failure.
    pub interrupted_runtime_recovery: bool,
    /// Durable base admission held through the first loop claim/seed.
    pub interrupted_runtime_recovery_admission_token: Option<String>,
    pub interrupted_runtime_recovery_source_revision: Option<u64>,

    /// Runtime-only authority for one HMAC-verified Delegation checkpoint to
    /// create its declared successor when that exact segment is absent.
    ///
    /// PlacementRetry and indeterminate-effect recovery also name exact
    /// segments, but must fail closed when those sources are missing. Keeping
    /// this permission separate prevents an ordinary exact resume from
    /// accidentally acquiring successor-creation authority.
    pub stateless_resume_may_seed_declared_successor: bool,

    /// Exact loop-state segment currently driven by this in-memory invocation.
    ///
    /// Set by the stateless arm after consuming the optional source override.
    /// Event buffering and pause construction read this value so their address
    /// cannot drift from the store key actually being advanced. A cloned child
    /// or refinement invocation replaces it when its own arm is constructed.
    pub stateless_active_segment: Option<String>,

    /// Maximum consecutive identical actions before loop detection
    pub max_repeated_actions: usize,

    /// What to do when agent cannot proceed or detects a loop.
    /// `AskUser` pauses and asks for help. `Fail` keeps existing terminal behavior.
    pub on_failure: OnFailureMode,

    /// Initial URL to navigate to when creating browser session.
    /// Avoids the about:blank delay by opening directly on the target page.
    /// Extracted from the goal if it contains a URL.
    pub initial_url: Option<String>,

    // === Observability Fields ===
    /// Legacy execution identity carrier retained only while compat seams exist.
    pub legacy_execution_id: Option<String>,

    /// Plan ID for event emission
    pub plan_id: Option<String>,

    /// Step ID for event emission
    pub step_id: Option<String>,

    /// Canonical artifact store key for this execution.
    ///
    /// This lets direct agentic runs and plan-graph step executions share the
    /// same download persistence path even when legacy execution routing is still present.
    pub artifact_chain_id: Option<String>,

    // === Agent Routing Fields (TRUE_AGENTS Phase 0) ===
    /// Agent ID — set when this context is running inside an agent cycle.
    pub agent_id: Option<String>,

    /// Goal ID — the specific goal the agent is pursuing.
    pub goal_id: Option<String>,

    /// Cycle ID — the specific observe-decide-execute cycle within the goal.
    pub cycle_id: Option<String>,

    /// Trust level applied for pre-dispatch action gating.
    ///
    /// `None` means trust gating is disabled for this execution context.
    pub trust_level: Option<String>,

    /// Absolute path to the scoped `agent_runtime/system/trust_policies.yaml`
    /// file for this execution scope.
    ///
    /// Required when `trust_level` is set.
    pub trust_policies_path: Option<PathBuf>,

    /// Optional preloaded trust policy enforcer for this execution scope.
    ///
    /// When set, executor dispatch can reuse this parsed policy and skip reparsing
    /// `trust_policies_path` on hot paths (for example manual triggers).
    pub preloaded_trust_enforcer: Option<Arc<TrustPolicyEnforcer>>,

    /// Approval rules that require explicit human confirmation before dispatch.
    ///
    /// Empty means no extra approval gating for this execution context.
    pub approval_rules: Vec<ApprovalRule>,

    /// One-time action payloads that were explicitly approved by a human.
    ///
    /// Used to bypass re-triggering the same approval gate exactly once after
    /// resume from `WaitingForConfirmation`, but only for the owner that
    /// originally received the approval.
    pub approved_confirmation_actions: Vec<ApprovedConfirmationAction>,

    /// Current execution owner for execution-owned runtime.
    pub active_owner_agent_id: Option<String>,

    /// Suspended owners that authorize the current active owner.
    pub owner_stack: Vec<String>,

    /// Exact invocation authority for each suspended owner, positionally
    /// aligned with `owner_stack`. `None` preserves compatibility for legacy
    /// owner frames that predate typed surface authorization.
    pub owner_invocation_stack: Vec<Option<crate::magician_v2::agents::AgentInvocationContext>>,

    /// Suspended parent frames for same-execution nested work. Persisted in
    /// pause state and unwound after the resumed nested outcome terminates.
    pub continuation_frames: Vec<AgenticContinuationFrame>,

    // === Input Tracking Fields (Elicitation Unification) ===
    /// Pending inputs that need to be resolved for this step.
    /// Populated from UnresolvedInput during planning → execution handoff.
    pub pending_inputs: Vec<PendingInput>,

    /// Resolved input values provided by the user.
    /// Key is the input ID, value is the JSON value provided.
    /// This persists across pause/resume cycles.
    pub resolved_inputs: HashMap<String, Value>,

    /// Sensitivity decided when each resolved input was collected, keyed by
    /// input ID (or `"<input id>.<field id>"` for a form field). Read by every
    /// later rendering and vaulting decision so a value's name never has to
    /// carry the decision. Persists with the continuation frame.
    pub resolved_input_sensitivity:
        HashMap<String, crate::magician_v2::user_requests::SensitiveKind>,

    /// The parameter ID currently being asked (for tracking during pause/resume).
    /// Used to associate user's response with the correct pending input.
    pub asking_for_parameter: Option<String>,

    /// Optional model override for agent-scoped LLM routing.
    ///
    /// When set, decision calls use this model instead of operation defaults.
    pub llm_model_override: Option<String>,

    /// Optional per-operation provider/model overrides for this agent cycle.
    pub llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,

    /// Immutable execution-local routing overlay. `llm_routing_overrides` is
    /// the current effective merge of the active owner's defaults with this
    /// overlay; owner refresh may replace the former but never this field.
    pub execution_llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,

    /// Runtime-only app disclosure guard. App workflows rebuild this on every
    /// launch/resume; generic executions leave it empty. It is deliberately
    /// excluded from durable pause state because mutable provider authority
    /// must be re-attested instead of replayed after a process boundary.
    pub app_disclosure_guard: Option<magicllm::LlmDisclosureGuard>,

    /// Metadata-only marker restored from a guarded pause. It can reject a
    /// missing/mismatched fresh guard but can never mint one.
    pub app_disclosure_checkpoint: Option<magicllm::LlmDisclosureCheckpoint>,

    /// Optional prompt identity context (persona + bounded autonomous controls).
    pub prompt_identity: Option<PromptIdentityContext>,

    /// Tree recursion depth for dynamic sub-goals.
    /// Prevents infinite descent; checked against `max_delegation_depth`.
    pub depth: usize,

    /// When `true`, a SINGLE-target `delegate_to_agent` decision runs IN-CONTEXT
    /// on THIS execution (owner swap + inline `execute_agentically`, deliverable
    /// threaded into `prior_environment_knowledge`) instead of spawning a child
    /// `ExecutionRun` + reconcile (Option B). Set by the service for chat
    /// `orchestrate_pipeline` tasks when `pipeline_single_context_enabled`. Cloned
    /// into in-context sub-agents so nested single delegations also stay in-context
    /// (depth-bounded). Multi-target (parallel) delegations always spawn children.
    pub delegate_single_in_context: bool,

    /// RCA fix #5 — VibeDev coding-coordinator signal (see
    /// `AgenticContextOverrides::coding_coordinator_run`). `true` only for a
    /// VibeDev Build run that must engage the coding pipeline. Read by the
    /// executor (#2 sandbox arming), `refinement_gaps_if_warranted` (#3),
    /// `CatalogBuildContext` (#4), and the artifact_v2 terminal verdict (#1).
    /// Cloned forward by `build_refinement_context` (same agent, same intent);
    /// defaults to `false`.
    pub coding_coordinator_run: bool,

    /// Resume-only carry: the live conversation restored from the pause blob
    /// (`AgenticPauseState::live_messages`). Cold resumes namespace synthetic
    /// ids by generation; provider-checkpoint resumes preserve the provider's
    /// exact tool-call ids so its new tool-result suffix still correlates. The executor seeds the
    /// fresh `ExecutionHistory` from this exactly once at run start
    /// (`seed_resumed_history`); empty for non-resume runs. Runtime-only —
    /// never serialized.
    pub resume_live_messages: Vec<magicllm::prelude::LLMMessage>,

    /// Resume-only provider continuation checkpoint. Runtime-only: the durable
    /// source is `AgenticPauseState::continuation_response_id`.
    pub resume_response_id: Option<String>,

    /// Image cohort associated with `resume_response_id`.
    pub resume_last_has_images: Option<bool>,

    /// Loop-protective state carried across a pause, drained into the
    /// loop's own bundle at the top of `execute_agentically_inner`.
    ///
    /// Resume-only, exactly like the three fields above, and taken rather
    /// than cloned for the same reason: a context cloned forward into a
    /// nested continuation segment must not re-seed it, or the child would
    /// inherit the parent's retry budgets and cycle history as its own.
    pub resume_loop_protective: Option<LoopProtectiveState>,

    /// Maximum delegation/sub-goal depth allowed for this agent.
    /// Sourced from the agent definition's `constraints.coordination.max_delegation_depth`.
    /// Defaults to 2. The depth check in the executor compares `depth >= max_delegation_depth`.
    pub max_delegation_depth: u8,

    /// Available delegation targets for this agent. Empty = delegation not available.
    pub delegation_targets: Vec<super::delegation_dispatch::DelegationTarget>,

    /// Pre-resolved merged agent tools (own + delegate, with whitelist/blacklist applied).
    /// Authoritative tool list for both planning (name+description) and execution
    /// (with optional per-tool enrichment via `focused_tool`).
    pub merged_agent_tools: Vec<runtime_core::ToolInfo>,

    /// Whether initial owner hydration must retain the caller-supplied direct
    /// tool scope instead of widening to the owner's complete profile.
    pub preserve_initial_tool_scope: bool,

    /// Per-tool parameter prefix deny patterns (from `AgentDefinition.denied_tool_params`).
    pub denied_tool_params:
        std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,

    /// Whole-tool deny set for this agent — the union of the definition's
    /// `excluded_tools` + `denied_tools`. The YAML deny lists only filter the
    /// agent's *explicit* tools (`resolved_tools`); this set is ALSO consulted
    /// when injecting the `UNIVERSAL_BACKEND_PACKS`, so a denied universal tool
    /// (e.g. `shell`) is stripped from the catalog too — and rejected at
    /// execution. Populated by the orchestrator from the agent definition;
    /// empty = nothing denied.
    pub denied_capability_names: Vec<String>,

    /// Plane-delegated launch attenuation (plane Task 6b). Launch-pinned so
    /// agent-profile application MERGES rather than replaces: the families a
    /// plane grant can never reach, applied on top of the agent's own deny
    /// list and inherited by spawned children through
    /// `denied_capability_names`.
    pub plane_denied_capability_names: Vec<String>,
    /// `Some` narrows this run's usable catalog to the intersection with
    /// these names — a narrow grant must stay narrow through the `run_task`
    /// hop. `None` keeps the agent's surface (minus denied).
    pub plane_allowed_capability_names: Option<Vec<String>>,
    /// Who thinks during this run. Set at launch from the grant's harness
    /// (Variant 2); the process snapshot is only the fallback. `"magician"`
    /// is a deliberate pin, never a silent fallback.
    pub harness_engine: Option<String>,
    /// The engine, harness model, and Pi profile fixed when this run
    /// launched. Composition sets it on every run (with `harness_engine` to
    /// its engine), so a Settings switch never moves a run already going.
    /// On a chat harness turn's grant it is the chat's pin
    /// (`chat_turn_run_pin`), which a run the turn launches inherits.
    pub run_engine_pin: Option<crate::magician_v2::execution::plane::RunEnginePin>,
    /// Per-run cost ceiling. Lowered by plane-delegated launch attenuation
    /// (Task 6b); the effective cap at the cost checks is the minimum of
    /// this and the configured global cap.
    pub max_cost_usd: Option<f64>,

    /// Browser transports the active owner may use, copied verbatim from its
    /// `AgentDefinition::browser_transports`. **Empty means all three** — an
    /// opt-in ceiling, so an agent that declares nothing keeps today's
    /// behaviour. Replaced atomically on every owner transition alongside the
    /// deny state, so a delegate cannot inherit its parent's wider ceiling.
    /// Threaded into flat dispatch via
    /// `PrimitiveExecCtx::with_browser_transports` and applied by
    /// `BrowserTransportCeiling`.
    pub browser_transports: Vec<String>,

    /// Invocation boundary resolved from the active owner's definition.
    /// Owner transitions replace this atomically with trust and deny state.
    pub invocation_policy: crate::magician_v2::agents::AgentInvocationPolicy,

    /// Exact scoped definition for the active owner. Present on production
    /// executions after owner-profile hydration; detached unit contexts may
    /// leave it empty and use the legacy fail-closed catalog projection.
    /// Immutable definition for the active owner frame.
    ///
    /// Shared so action/state scheduler snapshots clone one pointer instead of
    /// recursively cloning the full definition on an already-deep execution
    /// worker stack.
    pub owner_definition: Option<std::sync::Arc<crate::magician_v2::agents::AgentDefinition>>,

    /// Exact server-authenticated surface inherited by the active owner frame.
    /// Product sub-runs, delegated children and handovers install this value;
    /// owner transitions clear it before installing their own fresh context.
    pub invocation_context_override: Option<crate::magician_v2::agents::AgentInvocationContext>,

    /// Work authority this execution runs under (§4.2c carrier).
    /// Unlike `invocation_context_override` above, the work binds the
    /// execution — not one owner frame — so owner transitions, handovers and
    /// in-context delegation must all preserve it; children inherit it
    /// verbatim from the parent's durable record. Never deserialized from
    /// task/API/model input (§4.2c row 5).
    ///
    /// # Why this is the generic carrier and not the engagement one
    ///
    /// It carries **every** arm of
    /// [`crate::magician_v2::work_context::WorkContextKind`], exactly as the
    /// durable `ExecutionRun.work_authority` it is hydrated from does. When
    /// this field was `EngagementAuthorityRef`, a program-scoped execution had
    /// no slot to arrive in: the narrowing conversion refuses the `Program`
    /// arm, so the run reached the dispatch boundary carrying nothing and every
    /// outward act it performed was filed under no work axis at all.
    ///
    /// An **`Engagement` arm is enforced exactly as it always was**: the
    /// dispatch boundary reads the live ceiling from the roster and refuses on
    /// revoke, expiry or an unreadable store. A **`Program` arm names the work
    /// and narrows nothing** — no roster owns programs, so there is no ceiling
    /// to read — and callers that need the roster ceiling must ask for it
    /// through [`AgenticContext::engagement_ceiling_authority`] rather than
    /// converting this field themselves.
    pub work_authority: Option<crate::magician_v2::work_context::WorkAuthorityRef>,

    /// Digest of the immutable effective policy used for the current decision
    /// boundary. Persisted/logged by entry points that resolve a snapshot.
    pub policy_snapshot_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,

    /// The newest decision's effective policy snapshot itself: its
    /// `provider_specs` are the tools this agent was authorized to see. The
    /// step judges read the steps a surface's tool offers from it.
    pub policy_snapshot: std::sync::Arc<
        std::sync::Mutex<Option<std::sync::Arc<super::EffectiveToolPolicySnapshot>>>,
    >,

    /// Exact callable-name ceiling from the same immutable policy snapshot.
    /// `None` means the current decision has not resolved policy yet; an empty
    /// set means it resolved fail-closed. Deferred names enter this set only
    /// after `tool_search` promotes them into a later provider decision.
    pub policy_dispatch_tool_names:
        std::sync::Arc<std::sync::Mutex<Option<std::collections::BTreeSet<String>>>>,

    /// Exact server-side metadata-action grants from the active snapshot.
    /// These actions are intentionally absent from the provider catalog and
    /// cannot be used for ordinary explicit tool dispatch.
    pub policy_implicit_tool_names:
        std::sync::Arc<std::sync::Mutex<Option<std::collections::BTreeSet<String>>>>,

    /// Registry revision paired with `tool_index` for this execution scope.
    /// It is installed from the same immutable L0 snapshot used by dispatch.
    pub scope_capability_revision: Option<String>,

    /// Rollout controls and shared L2/L3 state installed by the executor
    /// factory. Detached tests default to the fully-off compatibility path.
    pub agent_surface_runtime_config: crate::config::AgentSurfaceRuntimeConfig,
    pub surface_plan_cache: Option<crate::magician_v2::execution::flat_loop::SurfacePlanCache>,
    pub surface_working_sets:
        Option<crate::magician_v2::execution::flat_loop::SurfaceWorkingSetStore>,

    /// Per-execution scratch that lives on the context.
    ///
    /// Eight loose interior-mutable fields until 2026-08-26. They keep their
    /// individual `Arc`s inside the group, and here that is sharper than
    /// elsewhere: this context is CLONED per owner transition and per inline
    /// delegation, and three of these are documented as shared by those clones
    /// so the run keeps one ledger rather than one per owner. See
    /// [`crate::magician_v2::execution::agentic::run_loop::context_scratch::ContextScratch`].
    pub scratch: crate::magician_v2::execution::agentic::run_loop::context_scratch::ContextScratch,

    /// Whether planner-visible provisioned secret entry points should be exposed
    /// in the decision schema for this execution.
    ///
    /// The executor remains the hard boundary, but the prompt/schema layer must
    /// not advertise `credential_id` / `credential_token` on hosts where
    /// provisioned vault storage is disabled.
    pub provisioned_secret_access_enabled: bool,

    /// Legacy source-agent delegation setting retained for pause/profile
    /// compatibility. It is no longer an upper clamp on child work budgets.
    pub delegation_timeout_secs: u64,

    /// Spend token IDs propagated from the delegating agent.
    /// When non-empty, the executor uses these tokens for budget-gated execution.
    pub spend_token_ids: Vec<String>,

    /// Memory tier definitions for this agent, enabling mid-execution tier access.
    pub tier_definitions: Vec<MemoryTierDefinition>,

    /// Rendered memory context from the delegating agent (the agent that owns the task),
    /// injected when `providing_agent_id != task.agent_id` (delegated step).
    /// Contains semantic + user-scoped memory tiers formatted as a prompt section.
    pub delegating_agent_context: Option<String>,

    /// Pre-rendered durable user-tier memory for prompt injection.
    pub prior_user_memory: Option<String>,

    /// The owner's taste profile, frozen for the life of this run.
    ///
    /// Resolved once where retrieval freezes — a run that started under
    /// profile v12 keeps v12 across resume and steer, so a mid-run edit never
    /// shifts the ground under a live execution. `None` means no profile note
    /// exists, the feature is off, or the notes provider could not be read;
    /// all three render nothing, and prompt assembly never fails for it.
    pub taste_profile: Option<crate::magician_v2::taste_profile::TasteProfileSnapshot>,

    /// Pre-rendered durable agent-tier memory for prompt injection.
    pub prior_agent_memory: Option<String>,

    /// Pre-rendered goal-scoped agent memory for prompt injection.
    pub prior_agent_goal_memory: Option<String>,

    /// Pre-rendered active reusable procedures selected for the current goal.
    /// Procedures are operational guidance, so they are kept separate from
    /// semantic memory tiers while still travelling with the same prompt context.
    pub prior_procedure_memory: Option<String>,

    /// Monotonic semantic-context checkpoint for autonomous task execution.
    ///
    /// Retrieval is intentionally tied to meaningful task boundaries (run
    /// start, resume/user input, operator steer, completed/failed step, owner
    /// transition, or policy revision), not to every internal tool-result
    /// iteration. `task_prompt_context_hydrated_generation` is advanced even
    /// when a checkpoint produces no matches or a fail-open retrieval error so
    /// the same checkpoint cannot repeatedly pay for identical lookups.
    pub task_prompt_context_generation: u64,
    pub task_prompt_context_hydrated_generation: Option<u64>,
    pub task_prompt_context_checkpoint_hint: Option<String>,
    pub task_prompt_context_hydration_count: u64,

    /// Procedure skills the active agent has allowlisted via its `tools:`
    /// block (resolved through `skills::agent_procedure_skill_catalog`).
    /// Populated by `apply_owner_execution_profile` from
    /// `OwnerExecutionProfile.available_procedure_skills`. Drives two
    /// prompt surfaces: (1) whether `activate_skill` / `deactivate_skill`
    /// appear in the outer-loop native catalog, (2) the
    /// `## AVAILABLE PROCEDURE SKILLS` user-prompt block listing
    /// (name, description) so the LLM knows what's activatable.
    pub available_procedure_skills: Vec<(String, String)>,

    /// Pre-rendered environment knowledge section injected at execution start.
    ///
    /// Populated by loading the `environment_knowledge` tier and filtering entries
    /// whose `environment_key` matches URLs/commands extracted from the goal.
    /// Rendered as a "What you already know about this environment" prompt section.
    pub prior_environment_knowledge: Option<String>,

    /// Cached environment knowledge entries loaded at execution start.
    /// Reused by lazy domain-change lookups to avoid repeated disk I/O.
    pub cached_environment_entries: Vec<serde_json::Value>,

    // === Autonomous Execution Scoping ===
    /// Allowed built-in action types (e.g. "browser", "file", "bash", "http").
    /// When Some, the decision schema filters the `action_type` enum and the
    /// executor rejects actions not in this list at runtime.
    /// None means all action types are allowed (backward compatible).
    pub allowed_action_types: Option<Vec<String>>,

    /// Maximum number of tasks the agent can spawn in a single cycle.
    /// Enforced in the CreateTask handler. None means no limit.
    pub max_spawned_tasks: Option<u32>,

    /// Principal identity for task tenancy propagation.
    /// When set, CreateTask inherits this instead of deriving from agent_id.
    pub principal: Option<String>,

    /// Workspace for task tenancy propagation.
    /// When set, CreateTask inherits this instead of hardcoded "default".
    pub workspace: Option<String>,

    // === Task Execution Context ===
    /// Task ID — set when this execution is running on behalf of a task.
    /// Surfaced in the prompt so the agent can call task management APIs (e.g. `/defer`).
    pub task_id: Option<String>,

    /// Chat session that originated this task, when any.
    ///
    /// This is durable analytics lineage rather than an execution-routing key:
    /// autonomous tasks legitimately leave it empty, while chat-created tasks
    /// hydrate it from `TaskManifest.chat_session_id` before their first model call.
    pub chat_session_id: Option<String>,

    /// Execution ID — the current execution record for audit.
    pub execution_id: Option<String>,

    /// Root execution ID for rerun-level task output accumulation.
    pub root_execution_id: Option<String>,

    /// Task output projection mode for this run.
    pub task_output_mode: TaskOutputMode,

    /// Task-scoped persisted state from prior runs.  Pre-loaded by orchestrator,
    /// injected into prompt as `{task_state_section}`.
    pub task_state: Option<String>,

    /// API port for the magician HTTP server. Used to construct API URLs in prompts.
    pub api_port: Option<u16>,

    // === Runtime Context Identity Fields ===
    /// Stable goal hash used to scope runtime artifacts for this execution.
    /// This remains fixed across pause/resume even if `goal` is augmented with
    /// continuation or user-input context.
    pub stable_goal_hash: Option<String>,

    /// Base storage root for execution-scoped runtime artifacts and prompt dumps.
    /// Defaults from `MAGICIAN_STORAGE_PATH` when present, otherwise falls back
    /// to the standard runtime storage root.
    pub storage_base_path: PathBuf,

    /// Tracks how many times this execution has been continued via escalation.
    /// Flows from `AgenticPauseState` on resume and is captured back into
    /// new pause states via `build_full_pause_state`.
    pub continuation_count: usize,

    /// Expected artifact declarations from the plan step or delegation request.
    /// Used to enrich execution artifacts with `render_hints` and `artifact_type`
    /// from the planning layer before returning them in `AgenticOutcome::Success`.
    pub expected_artifact_declarations: Vec<crate::magician_v2::agents::types::ArtifactDeclaration>,

    /// True when this execution backs an inline chat turn.
    ///
    /// The native tool-call adapter uses this to allow `tool_choice:auto` and
    /// text-only terminal answers for chat while preserving strict tool calls
    /// for automation flows.
    pub chat_inline: bool,

    /// Developer-Mode plan-mode gate (Phase 5 of
    /// `docs/plans/2026-05-13-developer-mode-workbench.md`).
    ///
    /// When `true`, the executor pauses for human approval before each
    /// non-read-only action. Sourced from the thread's `plan_mode`
    /// column at orchestrator wiring time. Defaults to `false` so
    /// existing chat-mode executions never see the gate.
    pub plan_mode_enabled: bool,

    /// Accept-in-scope permission overlay. In-tree file edits skip
    /// tool-authorization HITL. Destructive, sandbox, and out-of-tree
    /// paths still prompt.
    pub accept_in_scope_enabled: bool,

    /// Refinement-pass index (tactical pattern T1 multi-pass refinement). `0` for
    /// the original execution; the wrapper bumps this each time it
    /// commissions a focused refinement pass. The bound enforced by
    /// `MAX_REFINEMENT_PASSES` (in executor.rs) caps total passes so a
    /// pathological "refinement loop" can't burn iterations forever.
    /// Carrying this on the context lets the wrapper see how deep it
    /// is without out-of-band state.
    pub refinement_pass_index: u32,

    /// Keep the agent's CDP connection and agent-browser daemon alive
    /// after this execution terminates, so a follow-up execution can
    /// attach to the same Chromium with its full session state
    /// (auth, cookies, scroll, form contents) intact.
    ///
    /// Use for multi-execution chat-pack flows where each LLM turn
    /// reuses the same authenticated browser, or for paused executions
    /// that will resume on the same page.
    ///
    /// Do NOT use this just because a human will inspect the page —
    /// the CDP attachment keeps the agent in control and prevents the
    /// user from closing the window cleanly. Use
    /// `keep_browser_window_open` for that.
    ///
    /// Default `false` — terminal outcomes close the daemon + window
    /// so resources don't leak. Pause-shaped outcomes
    /// (`WaitingForUser`, resumable `MaxIterationsReached`,
    /// `WaitingForChildren`, etc.) NEVER close regardless of this
    /// flag, so resume always works.
    ///
    /// Renamed 2026-05-24 from `keep_browser_session_alive`. The old
    /// name is still accepted as an alias on the LLM-facing tool
    /// parameter for one release.
    pub keep_browser_cdp_connection_alive: bool,

    /// Hand the agent-browser-driven Chromium off to the user as a
    /// standalone window. The runtime detaches the CDP socket (without
    /// asking Chromium to close), the daemon exits, and Chromium
    /// becomes a normal user-owned browser window: the user can
    /// interact freely and close it cleanly when done. No agent-side
    /// process stays running afterward.
    ///
    /// Use when a human will want to inspect the post-run page state
    /// (SoTA test Pass/Fail evidence, generated form review, search
    /// result reading). Default `false` — full cleanup closes the
    /// window.
    ///
    /// Mutually exclusive with `keep_browser_cdp_connection_alive`:
    /// if both are true, `keep_browser_cdp_connection_alive` wins
    /// (the runtime preserves CDP rather than detach).
    ///
    /// Routing: the outer-loop cleanup invokes
    /// `agent-browser close --keep-browser` (requires vendor version
    /// `0.29.0-Magician.1` or later). The daemon shuts down without
    /// sending `Browser.close` and without killing the Chrome process
    /// group; Chromium is reparented to launchd/init and stays visible.
    /// On older agent-browser binaries the flag is silently ignored.
    /// See `docs/plans/2026-05-24-browser-session-lifecycle-redesign.md`.
    pub keep_browser_window_open: bool,

    /// agent-browser `--session` id override. When set, this execution's
    /// headed browser attaches to this existing session instead of
    /// spawning a fresh Chrome window keyed on `execution_id`. Plumbed
    /// from `AgenticContextOverrides.browser_session_id_override`. Read
    /// by `primitive/dispatch.rs` at `AgentBrowserSession::new` time
    /// (via `PrimitiveExecCtx.browser_session_id_override`). `None`
    /// preserves the legacy per-execution session derivation.
    pub browser_session_id_override: Option<String>,

    /// Flat-mode tool index (Phase 3b). Built lazily in `execute_agentically`
    /// from the effective capability registry when `loop_mode == Flat`, then
    /// used at the decision seam to build the hot/deferred flat catalog. `None`
    /// in Primitive mode (never read) and until first populated.
    pub tool_index: Option<std::sync::Arc<crate::magician_v2::execution::flat_loop::ToolIndex>>,
}

impl Default for AgenticContext {
    fn default() -> Self {
        Self {
            goal: String::new(),
            success_criteria: String::new(),
            plane_denied_capability_names: Vec::new(),
            plane_allowed_capability_names: None,
            harness_engine: None,
            run_engine_pin: None,
            max_cost_usd: None,
            hint_action: None,
            max_iterations: 4000,
            max_tokens_per_cycle: None,
            work_budget_secs: None,
            work_budget_consumed_ms: 0,
            work_budget_segment_started_at: None,
            llm_tokens_used: 0,
            iteration_offset: 0,
            pipeline_loop_state_segment: None,
            stateless_resume_source_segment: None,
            interrupted_runtime_recovery: false,
            interrupted_runtime_recovery_admission_token: None,
            interrupted_runtime_recovery_source_revision: None,
            stateless_resume_may_seed_declared_successor: false,
            stateless_active_segment: None,
            max_repeated_actions: 3,
            on_failure: OnFailureMode::default(),
            initial_url: None,
            legacy_execution_id: None,
            plan_id: None,
            step_id: None,
            artifact_chain_id: None,
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            trust_level: None,
            trust_policies_path: None,
            preloaded_trust_enforcer: None,
            approval_rules: Vec::new(),
            approved_confirmation_actions: Vec::new(),
            active_owner_agent_id: None,
            owner_stack: Vec::new(),
            owner_invocation_stack: Vec::new(),
            continuation_frames: Vec::new(),
            pending_inputs: Vec::new(),
            resolved_inputs: HashMap::new(),
            resolved_input_sensitivity: HashMap::new(),
            asking_for_parameter: None,
            llm_model_override: None,
            llm_routing_overrides: None,
            execution_llm_routing_overrides: None,
            app_disclosure_guard: None,
            app_disclosure_checkpoint: None,
            prompt_identity: None,
            depth: 0,
            max_delegation_depth: 2,
            delegation_targets: Vec::new(),
            merged_agent_tools: Vec::new(),
            preserve_initial_tool_scope: false,
            denied_tool_params: std::collections::HashMap::new(),
            denied_capability_names: Vec::new(),
            // Empty: every transport. The ceiling is opt-in.
            browser_transports: Vec::new(),
            invocation_policy: crate::magician_v2::agents::AgentInvocationPolicy::default(),
            owner_definition: None,
            invocation_context_override: None,
            work_authority: None,
            policy_snapshot_id: std::sync::Arc::new(std::sync::Mutex::new(None)),
            policy_snapshot: std::sync::Arc::new(std::sync::Mutex::new(None)),
            policy_dispatch_tool_names: std::sync::Arc::new(std::sync::Mutex::new(None)),
            policy_implicit_tool_names: std::sync::Arc::new(std::sync::Mutex::new(None)),
            scope_capability_revision: None,
            agent_surface_runtime_config: crate::config::AgentSurfaceRuntimeConfig::default(),
            surface_plan_cache: None,
            surface_working_sets: None,
            scratch: Default::default(),
            provisioned_secret_access_enabled: true,
            delegation_timeout_secs: 300,
            spend_token_ids: Vec::new(),
            tier_definitions: Vec::new(),
            delegating_agent_context: None,
            prior_user_memory: None,
            taste_profile: None,
            prior_agent_memory: None,
            prior_agent_goal_memory: None,
            prior_procedure_memory: None,
            task_prompt_context_generation: 0,
            task_prompt_context_hydrated_generation: None,
            task_prompt_context_checkpoint_hint: None,
            task_prompt_context_hydration_count: 0,
            available_procedure_skills: Vec::new(),
            prior_environment_knowledge: None,
            cached_environment_entries: Vec::new(),
            allowed_action_types: None,
            max_spawned_tasks: None,
            principal: None,
            workspace: None,
            task_id: None,
            chat_session_id: None,
            execution_id: None,
            root_execution_id: None,
            task_output_mode: TaskOutputMode::Accumulate,
            task_state: None,
            api_port: None,
            stable_goal_hash: None,
            storage_base_path: crate::magician_v2::process_storage::runtime_root(),
            continuation_count: 0,
            expected_artifact_declarations: Vec::new(),
            chat_inline: false,
            plan_mode_enabled: false,
            accept_in_scope_enabled: false,
            refinement_pass_index: 0,
            keep_browser_cdp_connection_alive: false,
            keep_browser_window_open: false,
            browser_session_id_override: None,
            tool_index: None,
            delegate_single_in_context: false,
            coding_coordinator_run: false,
            resume_live_messages: Vec::new(),
            resume_response_id: None,
            resume_last_has_images: None,
            resume_loop_protective: None,
        }
    }
}

impl AgenticContext {
    /// The engagement whose **roster ceiling** applies to this execution.
    ///
    /// Answers one question and only that one: *which engagement's tool
    /// ceiling, `team[]` and liveness must this dispatch be checked against?*
    ///
    /// `None` for a `Program` carrier is a **true fact, not a swallowed
    /// refusal**: no roster owns programs, so there is no ceiling, no `team[]`
    /// and no revision to re-read. It is deliberately spelled as an exhaustive
    /// match rather than as `EngagementAuthorityRef::try_from(..).ok()` — that
    /// shape discards the refusal text for every arm at once, which is exactly
    /// how an unenforceable carrier comes to read as an absent one.
    ///
    /// A caller that needs **containment** (which corpus, which browser
    /// namespace — a partition, not a ceiling) must read
    /// [`AgenticContext::work_authority`] directly and decide per arm. A
    /// program is not "unbound" for those questions; it is a binding this
    /// build cannot yet express, which is a refusal rather than a pass.
    pub fn engagement_ceiling_authority(
        &self,
    ) -> Option<crate::magician_v2::engagements::EngagementAuthorityRef> {
        use crate::magician_v2::{
            engagements::EngagementAuthorityRef, work_context::WorkContextKind,
        };

        let carried = self.work_authority.as_ref()?;
        match &carried.work {
            WorkContextKind::Engagement(_) => EngagementAuthorityRef::try_from(carried).ok(),
            WorkContextKind::Program(_) => None,
        }
    }

    /// Open a new semantic retrieval checkpoint for the next provider
    /// decision. The hint is bounded because it is appended to the retrieval
    /// query, not retained as an unbounded execution transcript.
    pub fn advance_task_prompt_context_checkpoint(&mut self, hint: impl AsRef<str>) {
        self.task_prompt_context_generation = self.task_prompt_context_generation.saturating_add(1);
        let hint = hint.as_ref().trim();
        self.task_prompt_context_checkpoint_hint = (!hint.is_empty()).then(|| {
            const MAX_HINT_CHARS: usize = 1_024;
            hint.chars().take(MAX_HINT_CHARS).collect()
        });
        self.task_prompt_context_hydrated_generation = None;
    }

    /// Mark caller-seeded run-start memory/procedure context as the completed
    /// generation-zero checkpoint, including the legitimate empty-result case.
    pub fn mark_task_prompt_context_seeded(&mut self) {
        self.task_prompt_context_hydrated_generation = Some(self.task_prompt_context_generation);
    }

    /// Create a new context with goal and success criteria
    pub fn new(goal: impl Into<String>, success_criteria: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            success_criteria: success_criteria.into(),
            ..Default::default()
        }
    }

    /// Apply plane-delegated launch attenuation (Task 6b): pinned denial,
    /// catalog narrowing, and the run's harness engine.
    pub fn with_plane_attenuation(
        mut self,
        attenuation: Option<PlaneDelegationAttenuation>,
    ) -> Self {
        if let Some(attenuation) = attenuation {
            self.plane_denied_capability_names = attenuation.denied;
            self.plane_allowed_capability_names = attenuation.allowed;
            self.harness_engine = attenuation.harness_engine;
            // Ceilings only narrow: a grant lowers whatever limit is already
            // in force and never raises one.
            self.max_cost_usd = match (self.max_cost_usd, attenuation.max_usd) {
                (Some(existing), Some(grant)) => Some(existing.min(grant)),
                (existing, grant) => existing.or(grant),
            };
            self.work_budget_secs = match (self.work_budget_secs, attenuation.max_wall_clock_secs) {
                (Some(existing), Some(grant)) => Some(existing.min(grant)),
                (existing, grant) => existing.or(grant),
            };
        }
        self
    }

    /// Fix the run's engine, harness model, and Pi profile for its whole
    /// life. `saved` — the execution's durable pin, or the one a pause
    /// carried — stands when it agrees with the engine the attenuation
    /// named. Otherwise the pin resolves now: the named engine, else the
    /// launching run's pin (a delegated child inherits its parent), else the
    /// Settings choice at this instant. `harness_engine` is set to the pin's
    /// engine, so no later read falls back to the process snapshot.
    pub fn with_run_engine_pin(
        mut self,
        saved: Option<crate::magician_v2::execution::plane::RunEnginePin>,
    ) -> Self {
        let named = self
            .harness_engine
            .as_deref()
            .map(str::trim)
            .filter(|engine| !engine.is_empty());
        let pin = match saved {
            Some(pin) if named.is_none_or(|engine| engine == pin.engine) => pin,
            _ => crate::magician_v2::execution::plane::resolve_launch_pin(named),
        };
        self.harness_engine = Some(pin.engine.clone());
        self.run_engine_pin = Some(pin);
        self
    }

    /// Set the hint action
    pub fn with_hint(mut self, action: ExecutableAction) -> Self {
        self.hint_action = Some(action);
        self
    }

    /// Set max iterations
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Set the provider-reported token ceiling for this execution segment.
    pub fn with_max_tokens_per_cycle(mut self, max: u64) -> Self {
        self.max_tokens_per_cycle = Some(max);
        self
    }

    /// Set completed-iteration offset for resumed execution segments.
    pub fn with_iteration_offset(mut self, offset: usize) -> Self {
        self.iteration_offset = offset;
        self
    }

    /// Set max repeated actions for loop detection
    pub fn with_max_repeated_actions(mut self, max: usize) -> Self {
        self.max_repeated_actions = max;
        self
    }

    /// Set the on_failure mode (AskUser or Fail)
    pub fn with_on_failure(mut self, mode: OnFailureMode) -> Self {
        self.on_failure = mode;
        self
    }

    /// Set initial URL to navigate to when creating browser session.
    /// Avoids the about:blank delay by opening directly on the target page.
    pub fn with_initial_url(mut self, url: impl Into<String>) -> Self {
        self.initial_url = Some(url.into());
        self
    }

    /// Set tier definitions for mid-execution tier access.
    pub fn with_tier_definitions(mut self, tier_definitions: Vec<MemoryTierDefinition>) -> Self {
        self.tier_definitions = tier_definitions;
        self
    }

    /// Control whether the planner-visible schema should expose provisioned
    /// secret sidecars for this execution context.
    pub fn with_provisioned_secret_access(mut self, enabled: bool) -> Self {
        self.provisioned_secret_access_enabled = enabled;
        self
    }

    /// Set observability context (execution_id, plan_id, step_id)
    pub fn with_observability(
        mut self,
        execution_id: impl Into<String>,
        plan_id: impl Into<String>,
        step_id: impl Into<String>,
    ) -> Self {
        let execution_id = execution_id.into();
        self.execution_id = Some(execution_id.clone());
        self.legacy_execution_id = Some(execution_id);
        self.plan_id = Some(plan_id.into());
        self.step_id = Some(step_id.into());
        self
    }

    /// Set the canonical artifact chain/store ID for this execution.
    pub fn with_artifact_chain_id(mut self, chain_id: impl Into<String>) -> Self {
        self.artifact_chain_id = Some(chain_id.into());
        self
    }

    /// Check if observability is configured
    pub fn has_observability(&self) -> bool {
        (self.execution_id.is_some() || self.legacy_execution_id.is_some())
            && self.plan_id.is_some()
            && self.step_id.is_some()
    }

    // === Agent Routing Methods (TRUE_AGENTS Phase 0) ===

    /// Create a context for agent-driven execution.
    ///
    /// Sets agent routing fields and uses higher default `max_iterations` (4000)
    /// because agents typically pursue larger goals than single thread steps.
    pub fn for_agent(
        agent_id: impl Into<String>,
        goal_id: impl Into<String>,
        cycle_id: impl Into<String>,
        goal: impl Into<String>,
        success_criteria: impl Into<String>,
    ) -> Self {
        Self {
            goal: goal.into(),
            success_criteria: success_criteria.into(),
            agent_id: Some(agent_id.into()),
            goal_id: Some(goal_id.into()),
            cycle_id: Some(cycle_id.into()),
            max_iterations: 4000,
            ..Default::default()
        }
    }

    /// Builder: set agent routing fields on an existing context.
    pub fn with_agent_routing(
        mut self,
        agent_id: impl Into<String>,
        goal_id: impl Into<String>,
        cycle_id: impl Into<String>,
    ) -> Self {
        self.agent_id = Some(agent_id.into());
        self.goal_id = Some(goal_id.into());
        self.cycle_id = Some(cycle_id.into());
        self
    }

    /// Set trust policy context for pre-dispatch action enforcement.
    pub fn with_trust_policy(
        mut self,
        trust_level: impl Into<String>,
        trust_policies_path: impl Into<PathBuf>,
    ) -> Self {
        self.trust_level = Some(trust_level.into());
        self.trust_policies_path = Some(trust_policies_path.into());
        self
    }

    /// Set a preloaded trust policy enforcer for this execution context.
    pub fn with_preloaded_trust_enforcer(mut self, enforcer: Arc<TrustPolicyEnforcer>) -> Self {
        self.preloaded_trust_enforcer = Some(enforcer);
        self
    }

    /// Set approval rules for pre-dispatch approval gating.
    pub fn with_approval_rules(mut self, approval_rules: Vec<ApprovalRule>) -> Self {
        self.approval_rules = approval_rules;
        self
    }

    /// Set one-time confirmation-approved action payloads for resume execution.
    pub fn with_approved_confirmation_actions(
        mut self,
        actions: Vec<ApprovedConfirmationAction>,
    ) -> Self {
        self.approved_confirmation_actions = actions;
        self
    }

    /// Set the current execution owner and inherited owner stack.
    pub fn with_owner_snapshot(
        mut self,
        active_owner_agent_id: impl Into<String>,
        owner_stack: Vec<String>,
    ) -> Self {
        self.active_owner_agent_id = Some(active_owner_agent_id.into());
        self.owner_stack = owner_stack;
        self
    }

    /// Check whether this context has agent routing set.
    pub fn has_agent_routing(&self) -> bool {
        self.agent_id.is_some() && self.goal_id.is_some() && self.cycle_id.is_some()
    }

    /// Return a routing key suitable for storage/event dispatch.
    ///
    /// - Agent contexts: `agent:{agent_id}:{goal_id}:{cycle_id}`
    /// - Execution contexts: `{execution_id}:{plan_id}:{step_id}`
    /// - Neither: a random UUID
    pub fn routing_key(&self) -> String {
        use crate::magician_v2::agents::AGENT_KEY_PREFIX;
        if let (Some(aid), Some(gid), Some(cid)) = (&self.agent_id, &self.goal_id, &self.cycle_id) {
            format!("{}{}:{}:{}", AGENT_KEY_PREFIX, aid, gid, cid)
        } else if let (Some(execution_id), Some(pid), Some(sid)) =
            (&self.execution_id, &self.plan_id, &self.step_id)
        {
            format!("{}:{}:{}", execution_id, pid, sid)
        } else {
            uuid::Uuid::new_v4().to_string()
        }
    }

    /// Current owner snapshot when this context is execution-owned.
    pub fn current_owner_snapshot(&self) -> Option<OwnerSnapshot> {
        self.active_owner_agent_id
            .as_ref()
            .map(|active_owner_agent_id| OwnerSnapshot {
                active_owner_agent_id: active_owner_agent_id.clone(),
                owner_stack: self.owner_stack.clone(),
            })
    }

    /// Authorization chain that allows the current owner to act.
    pub fn owner_authorization_chain(&self) -> &[String] {
        &self.owner_stack
    }

    // === Input Tracking Methods ===

    /// Set pending inputs from planning phase handoff
    pub fn with_pending_inputs(mut self, inputs: Vec<PendingInput>) -> Self {
        self.pending_inputs = inputs;
        self
    }

    /// Set resolved inputs (for resume from pause)
    pub fn with_resolved_inputs(mut self, resolved: HashMap<String, Value>) -> Self {
        self.resolved_inputs = resolved;
        self
    }

    pub fn with_input_sensitivity(
        mut self,
        sensitivity: HashMap<String, crate::magician_v2::user_requests::SensitiveKind>,
    ) -> Self {
        self.resolved_input_sensitivity = sensitivity;
        self
    }

    /// Set per-context LLM model override.
    pub fn with_llm_model_override(mut self, model: Option<String>) -> Self {
        self.llm_model_override = model.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Set per-operation LLM routing overrides for this execution context.
    pub fn with_llm_routing_overrides(
        mut self,
        overrides: Option<
            crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides,
        >,
    ) -> Self {
        self.llm_routing_overrides = overrides;
        self
    }

    /// Set the immutable execution-local routing overlay.
    pub fn with_execution_llm_routing_overrides(
        mut self,
        overrides: Option<
            crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides,
        >,
    ) -> Self {
        self.execution_llm_routing_overrides = overrides;
        self
    }

    /// Set prompt identity context for system prompt rendering.
    pub fn with_prompt_identity(mut self, identity: Option<PromptIdentityContext>) -> Self {
        self.prompt_identity = identity;
        self
    }

    /// Set the stable goal hash for execution-scoped runtime artifacts.
    pub fn with_stable_goal_hash(mut self, goal_hash: Option<String>) -> Self {
        self.stable_goal_hash = goal_hash.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Set the execution storage base path for scoped runtime artifacts.
    pub fn with_storage_base_path(mut self, storage_base_path: PathBuf) -> Self {
        self.storage_base_path = storage_base_path;
        self
    }

    /// Set expected artifact declarations for enrichment on completion.
    /// These are matched by name against produced artifacts to copy
    /// `artifact_type` and `render_hints` from the planning layer.
    pub fn with_expected_artifact_declarations(
        mut self,
        declarations: Vec<crate::magician_v2::agents::types::ArtifactDeclaration>,
    ) -> Self {
        self.expected_artifact_declarations = declarations;
        self
    }

    /// Get pending inputs for a specific step (filtered by step_id)
    pub fn get_pending_inputs_for_step(&self, step_id: &str) -> Vec<&PendingInput> {
        self.pending_inputs
            .iter()
            .filter(|input| {
                // Input is for this step if step_id matches or is None (global)
                input.step_id.as_deref() == Some(step_id) || input.step_id.is_none()
            })
            .filter(|input| !self.is_input_resolved(&input.id))
            .collect()
    }

    /// Check if an input has been resolved
    pub fn is_input_resolved(&self, input_id: &str) -> bool {
        self.resolved_inputs.contains_key(input_id)
    }

    /// Record a resolved input value
    pub fn record_resolved_input(&mut self, input_id: String, value: Value) {
        self.resolved_inputs.insert(input_id.clone(), value.clone());
        // Also update the pending input if it exists
        if let Some(pending) = self.pending_inputs.iter_mut().find(|p| p.id == input_id) {
            pending.resolved_value = Some(value);
        }
    }

    /// Get a resolved input value by ID
    pub fn get_resolved_input(&self, input_id: &str) -> Option<&Value> {
        self.resolved_inputs.get(input_id)
    }

    /// Get all unresolved pending inputs (not yet answered)
    pub fn get_unresolved_inputs(&self) -> Vec<&PendingInput> {
        self.pending_inputs
            .iter()
            .filter(|input| !self.is_input_resolved(&input.id))
            .collect()
    }

    /// Format pending inputs for inclusion in decision prompt
    ///
    /// NOTE: Currently disabled - we let the agent observe and decide what inputs it needs
    /// rather than pre-loading it with "missing information" that it should ask about.
    /// The agent has vision and should determine what's needed from observation.
    pub fn format_pending_inputs_for_llm(&self, _step_id: &str) -> String {
        // Skip handoff questions - let the agent observe and decide
        // The agent will ask for passwords, CAPTCHAs, etc. when it encounters them
        // rather than being told upfront to ask about observable things like "are you logged in"
        String::new()

        // Original implementation (disabled):
        // let pending = self.get_pending_inputs_for_step(step_id);
        // if pending.is_empty() {
        //     return String::new();
        // }
        //
        // let mut parts = vec!["⚠️ MISSING INFORMATION (ask user if needed):".to_string()];
        // for input in pending {
        //     let desc = input
        //         .description
        //         .as_deref()
        //         .unwrap_or("(no description)");
        //     parts.push(format!("  - {}: {}", input.parameter, desc));
        // }
        // parts.join("\n")
    }

    /// Format resolved inputs for inclusion in decision prompt
    pub fn format_resolved_inputs_for_llm(&self) -> String {
        if self.resolved_inputs.is_empty() {
            return String::new();
        }

        let format_value = |value: &Value| match value {
            Value::String(s) => s.clone(),
            Value::Object(_) | Value::Array(_) => {
                let rendered =
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
                if rendered.len() > 1200 {
                    format!(
                        "{}... ({} chars)",
                        truncate_utf8(&rendered, 1200),
                        rendered.len()
                    )
                } else {
                    rendered
                }
            },
            _ => value.to_string(),
        };

        let mut upstream_parts = Vec::new();
        let mut user_parts = Vec::new();
        for (id, value) in &self.resolved_inputs {
            if let Some(step_id) = id.strip_prefix("__upstream__") {
                upstream_parts.push(format!("  - {}: {}", step_id, format_value(value)));
                continue;
            }

            // Find the parameter name from pending_inputs
            let param_name = self
                .pending_inputs
                .iter()
                .find(|p| p.id == *id)
                .map(|p| p.parameter.as_str())
                .unwrap_or(id.as_str());

            // Never expose credential material to the LLM. The decision made
            // at collection (`resolved_input_sensitivity`) wins; the name
            // heuristic remains as compatibility detection. The placeholder
            // includes the param ID so the executor can map it back to the
            // correct resolved_input at action time.
            let display_value = if self.resolved_input_sensitivity.contains_key(id)
                || is_secret_param_name(param_name)
            {
                redacted_placeholder(id)
            } else if let Some(entries) = form_answer_entries(value) {
                // A form: render each field, withholding the flagged ones.
                self.format_form_entries(id, entries)
            } else {
                format_value(value)
            };
            user_parts.push(format!("  - {}: {}", param_name, display_value));
        }

        let mut parts = Vec::new();
        if !upstream_parts.is_empty() {
            parts.push("📎 UPSTREAM STEP RESULTS:".to_string());
            parts.extend(upstream_parts);
        }
        if !user_parts.is_empty() {
            parts.push("📋 USER-PROVIDED VALUES:".to_string());
            parts.extend(user_parts);
        }
        parts.join("\n")
    }

    /// Whether a form field of resolved input `input_id` must be withheld from
    /// the model: flagged at collection, or named like a secret.
    pub fn form_field_is_sensitive(&self, input_id: &str, field_id: &str) -> bool {
        self.resolved_input_sensitivity
            .contains_key(&format!("{input_id}.{field_id}"))
            || is_secret_param_name(field_id)
    }

    fn format_form_entries(&self, input_id: &str, entries: &[Value]) -> String {
        let rendered: Vec<String> = entries
            .iter()
            .map(|entry| {
                let field_id = entry.get("id").and_then(Value::as_str).unwrap_or("");
                if self.form_field_is_sensitive(input_id, field_id) {
                    format!(
                        "{field_id}: {}",
                        redacted_placeholder(&format!("{input_id}.{field_id}"))
                    )
                } else {
                    let value = entry
                        .get("value")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            entry
                                .get("selected_ids")
                                .and_then(Value::as_array)
                                .map(|ids| {
                                    ids.iter()
                                        .filter_map(Value::as_str)
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                })
                        })
                        .unwrap_or_default();
                    format!("{field_id}: {value}")
                }
            })
            .collect();
        format!("[{}]", rendered.join("; "))
    }
}

/// A resolved form value is the serialized `Vec<FormAnswer>`: an array of
/// objects with an `id`. Anything else is not a form.
pub fn form_answer_entries(value: &Value) -> Option<&[Value]> {
    let entries = value.as_array()?;
    (!entries.is_empty()
        && entries
            .iter()
            .all(|e| e.get("id").and_then(Value::as_str).is_some()))
    .then_some(entries.as_slice())
}

// ============================================================================
// User Input Types (for WaitingForUser outcome)
// ============================================================================

/// Type of user input requested by the agentic loop.
///
/// This determines how the UI should render the input prompt and
/// what kind of response value is expected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserInputType {
    /// Free-form text input (e.g., email, username, answer to a question)
    Text {
        /// Placeholder text for the input field
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        /// Whether multiline input is allowed
        #[serde(default)]
        multiline: bool,
    },

    /// Sensitive input that should be masked in UI and redacted in logs
    /// (e.g., passwords, API keys, tokens)
    Password {
        /// Placeholder text for the input field
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    /// A one-time verification code (OTP / 2FA / SMS or email code).
    ///
    /// Masked like a password, but classified `otp` and one-time: the shared
    /// custody holds it for exactly one bound submission and the collection
    /// window is clamped. The answer rides `UserInputValue::Password` — an
    /// exact string, never coerced through a number — so every client that
    /// can answer a password can answer a code; the type, not the value,
    /// carries the distinction.
    Otp {
        /// Placeholder text for the input field
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
    },

    /// Single selection from a list of options
    Choice {
        /// Available options to choose from
        options: Vec<ChoiceOption>,
        /// Whether to allow an "other" freeform option
        #[serde(default)]
        allow_other: bool,
    },

    /// Multiple selections from a list of options
    MultiChoice {
        /// Available options to choose from
        options: Vec<ChoiceOption>,
        /// Minimum number of selections required
        #[serde(default)]
        min_selections: usize,
        /// Maximum number of selections allowed (0 = unlimited)
        #[serde(default)]
        max_selections: usize,
    },

    /// Yes/No confirmation (e.g., "Proceed with deletion?")
    Confirmation {
        /// Label for the confirm button (default: "Yes")
        #[serde(skip_serializing_if = "Option::is_none")]
        confirm_label: Option<String>,
        /// Label for the deny button (default: "No")
        #[serde(skip_serializing_if = "Option::is_none")]
        deny_label: Option<String>,
        /// Whether this is a destructive action (renders differently in UI)
        #[serde(default)]
        destructive: bool,
    },

    /// User needs to perform an action outside the system
    /// (e.g., solve CAPTCHA, verify email, complete 2FA)
    ExternalAction {
        /// Instructions for what the user needs to do
        instructions: String,
        /// Label for the "done" button (default: "I've completed this")
        #[serde(skip_serializing_if = "Option::is_none")]
        done_label: Option<String>,
    },

    /// User provides a file path
    /// (e.g., for upload, config file location)
    FilePath {
        /// File type filter (e.g., "*.pdf", "*.csv")
        #[serde(skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
        /// Whether multiple files are allowed
        #[serde(default)]
        multiple: bool,
    },

    /// User provides guidance or advice on how to proceed
    /// (e.g., when the agent is stuck and needs direction)
    Guidance {
        /// Context about what approaches have been tried
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<String>,
        /// Suggested options the user might consider
        #[serde(skip_serializing_if = "Option::is_none")]
        suggestions: Option<Vec<String>>,
    },

    /// Tool authorization request -- agent wants to use an unlisted tool.
    ToolAuthorization {
        tool_name: String,
        params_summary: String,
    },

    /// Sandbox override request -- command violates sandbox policy.
    SandboxOverride {
        command: String,
        violation: String,
        /// File roots approved for this execution. Empty for shell overrides
        /// and for pause records written before file-path HITL support.
        #[serde(default)]
        allowed_roots: Vec<String>,
    },

    /// Approval for a staged file-edit transaction or code-change
    /// proposal (multi-file edit).
    ///
    /// The runtime stages the transaction with
    /// [`crate::magician_v2::execution::file_edit::transaction::TransactionStore::stage`]
    /// — nothing is written to disk at that point. This input type
    /// carries either a native transaction id or a Pi-backed proposal
    /// id (so the dispatcher can look it up on response), the per-file
    /// diff payload (so the frontend `DiffStrip` component can render
    /// the pending changes without a second fetch), and a human-readable
    /// rationale shown above the diff.
    ///
    /// Operator response options:
    ///   - **apply** — the owning backend writes the queued edit/patch,
    ///     snapshot captured for revert where supported.
    ///   - **reject** — the owning backend marks the request rejected
    ///     with no disk mutation.
    ///   - **apply_partial** (future) — apply only a subset of files;
    ///     the unselected files stay as a residual pending transaction
    ///     the operator can revisit. v1 surfaces only apply / reject;
    ///     the partial path lights up once the frontend per-hunk /
    ///     per-file selection model is wired through.
    DiffApproval {
        /// Native staged transaction id; the dispatcher loads it via
        /// `TransactionStore::load`. Present for Magician-native
        /// `write_file` / `edit_file` / `apply_patch` tools.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transaction_id: Option<String>,
        /// Pi-backed code-change proposal id; the dispatcher loads it
        /// via `CodeChangeProposalStore::load`. Present for
        /// shadow-workspace proposal approvals.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal_id: Option<String>,
        /// Optional source hint for UI/debugging. Expected values:
        /// `transaction` or `proposal`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        approval_source: Option<String>,
        /// Human-readable explanation of *why* these edits are
        /// proposed. Surfaced above the diff in the approval modal.
        rationale: String,
        /// Per-file payloads matching the frontend `DiffFile` shape.
        files: Vec<DiffApprovalFile>,
    },

    /// Several questions in one pause. Nested `Form` fields are rejected
    /// at lowering; each entry is a single-field ask.
    Form { questions: Vec<FormQuestion> },
}

/// One field inside [`UserInputType::Form`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormQuestion {
    pub id: String,
    pub prompt: String,
    /// `text`, `password`, `otp`, `choice`, or `multi_choice`. A `password`
    /// or `otp` field is sensitive by type (see
    /// `secrets::classify::classify_form_fields`); the others may still be
    /// flagged by wording or the login-identifier bundle rule.
    #[serde(default = "default_form_question_input_type")]
    pub input_type: String,
    #[serde(default)]
    pub options: Vec<ChoiceOption>,
}

fn default_form_question_input_type() -> String {
    "text".to_string()
}

/// Per-file diff payload for the `DiffApproval` input type. Mirrors
/// the frontend `DiffStrip` `DiffFile` interface byte-for-byte so the
/// approval modal renders the payload directly without an adapter.
///
/// Populated by
/// [`crate::magician_v2::execution::file_edit::transaction::FileEditTransaction::diff_approval_files`]
/// from a staged transaction. See `DiffStrip.svelte` for the rendering
/// contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffApprovalFile {
    pub path: String,
    /// Single-letter git status (`A` = added, `M` = modified,
    /// `D` = deleted, `R` = renamed/moved).
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    /// Standard unified-diff string; `--- a/<path>` / `+++ b/<path>`
    /// headers plus `@@` hunks. Empty when the diff is a no-op.
    pub unified_diff: String,
}

impl Default for UserInputType {
    fn default() -> Self {
        Self::Text {
            placeholder: None,
            multiline: false,
        }
    }
}

impl UserInputType {
    /// Get the type name as a string (for event serialization).
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Password { .. } => "password",
            Self::Otp { .. } => "otp",
            Self::Choice { .. } => "choice",
            Self::MultiChoice { .. } => "multi_choice",
            Self::Confirmation { .. } => "confirmation",
            Self::ExternalAction { .. } => "external_action",
            Self::FilePath { .. } => "file_path",
            Self::Guidance { .. } => "guidance",
            Self::ToolAuthorization { .. } => "tool_authorization",
            Self::SandboxOverride { .. } => "sandbox_override",
            Self::DiffApproval { .. } => "diff_approval",
            Self::Form { .. } => "form",
        }
    }

    /// Extract options as a JSON string (for Choice/MultiChoice types).
    ///
    /// Returns None for input types that don't have options.
    pub fn options_json(&self) -> Option<String> {
        match self {
            Self::Choice { options, .. } | Self::MultiChoice { options, .. } => {
                serde_json::to_string(options).ok()
            },
            Self::ToolAuthorization { .. } => Some(
                serde_json::json!([
                    {"id": "allow_once", "label": "Allow Once"},
                    {"id": "allow_always", "label": "Allow for This Run"},
                    {"id": "deny", "label": "Deny"}
                ])
                .to_string(),
            ),
            Self::SandboxOverride { .. } => Some(
                serde_json::json!([
                    {"id": "allow_once", "label": "Allow Once"},
                    {"id": "deny", "label": "Deny"}
                ])
                .to_string(),
            ),
            // DiffApproval surfaces apply/reject as a Choice-shaped
            // payload so the existing apply/reject dispatcher in
            // `web_api.rs::respond_hitl_handler` can route the
            // operator's response without a new input-value variant.
            // Per-file or per-hunk granular selection rides as a
            // separate `selected_paths` field on the response body
            // (added in the dispatcher arm in this same change).
            Self::DiffApproval { .. } => Some(
                serde_json::json!([
                    {"id": "apply", "label": "Apply"},
                    {"id": "reject", "label": "Reject"}
                ])
                .to_string(),
            ),
            _ => None,
        }
    }

    /// Preserve only the UI control class and non-content-bearing shape.
    /// Protected app pauses use this projection in generic pending/status
    /// indexes; prompts, options, commands, paths and diffs stay exclusively
    /// inside the sealed workflow continuation.
    pub fn metadata_only_projection(&self) -> Self {
        match self {
            Self::Text { multiline, .. } => Self::Text {
                placeholder: None,
                multiline: *multiline,
            },
            Self::Password { .. } => Self::Password { placeholder: None },
            Self::Otp { .. } => Self::Otp { placeholder: None },
            Self::Choice { allow_other, .. } => Self::Choice {
                options: Vec::new(),
                allow_other: *allow_other,
            },
            Self::MultiChoice {
                min_selections,
                max_selections,
                ..
            } => Self::MultiChoice {
                options: Vec::new(),
                min_selections: *min_selections,
                max_selections: *max_selections,
            },
            Self::Confirmation { destructive, .. } => Self::Confirmation {
                confirm_label: None,
                deny_label: None,
                destructive: *destructive,
            },
            Self::ExternalAction { .. } => Self::ExternalAction {
                instructions: "Protected app workflow external action".to_owned(),
                done_label: None,
            },
            Self::FilePath { multiple, .. } => Self::FilePath {
                filter: None,
                multiple: *multiple,
            },
            Self::Guidance { .. } => Self::Guidance {
                context: None,
                suggestions: None,
            },
            Self::ToolAuthorization { .. } => Self::ToolAuthorization {
                tool_name: "protected_app_tool".to_owned(),
                params_summary: "Protected parameters".to_owned(),
            },
            Self::SandboxOverride { .. } => Self::SandboxOverride {
                command: "<protected-command>".to_owned(),
                violation: "Protected app workflow policy decision".to_owned(),
                allowed_roots: Vec::new(),
            },
            Self::DiffApproval { .. } => Self::DiffApproval {
                transaction_id: None,
                proposal_id: None,
                approval_source: None,
                rationale: "Protected app workflow changes".to_owned(),
                files: Vec::new(),
            },
            Self::Form { questions } => Self::Form {
                questions: questions
                    .iter()
                    .map(|question| FormQuestion {
                        id: question.id.clone(),
                        prompt: String::new(),
                        input_type: question.input_type.clone(),
                        options: Vec::new(),
                    })
                    .collect(),
            },
        }
    }

    /// Serialize the full input schema for UI/feed consumers that need the
    /// complete typed contract rather than a flattened type/options view.
    pub fn schema_json_value(&self) -> Option<Value> {
        serde_json::to_value(self).ok()
    }
}

/// An option for Choice or MultiChoice input types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChoiceOption {
    /// Unique identifier for this option
    pub id: String,
    /// Display label for the option
    pub label: String,
    /// Optional longer description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ChoiceOption {
    /// Create a new choice option with just id and label
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
        }
    }

    /// Create a choice option with description
    pub fn with_description(
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: Some(description.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StatelessTerminalPausePreparation {
    pub source_revision: u64,
    pub terminal_seq: u64,
    pub worker_id: String,
    pub lease_fence: u64,
    pub lease_expires_at_ms: i64,
}

/// State preserved when the agentic loop pauses for user input.
///
/// This contains all the context needed to seamlessly resume execution
/// after the user provides their response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticPauseState {
    /// See `AgenticContext::resolved_input_sensitivity`. Travels with the
    /// pause into the durable pause record and back through resume; absent in
    /// records written before the contract existed.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resolved_input_sensitivity:
        HashMap<String, crate::magician_v2::user_requests::SensitiveKind>,
    /// The value-free sensitivity of the ask this pause waits on, decided when
    /// the pause was raised (`sensitive_spec_for_pause`) with the same rules
    /// the request service applies at accept. Published on `hitl.requested`
    /// (`input_schema.sensitive`) so every client masks by the spec, carried
    /// on the pending-pause listing (the plane refuses by it), and read back
    /// at resume: its kind is the answer's sensitivity (never re-derived) and
    /// an answer after `collection_deadline_ms` is refused and re-asked with
    /// a fresh window (`revision` + 1). `None` for a decision prompt and for
    /// records written before P3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_sensitive: Option<crate::magician_v2::user_requests::SensitiveInputSpec>,
    /// Plane launch attenuation carried across the pause (plane Task 6b).
    /// These are launch-pinned facts about how the run was started, not loop
    /// state: the denial a plane grant pinned, the allowlist it narrowed to,
    /// and the harness the run thinks with. Without the carry, a resumed
    /// plane-started run silently widens back to the agent's surface and
    /// falls back to the process snapshot's engine — the double-bill
    /// anti-pattern.
    #[serde(default)]
    pub plane_denied_capability_names: Vec<String>,
    #[serde(default)]
    pub plane_allowed_capability_names: Option<Vec<String>>,
    #[serde(default)]
    pub harness_engine: Option<String>,
    /// The run's launch pin, carried across the pause so a resumed run keeps
    /// the model and profile it launched with (see
    /// [`AgenticContext::run_engine_pin`]).
    #[serde(default)]
    pub run_engine_pin: Option<crate::magician_v2::execution::plane::RunEnginePin>,
    /// Per-run cost ceiling carried across the pause (Task 6b grant
    /// ceiling; see [`AgenticContext::max_cost_usd`]).
    #[serde(default)]
    pub max_cost_usd: Option<f64>,
    /// The identity of this ask, distinct for every pause of the same step.
    /// The storage key names *the* pause of a step and is reused by the next
    /// question the same step raises; the canonical `hitl.requested` and
    /// `hitl.resolved` events, whose lifecycle is one-shot per correlation
    /// id, carry [`Self::hitl_correlation_id`] — the key plus this — so a
    /// second question is a new request on every feed, a retrieval watch for
    /// it is a new challenge, and an answer posted for an earlier ask of the
    /// step is refused rather than resuming this one. `None` only for records
    /// written before the field existed; those keep the bare key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_id: Option<String>,
    /// Current iteration number when paused
    pub iteration: usize,

    /// Number of iterations completed before the paused execution segment began.
    ///
    /// Kept separate from `iteration` because resume-budget calculations rely on
    /// `iteration` staying local to the segment.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub iteration_offset: usize,

    /// Index of the step being executed (if running as part of a plan)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,

    /// The goal being pursued
    pub goal: String,

    /// Success criteria for the goal
    pub success_criteria: String,

    /// Last observed environment state
    pub environment_state: EnvironmentState,

    /// Summary of actions taken so far (for prompt context)
    pub action_history_summary: String,

    /// Timestamp when paused
    pub paused_at: DateTime<Utc>,

    /// Plan ID (for resume routing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,

    /// Step ID (for resume routing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,

    /// Task ID for task-scoped resume continuity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,

    /// Originating chat session for analytics continuity across pause/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_session_id: Option<String>,

    /// Execution ID for execution-scoped taskplan/artifact continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,

    /// Exact fixed-roster stage/attempt discriminator. Pipeline stages retain
    /// the root execution id for routing, so this axis must survive a pause or
    /// two stages at the same iteration would mint the same successor segment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_loop_state_segment: Option<PipelineLoopStateSegment>,

    /// Exact loop-state segment that produced this checkpoint.
    ///
    /// Ordinary resumes do not reuse this address: they advance under a
    /// separately derived resume-generation key. The child-completion owner
    /// carries the source so it can retire a parked segment before dispatching
    /// the checkpoint. The one exception is an indeterminate-effect answer,
    /// which must reclaim this source because its pending effect ledger lives
    /// there; that override is consumed exactly once. `None` is the
    /// backward-compatible shape for checkpoints written before these exact
    /// source-address protocols existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stateless_parked_segment: Option<String>,

    /// Exact lease/revision authority that prepared a hidden terminal pause
    /// before the source LoopState CAS. The pause store uses this bounded body-
    /// hashed fence to refuse late lower-fence writers and to supersede only an
    /// expired, never-committed preparation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stateless_terminal_preparation: Option<StatelessTerminalPausePreparation>,

    /// Exact timer generation for a placement-retry checkpoint. This is
    /// covered by `authorization_hash` and paired with
    /// `stateless_parked_segment`; a copied or stale queue entry cannot select
    /// a different continuation merely by naming the same execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stateless_retry_due_at: Option<DateTime<Utc>>,

    /// Root execution ID for rerun-level task output accumulation continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,

    /// Principal for scope-local approval/resume storage continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,

    /// Workspace for scope-local approval/resume storage continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,

    /// Exact server-authenticated product invocation inherited by this owner
    /// frame. It is persisted only by the trusted pause store and current
    /// definition/policy is re-resolved before the resumed provider decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_context_override: Option<crate::magician_v2::agents::AgentInvocationContext>,

    /// Work authority the paused execution ran under. Security-bearing:
    /// it participates in `authorization_hash`, so a tampered pause blob
    /// cannot swap the work a resume re-attaches to (§4.2c row 11).
    ///
    /// # A pause written by a build that carried `engagement_authority` will
    /// not resume
    ///
    /// The field renamed, so an on-disk pause from the previous build
    /// deserializes this as `None` (it is `#[serde(default)]`), the recomputed
    /// `authorization_hash` no longer matches the envelope's, and the resume is
    /// refused. That is the fail-closed direction and it is deliberate: a serde
    /// alias would have to decide what an engagement-shaped blob means as a
    /// generic carrier, and guessing wrong re-scopes the authority the resume
    /// re-attaches to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_authority: Option<crate::magician_v2::work_context::WorkAuthorityRef>,

    /// Exact invocation authority for suspended owners, aligned with
    /// `owner_stack`. This prevents returning from a specialist after resume
    /// from reconstructing a protected parent lane as a generic handover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_invocation_stack: Vec<Option<crate::magician_v2::agents::AgentInvocationContext>>,

    /// Current source/target relationship that authorized this owner frame.
    /// Resume re-resolves it against the live scoped definition set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_authorization: Option<TransitionAuthorizationBinding>,

    /// Parent frames suspended by nested work on this same execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continuation_frames: Vec<AgenticContinuationFrame>,

    /// Artifact chain identifier to preserve durable artifact scoping across resume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_chain_id: Option<String>,

    // === Agent Routing Fields (TRUE_AGENTS Phase 0) ===
    /// Agent ID (for agent-scoped resume routing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,

    /// Goal ID (for agent-scoped resume routing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,

    /// Cycle ID (for agent-scoped resume routing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_id: Option<String>,

    /// Trust level used during the paused execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,

    /// Path to trust policy file used during the paused execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_policies_path: Option<PathBuf>,

    /// Approval rules used during the paused execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approval_rules: Vec<ApprovalRule>,

    /// Active owner when the pause was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_owner_agent_id: Option<String>,

    /// Suspended owners authorizing the active owner when the pause was captured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_stack: Vec<String>,

    /// Spend token IDs available to this paused execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spend_token_ids: Vec<String>,

    /// A caller-deliberately narrowed direct-tool scope (for example a
    /// one-pack chat sub-run) must survive approval/user-input pause and
    /// resume. Without this snapshot, resume-time owner hydration replaces the
    /// narrow set with the owner's full catalog.
    #[serde(default)]
    pub preserve_initial_tool_scope: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserved_initial_tools: Vec<runtime_core::ToolInfo>,

    /// Maximum iterations allowed (to continue countdown)
    pub max_iterations: usize,

    // ── Ceilings on what the run may REACH ─────────────────────────────────
    //
    // Four restrictions that were absent from the boundary, and absent in the
    // one direction that matters: every one of them is empty-means-unrestricted,
    // so a resumed run was LESS restricted than the run it continued. A run
    // pinned to `allowed_action_types: ["browser"]` came back able to use the
    // tool lane, because `tool_lane_allowed` reads `None => true`.
    //
    // §2 of the turn-boundary contract states the rule they were breaking:
    // *"Anything enforcement-bearing — ceilings, counters, approvals — must be
    // represented, or the enforcement is advisory."* These are the ceilings half.
    //
    // They participate in `authorization_hash` below, so a disk-tampered pause
    // cannot widen them where that hash is checked. They deliberately do NOT
    // make a pause an elevation: promoting every restricted run would make it
    // fail closed after a restart and discard the user's work, which is the
    // regression `approved_confirmation_actions` was careful to avoid.
    /// The action-type ceiling. `None` is unrestricted, which is why losing it
    /// widened the run rather than narrowing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_action_types: Option<Vec<String>>,

    /// Capabilities this run may not reach. A deny list: empty denies nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_capability_names: Vec<String>,

    /// Per-tool parameter denials. Same shape, one level deeper.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub denied_tool_params: HashMap<String, HashMap<String, Vec<String>>>,

    // `delegation_targets` is deliberately NOT here. It looks like a ceiling and
    // is not one: `DelegationTarget` carries a description and a tool list, so it
    // is a CATALOG of who exists, rebuilt from agent definitions. Losing it fails
    // CLOSED — the model cannot name a target — rather than open, and carrying it
    // would put every delegate's description into every pause record.
    /// The browser transports this run may use. Empty is unrestricted — an agent
    /// that declares nothing keeps every transport — so losing it handed a
    /// narrowed run the full set back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_transports: Vec<String>,

    /// Provider-reported token ceiling preserved for resumed execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_per_cycle: Option<u64>,

    /// Soft active-work budget restored for the resumed logical run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_budget_secs: Option<u64>,

    /// Active-work milliseconds consumed before this durable pause. Paused
    /// wall-clock time is intentionally excluded when execution resumes.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub work_budget_consumed_ms: u64,

    /// Cumulative execution tokens charged before this pause.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub llm_tokens_used: u64,

    /// Delegation depth ceiling preserved for resumed execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_delegation_depth: Option<u8>,

    /// Legacy source-agent delegation setting preserved for resume/profile
    /// compatibility. It does not clamp delegated child work budgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_timeout_secs: Option<u64>,

    /// Maximum repeated actions allowed
    pub max_repeated_actions: usize,

    /// Optional model override for agent-scoped LLM routing continuity across resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_model_override: Option<String>,

    /// Optional per-operation provider/model routing overrides for resume continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,

    /// Immutable execution-local routing overlay for resume continuity.
    /// Older pauses omit it; restore treats their effective routing snapshot as
    /// the fail-closed overlay so a legacy resume cannot escape its prior lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_llm_routing_overrides:
        Option<crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides>,

    /// Metadata-only marker that this pause contains app-disclosure governed
    /// prompt/history bytes. Never authority; resume must match it against a
    /// freshly admitted runtime guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_disclosure_checkpoint: Option<magicllm::LlmDisclosureCheckpoint>,

    /// Metadata-only digest of the complete protected app pause payload. It is
    /// never authority: resume must reconstruct current app labels/policy and
    /// re-admit the exact serialized bytes before hydrating goal/history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_workflow_continuation_checkpoint:
        Option<crate::magician_v2::apps::tool_disclosure::AppWorkflowContinuationCheckpoint>,

    /// Prompt identity context (persona + bounded autonomous controls) for resume continuity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_identity: Option<PromptIdentityContext>,

    /// Active reusable procedure guidance rendered before the pause.
    ///
    /// This is prompt context, not durable task state. It must survive resume so
    /// a user-input pause does not silently drop the operating procedure that
    /// shaped the pre-pause run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_procedure_memory: Option<String>,

    /// Environment/delegation evidence already admitted into the prompt before
    /// the pause. Delegation continuation appends completed child results here
    /// and resumes the same loop instead of rebuilding a fresh root execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_environment_knowledge: Option<String>,

    /// Short-lived provider continuation checkpoint for exact resume. Stateless
    /// providers ignore it and consume `live_messages`; stateful providers can
    /// send only the new child-result delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_response_id: Option<String>,

    /// Image cohort paired with `continuation_response_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_last_has_images: Option<bool>,

    /// Stable goal hash so pause/resume keeps using the same runtime identity
    /// even if the in-memory goal string is augmented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_goal_hash: Option<String>,

    /// Base storage root for scoped runtime artifacts and prompt dumps.
    #[serde(default = "process_runtime_root")]
    pub storage_base_path: PathBuf,

    /// Persisted task state from prior runs, carried across pause/resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_state: Option<String>,

    /// Task output projection mode for this run.
    #[serde(default)]
    pub task_output_mode: TaskOutputMode,

    /// On-failure mode for pause/resume continuity.
    /// Preserves the escalation behavior across pause/resume cycles.
    #[serde(default)]
    pub on_failure: OnFailureMode,

    /// Tracks how many times this execution has been continued via escalation.
    /// Used to cap the number of "Keep Trying" attempts and prevent infinite loops.
    #[serde(default)]
    pub continuation_count: usize,

    /// Expected artifact declarations preserved across pause/resume so that the
    /// enrichment step can copy `render_hints` and `artifact_type` to produced
    /// artifacts on successful completion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifact_declarations: Vec<crate::magician_v2::agents::types::ArtifactDeclaration>,

    /// True when the paused execution backs an inline chat turn.
    #[serde(default)]
    pub chat_inline: bool,

    /// RCA fix #5 — VibeDev coding-coordinator signal, preserved across pause so a
    /// Build coordinator that pauses (clarifying question / confirmation /
    /// max-iterations) keeps the guardrails (#1-#4) armed on resume instead of
    /// silently disarming. `#[serde(default)]` → older persisted states resume
    /// as `false`. Mirrors `chat_inline` capture/restore.
    #[serde(default)]
    pub coding_coordinator_run: bool,

    /// Live, append-only conversation captured at pause time — the model's
    /// real input (see `ExecutionHistory::live_messages`), including any
    /// iteration records not yet folded when the pause fired. Restored on
    /// resume so the model continues its own frozen conversation instead of
    /// a text-summary reconstruction. `#[serde(default)]` → pre-migration
    /// blobs load with an empty conversation and keep the summary-into-goal
    /// hydration path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_messages: Vec<magicllm::prelude::LLMMessage>,

    /// One-time action approvals already granted by a human and not yet
    /// consumed.
    ///
    /// SECURITY-BEARING. Restoring these re-grants authority, so this field is
    /// covered by both [`AgenticPauseState::is_elevation`] and
    /// [`AgenticPauseState::authorization_hash`]. Without that pairing a forged
    /// on-disk pause could inject a pre-approved action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_confirmation_actions: Vec<ApprovedConfirmationAction>,

    /// Cross-iteration loop-protective state. See [`LoopProtectiveState`].
    /// `None` on pre-migration blobs, which resume with the pre-fix behaviour
    /// of restarting every counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_protective_state: Option<LoopProtectiveState>,
}

/// Cross-iteration loop state that protects a run from itself — cycle
/// detection, retry and rejection budgets, the per-run cost ceiling, and the
/// observation policy's inputs.
///
/// These live as `let mut` locals in `execute_agentically_inner`, so before
/// this type existed **none of them survived a pause**: every resume silently
/// restarted loop detection, reset every retry and abort counter, and zeroed
/// the accumulated USD spend. A resumed run could therefore re-enter a cycle it
/// had already detected, or retry past a cap it had already exhausted.
///
/// Deliberately excluded, each for a stated reason:
/// * `ExecutionHistory` — its per-iteration environment states bypass the
///   hand-written `validate_pause_state_json_limits` inventory, and
///   `live_messages` already persists the same conversation ("persist one
///   conversation representation, not two").
/// * `previous_merkle_tree` — a full DOM node map, and page state is externally
///   mutable, so resume must re-observe regardless.
/// * `pending_agentic_tool_lineages` — describes in-flight dispatches that did
///   not complete; restoring them would emit false telemetry.
/// * `pending_operator_steer` — folded verbatim into the next decision
///   prompt, so restoring it from disk would make a tampered pause record a
///   prompt-injection vector. An unconsumed steer is cheaply re-issued.
/// * `trust_dispatch_guard` — holds `Arc<TrustPolicyEnforcer>`; deriving
///   `Deserialize` would bypass `TrustPolicyEnforcer::new`'s `validate_policies`
///   gate. It is rebuilt every iteration by
///   `refresh_trust_dispatch_guard_for_decision`.
///
/// Every field is bounded: fingerprint deques are capped by `LoopDetector`'s
/// `max_history`, the summary deque by `STUCK_WARNING_THRESHOLD`, and the rest
/// are scalars or maps keyed by tool name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoopProtectiveState {
    /// Cycle/no-progress detector, including its fingerprint history.
    #[serde(default, skip_serializing_if = "loop_detector_is_pristine")]
    pub loop_detector: crate::magician_v2::execution::agentic::loop_detector::LoopDetector,

    /// What is left of the run's wall-clock ceiling.
    ///
    /// A **duration**, not an instant, and that is the point. The loop holds the
    /// ceiling as `deadline: Option<Instant>` — process-local, and meaningless to
    /// any other holder, exactly as §2 of the turn-boundary contract says of
    /// `work_budget_segment_started_at`. A resumed run recomputed it from a fresh
    /// `Instant::now()`, so **a run could outlive its wall-clock ceiling simply by
    /// pausing** — the defect the cost ceiling below had before this bundle
    /// carried it.
    ///
    /// Remaining rather than absolute, so a pause waiting on a human does not
    /// spend the ceiling. The bound exists to catch a runaway or hung turn; a
    /// question left unanswered overnight is neither, and an absolute deadline
    /// would end that run the moment it resumed.
    ///
    /// Refreshed at the top of each iteration rather than at each of the 28 pause
    /// sites, so a pause carries a value at most one iteration stale. That drift
    /// favours the run — it can gain back the current iteration's elapsed time —
    /// and is the price of not threading an `Instant` through 28 signatures. A
    /// per-pause refresh would need the deadline at every one of them.
    ///
    /// `None` means no ceiling was configured — not a ceiling of zero.
    ///
    /// One consequence worth stating: a run WITH a ceiling is never pristine, so
    /// every one of its pauses carries this bundle. The "an untouched run adds
    /// zero bytes" guarantee therefore holds only where no wall-clock ceiling is
    /// configured. That is the correct trade — a ceiling nobody carried is a
    /// ceiling nobody enforces — and the cost is one integer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_max_duration_ms: Option<u64>,

    /// The same ceiling as an absolute WALL-CLOCK instant, for a holder that is
    /// not this process.
    ///
    /// §4 of the turn-boundary contract, second trap: *"a wall-clock deadline
    /// implemented as a spawned `sleep` dies with its holder. Deadlines must be
    /// stored and checked at boundary entry."* The loop's own bound is a
    /// `std::time::Instant`, which is monotonic and therefore correct under a
    /// clock jump — and meaningless to anyone else, because it is an opaque
    /// offset from an arbitrary process-local origin.
    ///
    /// So both, each doing what it is good at, and the run ends when EITHER
    /// fires. This is not redundancy:
    ///
    /// - The `Instant` is immune to NTP steps and manual clock changes, which a
    ///   wall-clock deadline is not.
    /// - This one is portable, and it advances while the machine is SUSPENDED —
    ///   `CLOCK_MONOTONIC` may not, so a laptop that slept through the ceiling
    ///   woke with its budget intact under the `Instant` alone.
    ///
    /// Taking the earlier of the two means a forward clock step can end a run
    /// before its ceiling really elapsed. That is the right side to err on: this
    /// is a CEILING, so overrunning it is the failure it exists to prevent, and
    /// ending early settles the run with an ordinary timeout disposition that a
    /// caller can resume. Overrunning silently — which is what a suspended
    /// laptop produced — has no such recovery, because nothing notices.
    ///
    /// Fixed for the segment rather than refreshed: it is an absolute instant,
    /// and the point is that another holder can evaluate it without asking this
    /// process anything. It is re-derived on resume from
    /// [`Self::remaining_max_duration_ms`], which is what keeps paused time from
    /// counting against the budget — the two fields answer different questions
    /// and neither replaces the other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<DateTime<Utc>>,

    /// P3 (#17): cumulative USD spend across every decision LLM call this run.
    /// Accumulated from `decision_telemetry.cost_usd`; checked against the
    /// per-run cost ceiling at the top of each iteration. Losing this on
    /// resume let a run exceed its ceiling simply by pausing.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub cumulative_run_cost_usd: f64,

    /// How many times each tool has been dispatched with the same arguments.
    ///
    /// Observability continuity, NOT a limiter — nothing in the loop reads this
    /// to refuse a dispatch. It is carried across a pause so the repeat and
    /// recovery counts a resumed run reports continue the original run's series
    /// instead of restarting at one, which would make a resumed run look like a
    /// fresh one to anyone reading the lineage.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub agentic_tool_repeat_counts: HashMap<String, u32>,

    /// Tools that have already failed once with these arguments.
    ///
    /// Also observability continuity: its only consumer is
    /// `observe_tool_recovery`, which reads it to decide whether a success
    /// counts as a RECOVERY from an earlier failure. Nothing withholds a tool
    /// from the model on the strength of it.
    /// A `BTreeSet`, not a `HashSet`: this state is inside the pause
    /// authorization hash, which canonicalises object keys but not array
    /// order, and a `HashSet` serialises as an array in hash-seed order. Every
    /// serde round trip of a non-empty set reordered it and the resumed pause
    /// failed its own tamper check — intermittently, since the set is only
    /// non-empty after a tool call has failed. The JSON shape is unchanged.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub agentic_failed_tool_fingerprints: std::collections::BTreeSet<(String, String)>,

    /// Five-strike decision-parse abort.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub consecutive_parse_failures: usize,

    /// P0.3: dedicated counter for transient PROVIDER errors (rate-limit /
    /// 429 / 503 / timeout hiccups in the decision-error branch) so they back
    /// off + retry on their OWN budget rather than feeding the 5-strike
    /// `consecutive_parse_failures` parse-abort. Reset on a clean decision.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub transient_retry_count: usize,

    /// Structured-decision gate: consecutive steps decided by the decision
    /// plane instead of the LLM. Capped by the operation's
    /// `gate.max_consecutive_steps` (default 3) so a periodic LLM step
    /// re-grounds the run; any LLM-decided step resets it to zero.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    #[serde(rename = "step_judge_gated_consecutive_steps")]
    pub decision_rail_consecutive_steps: usize,

    /// Opaque engine-owned continuation, persisted with the loop across pauses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_rail_plan: Option<decision_engine_contract::action::ActionPlan>,

    /// P0.1: separate counter for all-transient YIELD blocker retries. Kept
    /// apart from `transient_retry_count` because a yield IS a successful
    /// decision (which resets that counter) — sharing it would let the retry
    /// loop never hit its cap. NOT reset on decision success; only bounded by
    /// `MAX_TRANSIENT_RETRIES` — which is exactly why it must survive a pause.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub yield_transient_retry_count: usize,

    /// Three-strike abort for repeatedly rejected terminal-success claims.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub consecutive_goal_reached_rejections: usize,

    /// Independent budget for the give-up gate (mirror of the
    /// success-rejection counter) so premature-surrender bounces don't share
    /// the success cap. Reset alongside it on any real intervening action; the
    /// shared 3-strike abort in `handle_terminal_evidence_rejection` honours
    /// the give-up after 3 bounces.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub consecutive_giveup_rejections: usize,

    /// Bounces spent telling the model not to repeat a tool call that already
    /// failed with identical arguments.
    ///
    /// Its own budget, and it must never deadlock the loop: after
    /// `MAX_REPEAT_FAILED_ACTION_REJECTIONS` the guard stands down and lets the
    /// call through to fail on its own. A guard that can refuse forever would
    /// turn a model that insists on one bad call into a run that never reaches
    /// any terminal at all — strictly worse than the wasted call it prevents.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub consecutive_repeat_failed_action_rejections: usize,

    /// Stuck-iteration detector (observability-only).
    ///
    /// Counts consecutive iterations that completed without landing a
    /// successful tool call (`iteration_landed_a_tool` returning false). At
    /// the threshold the executor emits `AgenticStepStuckWarning` every
    /// iteration it stays above it. Never alters control flow.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub consecutive_no_action_iterations: usize,

    /// Remaining iterations to skip after a nested sub-goal consumed budget.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub skip_iterations: usize,

    // `last_action_context` lived here until 2026-08-27. It was the sole input
    // to `should_observe_after_action`, and it had no producer anywhere in the
    // workspace: three sites cleared it, none ever assigned a `Some`. The policy
    // therefore answered `Full` every iteration and its skip arm was
    // unreachable, so both the field and the policy were deleted together. A
    // pause record that still carries the key simply ignores it — the struct
    // does not `deny_unknown_fields`.
    /// LOOP-PRESSURE: the last repetition advisory, so the next model turn can
    /// see repeated proposals without Rust deciding task failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_recovery_context:
        Option<crate::magician_v2::execution::agentic::executor::LoopRecoveryContext>,

    /// Precision feedback is returned to the same agent as a bounded repair
    /// observation. The agent remains free to revise the draft or choose more
    /// research/read actions; the runtime only prevents an unsupported
    /// terminal from being published or retried forever.
    #[serde(default, skip_serializing_if = "repair_state_is_pristine")]
    pub terminal_grounding_repair_state:
        crate::magician_v2::execution::agentic::executor::TerminalGroundingRepairState,

    /// Recent no-action summaries backing the stuck warning.
    #[serde(default, skip_serializing_if = "std::collections::VecDeque::is_empty")]
    pub recent_no_action_summaries: std::collections::VecDeque<String>,

    /// First-class plan event taxonomy (v0.6.460+): step ids we have already
    /// emitted `plan.step.started` for, so the UI does not get double started
    /// events. The decision LLM emits no `step_started` signal — we backfill
    /// at finish time so every finished step has a paired started, and the UI
    /// can rely on started→finished ordering even when timestamps are nearly
    /// identical for fast steps.
    #[serde(default, skip_serializing_if = "std::collections::HashSet::is_empty")]
    pub started_step_ids: std::collections::HashSet<String>,

    /// TASKPLAN V3: step-completed signal from the decision LLM, consumed
    /// after action execution to emit canonical step events. It is read at the
    /// top of the NEXT iteration, so a pause between the two would otherwise
    /// drop the event entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_step_completed: Option<String>,

    /// TASKPLAN V3: step-failed signal from the decision LLM. Same
    /// next-iteration consumption as `pending_step_completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_step_failed: Option<String>,

    /// The decision LLM's request for hover discovery in the next observation.
    /// When true, the next observation enables hover probing to discover
    /// hidden menus/dropdowns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request_hover_discovery: Option<bool>,
}

impl LoopProtectiveState {
    /// True when nothing in the bundle has been touched, so a pause can omit
    /// the whole field rather than writing `"loop_protective_state":{}` onto
    /// every record — including runs that never trip a counter.
    pub fn is_pristine(&self) -> bool {
        self.loop_detector.is_pristine()
            && self.cumulative_run_cost_usd == 0.0
            && self.remaining_max_duration_ms.is_none()
            && self.deadline_at.is_none()
            && self.agentic_tool_repeat_counts.is_empty()
            && self.agentic_failed_tool_fingerprints.is_empty()
            && self.consecutive_parse_failures == 0
            && self.transient_retry_count == 0
            && self.yield_transient_retry_count == 0
            && self.consecutive_goal_reached_rejections == 0
            && self.consecutive_giveup_rejections == 0
            && self.consecutive_repeat_failed_action_rejections == 0
            && self.consecutive_no_action_iterations == 0
            && self.skip_iterations == 0
            && self.loop_recovery_context.is_none()
            && self.terminal_grounding_repair_state.is_pristine()
            && self.recent_no_action_summaries.is_empty()
            && self.started_step_ids.is_empty()
            && self.pending_step_completed.is_none()
            && self.pending_step_failed.is_none()
            && self.last_request_hover_discovery.is_none()
    }
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

fn loop_detector_is_pristine(
    detector: &crate::magician_v2::execution::agentic::loop_detector::LoopDetector,
) -> bool {
    detector.is_pristine()
}

fn repair_state_is_pristine(
    state: &crate::magician_v2::execution::agentic::executor::TerminalGroundingRepairState,
) -> bool {
    state.is_pristine()
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Joins a pause's storage key to its ask id in a HITL correlation id. Never
/// part of a storage key (those are execution, plan, step, agent, goal and
/// cycle ids joined by `:`) and unreserved in a URL path, where clients put
/// the correlation id verbatim.
pub const HITL_ASK_SEPARATOR: char = '~';

const ASK_ID_LEN: usize = 12;

fn fresh_ask_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..ASK_ID_LEN].to_owned()
}

/// The storage key a HITL correlation id addresses and, when the id names
/// one, the ask it was answering. A bare storage key — an id from a record
/// written before asks had identities, or a legacy client naming the pause
/// by its key — comes back with no ask, and resolves to whatever pause the
/// key holds now.
pub fn split_hitl_correlation_id(correlation_id: &str) -> (&str, Option<&str>) {
    match correlation_id.rsplit_once(HITL_ASK_SEPARATOR) {
        Some((key, ask))
            if !key.is_empty()
                && ask.len() == ASK_ID_LEN
                && ask.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            (key, Some(ask))
        },
        _ => (correlation_id, None),
    }
}

impl AgenticPauseState {
    /// Create a new pause state from current execution context
    pub fn new(
        iteration: usize,
        goal: impl Into<String>,
        success_criteria: impl Into<String>,
        environment_state: EnvironmentState,
        action_history_summary: impl Into<String>,
        max_iterations: usize,
        max_repeated_actions: usize,
    ) -> Self {
        Self {
            allowed_action_types: None,
            denied_capability_names: Vec::new(),
            plane_denied_capability_names: Vec::new(),
            plane_allowed_capability_names: None,
            harness_engine: None,
            run_engine_pin: None,
            max_cost_usd: None,
            denied_tool_params: HashMap::new(),
            browser_transports: Vec::new(),
            ask_id: Some(fresh_ask_id()),
            iteration,
            iteration_offset: 0,
            resolved_input_sensitivity: HashMap::new(),
            pending_sensitive: None,
            step_index: None,
            goal: goal.into(),
            success_criteria: success_criteria.into(),
            environment_state,
            action_history_summary: action_history_summary.into(),
            paused_at: Utc::now(),
            plan_id: None,
            step_id: None,
            task_id: None,
            chat_session_id: None,
            execution_id: None,
            pipeline_loop_state_segment: None,
            stateless_parked_segment: None,
            stateless_terminal_preparation: None,
            stateless_retry_due_at: None,
            root_execution_id: None,
            principal: None,
            workspace: None,
            invocation_context_override: None,
            work_authority: None,
            owner_invocation_stack: Vec::new(),
            transition_authorization: None,
            continuation_frames: Vec::new(),
            artifact_chain_id: None,
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            trust_level: None,
            trust_policies_path: None,
            approval_rules: Vec::new(),
            active_owner_agent_id: None,
            owner_stack: Vec::new(),
            spend_token_ids: Vec::new(),
            preserve_initial_tool_scope: false,
            preserved_initial_tools: Vec::new(),
            max_iterations,
            max_tokens_per_cycle: None,
            work_budget_secs: None,
            work_budget_consumed_ms: 0,
            llm_tokens_used: 0,
            max_delegation_depth: None,
            delegation_timeout_secs: None,
            max_repeated_actions,
            llm_model_override: None,
            llm_routing_overrides: None,
            execution_llm_routing_overrides: None,
            app_disclosure_checkpoint: None,
            app_workflow_continuation_checkpoint: None,
            prompt_identity: None,
            prior_procedure_memory: None,
            prior_environment_knowledge: None,
            continuation_response_id: None,
            continuation_last_has_images: None,
            stable_goal_hash: None,
            storage_base_path: crate::magician_v2::process_storage::runtime_root(),
            task_state: None,
            task_output_mode: TaskOutputMode::Accumulate,
            on_failure: OnFailureMode::default(),
            continuation_count: 0,
            expected_artifact_declarations: Vec::new(),
            chat_inline: false,
            coding_coordinator_run: false,
            live_messages: Vec::new(),
            approved_confirmation_actions: Vec::new(),
            loop_protective_state: None,
        }
    }

    /// Set the step index
    pub fn with_step_index(mut self, step_index: usize) -> Self {
        self.step_index = Some(step_index);
        self
    }

    /// Set the execution storage base path for scoped runtime artifacts.
    pub fn with_storage_base_path(mut self, storage_base_path: PathBuf) -> Self {
        self.storage_base_path = storage_base_path;
        self
    }

    /// Set observability context
    pub fn with_observability(
        mut self,
        execution_id: impl Into<String>,
        plan_id: impl Into<String>,
        step_id: impl Into<String>,
    ) -> Self {
        let execution_id = execution_id.into();
        self.execution_id = Some(execution_id);
        self.plan_id = Some(plan_id.into());
        self.step_id = Some(step_id.into());
        self
    }

    pub fn with_scope(
        mut self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        self.principal = Some(principal.into());
        self.workspace = Some(workspace.into());
        self
    }

    pub fn with_task_execution_context(
        mut self,
        task_id: Option<String>,
        execution_id: Option<String>,
        artifact_chain_id: Option<String>,
    ) -> Self {
        self.task_id = task_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self.execution_id = execution_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self.artifact_chain_id = artifact_chain_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    pub fn with_task_output_mode(mut self, task_output_mode: TaskOutputMode) -> Self {
        self.task_output_mode = task_output_mode;
        self
    }

    pub fn with_root_execution_id(mut self, root_execution_id: Option<String>) -> Self {
        self.root_execution_id = root_execution_id.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Set agent routing context for agent-scoped pause/resume.
    pub fn with_agent_routing(
        mut self,
        agent_id: impl Into<String>,
        goal_id: impl Into<String>,
        cycle_id: impl Into<String>,
    ) -> Self {
        self.agent_id = Some(agent_id.into());
        self.goal_id = Some(goal_id.into());
        self.cycle_id = Some(cycle_id.into());
        self
    }

    /// Set trust policy context for resume-time trust enforcement continuity.
    pub fn with_trust_policy(
        mut self,
        trust_level: impl Into<String>,
        trust_policies_path: impl Into<PathBuf>,
    ) -> Self {
        self.trust_level = Some(trust_level.into());
        self.trust_policies_path = Some(trust_policies_path.into());
        self
    }

    /// Set approval rules for resume-time approval gating continuity.
    pub fn with_approval_rules(mut self, approval_rules: Vec<ApprovalRule>) -> Self {
        self.approval_rules = approval_rules;
        self
    }

    /// Set the current execution owner and inherited owner stack.
    pub fn with_owner_snapshot(
        mut self,
        active_owner_agent_id: impl Into<String>,
        owner_stack: Vec<String>,
    ) -> Self {
        self.active_owner_agent_id = Some(active_owner_agent_id.into());
        self.owner_stack = owner_stack;
        self
    }

    /// Set per-context LLM model override for resume continuity.
    pub fn with_llm_model_override(mut self, model: Option<String>) -> Self {
        self.llm_model_override = model.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Set per-operation LLM routing overrides for resume continuity.
    pub fn with_llm_routing_overrides(
        mut self,
        overrides: Option<
            crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides,
        >,
    ) -> Self {
        self.llm_routing_overrides = overrides;
        self
    }

    /// Set the immutable execution-local routing overlay for resume.
    pub fn with_execution_llm_routing_overrides(
        mut self,
        overrides: Option<
            crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides,
        >,
    ) -> Self {
        self.execution_llm_routing_overrides = overrides;
        self
    }

    /// Set prompt identity context for resume continuity.
    pub fn with_prompt_identity(mut self, identity: Option<PromptIdentityContext>) -> Self {
        self.prompt_identity = identity;
        self
    }

    /// Set the stable goal hash for resume continuity.
    pub fn with_stable_goal_hash(mut self, goal_hash: Option<String>) -> Self {
        self.stable_goal_hash = goal_hash.and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        });
        self
    }

    /// Set the on_failure mode for pause/resume continuity
    pub fn with_on_failure(mut self, mode: OnFailureMode) -> Self {
        self.on_failure = mode;
        self
    }

    /// Set the continuation count for escalation rate limiting.
    pub fn with_continuation_count(mut self, count: usize) -> Self {
        self.continuation_count = count;
        self
    }

    pub fn with_task_state(mut self, task_state: Option<String>) -> Self {
        self.task_state = task_state;
        self
    }

    /// Set the expected artifact declarations for enrichment across pause/resume.
    pub fn with_expected_artifact_declarations(
        mut self,
        declarations: Vec<crate::magician_v2::agents::types::ArtifactDeclaration>,
    ) -> Self {
        self.expected_artifact_declarations = declarations;
        self
    }

    /// The correlation id the canonical `hitl.requested` for this pause
    /// carries and its `hitl.resolved` must match: the storage key joined to
    /// [`Self::ask_id`] by [`HITL_ASK_SEPARATOR`]. A client answers with it
    /// verbatim; [`split_hitl_correlation_id`] recovers the key it addresses
    /// and the ask it was answering.
    pub fn hitl_correlation_id(&self) -> String {
        match self.ask_id.as_deref() {
            Some(ask) => format!("{}{HITL_ASK_SEPARATOR}{ask}", self.storage_key()),
            None => self.storage_key(),
        }
    }

    /// Generate a unique key for storing this pause state.
    ///
    /// - Agent-scoped: `agent:{agent_id}:{goal_id}:{cycle_id}`
    /// - Execution-scoped: `{execution_id}:{plan_id}:{step_id}`
    /// - Neither: a random UUID
    pub fn storage_key(&self) -> String {
        use crate::magician_v2::agents::AGENT_KEY_PREFIX;
        if let (Some(aid), Some(gid), Some(cid)) = (&self.agent_id, &self.goal_id, &self.cycle_id) {
            format!("{}{}:{}:{}", AGENT_KEY_PREFIX, aid, gid, cid)
        } else if let (Some(eid), Some(pid), Some(sid)) =
            (&self.execution_id, &self.plan_id, &self.step_id)
        {
            format!("{}:{}:{}", eid, pid, sid)
        } else {
            uuid::Uuid::new_v4().to_string()
        }
    }

    /// Current owner snapshot when this pause state was captured.
    pub fn owner_snapshot(&self) -> Option<OwnerSnapshot> {
        self.active_owner_agent_id
            .as_ref()
            .map(|active_owner_agent_id| OwnerSnapshot {
                active_owner_agent_id: active_owner_agent_id.clone(),
                owner_stack: self.owner_stack.clone(),
            })
    }

    /// True when this pause carries an ELEVATION — a re-authorization that
    /// `restore_context_from_pause` would grant back to the resumed run:
    /// operator-approved action rules (`approval_rules`), already-granted
    /// one-time approvals (`approved_confirmation_actions`), an owner authority
    /// chain (`active_owner_agent_id` / `owner_stack`). These are exactly the
    /// fields the resume path restores unconditionally, so a forged or
    /// tampered-on-disk pause could re-mint authority it was never granted.
    ///
    /// `approved_confirmation_actions` joined this set when loop-protective
    /// state became durable: a granted-but-unconsumed approval is authority a
    /// resumed run spends without asking again, so a pause carrying one is an
    /// elevation even when nothing else on it is.
    ///
    /// Benign pauses (max-iterations, budget, plain user-input, manual) have no
    /// approval, owner-chain, protected product-surface authority, or preserved
    /// caller-defined tool scope and are NOT elevation — they resume with no
    /// authority binding check, exactly as before.
    ///
    /// Note: the secret-store approval markers (`secret_approval_challenge_id` /
    /// `secret_approval_request`) live on the `AgenticOutcome::WaitingForConfirmation`
    /// variant and on `FullPauseData`, NOT on `AgenticPauseState`, so they are not
    /// reachable here; the owner/approval-rule fields above fully cover the
    /// re-authorization surface this pause struct restores.
    pub fn is_elevation(&self) -> bool {
        !self.approval_rules.is_empty()
            || !self.approved_confirmation_actions.is_empty()
            || !self.owner_stack.is_empty()
            || !self.owner_invocation_stack.is_empty()
            || self.transition_authorization.is_some()
            || !self.continuation_frames.is_empty()
            || self.active_owner_agent_id.is_some()
            || self.invocation_context_override.is_some()
            || self.work_authority.is_some()
            || self.preserve_initial_tool_scope
            || !self.preserved_initial_tools.is_empty()
            || self.app_disclosure_checkpoint.is_some()
            || self.stateless_retry_due_at.is_some()
    }

    /// Stable blake3 hash over ONLY this pause's security-bearing fields — the
    /// exact subset `restore_context_from_pause` re-authorizes from.
    ///
    /// `loop_protective_state` is included even though it grants no authority:
    /// the gated resume path restores it, and editing it on disk re-mints the
    /// per-run USD ceiling, disarms the parse-abort and terminal-rejection
    /// counters, or sets `skip_iterations` high enough to burn a whole budget
    /// in no-op iterations. Anything the resume path restores belongs here. Computed
    /// identically at stage (when the elevation pause is persisted) and at resume
    /// (before those fields are restored). A mismatch means the on-disk
    /// authorization was tampered; an authority miss means the pause was never
    /// staged this boot (forged, or predates a restart). Both fail closed.
    ///
    /// Serialized as a tuple of borrowed refs so the field order is fixed and
    /// independent of the struct's serde layout. Fields are APPEND-ONLY and the
    /// order is load-bearing — reordering silently invalidates every staged
    /// pause. `approved_confirmation_actions` sits in the trailing tuple purely
    /// to keep related authority grouped; the outer tuple still has room (15 of
    /// serde's 16 slots), so a future field may be appended there directly.
    /// Degrades to an empty-input hash on
    /// the (unreachable, all fields are plainly serializable) serialize error rather
    /// than panicking.
    ///
    /// Canonicalized through `serde_json::to_value` FIRST: `approval_rules` nests a
    /// `HashMap` (`ApprovalCondition::param_matches`), whose direct `to_vec` key
    /// order is per-instance — a freshly-staged pause and a disk-recovered one could
    /// serialize the same map in different orders and produce a false hash mismatch
    /// (a spurious fail-closed on a legitimate resume). Without serde_json's
    /// `preserve_order` feature, `serde_json::Value`'s object map is a `BTreeMap`, so
    /// routing through `to_value` sorts every object's keys and pins the bytes. NOTE:
    /// if `preserve_order` is ever enabled workspace-wide this must switch to an
    /// explicit recursive key-sort — `to_value` would then preserve insertion order.
    pub fn authorization_hash(&self) -> blake3::Hash {
        // Hash `loop_protective_state` in the form it has AFTER one serde
        // round trip. The stage side hashes the in-memory pause and the
        // resume side hashes the body read back from disk, and this is the
        // field that does not survive the trip — its loop detector's
        // "pristine" skip is decided by state serde does not carry — so the
        // two hashes differed. The envelope seal was already taken over the
        // read-back body for that reason; the process-authority record in
        // `FullPauseStore::store` was not, so an elevated pause staged by
        // this very boot was refused on resume as "not staged by magician
        // this boot", the pause dropped, and the owner's next retry found
        // nothing to resume (a phone operator's HITL card, answered once,
        // then a timeout). A round trip is idempotent, so both sides hash
        // the same bytes by construction.
        //
        // Only this field. Round-tripping the whole pause — live messages,
        // environment state, attached images — on the execution worker's
        // 2 MiB stack overflowed it in a debug build at the very commit that
        // parks a run, three times in a row, until the supervisor's restart
        // budget tripped.
        let normalized_loop_protective_state: Option<LoopProtectiveState> = self
            .loop_protective_state
            .as_ref()
            .and_then(|state| serde_json::to_value(state).ok())
            .and_then(|value| serde_json::from_value(value).ok());
        self.authorization_hash_of_fields(
            normalized_loop_protective_state
                .as_ref()
                .or(self.loop_protective_state.as_ref()),
        )
    }

    fn authorization_hash_of_fields(
        &self,
        loop_protective_state: Option<&LoopProtectiveState>,
    ) -> blake3::Hash {
        let canonical = serde_json::to_value((
            &self.trust_level,
            &self.trust_policies_path,
            &self.approval_rules,
            &self.active_owner_agent_id,
            &self.owner_stack,
            &self.owner_invocation_stack,
            &self.transition_authorization,
            &self.continuation_frames,
            &self.spend_token_ids,
            &self.invocation_context_override,
            &self.work_authority,
            &self.preserve_initial_tool_scope,
            &self.preserved_initial_tools,
            &self.llm_routing_overrides,
            (
                &self.execution_llm_routing_overrides,
                &self.app_disclosure_checkpoint,
                &self.live_messages,
                &self.approved_confirmation_actions,
                &loop_protective_state,
                &self.stateless_parked_segment,
                &self.pipeline_loop_state_segment,
                &self.stateless_terminal_preparation,
                &self.stateless_retry_due_at,
                // The ceilings. Widening a restriction is an escalation, so the
                // hash must cover them even though they do not, by themselves,
                // make a pause an elevation.
                //
                // `denied_tool_params` is a `HashMap`, and this hash is only
                // stable because `serde_json`'s `Map` is `BTreeMap`-backed here:
                // `to_value` therefore sorts its keys instead of preserving the
                // map's own randomised iteration order. Enabling serde_json's
                // `preserve_order` feature anywhere in the tree would silently
                // reverse that, and every elevation resume would then fail its
                // own tamper check with no edit to blame.
                (
                    &self.allowed_action_types,
                    &self.denied_capability_names,
                    &self.denied_tool_params,
                    &self.browser_transports,
                ),
            ),
        ))
        .unwrap_or(serde_json::Value::Null);
        // The launch pin decides which foreign harness thinks after resume,
        // so a tampered pin (or engine) must fail the check like a widened
        // ceiling. Both join the hash only when a pin is present: every run
        // composed since pins exist pauses with one, while a pause written
        // before them — even a plane-started one that names an engine —
        // keeps the exact hash it was sealed with. Stripping the pin from a
        // newer pause changes its hash.
        let canonical = if self.run_engine_pin.is_some() {
            serde_json::to_value((&canonical, &self.harness_engine, &self.run_engine_pin))
                .unwrap_or(serde_json::Value::Null)
        } else {
            canonical
        };
        let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
        blake3::hash(&bytes)
    }
}

/// User's response to an input request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputResponse {
    /// The type of input that was requested
    pub input_type: UserInputType,

    /// The value provided by the user
    pub value: UserInputValue,

    /// When the response was received
    pub timestamp: DateTime<Utc>,
}

impl UserInputResponse {
    /// Create a new user input response
    pub fn new(input_type: UserInputType, value: UserInputValue) -> Self {
        Self {
            input_type,
            value,
            timestamp: Utc::now(),
        }
    }

    /// Check if the user aborted
    pub fn is_aborted(&self) -> bool {
        matches!(self.value, UserInputValue::Aborted { .. })
    }
}

/// Value provided by the user in response to an input request.
///
/// Each variant corresponds to a `UserInputType`, plus an `Aborted` variant
/// for when the user cancels the operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserInputValue {
    /// Free-form text response
    Text { value: String },

    /// Password/sensitive value (will be redacted in logs)
    Password { value: String },

    /// Single choice selection (by option id)
    Choice {
        selected_id: String,
        /// If allow_other was true and user chose "other"
        other_value: Option<String>,
    },

    /// Multiple choice selections (by option ids)
    MultiChoice { selected_ids: Vec<String> },

    /// Confirmation response
    Confirmation { confirmed: bool },

    /// External action completed
    ExternalActionCompleted {
        /// Optional user guidance text to inject into next iteration context
        #[serde(skip_serializing_if = "Option::is_none", default)]
        guidance: Option<String>,
    },

    /// File path(s) provided
    FilePath { paths: Vec<String> },

    /// Guidance/advice provided
    Guidance { advice: String },

    /// User aborted/cancelled the input request
    Aborted {
        /// Optional reason for aborting
        reason: Option<String>,
    },

    /// Answers to [`UserInputType::Form`]. `skipped` is not abort.
    Form { answers: Vec<FormAnswer> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormAnswer {
    pub id: String,
    #[serde(default)]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_ids: Vec<String>,
}

impl UserInputValue {
    /// Create a text value
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text {
            value: value.into(),
        }
    }

    /// Create a password value
    pub fn password(value: impl Into<String>) -> Self {
        Self::Password {
            value: value.into(),
        }
    }

    /// Create a confirmation value
    pub fn confirmed(confirmed: bool) -> Self {
        Self::Confirmation { confirmed }
    }

    /// Create an aborted value
    pub fn aborted(reason: Option<String>) -> Self {
        Self::Aborted { reason }
    }

    /// Get the text value if this is a Text variant
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { value } => Some(value),
            _ => None,
        }
    }

    /// Get the password value if this is a Password variant
    /// NOTE: Use carefully - this exposes the sensitive value
    pub fn as_password(&self) -> Option<&str> {
        match self {
            Self::Password { value } => Some(value),
            _ => None,
        }
    }

    /// Check if this is an abort
    pub fn is_aborted(&self) -> bool {
        matches!(self, Self::Aborted { .. })
    }
}

// ============================================================================
// Execution History
// ============================================================================

/// Provider/native tool call emitted by the outer-loop assistant turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgenticAssistantToolCallRecord {
    /// Provider-assigned tool-call id when available.
    pub id: String,
    /// Native tool name selected by the model.
    pub name: String,
    /// Tool-call arguments exactly as projected from the provider response.
    pub arguments: Value,
}

/// Assistant-visible outer-loop model turn.
///
/// This stores the text/tool-call surface that the model emitted. It is not
/// private reasoning. Prompt context uses a bounded rendering of these records
/// so the next outer decision sees the causal chain between prior decisions and
/// tool dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgenticAssistantTurnRecord {
    /// Outer-loop iteration this turn belongs to. Zero means not yet attached.
    pub iteration: usize,
    /// Native operation or synthesized source, e.g. `agentic_decision`.
    pub operation: String,
    /// Authoritative local identity for the model call that produced this
    /// assistant turn. It is persisted for causal tool/result lineage but is
    /// intentionally omitted from the prompt rendering below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_trace_context: Option<magicllm::LlmTraceContext>,
    /// Assistant-visible text emitted alongside tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Native tool calls emitted by the assistant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AgenticAssistantToolCallRecord>,
    /// Provider finish reason when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Prompt tokens reported by the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    /// Completion tokens reported by the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    /// Provider-emitted reasoning / chain-of-thought summary for this turn
    /// (OpenAI Responses reasoning items, Anthropic extended thinking, etc.).
    /// The active profile requests this summary; previously it was parsed off
    /// the response and discarded. Persisted here so the decision trail keeps
    /// the model's stated reasoning alongside its text + tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Timestamp when this turn was captured.
    pub timestamp: DateTime<Utc>,
}

impl AgenticAssistantTurnRecord {
    pub fn with_iteration(mut self, iteration: usize) -> Self {
        self.iteration = iteration;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.text
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
            && self.tool_calls.is_empty()
    }
}

fn format_agentic_assistant_turn_for_llm(turn: &AgenticAssistantTurnRecord) -> String {
    let mut entry = format!(
        "- Outer iteration {} [{}]",
        if turn.iteration == 0 {
            "?".to_string()
        } else {
            turn.iteration.to_string()
        },
        turn.operation
    );

    if let Some(text) = turn
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        let text = if text.len() > ASSISTANT_TURN_TEXT_LIMIT {
            truncate_middle(text, ASSISTANT_TURN_TEXT_LIMIT)
        } else {
            text.to_string()
        };
        entry.push_str(&format!("\n  Assistant text: {}", text));
    }

    if !turn.tool_calls.is_empty() {
        let calls = turn
            .tool_calls
            .iter()
            .map(|call| {
                let args = serde_json::to_string(&call.arguments)
                    .unwrap_or_else(|_| call.arguments.to_string());
                let args = if args.len() > ASSISTANT_TURN_ARGUMENT_LIMIT {
                    truncate_middle(&args, ASSISTANT_TURN_ARGUMENT_LIMIT)
                } else {
                    args
                };
                format!("{}({})", call.name, args)
            })
            .collect::<Vec<_>>()
            .join("; ");
        entry.push_str(&format!("\n  Tool calls: {}", calls));
    }

    let token_summary = match (turn.prompt_tokens, turn.completion_tokens) {
        (Some(prompt), Some(completion)) => Some(format!("tokens={}+{}", prompt, completion)),
        (Some(prompt), None) => Some(format!("prompt_tokens={}", prompt)),
        (None, Some(completion)) => Some(format!("completion_tokens={}", completion)),
        (None, None) => None,
    };
    if turn.finish_reason.is_some() || token_summary.is_some() {
        let mut meta = Vec::new();
        if let Some(reason) = turn.finish_reason.as_deref() {
            meta.push(format!("finish_reason={reason}"));
        }
        if let Some(tokens) = token_summary {
            meta.push(tokens);
        }
        entry.push_str(&format!("\n  Metadata: {}", meta.join(", ")));
    }

    entry
}

/// Record of a single iteration in the agentic loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IterationRecord {
    /// Iteration number (1-indexed)
    pub iteration: usize,

    /// State observed before taking action
    pub state_before: EnvironmentState,

    /// Action that was executed
    pub action: ExecutableAction,

    /// Result of the action
    pub result: ActionResultRecord,

    /// State after action execution
    pub state_after: EnvironmentState,

    /// Timestamp of this iteration
    pub timestamp: DateTime<Utc>,

    /// SOTA Phase 6: Action verification result (pre/post visual comparison)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<ActionVerification>,

    /// LLM's reasoning/thinking for choosing this action (for debugging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_reasoning: Option<String>,
}

/// Tri-state outcome category for verified actions.
/// Provides richer feedback than binary success/fail so the agentic LLM
/// can decide whether to continue the same approach or switch strategies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ActionOutcomeCategory {
    /// The specific goal of this action has been fully achieved
    GoalReached,
    /// Action worked (scroll moved, slider changed) but goal not yet met.
    /// Agent should CONTINUE with the same approach.
    PartialProgress,
    /// No observable change occurred (scroll at boundary, click had no effect).
    /// Agent should try a DIFFERENT approach.
    NoEffect,
    /// Action execution itself failed (error, timeout, etc.)
    Failed,
    /// The App owner durably released this attempt before I/O because its
    /// admission expired. This is a bounded retry hint, never I/O authority.
    AdmissionExpiredBeforeIo,
}

/// Result of executing an action (for history tracking).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResultRecord {
    /// Whether the action succeeded
    pub success: bool,

    /// Output from the action (if successful)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,

    /// Error message (if failed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Duration in milliseconds
    pub duration_ms: u64,

    /// Tri-state outcome from verification (richer than binary success/fail)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_category: Option<ActionOutcomeCategory>,

    // ── API Mining Observability ──────────────────────────────
    /// Whether API replay was attempted and succeeded for this action.
    /// `None` means API mining is disabled or not applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_replay_used: Option<bool>,

    /// Wall-clock time from action dispatch to response (ms) when API replay was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_replay_time_ms: Option<u64>,

    /// If browser automation was used after an API replay attempt failed,
    /// the reason for falling back (e.g., "verification_failed", "replay_timeout",
    /// "capability_not_found", "extension_error").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_fallback_reason: Option<String>,

    /// Versioned bounded model result plus scope-bound raw descriptor. Legacy
    /// `output` remains readable during transcript/history migration, while
    /// new decision prompts consume this structured value when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_projection:
        Option<crate::magician_v2::tool_result_projection::ProjectedToolResultV1>,
}

impl ActionResultRecord {
    /// Allow one fresh governed attempt after a proven pre-I/O expiry. Prose,
    /// unknown outcomes, cancellation and ordinary failures do not qualify.
    /// The next attempt still owes every admission and resource check.
    pub fn permits_admission_retry(failures: &[&Self]) -> bool {
        matches!(failures, [failure] if !failure.success
            && failure.outcome_category == Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo))
    }

    /// Create a new record with API mining fields defaulting to None.
    pub fn new(
        success: bool,
        output: Option<String>,
        error: Option<String>,
        duration_ms: u64,
        outcome_category: Option<ActionOutcomeCategory>,
    ) -> Self {
        Self {
            success,
            output,
            error,
            duration_ms,
            outcome_category,
            api_replay_used: None,
            api_replay_time_ms: None,
            browser_fallback_reason: None,
            tool_result_projection: None,
        }
    }

    /// Return the only model-authoritative value for this result.
    ///
    /// New autonomous results persist a bounded, schema-versioned projection
    /// beside the legacy `output` string. Large legacy strings are deliberately
    /// replaced by typed omission records, so consumers must never prefer that
    /// compatibility field when a valid projection exists. Conversely, an
    /// invalid/future projection fails closed instead of making legacy bytes
    /// authoritative again.
    pub fn model_authoritative_value(&self) -> Option<serde_json::Value> {
        if let Some(projection) = self.tool_result_projection.as_ref() {
            return projection.validate_schema_version().is_ok().then(|| {
                crate::magician_v2::tool_result_projection::provider_safe_model_value(projection)
            });
        }

        self.output
            .as_deref()
            .and_then(|output| serde_json::from_str(output).ok())
    }
}

#[derive(Debug, Clone)]
struct PrimitiveHistoryContext {
    parent_outer_iteration: Option<usize>,
    inner_iteration: Option<usize>,
    inner_run_index: Option<usize>,
    inner_run_id: Option<String>,
    capability_name: Option<String>,
    objective_id: Option<String>,
}

fn value_as_usize(value: Option<&Value>) -> Option<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn value_as_non_empty_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn primitive_history_context(record: &IterationRecord) -> Option<PrimitiveHistoryContext> {
    let ExecutableAction::Pack {
        resolved_params, ..
    } = &record.action
    else {
        return None;
    };
    if resolved_params.get("__loop_kind").and_then(Value::as_str) != Some("inner") {
        return None;
    }

    Some(PrimitiveHistoryContext {
        parent_outer_iteration: value_as_usize(resolved_params.get("__parent_outer_iteration")),
        inner_iteration: value_as_usize(resolved_params.get("__inner_iteration")),
        inner_run_index: value_as_usize(resolved_params.get("__inner_run_index")),
        inner_run_id: value_as_non_empty_string(resolved_params.get("__inner_run_id")),
        capability_name: value_as_non_empty_string(resolved_params.get("__inner_capability")),
        objective_id: value_as_non_empty_string(resolved_params.get("__inner_objective_id")),
    })
}

fn format_primitive_history_label(
    ctx: &PrimitiveHistoryContext,
    fallback_iteration: usize,
) -> String {
    let capability = ctx.capability_name.as_deref().unwrap_or("primitive");
    let run_label = ctx
        .inner_run_id
        .as_deref()
        .map(ToString::to_string)
        .or_else(|| {
            ctx.inner_run_index
                .map(|index| format!("{capability}_run_{index:04}"))
        })
        .unwrap_or_else(|| format!("{capability}_run_unknown"));
    let inner_iteration = ctx.inner_iteration.unwrap_or(fallback_iteration);
    let parent = ctx
        .parent_outer_iteration
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let objective = ctx
        .objective_id
        .as_deref()
        .map(|value| format!(", objective_id={value}"))
        .unwrap_or_default();
    format!(
        "Inner iteration {inner_iteration} ({capability}, {run_label}, parent_outer_iteration={parent}{objective})"
    )
}

// ============================================================================
// Artifact Prompt Budgeting Constants
// ============================================================================

/// Maximum number of artifacts shown per kind in the
/// `## AVAILABLE ARTIFACTS` section of the decision prompt.
pub const ARTIFACT_PER_KIND_MAX: usize = 5;

/// Overall cap on artifacts shown in the
/// `## AVAILABLE ARTIFACTS` section. When per-kind budgets would
/// exceed this total, the kind with the largest current budget is
/// decremented repeatedly until the sum equals this cap.
pub const ARTIFACT_TOTAL_MAX: usize = 15;

/// Extract the `artifact_kind` string from an artifact's JSON
/// payload, or `"other"` for non-JSON / missing-field artifacts.
#[derive(Deserialize)]
struct ArtifactKindHeader {
    #[serde(default)]
    artifact_kind: Option<String>,
}

const MAX_ARTIFACT_HEADER_INSPECTION_BYTES: usize = 4 * 1024 * 1024;

pub fn bounded_artifact_json_is_valid(data: &[u8], max_bytes: usize) -> bool {
    data.len() <= max_bytes
        && crate::magician_v2::json_traversal::json_bytes_nesting_is_bounded(
            data,
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
        )
        && serde_json::from_slice::<serde::de::IgnoredAny>(data).is_ok()
}

/// Decode only the bounded logical header. Unknown fields are skipped by the
/// deserializer rather than being materialized into a complete `Value` tree.
pub fn artifact_header_from_json<T>(data: &[u8]) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    if data.len() > MAX_ARTIFACT_HEADER_INSPECTION_BYTES
        || !crate::magician_v2::json_traversal::json_bytes_nesting_is_bounded(
            data,
            crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH,
        )
    {
        return None;
    }
    serde_json::from_slice::<T>(data).ok()
}

pub fn artifact_kind_from_json_header(data: &[u8]) -> Option<String> {
    artifact_header_from_json::<ArtifactKindHeader>(data).and_then(|header| header.artifact_kind)
}

fn artifact_kind(artifact: &Artifact) -> String {
    if artifact.content_type != "application/json" {
        return "other".to_string();
    }
    artifact
        .artifact_type
        .clone()
        .or_else(|| artifact_kind_from_json_header(&artifact.data))
        .unwrap_or_else(|| "other".to_string())
}

/// Compute per-kind display budgets for the AVAILABLE ARTIFACTS
/// section. See docs/plans/2026-04-23-artifact-per-kind-cap-design.md.
///
/// Algorithm:
/// 1. For each kind, budget = min(per_kind_max, available_count).
/// 2. While sum(budgets) > total_max, decrement the kind with the
///    largest current budget. Ties broken by larger available count,
///    then by lexicographically smaller kind name.
fn compute_artifact_budgets(
    kinds_with_counts: Vec<(String, usize)>,
    per_kind_max: usize,
    total_max: usize,
) -> std::collections::HashMap<String, usize> {
    use std::collections::HashMap;

    let available: HashMap<String, usize> = kinds_with_counts.iter().cloned().collect();

    let mut budgets: HashMap<String, usize> = kinds_with_counts
        .iter()
        .map(|(k, cnt)| (k.clone(), per_kind_max.min(*cnt)))
        .collect();

    loop {
        let sum: usize = budgets.values().sum();
        if sum <= total_max {
            break;
        }

        let pick = budgets
            .iter()
            .filter(|(_, &b)| b > 0)
            .max_by(|a, b| {
                a.1.cmp(b.1)
                    .then_with(|| available[a.0].cmp(&available[b.0]))
                    .then_with(|| b.0.cmp(a.0))
            })
            .map(|(k, _)| k.clone());

        match pick {
            Some(k) => {
                if let Some(b) = budgets.get_mut(&k) {
                    *b -= 1;
                }
            },
            None => break,
        }
    }

    budgets
}

/// Full execution history for the agentic run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionHistory {
    /// All iteration records
    pub iterations: Vec<IterationRecord>,
    /// Assistant-visible outer-loop model turns captured before dispatch.
    pub assistant_turns: Vec<AgenticAssistantTurnRecord>,
    /// Artifacts produced throughout the current run (including sub-goals).
    pub artifacts: Vec<Artifact>,
    /// Persisted artifacts loaded from the artifact store at run start.
    ///
    /// These are available for prompt context and follow-up actions, but are
    /// not treated as newly produced artifacts for the current run.
    #[serde(default, skip)]
    pub seeded_artifacts: Vec<Artifact>,
    /// Opaque provider continuation id carried from the prior outer-loop turn.
    /// Re-fed on the next compatible turn so only new suffix input is sent.
    /// Reset when request shape, model, provider, or transport cohort changes;
    /// the chain then rebuilds from the bounded full conversation. Inert for
    /// stateless providers.
    pub last_response_id: Option<String>,
    /// Image-shape of the PRIOR outer-loop turn's request (`true` when it
    /// carried a screenshot). Compared against the current turn's shape in
    /// `decide_next_action`: when it flips, `last_response_id` must be dropped
    /// for that call, because a vision toggle can select a different
    /// model/profile and provider continuation ids are cohort-scoped. `None`
    /// before the first turn records a shape.
    pub last_has_images: Option<bool>,
    /// Live, append-only conversation the model reasons over (CC's
    /// `mutableMessages` model). Each completed outer iteration contributes
    /// exactly one `(Assistant, User)` pair, folded in once by
    /// `decision::sync_live_messages` and never rebuilt afterwards — so a past
    /// turn's content is frozen at write-time rather than re-derived (and
    /// possibly re-truncated/re-evicted) every decision. `decide_next_action`
    /// snapshots this via `compact_live_messages`; the `iterations` /
    /// `assistant_turns` records remain the durable full log and the source
    /// these pairs are folded from. Empty until the first sync.
    pub live_messages: Vec<magicllm::prelude::LLMMessage>,
    /// Number of `iterations` records already folded into `live_messages`.
    /// `sync_live_messages` processes `iterations[live_synced_records..]`,
    /// appends their pairs, then advances this to `iterations.len()`. Kept in
    /// lockstep with `live_messages` so a turn is folded exactly once. On
    /// resume, set to `iterations.len()` whenever `live_messages` was
    /// persisted (it already covers every persisted iteration).
    pub live_synced_records: usize,
}

impl ExecutionHistory {
    /// Create a new empty history
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an assistant-visible outer-loop turn to the execution history.
    pub fn record_assistant_turn(&mut self, turn: AgenticAssistantTurnRecord) {
        if !turn.is_empty() {
            self.assistant_turns.push(turn);
        }
    }

    /// Check if proposed action would create a repeat pattern.
    /// Includes the proposed action in the check to catch loops early.
    pub fn would_repeat(&self, proposed_action: &ExecutableAction, threshold: usize) -> bool {
        if threshold == 0 {
            return false;
        }

        let proposed_sig = action_signature(proposed_action);

        // Need at least (threshold - 1) previous actions to form a pattern with proposed
        if self.iterations.len() < threshold - 1 {
            return false;
        }

        // Check if last (threshold - 1) actions + proposed are all identical
        let recent_sigs: Vec<_> = self
            .iterations
            .iter()
            .rev()
            .take(threshold - 1)
            .map(|i| action_signature(&i.action))
            .collect();

        // All recent actions must match the proposed action
        recent_sigs.iter().all(|sig| sig == &proposed_sig)
    }

    /// Render the execution history excluding the last `skip_recent`
    /// iterations and assistant turns. The outer-loop prompt builder calls
    /// this when the most recent K iterations are already in the message
    /// list as proper assistant / user tool pairs — including them again in
    /// the summary section would double-count tokens and confuse the model
    /// about how much history actually exists.
    pub fn format_for_llm_skip_recent(&self, skip_recent: usize, max_iterations: usize) -> String {
        if skip_recent == 0 {
            return self.format_for_llm(max_iterations);
        }

        if self.iterations.is_empty() {
            let mut parts = Vec::new();
            if let Some(assistant_turns) =
                self.format_recent_assistant_turns_for_llm_with_skip(skip_recent, max_iterations)
            {
                parts.push(assistant_turns);
            }
            if let Some(artifacts_section) =
                self.format_artifacts_for_llm(ARTIFACT_PER_KIND_MAX, ARTIFACT_TOTAL_MAX)
            {
                parts.push(artifacts_section);
            }
            let rendered = if parts.is_empty() {
                String::new()
            } else {
                parts.join("\n\n")
            };
            return bound_outer_prompt_context(
                "outer execution history",
                rendered,
                OUTER_HISTORY_CONTEXT_LIMIT,
            );
        }

        let mut parts = self
            .format_recent_iteration_details_for_llm_with_skip(skip_recent, max_iterations)
            .unwrap_or_default();

        if let Some(assistant_turns) =
            self.format_recent_assistant_turns_for_llm_with_skip(skip_recent, max_iterations)
        {
            parts.push(assistant_turns);
        }

        if let Some(artifacts_section) =
            self.format_artifacts_for_llm(ARTIFACT_PER_KIND_MAX, ARTIFACT_TOTAL_MAX)
        {
            parts.push(artifacts_section);
        }

        bound_outer_prompt_context(
            "outer execution history",
            parts.join("\n\n"),
            OUTER_HISTORY_CONTEXT_LIMIT,
        )
    }

    /// Get a summary of recent iterations for LLM prompt
    pub fn format_for_llm(&self, max_iterations: usize) -> String {
        if self.iterations.is_empty() {
            let mut parts = Vec::new();
            if let Some(assistant_turns) =
                self.format_recent_assistant_turns_for_llm(max_iterations)
            {
                parts.push(assistant_turns);
            }
            if let Some(artifacts_section) =
                self.format_artifacts_for_llm(ARTIFACT_PER_KIND_MAX, ARTIFACT_TOTAL_MAX)
            {
                parts.push(artifacts_section);
            }
            let rendered = if parts.is_empty() {
                "No actions taken yet.".to_string()
            } else {
                parts.join("\n\n")
            };
            return bound_outer_prompt_context(
                "outer execution history",
                rendered,
                OUTER_HISTORY_CONTEXT_LIMIT,
            );
        }

        let mut parts = self
            .format_recent_iteration_details_for_llm(max_iterations)
            .unwrap_or_default();

        if let Some(assistant_turns) = self.format_recent_assistant_turns_for_llm(max_iterations) {
            parts.push(assistant_turns);
        }

        if let Some(artifacts_section) =
            self.format_artifacts_for_llm(ARTIFACT_PER_KIND_MAX, ARTIFACT_TOTAL_MAX)
        {
            parts.push(artifacts_section);
        }

        bound_outer_prompt_context(
            "outer execution history",
            parts.join("\n\n"),
            OUTER_HISTORY_CONTEXT_LIMIT,
        )
    }

    pub fn format_recent_history_for_llm(&self, max_iterations: usize) -> String {
        let rendered = self
            .format_recent_iteration_details_for_llm(max_iterations)
            .map(|entries| entries.join("\n\n"))
            .unwrap_or_else(|| "No actions taken yet.".to_string());
        bound_outer_prompt_context(
            "outer recent history",
            rendered,
            OUTER_RECENT_HISTORY_CONTEXT_LIMIT,
        )
    }

    pub fn format_recent_assistant_turns_for_llm(&self, max_turns: usize) -> Option<String> {
        self.format_recent_assistant_turns_for_llm_with_skip(0, max_turns)
    }

    /// Variant that skips the last `skip_recent` assistant turns before
    /// taking the next `max_turns` going backward. Used by the outer-loop
    /// prompt builder when the most recent turns are already in the message
    /// list as real tool-call / tool-result pairs and should not be
    /// duplicated in the summary section.
    pub fn format_recent_assistant_turns_for_llm_with_skip(
        &self,
        skip_recent: usize,
        max_turns: usize,
    ) -> Option<String> {
        if self.assistant_turns.is_empty() || max_turns == 0 {
            return None;
        }

        let mut recent: Vec<_> = self
            .assistant_turns
            .iter()
            .rev()
            .skip(skip_recent)
            .take(max_turns)
            .collect();
        if recent.is_empty() {
            return None;
        }
        recent.reverse();

        let mut lines = Vec::with_capacity(recent.len() + 1);
        lines.push("Recent outer assistant turns (visible text + native tool calls):".to_string());
        for turn in recent {
            lines.push(format_agentic_assistant_turn_for_llm(turn));
        }
        Some(lines.join("\n"))
    }

    pub fn format_artifact_list_for_llm(&self, per_kind_max: usize, total_max: usize) -> String {
        self.format_artifacts_for_llm(per_kind_max, total_max)
            .map(|section| {
                section
                    .strip_prefix("Available artifacts:\n")
                    .unwrap_or(section.as_str())
                    .to_string()
            })
            .unwrap_or_else(|| "No artifacts available.".to_string())
    }

    fn format_recent_iteration_details_for_llm(
        &self,
        max_iterations: usize,
    ) -> Option<Vec<String>> {
        self.format_recent_iteration_details_for_llm_with_skip(0, max_iterations)
    }

    /// Variant that skips the last `skip_recent` iterations before taking
    /// the next `max_iterations` going backward. Used by the outer-loop
    /// prompt builder so the summary section doesn't duplicate iterations
    /// already present in the message list as proper tool-call pairs.
    fn format_recent_iteration_details_for_llm_with_skip(
        &self,
        skip_recent: usize,
        max_iterations: usize,
    ) -> Option<Vec<String>> {
        if self.iterations.is_empty() {
            return None;
        }

        let recent: Vec<_> = self
            .iterations
            .iter()
            .rev()
            .skip(skip_recent)
            .take(max_iterations)
            .collect();
        if recent.is_empty() {
            return None;
        }
        let mut parts = vec![];

        // Detect consecutive failures and add a prominent progress-pressure warning.
        // This helps the LLM understand when its current approach isn't working.
        let consecutive_failures = self.count_consecutive_failures();
        if consecutive_failures >= 2 {
            let failed_actions = self.get_recent_failed_actions(consecutive_failures);
            let unique_targets: std::collections::HashSet<_> = failed_actions.iter().collect();
            let recovery_hint = "Consider: inspect the returned error/output, verify assumptions against available state, and try a different tool or parameters.";

            let warning = if unique_targets.len() == 1 {
                // Same action failing repeatedly
                format!(
                    "⚠️ **REPEATED FAILURE WARNING**: The same action has failed {} times consecutively.\n\
                     Failed action: {}\n\
                     **YOU MUST TRY A DIFFERENT APPROACH** - the current method is not working.\n\
                     {}\n",
                    consecutive_failures,
                    failed_actions.first().unwrap_or(&"unknown".to_string()),
                    recovery_hint
                )
            } else {
                // Different actions failing
                format!(
                    "⚠️ **CONSECUTIVE FAILURE WARNING**: {} actions have failed in a row.\n\
                     Failed actions: {}\n\
                     **CHANGE YOUR APPROACH** - something about the current page state may be blocking progress.\n\
                     Consider: check for overlays/modals, scroll to different position, or verify the target exists.\n",
                    consecutive_failures,
                    failed_actions.join(", ")
                )
            };
            parts.push(warning);
        }

        // Determine the most recent record so we can give it full reasoning.
        // Inner-loop iteration numbers are local to their run, so do not key
        // this on the numeric iteration value alone.
        let most_recent_timestamp = recent.first().map(|r| &r.timestamp);

        for record in &recent {
            let action_desc = action_signature(&record.action);
            // Control actions like `create_task` are fire-and-forget delegations:
            // the parent loop must NOT "continue same approach" by spawning more
            // children. Without this carve-out, the generic PartialProgress
            // guidance loops the outer LLM into stacking duplicate child tasks.
            let is_create_task_record = action_desc.contains("# create_task:");
            let is_task_control_record = action_desc.contains("# list_tasks")
                || action_desc.contains("# run_task:")
                || action_desc.contains("# stop_task:");
            let status = match &record.result.outcome_category {
                Some(ActionOutcomeCategory::GoalReached) => "SUCCESS",
                Some(ActionOutcomeCategory::PartialProgress) => {
                    if is_create_task_record {
                        "DELEGATED (child task created — do NOT call create_task again for the same intent; either continue with another action or call `yield` if your work is complete)"
                    } else if is_task_control_record {
                        "CONTROL_RESULT (use the returned task information/action result to answer the user or choose the next distinct step)"
                    } else {
                        "PARTIAL (action worked but goal not yet achieved - continue same approach)"
                    }
                },
                Some(ActionOutcomeCategory::NoEffect) => {
                    "NO_EFFECT (no observable change - try completely different approach)"
                },
                Some(ActionOutcomeCategory::Failed) => "FAILED",
                Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo) => {
                    "NOT_DISPATCHED (admission expired before I/O; one fresh governed retry is allowed)"
                },
                None => {
                    if record.result.success {
                        "SUCCESS"
                    } else {
                        "FAILED"
                    }
                },
            };
            let iteration_label = if let Some(inner_context) = primitive_history_context(record) {
                format_primitive_history_label(&inner_context, record.iteration)
            } else {
                format!("Outer iteration {}", record.iteration)
            };
            let mut entry = format!(
                "{}: {} - {} ({}ms)",
                iteration_label, action_desc, status, record.result.duration_ms
            );

            // Include LLM reasoning to maintain context across iterations.
            // The most recent iteration gets full reasoning (up to 1000 chars) since
            // it contains the freshest intent/plan that should inform the next decision.
            // Older iterations use middle-truncation to preserve both context (head)
            // and intent/plan (tail), which LLMs naturally place at the end of reasoning.
            if let Some(reasoning) = &record.llm_reasoning {
                let is_most_recent = most_recent_timestamp == Some(&record.timestamp);
                let formatted_reasoning = if is_most_recent {
                    // Most recent: generous limit, full reasoning preferred
                    if reasoning.len() > REASONING_LIMIT_MOST_RECENT {
                        truncate_middle(reasoning, REASONING_LIMIT_MOST_RECENT)
                    } else {
                        reasoning.clone()
                    }
                } else {
                    // Older iterations: middle-truncate to preserve head + tail
                    if reasoning.len() > REASONING_LIMIT_OLDER {
                        truncate_middle(reasoning, REASONING_LIMIT_OLDER)
                    } else {
                        reasoning.clone()
                    }
                };
                entry.push_str(&format!("\n  Reasoning: {}", formatted_reasoning));
            }

            let projected_output = record.result.tool_result_projection.as_ref().map(|_| {
                record.result.model_authoritative_value()
                    .unwrap_or_else(|| serde_json::json!({"projection_unavailable":true,"raw_result_included":false}))
                    .to_string()
            });
            if let Some(output) = projected_output.as_ref().or(record.result.output.as_ref()) {
                let safe_output = summarize_action_output_for_prompt(&record.action, output);
                // JS/evaluate outputs often contain structured data (test cases, API responses)
                // that the LLM needs to make decisions - never truncate these.
                // ALSO: Do not truncate spawn_sub_goal outputs, as they contain critical artifact names.
                // Pack outputs (inner-loop wrappers like pack:browser) are summaries
                // already self-capped at the inner layer (terminal evidence ≤800,
                // summary ≤1000, dedup message ≤1794). Truncating them again here
                // to 200 chars destroyed the goal_reached evidence string and forced
                // the outer LLM to spin on `pack:browser` dispatches in
                // `task_b622ab28...` (the inner found "Akasa Air 05:45 ₹6,357 / IndiGo
                // 21:40 ₹7,450" but the outer never saw the answer).
                let is_data_heavy = matches!(
                    &record.action,
                    ExecutableAction::SpawnSubGoal { .. } | ExecutableAction::Pack { .. }
                );
                let formatted_output = if is_data_heavy {
                    safe_output // Full output for JS - LLM needs the data
                } else if safe_output.len() > 200 {
                    format!(
                        "{}... ({} chars)",
                        truncate_utf8(&safe_output, 200),
                        safe_output.len()
                    )
                } else {
                    safe_output
                };
                entry.push_str(&format!("\n  Output: {}", formatted_output));
            }
            if let Some(error) = &record.result.error {
                entry.push_str(&format!("\n  Error: {}", error));
            }

            // Include verification details so LLM can distinguish hard failures
            // from soft verification failures and near-misses
            if let Some(ref v) = record.verification {
                if v.confidence > 0.0 {
                    let match_str = match v.matches_expectation {
                        VerificationMatch::Match => "confirmed",
                        VerificationMatch::Mismatch => "mismatch",
                        VerificationMatch::NoChange => "no_change",
                        VerificationMatch::Unknown => "unknown",
                    };
                    entry.push_str(&format!(
                        "\n  Verification: {} (confidence: {:.2})",
                        match_str, v.confidence
                    ));
                    if let Some(ref assessment) = v.change_assessment {
                        let assessment_short = if assessment.len() > 150 {
                            format!("{}...", truncate_utf8(assessment, 150))
                        } else {
                            assessment.clone()
                        };
                        entry.push_str(&format!(" - {}", assessment_short));
                    }
                }
            }

            // Compute within-iteration Merkle diff (state changes caused by this action)
            // This helps LLM verify side effects of js/evaluate actions
            if let Some(state_diff) = Self::compute_action_effect(record) {
                entry.push_str(&format!("\n  Effect: {}", state_diff));
            }

            parts.push(entry);
        }

        Some(parts)
    }

    fn format_artifacts_for_llm(&self, per_kind_max: usize, total_max: usize) -> Option<String> {
        let combined = self.combined_artifacts_for_prompt_per_kind(per_kind_max, total_max);
        if combined.is_empty() {
            return None;
        }

        let mut lines = vec!["Available artifacts:".to_string()];

        for artifact in combined {
            let mut line = format!(
                "- {} ({}, {} bytes)",
                artifact.name,
                artifact.content_type,
                artifact.data.len()
            );

            if artifact.content_type == "application/json" {
                #[derive(Deserialize)]
                struct ArtifactPromptHeader {
                    #[serde(default)]
                    artifact_kind: Option<String>,
                    #[serde(default)]
                    file_name: Option<String>,
                    #[serde(default)]
                    purpose: Option<String>,
                    #[serde(default)]
                    absolute_path: Option<String>,
                    #[serde(default)]
                    export_path: Option<String>,
                    #[serde(default)]
                    relative_path: Option<String>,
                    #[serde(default)]
                    tool_name: Option<String>,
                    #[serde(default)]
                    task_absolute_path: Option<String>,
                    #[serde(default)]
                    task_relative_path: Option<String>,
                    #[serde(default)]
                    task_download_url: Option<String>,
                    #[serde(default)]
                    execution_download_url: Option<String>,
                }
                if let Some(header) =
                    artifact_header_from_json::<ArtifactPromptHeader>(&artifact.data)
                {
                    let kind = artifact
                        .artifact_type
                        .as_deref()
                        .or(header.artifact_kind.as_deref());
                    if kind == Some("downloaded_file") {
                        let file_name = header.file_name.as_deref().unwrap_or("unknown");
                        let purpose = header
                            .purpose
                            .as_deref()
                            .unwrap_or("downloaded during execution");
                        let path = header
                            .absolute_path
                            .as_deref()
                            .or(header.export_path.as_deref())
                            .or(header.relative_path.as_deref())
                            .unwrap_or("(path unavailable)");
                        line = format!(
                            "- {}: downloaded_file file={} purpose={} path={}",
                            artifact.name, file_name, purpose, path
                        );
                    } else if kind == Some("tool_output_file") {
                        let tool_name = header.tool_name.as_deref().unwrap_or("unknown");
                        let path = header
                            .task_absolute_path
                            .as_deref()
                            .or(header.task_relative_path.as_deref())
                            .unwrap_or("(path unavailable)");
                        let task_url = header
                            .task_download_url
                            .as_deref()
                            .unwrap_or("(url unavailable)");
                        let execution_url = header
                            .execution_download_url
                            .as_deref()
                            .unwrap_or("(url unavailable)");
                        line = format!(
                            "- {}: tool_output_file tool={} path={} task_url={} execution_url={}",
                            artifact.name, tool_name, path, task_url, execution_url
                        );
                    }
                }
            }

            lines.push(line);
        }

        Some(lines.join("\n"))
    }

    /// Select artifacts for prompt display using per-kind budgets.
    ///
    /// Applies `compute_artifact_budgets` over the union of
    /// `seeded_artifacts` + `artifacts` (deduped by `name`, newest wins),
    /// keeping the most-recent `budget[kind]` occurrences per kind and
    /// emitting the kept artifacts in their original relative order.
    fn combined_artifacts_for_prompt_per_kind(
        &self,
        per_kind_max: usize,
        total_max: usize,
    ) -> Vec<&Artifact> {
        use std::collections::{HashMap, HashSet};

        let mut ordered: Vec<&Artifact> =
            Vec::with_capacity(self.seeded_artifacts.len() + self.artifacts.len());
        ordered.extend(self.seeded_artifacts.iter());
        ordered.extend(self.artifacts.iter());

        // Dedup by name, keeping the LAST occurrence (newest wins),
        // preserving original relative order in the kept set.
        let mut last_index_by_name: HashMap<String, usize> = HashMap::new();
        for (i, artifact) in ordered.iter().enumerate() {
            last_index_by_name.insert(artifact.name.clone(), i);
        }
        let deduped: Vec<&Artifact> = ordered
            .iter()
            .enumerate()
            .filter(|(i, a)| last_index_by_name.get(&a.name).copied() == Some(*i))
            .map(|(_, a)| *a)
            .collect();

        // Group deduped artifact indices by kind.
        let mut per_kind_indices: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, a) in deduped.iter().enumerate() {
            per_kind_indices
                .entry(artifact_kind(a))
                .or_default()
                .push(i);
        }

        let kinds_with_counts: Vec<(String, usize)> = per_kind_indices
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect();

        let budgets = compute_artifact_budgets(kinds_with_counts, per_kind_max, total_max);

        // For each kind, keep only the LAST `budget[kind]` indices (most recent within the kind).
        let mut keep: HashSet<usize> = HashSet::new();
        for (kind, indices) in &per_kind_indices {
            let budget = budgets.get(kind).copied().unwrap_or(0);
            let skip = indices.len().saturating_sub(budget);
            for &i in indices.iter().skip(skip) {
                keep.insert(i);
            }
        }

        // Emit kept artifacts in the original deduped order.
        deduped
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep.contains(i))
            .map(|(_, a)| a)
            .collect()
    }

    /// Compute the page state change caused by a single action.
    /// Compares Merkle trees from state_before and state_after to identify
    /// what DOM elements were added, removed, or modified by the action.
    pub fn compute_action_effect(record: &IterationRecord) -> Option<String> {
        Self::compute_action_effect_from_states(&record.state_before, &record.state_after)
    }

    /// Compute the bounded browser effect directly from borrowed environment
    /// states. Production execution uses this before retaining an iteration so
    /// historical bookkeeping never has to clone complete DOM/CDP/accessibility
    /// trees merely to derive the prompt-visible effect.
    pub fn compute_action_effect_from_states(
        state_before: &EnvironmentState,
        state_after: &EnvironmentState,
    ) -> Option<String> {
        // Only compute for browser actions
        let before_tree = match state_before {
            EnvironmentState::Browser(page_state) => page_state.merkle_tree.as_ref(),
            _ => return None,
        };
        let after_tree = match state_after {
            EnvironmentState::Browser(page_state) => page_state.merkle_tree.as_ref(),
            _ => return None,
        };

        let (before, after) = match (before_tree, after_tree) {
            (Some(b), Some(a)) => (b, a),
            _ => return None,
        };

        let diff = before.diff(after);

        // If no changes, don't include in output
        if diff.added.is_empty() && diff.removed.is_empty() && diff.modified.is_empty() {
            return None;
        }

        // Generate concise effect description
        let mut effects = vec![];

        for subtree in &diff.changed_subtrees {
            let change_type = match subtree.change_type {
                crate::magician_v2::execution::merkle::SubtreeChangeType::Added => "ADDED",
                crate::magician_v2::execution::merkle::SubtreeChangeType::Removed => "REMOVED",
                crate::magician_v2::execution::merkle::SubtreeChangeType::Modified => "MODIFIED",
            };

            let element_desc = if subtree.affected_elements.is_empty() {
                subtree.path.clone()
            } else {
                subtree.affected_elements[0].clone()
            };

            effects.push(format!("{}: {}", change_type, element_desc));
        }

        // Limit to 3 effects to keep output concise
        if effects.len() > 3 {
            let count = effects.len();
            effects.truncate(3);
            effects.push(format!("...and {} more changes", count - 3));
        }

        if effects.is_empty() {
            None
        } else {
            Some(effects.join("; "))
        }
    }

    /// Count consecutive failures from the most recent iteration backwards.
    /// Returns the number of consecutive failed iterations at the end of history.
    fn count_consecutive_failures(&self) -> usize {
        self.iterations
            .iter()
            .rev()
            .take_while(|record| !record.result.success)
            .count()
    }

    /// Get action signatures for the N most recent failed actions.
    fn get_recent_failed_actions(&self, count: usize) -> Vec<String> {
        self.iterations
            .iter()
            .rev()
            .filter(|record| !record.result.success)
            .take(count)
            .map(|record| action_signature(&record.action))
            .collect()
    }
}

/// Extract (action_type, tool_name) from an ExecutableAction.
/// action_type is the category ("file", "http", "bash", "pack", "orchestrator")
/// tool_name is the specific action/target description.
pub fn action_type_and_tool_name(action: &ExecutableAction) -> (String, String) {
    match action {
        ExecutableAction::File(f) => ("file".to_string(), f.description()),
        ExecutableAction::Http(h) => ("http".to_string(), h.description()),
        ExecutableAction::Bash(b) => ("bash".to_string(), b.description()),
        ExecutableAction::DuckDb(d) => ("duckdb".to_string(), d.description()),
        ExecutableAction::Pack {
            capability_name, ..
        } => ("pack".to_string(), capability_name.clone()),
        ExecutableAction::SpawnSubGoal { .. } => {
            ("orchestrator".to_string(), "spawn_sub_goal".to_string())
        },
        ExecutableAction::DelegateToAgent { .. } => {
            ("orchestrator".to_string(), "delegate_to_agent".to_string())
        },
        ExecutableAction::HandoverToAgent { .. } => {
            ("orchestrator".to_string(), "handover_to_agent".to_string())
        },
        ExecutableAction::SleepUntil { .. } => ("scheduler".to_string(), "sleep_until".to_string()),
    }
}

/// Classify an action as read-only for the Developer-Mode plan-mode
/// gate. Read-only actions bypass the per-call approval prompt so
/// the user can still ask "what does this file contain?" or "is that
/// resource cached?" without ack-fatigue. Anything that mutates the
/// filesystem, an external service, or a long-lived process trips
/// the gate.
///
/// Conservative-by-default: unknown action shapes are treated as
/// non-read-only so plan-mode never silently lets through a write.
pub fn is_read_only_action(action: &ExecutableAction) -> bool {
    match action {
        ExecutableAction::File(file) => matches!(
            file,
            FileAction::Read { .. } | FileAction::List { .. } | FileAction::Exists { .. }
        ),
        ExecutableAction::Http(http) => {
            // GET / HEAD / OPTIONS are read-only; everything else may
            // mutate the remote state. `HttpMethod` is an enum; match
            // the safe verbs explicitly.
            matches!(
                http.method,
                HttpMethod::Get | HttpMethod::Head | HttpMethod::Options
            )
        },
        ExecutableAction::DuckDb(_) => {
            // DuckDB queries via this lane are SELECT-flavored
            // analytics in practice; safe to treat as read-only.
            true
        },
        ExecutableAction::Pack {
            capability_name, ..
        } => is_parallelizable_read_only_pack(capability_name),
        // Everything else (Browser, Bash, Pack writes, BrowserPack
        // navigations) counts as non-read-only.
        _ => false,
    }
}

/// Packs whose I/O may overlap in one Magician turn. Writes, shell, browser,
/// mail, and HTTP mutating verbs stay serial.
pub fn is_parallelizable_read_only_pack(capability_name: &str) -> bool {
    matches!(
        capability_name,
        "query_known_resource"
            | "list_agents"
            | "list_memory_tiers"
            | "list_artifacts"
            | "list_scheduled_tasks"
            | "list_tasks"
            | "get_active_executions"
            | "get_execution_history"
            | "search_memory"
            | "get_layout"
            | "grep"
            | "glob"
            | "read_file"
            | "web_search"
            | "web_fetch"
            | "content_search"
            | "content_read"
            | "inspect_agent"
            | "get_agent_details"
            | "find_agents_for_capability"
            | "list_episodes"
            | "list_proposals"
            | "system_status"
            | "time_math"
            | "read_program_state"
            | "read_trace"
    )
}

pub fn is_parallelizable_read_only_action(action: &ExecutableAction) -> bool {
    // Narrower than [`is_read_only_action`]: HTTP GET and DuckDB are
    // read-only for plan-mode HITL but stay serial here. Overlapping
    // HTTP/DuckDB would share connection/session state the exclusive
    // scheduler-root wrapper used to protect.
    match action {
        ExecutableAction::File(file) => matches!(
            file,
            FileAction::Read { .. } | FileAction::List { .. } | FileAction::Exists { .. }
        ),
        ExecutableAction::Pack {
            capability_name, ..
        } => is_parallelizable_read_only_pack(capability_name),
        _ => false,
    }
}

/// Create a string signature for an action (for comparison/display).
pub fn action_signature(action: &ExecutableAction) -> String {
    match action {
        ExecutableAction::File(f) => format!("file:{}", f.description()),
        ExecutableAction::Http(h) => {
            format!("http:{}", h.description())
        },
        ExecutableAction::Bash(b) => {
            format!("bash:{}", b.description())
        },
        ExecutableAction::DuckDb(d) => {
            format!("duckdb:{}", d.description())
        },
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => {
            // Include a deterministic param fingerprint so calls with different
            // params produce different signatures (avoids false loop positives).
            let mut keys: Vec<&String> = resolved_params
                .keys()
                .filter(|key| !is_primitive_metadata_key(key))
                .collect();
            keys.sort();
            let param_summary: String = keys
                .iter()
                .map(|k| format!("{}={}", k, resolved_params[*k]))
                .collect::<Vec<_>>()
                .join(",");
            format!("pack:{}({})", capability_name, param_summary)
        },
        ExecutableAction::SpawnSubGoal { goal, .. } => {
            let truncated: String = goal.chars().take(100).collect();
            if truncated.len() < goal.len() {
                format!("orchestrator:spawn_sub_goal:{}...", truncated)
            } else {
                format!("orchestrator:spawn_sub_goal:{}", goal)
            }
        },
        ExecutableAction::DelegateToAgent { targets } => format!(
            "delegate:{}",
            targets
                .iter()
                .map(|target| format!("{}:{}", target.target_agent_id, target.context))
                .collect::<Vec<_>>()
                .join("|")
        ),
        ExecutableAction::HandoverToAgent {
            target_agent_id,
            context,
        } => {
            format!("handover:{}:{}", target_agent_id, context)
        },
        ExecutableAction::SleepUntil { wake_at, .. } => {
            format!("scheduler:sleep_until:{}", wake_at.to_rfc3339())
        },
    }
}

fn is_primitive_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "__loop_kind"
            | "__parent_outer_iteration"
            | "__inner_iteration"
            | "__inner_capability"
            | "__inner_objective_id"
            | "__inner_run_index"
            | "__inner_run_id"
    )
}

// ============================================================================
// Outcomes
// ============================================================================

/// What a `completed` terminal delivered. Orthogonal to lifecycle status:
/// a partial *completed*. Ordered so that the weakest kind is the minimum —
/// an aggregate over several parts takes `min()` across them.
///
/// `Partial` means at least one requested item was delivered and supported,
/// and at least one is declared open with a reason. Nothing delivered is not
/// a kind; it is a failed terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Partial,
    Full,
}

/// Result of an agentic execution run.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgenticOutcome {
    /// Goal was achieved — fully, or partially with the open items declared.
    ///
    /// The kind is carried HERE, on the value every layer above consumes,
    /// because it was previously carried only in a summary string and a
    /// markdown artifact: the runtime received a bare `Success`, wrote
    /// `success` over a `goal_achieved_partial` observation, the delegation
    /// root wrote a constant, and the task said `completed`. Every consumer
    /// reported honestly what it was handed. Exhaustive construction is the
    /// enforcement: each site says which it is.
    Success {
        /// Full or partial delivery.
        completion: CompletionKind,
        /// The yield's `open[]`, verbatim — what was declared undone and why.
        open: Vec<String>,
        /// Final environment state
        final_state: EnvironmentState,
        /// Number of iterations used
        iterations_used: usize,
        /// Artifacts produced (files, screenshots, etc.)
        artifacts: Vec<Artifact>,
    },

    /// Execution failed (unrecoverable error)
    Failed {
        /// Reason for failure
        reason: String,
        /// Last observed state
        last_state: EnvironmentState,
        /// Number of iterations used
        iterations_used: usize,
    },

    /// Hit iteration limit without achieving goal
    ///
    /// When `pause_state` is `Some`, this is a pausable outcome — the execution
    /// can be resumed with a fresh iteration budget. When `None`, it's a terminal failure.
    MaxIterationsReached {
        /// Last observed state
        last_state: EnvironmentState,
        /// Number of iterations used (equals max)
        iterations_used: usize,
        /// Preserved state for resuming execution with fresh iterations (if available)
        #[serde(skip_serializing_if = "Option::is_none")]
        pause_state: Option<AgenticPauseState>,
    },

    /// Detected repetitive behavior (likely stuck)
    ///
    /// This can be triggered by:
    /// - **state_loop**: Same environment state seen multiple times
    /// - **action_cycle**: Repeating action patterns (A→B→A→B)
    /// - **no_progress**: State unchanged despite multiple actions
    LoopDetected {
        /// Type of loop detected: "state_loop", "action_cycle", "no_progress"
        detection_type: String,
        /// The repeated action signature (if applicable)
        repeated_action: String,
        /// Human-readable recommendation for breaking the loop
        recommendation: String,
        /// Last observed state
        last_state: EnvironmentState,
        /// Number of iterations used
        iterations_used: usize,
        /// For action cycles: the pattern of actions in the cycle
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle_pattern: Option<Vec<String>>,
        /// Similarity score (for state_loop and no_progress detection)
        #[serde(skip_serializing_if = "Option::is_none")]
        similarity: Option<f64>,
    },

    /// Execution paused waiting for user input
    ///
    /// This is NOT a failure - the workflow will resume when the user provides
    /// their response. The pause_state contains all context needed to continue.
    WaitingForUser {
        /// The question/prompt to display to the user
        question: String,
        /// Type of input expected (determines UI rendering)
        input_type: UserInputType,
        /// Optional hint to help the user
        #[serde(skip_serializing_if = "Option::is_none")]
        hint: Option<String>,
        /// Preserved state for resuming execution
        pause_state: Box<AgenticPauseState>,
        /// The parameter ID being asked (for associating response with pending input)
        #[serde(skip_serializing_if = "Option::is_none")]
        asking_for_parameter: Option<String>,
        /// Pending inputs that still need to be resolved (for handoff to FullPauseData)
        pending_inputs: Vec<PendingInput>,
        /// Already resolved input values (for handoff to FullPauseData)
        resolved_inputs: HashMap<String, Value>,
        /// Set when this pause was triggered by failure escalation (on_failure: ask_user).
        /// Values: "cannot_proceed", "loop_detected", or None for normal user input pauses.
        #[serde(skip_serializing_if = "Option::is_none")]
        escalation_trigger: Option<String>,
    },

    /// Execution paused waiting for user confirmation of a potentially destructive action
    ///
    /// This is NOT a failure - the workflow will resume when the user confirms or rejects.
    /// Used for actions like: delete operations, navigating to new domains, POST/DELETE requests.
    WaitingForConfirmation {
        /// Human-readable summary of the action requiring confirmation
        action_summary: String,
        /// Why this action requires confirmation
        reason: String,
        /// The action type emitted on confirmation event surfaces.
        action_type: String,
        /// The action that will be executed if confirmed (serialized)
        action_json: String,
        /// Optional secret-store approval challenge to grant on positive resume.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_approval_challenge_id: Option<String>,
        /// Optional secret approval context used to reconstruct approval after restart.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret_approval_request: Option<SecretApprovalRequest>,
        /// Preserved state for resuming execution
        pause_state: AgenticPauseState,
        /// Number of iterations used so far
        iterations_used: usize,
    },

    /// Execution paused by an explicit user pause request.
    ///
    /// Unlike WaitingForUser/WaitingForConfirmation, this does not require new
    /// user input. The preserved pause state is used for exact continuation.
    PausedByUser {
        /// Preserved state for exact continuation
        pause_state: AgenticPauseState,
        /// Number of iterations completed before the pause boundary
        iterations_used: usize,
    },

    /// Execution yielded to delegated child executions and will resume when they finish.
    WaitingForChildren {
        /// Child execution IDs in the active delegation group.
        child_execution_ids: Vec<String>,
        /// Legacy state field retained as a rolling-upgrade wire contract.
        ///
        /// New checkpoints store `Uninitialized` here and keep the real state
        /// only in `pause_state`, so a large browser tree is not retained twice.
        /// Pre-checkpoint wire consumers still find their original state field.
        last_state: EnvironmentState,
        /// Number of iterations used before entering WaitingChildren.
        iterations_used: usize,
        /// Exact continuation checkpoint captured after the delegation action
        /// was recorded. `None` is the pre-checkpoint compatibility shape.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pause_state: Option<Box<AgenticPauseState>>,
    },

    /// Budget limit exceeded (time, cost, iterations, or per-action-type limits)
    ///
    /// Historically a terminal failure. When `pause_state` is `Some`, this
    /// mirrors [`AgenticOutcome::MaxIterationsReached`]: the execution can be
    /// resumed with a fresh budget via a "Continue execution?" HITL instead of
    /// discarding in-flight progress. When `None`, it remains a terminal
    /// failure (e.g. the fail-closed missing-usage path, where no resumable
    /// state was built).
    BudgetExhausted {
        /// Which budget dimension was exceeded
        dimension: BudgetDimension,
        /// Last observed state
        last_state: EnvironmentState,
        /// Number of iterations completed before exhaustion
        iterations_completed: usize,
        /// Total actions completed across all types
        actions_completed: usize,
        /// Preserved state for resuming execution with a fresh budget (if available).
        /// `Some` → resumable pause; `None` → terminal failure.
        #[serde(skip_serializing_if = "Option::is_none")]
        pause_state: Option<AgenticPauseState>,
    },

    /// Agent determined it cannot proceed (stuck, blocked, or missing requirements)
    ///
    /// Different from Failed - this is when the agent explicitly decides it cannot
    /// make progress, not when an action errors out.
    CannotProceed {
        /// Reason why the agent cannot proceed
        reason: String,
        /// Last observed state
        last_state: EnvironmentState,
        /// Number of iterations used
        iterations_used: usize,
    },

    /// Agent decided to sleep until `wake_at`. No real action was executed.
    /// The pipeline persists state and schedules a WakeUpQueue timer.
    Sleeping {
        wake_at: DateTime<Utc>,
        /// Optional env state snapshot to resume from after wake.
        paused_state: Option<Box<EnvironmentState>>,
        /// Exact stateless segment whose placement pin caused this sleep.
        /// `None` identifies an intentional pre-loop SleepUntil hint, which
        /// has no committed loop key to reclaim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stateless_source_segment: Option<String>,
        /// Bounded exact-continuation snapshot for a placement-pin retry.
        /// Absent for an intentional pre-loop SleepUntil hint. The lifecycle
        /// owner persists this in the trusted pause store before publishing
        /// the wake and refuses a wake whose scope/segment/generation does not
        /// match it.
        #[serde(skip)]
        placement_continuation: Option<Box<AgenticPauseState>>,
    },
}

/// Exact secret-access request that can be re-approved after pause persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretApprovalRequest {
    pub credential_id: String,
    pub tool: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Which budget dimension was exceeded
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BudgetDimension {
    /// Time limit exceeded
    Time {
        used_seconds: u64,
        limit_seconds: u64,
    },
    /// Cost limit exceeded
    Cost {
        used_dollars: f64,
        limit_dollars: f64,
    },
    /// Total iteration limit exceeded
    Iterations { used: usize, limit: usize },
    /// LLM call limit exceeded
    LlmCalls { used: usize, limit: usize },
    /// Provider-reported LLM token limit exceeded
    Tokens { used: u64, limit: u64 },
    /// Per-action-type limit exceeded
    Actions {
        action_type: String,
        used: usize,
        limit: usize,
    },
}

impl AgenticOutcome {
    /// Check if this outcome represents success
    pub fn is_success(&self) -> bool {
        matches!(self, AgenticOutcome::Success { .. })
    }

    /// Check if this outcome is waiting for user input or confirmation
    pub fn is_waiting_for_user(&self) -> bool {
        matches!(
            self,
            AgenticOutcome::WaitingForUser { .. } | AgenticOutcome::WaitingForConfirmation { .. }
        )
    }

    /// Check if this outcome is waiting for confirmation specifically
    pub fn is_waiting_for_confirmation(&self) -> bool {
        matches!(self, AgenticOutcome::WaitingForConfirmation { .. })
    }

    /// Whether this outcome requires an exact continuation rather than terminal
    /// settlement.
    ///
    /// Keep this distinct from `!is_terminal()`: sleeping is a continuation
    /// boundary even though legacy terminal classification still treats it as
    /// terminal, and budget/max-iteration outcomes are resumable only when they
    /// actually carry a checkpoint. Callers that cannot reconstruct their own
    /// outer control frame (for example the fixed-roster single-context pipeline)
    /// must fail closed instead of turning one of these outcomes into success.
    pub fn requires_continuation(&self) -> bool {
        matches!(
            self,
            AgenticOutcome::WaitingForUser { .. }
                | AgenticOutcome::WaitingForConfirmation { .. }
                | AgenticOutcome::PausedByUser { .. }
                | AgenticOutcome::WaitingForChildren { .. }
                | AgenticOutcome::Sleeping { .. }
                | AgenticOutcome::MaxIterationsReached {
                    pause_state: Some(_),
                    ..
                }
                | AgenticOutcome::BudgetExhausted {
                    pause_state: Some(_),
                    ..
                }
        )
    }

    /// Check if this is a terminal outcome (not waiting for user)
    pub fn is_terminal(&self) -> bool {
        match self {
            AgenticOutcome::MaxIterationsReached {
                pause_state: Some(_),
                ..
            }
            // A budget stop that built a resumable `pause_state` is a PAUSE
            // ("Continue execution?"), not a terminal failure — mirror
            // MaxIterationsReached so downstream circuit-breaker / episode
            // transitions treat it as non-terminal (it must NOT count toward
            // `max_consecutive_failures`). Only the fail-closed `pause_state:
            // None` variant stays terminal (falls through to the catch-all).
            | AgenticOutcome::BudgetExhausted {
                pause_state: Some(_),
                ..
            }
            | AgenticOutcome::PausedByUser { .. }
            | AgenticOutcome::WaitingForChildren { .. } => false,
            _ => !self.is_waiting_for_user(),
        }
    }

    /// Check if this outcome is a budget exhaustion
    pub fn is_budget_exhausted(&self) -> bool {
        matches!(self, AgenticOutcome::BudgetExhausted { .. })
    }

    /// Check if this outcome is cannot proceed
    pub fn is_cannot_proceed(&self) -> bool {
        matches!(self, AgenticOutcome::CannotProceed { .. })
    }

    /// Get the final/last state
    pub fn state(&self) -> &EnvironmentState {
        match self {
            AgenticOutcome::Success { final_state, .. } => final_state,
            AgenticOutcome::Failed { last_state, .. } => last_state,
            AgenticOutcome::MaxIterationsReached { last_state, .. } => last_state,
            AgenticOutcome::LoopDetected { last_state, .. } => last_state,
            AgenticOutcome::WaitingForUser { pause_state, .. } => &pause_state.environment_state,
            AgenticOutcome::WaitingForConfirmation { pause_state, .. } => {
                &pause_state.environment_state
            },
            AgenticOutcome::PausedByUser { pause_state, .. } => &pause_state.environment_state,
            AgenticOutcome::WaitingForChildren {
                last_state,
                pause_state,
                ..
            } => pause_state
                .as_deref()
                .map(|pause| &pause.environment_state)
                .unwrap_or(last_state),
            AgenticOutcome::BudgetExhausted { last_state, .. } => last_state,
            AgenticOutcome::CannotProceed { last_state, .. } => last_state,
            AgenticOutcome::Sleeping { paused_state, .. } => {
                paused_state
                    .as_deref()
                    .unwrap_or_else(|| panic!(
                        "AgenticOutcome::Sleeping::state() called with no paused_state;                          check the outcome kind before calling state()"
                    ))
            }
        }
    }

    /// Get iterations used
    pub fn iterations_used(&self) -> usize {
        match self {
            AgenticOutcome::Success {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::Failed {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::MaxIterationsReached {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::LoopDetected {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::WaitingForUser { pause_state, .. } => pause_state.iteration,
            AgenticOutcome::WaitingForConfirmation {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::PausedByUser {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::WaitingForChildren {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::BudgetExhausted {
                iterations_completed,
                ..
            } => *iterations_completed,
            AgenticOutcome::CannotProceed {
                iterations_used, ..
            } => *iterations_used,
            AgenticOutcome::Sleeping { .. } => 0,
        }
    }

    /// Get the pause state if waiting for user input or confirmation
    pub fn pause_state(&self) -> Option<&AgenticPauseState> {
        match self {
            AgenticOutcome::WaitingForUser { pause_state, .. } => Some(pause_state),
            AgenticOutcome::WaitingForConfirmation { pause_state, .. } => Some(pause_state),
            AgenticOutcome::MaxIterationsReached { pause_state, .. } => pause_state.as_ref(),
            AgenticOutcome::PausedByUser { pause_state, .. } => Some(pause_state),
            AgenticOutcome::WaitingForChildren { pause_state, .. } => pause_state.as_deref(),
            _ => None,
        }
    }

    /// Get the budget dimension if this is a budget exhaustion
    pub fn budget_dimension(&self) -> Option<&BudgetDimension> {
        match self {
            AgenticOutcome::BudgetExhausted { dimension, .. } => Some(dimension),
            _ => None,
        }
    }

    /// Get the outcome type as a string for event emission
    pub fn outcome_type(&self) -> &'static str {
        match self {
            AgenticOutcome::Success { .. } => "success",
            AgenticOutcome::Failed { .. } => "failed",
            AgenticOutcome::MaxIterationsReached { .. } => "max_iterations_reached",
            AgenticOutcome::LoopDetected { .. } => "loop_detected",
            AgenticOutcome::WaitingForUser { .. } => "waiting_for_user",
            AgenticOutcome::WaitingForConfirmation { .. } => "waiting_for_confirmation",
            AgenticOutcome::PausedByUser { .. } => "paused_by_user",
            AgenticOutcome::WaitingForChildren { .. } => "waiting_for_children",
            AgenticOutcome::BudgetExhausted { .. } => "budget_exhausted",
            AgenticOutcome::CannotProceed { .. } => "cannot_proceed",
            AgenticOutcome::Sleeping { .. } => "sleeping",
        }
    }
}

/// An artifact produced during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Name/identifier for this artifact
    pub name: String,

    /// MIME type or content type
    pub content_type: String,

    /// Artifact payload bytes.
    ///
    /// When created from LLM decision parsing, contains raw UTF-8 bytes of the
    /// data string (not base64). For JSON artifacts, these bytes are valid JSON
    /// parseable via `serde_json::from_slice`.
    ///
    /// The `base64_serde` attribute handles serialization when this struct is
    /// persisted to disk or sent over API — it base64-encodes on write and
    /// decodes on read, preserving the raw bytes through the roundtrip.
    #[serde(with = "base64_serde")]
    pub data: Vec<u8>,

    /// Logical artifact type (e.g., "data_bundle", "metric_set").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,

    /// Optional render hints for auto-surface publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_hints: Option<crate::magician_v2::artifacts::types::RenderHints>,

    /// Materialized file path when the artifact has been persisted to disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_path: Option<String>,
}

impl Artifact {
    /// Create a text artifact
    pub fn text(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            content_type: "text/plain".to_string(),
            data: content.into().into_bytes(),
            artifact_type: None,
            render_hints: None,
            materialized_path: None,
        }
    }

    /// The run's answer as its deliverable. Typed `task_deliverable`, this is
    /// what a task's terminal materialises and publishes byte-preserving as
    /// the verified terminal projection; an untyped text artifact carrying the
    /// same words is not one, and the task-user output is re-synthesised and
    /// judged for grounding instead.
    pub fn task_deliverable(body: impl Into<String>) -> Self {
        let mut artifact = Self::text("task_deliverable.md", body);
        artifact.content_type = "text/markdown".to_string();
        artifact.artifact_type = Some("task_deliverable".to_string());
        artifact
    }

    /// Create a JSON artifact
    pub fn json(
        name: impl Into<String>,
        value: &impl Serialize,
    ) -> Result<Self, serde_json::Error> {
        let data = serde_json::to_vec(value)?;
        Ok(Self {
            name: name.into(),
            content_type: "application/json".to_string(),
            // Preserve the historical wire contract: JSON constructors do not
            // promote a payload field into the explicit artifact_type field.
            // Metadata-only consumers use the bounded typed-header reader.
            artifact_type: None,
            data,
            render_hints: None,
            materialized_path: None,
        })
    }
}

/// Helper module for base64 serialization of binary data.
mod base64_serde {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        // Tolerant decode. The on-wire contract is base64 (serialize emits it),
        // but models routinely put RAW content (markdown, CSV, plain text) in an
        // artifact `data` field. Strict base64 decoding there fails the ENTIRE
        // struct — and when that struct is a `yield` / `goal_reached` payload, it
        // rejects the terminal decision, leaving the loop unable to stop (and, if
        // the response chain is then re-fed, cascading into "no tool output for
        // function call" failures). Prefer base64; fall back to the raw UTF-8
        // bytes rather than discarding a whole terminal over one mis-encoded
        // field. (See docs/plans/2026-05-31-inner-loop-to-flat-verification-port.md.)
        Ok(STANDARD.decode(s.trim()).unwrap_or_else(|_| s.into_bytes()))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::actions::{BashAction, FileAction, HttpAction, HttpMethod};
    use serde_json::json;

    fn pause_for_step() -> AgenticPauseState {
        let mut pause = AgenticPauseState::new(
            2,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        );
        pause.execution_id = Some("exec-1".to_string());
        pause.plan_id = Some("direct-abc".to_string());
        pause.step_id = Some("direct-step-1".to_string());
        pause
    }

    /// Two questions of one step share a storage key — the key names the
    /// step's pause slot — but each is its own ask: the canonical correlation
    /// id differs, splits back to the shared key plus the ask, and the ask a
    /// client answers can be checked against the pause it reaches. A record
    /// from before asks had identities keeps the bare key and names no ask.
    #[test]
    fn every_ask_of_a_step_has_its_own_correlation_id_on_the_shared_key() {
        let first = pause_for_step();
        let second = pause_for_step();
        assert_eq!(first.storage_key(), second.storage_key());
        assert_ne!(first.hitl_correlation_id(), second.hitl_correlation_id());

        let correlation_id = first.hitl_correlation_id();
        let (key, ask) = split_hitl_correlation_id(&correlation_id);
        assert_eq!(key, first.storage_key());
        assert_eq!(ask, first.ask_id.as_deref());
        assert!(ask.is_some_and(|ask| ask.len() == ASK_ID_LEN));
        assert_ne!(ask, second.ask_id.as_deref());

        // The bare key is what a legacy client or record carries.
        assert_eq!(
            split_hitl_correlation_id(&first.storage_key()),
            (first.storage_key().as_str(), None)
        );
        let mut legacy: AgenticPauseState =
            serde_json::from_value(serde_json::to_value(&first).unwrap()).unwrap();
        legacy.ask_id = None;
        assert_eq!(legacy.hitl_correlation_id(), legacy.storage_key());
        // A stray separator inside an id that is not followed by an ask
        // stays part of the key.
        assert_eq!(
            split_hitl_correlation_id("exec~odd:plan:step"),
            ("exec~odd:plan:step", None)
        );
        assert_eq!(
            split_hitl_correlation_id("exec:plan:step~notanaskid"),
            ("exec:plan:step~notanaskid", None)
        );
    }

    fn pack_action(name: &str) -> ExecutableAction {
        ExecutableAction::Pack {
            capability_name: name.to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "test".to_string(),
                },
            resolved_params: HashMap::new(),
        }
    }

    #[test]
    fn parallel_join_covers_file_reads_and_named_packs_not_http() {
        let read = ExecutableAction::File(FileAction::Read {
            path: std::path::PathBuf::from("/tmp/a"),
            encoding: None,
        });
        let write = ExecutableAction::File(FileAction::Write {
            path: std::path::PathBuf::from("/tmp/a"),
            content: "x".to_string(),
            create_dirs: true,
        });
        let get = ExecutableAction::Http(HttpAction {
            method: HttpMethod::Get,
            url: "https://example.test".to_string(),
            headers: HashMap::new(),
            body: None,
            content_type: None,
            timeout_secs: None,
            follow_redirects: true,
            carries_credential: false,
        });
        assert!(is_parallelizable_read_only_action(&read));
        assert!(is_parallelizable_read_only_action(&pack_action("grep")));
        assert!(is_read_only_action(&get), "GET stays plan-mode read-only");
        assert!(
            !is_parallelizable_read_only_action(&get),
            "HTTP must not enter the in-turn join_all prefix"
        );
        assert!(!is_parallelizable_read_only_action(&write));
        assert!(!is_parallelizable_read_only_action(&pack_action("shell")));
    }

    #[test]
    fn waiting_for_children_uses_single_canonical_environment_snapshot() {
        let mut page = PageState {
            url: Some("https://example.test/research".to_string()),
            dom_snapshot: Some("x".repeat(32_768)),
            ..Default::default()
        };
        page.visible_text = Some("answer evidence".to_string());
        let pause_state = AgenticPauseState::new(
            3,
            "research",
            "answer with evidence",
            EnvironmentState::Browser(page),
            "",
            10,
            3,
        );
        let outcome = AgenticOutcome::WaitingForChildren {
            child_execution_ids: vec!["child-1".to_string()],
            last_state: EnvironmentState::Uninitialized,
            iterations_used: 3,
            pause_state: Some(Box::new(pause_state)),
        };

        assert_eq!(outcome.state().type_name(), "browser");
        let encoded = serde_json::to_value(&outcome).expect("serialize waiting outcome");
        assert_eq!(
            encoded["last_state"]["type"], "uninitialized",
            "new waiting outcomes must serialize only the tiny legacy sentinel"
        );
    }

    /// A pack result is replayed to the provider as a native tool result; the
    /// state block used to paste a 1,500-byte cut of the same text beneath it
    /// with "(truncated)" — duplicate tokens, and a marker that read as "rows
    /// were cut" while the full table sat one message up (an Android Settings
    /// snapshot showed 20 of 24 rows that way). With the result in the window
    /// the block points at it; without one it still carries the bounded body.
    #[test]
    fn a_replayed_pack_result_is_referenced_not_repeated_in_the_state_block() {
        let table = (0..80)
            .map(|row| format!("{row} | android:id/title | Row {row} |  |  | [0,{row},1,2]"))
            .collect::<Vec<_>>()
            .join("\n");
        let state = EnvironmentState::Shell(ShellState {
            working_dir: PathBuf::from("/tmp"),
            last_command: Some("pack:android_snapshot".to_string()),
            last_stdout: Some(table.clone()),
            last_stderr: Some(String::new()),
            last_exit_code: Some(0),
        });

        let replayed = state.format_for_llm_with_replayed_result(true);
        assert!(replayed.contains("Last Command: pack:android_snapshot"));
        assert!(
            replayed.contains(&format!("tool result above ({} bytes", table.len())),
            "{replayed}"
        );
        assert!(
            !replayed.contains("Row 5 |"),
            "the body must not be repeated: {replayed}"
        );
        assert!(!replayed.contains("(truncated)"), "{replayed}");

        let inline = state.format_for_llm_with_replayed_result(false);
        assert!(inline.contains("Row 5 |"));
        assert!(
            inline.contains("(truncated)"),
            "the bounded body keeps its marker: {inline}"
        );
        assert_eq!(inline, state.format_for_llm());
    }

    #[test]
    fn format_for_llm_includes_outer_assistant_turns_without_iterations() {
        let mut history = ExecutionHistory::new();
        history.record_assistant_turn(AgenticAssistantTurnRecord {
            iteration: 2,
            operation: "agentic_decision".to_string(),
            llm_trace_context: None,
            text: Some("I will inspect the available controls before changing values.".into()),
            tool_calls: vec![AgenticAssistantToolCallRecord {
                id: "call_1".to_string(),
                name: "browser".to_string(),
                arguments: json!({"intent": "inspect controls"}),
            }],
            finish_reason: Some("tool_calls".to_string()),
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            reasoning: None,
            timestamp: Utc::now(),
        });

        let rendered = history.format_for_llm(5);
        assert!(rendered.contains("Recent outer assistant turns"));
        assert!(rendered.contains("I will inspect the available controls"));
        assert!(rendered.contains("browser({\"intent\":\"inspect controls\"})"));
        assert!(rendered.contains("tokens=10+5"));
    }

    #[test]
    fn assistant_turn_rendering_truncates_large_arguments() {
        let mut history = ExecutionHistory::new();
        history.record_assistant_turn(AgenticAssistantTurnRecord {
            iteration: 1,
            operation: "agentic_decision".to_string(),
            llm_trace_context: None,
            text: None,
            tool_calls: vec![AgenticAssistantToolCallRecord {
                id: "call_1".to_string(),
                name: "shell".to_string(),
                arguments: json!({"command": "x".repeat(2000)}),
            }],
            finish_reason: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning: None,
            timestamp: Utc::now(),
        });

        let rendered = history.format_recent_assistant_turns_for_llm(1).unwrap();
        assert!(rendered.contains("<...redacted for brevity>"));
        assert!(rendered.len() < 1400);
    }

    #[test]
    fn format_recent_history_labels_inner_iterations_with_parent_context() {
        let mut history = ExecutionHistory::new();
        let resolved_params = [
            ("selector".to_string(), json!("#target")),
            ("__loop_kind".to_string(), json!("inner")),
            ("__parent_outer_iteration".to_string(), json!(2)),
            ("__inner_iteration".to_string(), json!(14)),
            ("__inner_run_index".to_string(), json!(1)),
            ("__inner_run_id".to_string(), json!("browser_run_0001")),
            ("__inner_capability".to_string(), json!("browser")),
            ("__inner_objective_id".to_string(), json!("objective-123")),
        ]
        .into_iter()
        .collect();
        history.iterations.push(IterationRecord {
            iteration: 14,
            state_before: EnvironmentState::Uninitialized,
            action: ExecutableAction::Pack {
                capability_name: "browser.click".to_string(),
                implementation:
                    crate::magician_v2::execution::capability::ImplementationType::Compiled {
                        provider_name: "browser".to_string(),
                    },
                resolved_params,
            },
            result: ActionResultRecord::new(
                true,
                Some("clicked".to_string()),
                None,
                12,
                Some(ActionOutcomeCategory::PartialProgress),
            ),
            state_after: EnvironmentState::Uninitialized,
            timestamp: Utc::now(),
            verification: None,
            llm_reasoning: None,
        });

        let rendered = history.format_recent_history_for_llm(5);

        assert!(rendered.contains(
            "Inner iteration 14 (browser, browser_run_0001, parent_outer_iteration=2, objective_id=objective-123)"
        ));
        assert!(rendered.contains("pack:browser.click(selector=\"#target\")"));
        assert!(!rendered.contains("__inner_iteration"));
    }

    #[test]
    fn format_for_llm_hard_caps_outer_history_context() {
        let mut history = ExecutionHistory::new();
        history.iterations.push(IterationRecord {
            iteration: 1,
            state_before: EnvironmentState::Browser(PageState::default()),
            action: ExecutableAction::Pack {
                capability_name: "browser".to_string(),
                implementation:
                    crate::magician_v2::execution::capability::ImplementationType::Compiled {
                        provider_name: "browser".to_string(),
                    },
                resolved_params: HashMap::from([(
                    "command".to_string(),
                    json!("evaluate return window.largePayload"),
                )]),
            },
            result: ActionResultRecord {
                success: true,
                output: Some(format!(
                    "Browser result: {}",
                    "x".repeat(OUTER_HISTORY_CONTEXT_LIMIT + 20_000)
                )),
                error: None,
                duration_ms: 10,
                outcome_category: None,
                api_replay_used: None,
                api_replay_time_ms: None,
                browser_fallback_reason: None,
                tool_result_projection: None,
            },
            state_after: EnvironmentState::Browser(PageState::default()),
            timestamp: Utc::now(),
            verification: None,
            llm_reasoning: None,
        });

        let rendered = history.format_for_llm(5);
        assert!(rendered.contains("outer execution history truncated"));
        assert!(rendered.len() < OUTER_HISTORY_CONTEXT_LIMIT + 512);
    }

    #[test]
    fn test_would_repeat_empty_history() {
        let history = ExecutionHistory::new();
        let action = ExecutableAction::Bash(BashAction::new("ls"));
        assert!(!history.would_repeat(&action, 3));
    }

    #[test]
    fn test_would_repeat_not_enough_history() {
        let mut history = ExecutionHistory::new();
        history.iterations.push(IterationRecord {
            iteration: 1,
            state_before: EnvironmentState::Shell(ShellState::default()),
            action: ExecutableAction::Bash(BashAction::new("ls")),
            result: ActionResultRecord {
                success: true,
                output: Some("file1\nfile2".to_string()),
                error: None,
                duration_ms: 10,
                outcome_category: None,
                api_replay_used: None,
                api_replay_time_ms: None,
                browser_fallback_reason: None,
                tool_result_projection: None,
            },
            state_after: EnvironmentState::Shell(ShellState::default()),
            timestamp: Utc::now(),
            verification: None,
            llm_reasoning: None,
        });

        // With threshold 3, need 2 previous actions + proposed = 3
        // We only have 1, so should not detect
        let action = ExecutableAction::Bash(BashAction::new("ls"));
        assert!(!history.would_repeat(&action, 3));
    }

    #[test]
    fn test_would_repeat_detects_loop() {
        let mut history = ExecutionHistory::new();

        // Add 2 identical actions
        for i in 1..=2 {
            history.iterations.push(IterationRecord {
                iteration: i,
                state_before: EnvironmentState::Shell(ShellState::default()),
                action: ExecutableAction::Bash(BashAction::new("ls")),
                result: ActionResultRecord {
                    success: true,
                    output: Some("file1\nfile2".to_string()),
                    error: None,
                    duration_ms: 10,
                    outcome_category: None,
                    api_replay_used: None,
                    api_replay_time_ms: None,
                    browser_fallback_reason: None,
                    tool_result_projection: None,
                },
                state_after: EnvironmentState::Shell(ShellState::default()),
                timestamp: Utc::now(),
                verification: None,
                llm_reasoning: None,
            });
        }

        // Proposing 3rd identical action should trigger detection
        let action = ExecutableAction::Bash(BashAction::new("ls"));
        assert!(history.would_repeat(&action, 3));
    }

    #[test]
    fn test_would_repeat_different_action() {
        let mut history = ExecutionHistory::new();

        // Add 2 identical actions
        for i in 1..=2 {
            history.iterations.push(IterationRecord {
                iteration: i,
                state_before: EnvironmentState::Shell(ShellState::default()),
                action: ExecutableAction::Bash(BashAction::new("ls")),
                result: ActionResultRecord {
                    success: true,
                    output: None,
                    error: None,
                    duration_ms: 10,
                    outcome_category: None,
                    api_replay_used: None,
                    api_replay_time_ms: None,
                    browser_fallback_reason: None,
                    tool_result_projection: None,
                },
                state_after: EnvironmentState::Shell(ShellState::default()),
                timestamp: Utc::now(),
                verification: None,
                llm_reasoning: None,
            });
        }

        // Proposing different action should not trigger
        let action = ExecutableAction::Bash(BashAction::new("pwd"));
        assert!(!history.would_repeat(&action, 3));
    }

    #[test]
    fn test_format_for_llm_includes_download_artifacts_without_iterations() {
        let mut history = ExecutionHistory::new();
        history.seeded_artifacts.push(
            Artifact::json(
                "downloaded_file:test",
                &json!({
                    "artifact_kind": "downloaded_file",
                    "file_name": "statement.pdf",
                    "purpose": "goal=collect statements step=download",
                    "absolute_path": "/tmp/downloads/statement.pdf",
                    "relative_path": "downloads/thread/step/statement.pdf"
                }),
            )
            .unwrap(),
        );

        let formatted = history.format_for_llm(5);
        assert!(formatted.contains("Available artifacts:"));
        assert!(formatted.contains("statement.pdf"));
        assert!(formatted.contains("downloaded_file:test"));
        assert!(formatted.contains("/tmp/downloads/statement.pdf"));
    }

    #[test]
    fn test_format_resolved_inputs_for_llm_separates_upstream_results() {
        let mut ctx = AgenticContext::default();
        ctx.resolved_inputs.insert(
            "__upstream__download-step".to_string(),
            json!({
                "produced_artifact_ids": ["downloaded_file:test"],
                "last_action_result": {
                    "artifact_kind": "downloaded_file",
                    "absolute_path": "/tmp/downloads/statement.pdf"
                }
            }),
        );
        ctx.resolved_inputs.insert(
            "account_id".to_string(),
            Value::String("acct-123".to_string()),
        );

        let formatted = ctx.format_resolved_inputs_for_llm();
        assert!(formatted.contains("UPSTREAM STEP RESULTS"));
        assert!(formatted.contains("download-step"));
        assert!(formatted.contains("downloaded_file:test"));
        assert!(formatted.contains("USER-PROVIDED VALUES"));
        assert!(formatted.contains("account_id: acct-123"));
    }

    #[test]
    fn test_format_artifact_list_for_llm_omits_heading() {
        let mut history = ExecutionHistory::new();
        history.artifacts.push(
            Artifact::json(
                "downloaded_file:test",
                &json!({
                    "artifact_kind": "downloaded_file",
                    "file_name": "statement.pdf",
                    "purpose": "goal=collect statements step=download",
                    "absolute_path": "/tmp/downloads/statement.pdf",
                    "relative_path": "downloads/thread/step/statement.pdf"
                }),
            )
            .unwrap(),
        );

        let formatted = history.format_artifact_list_for_llm(5, 15);
        assert!(!formatted.contains("Available artifacts:"));
        assert!(formatted.contains("statement.pdf"));
        assert!(formatted.contains("downloaded_file:test"));
    }

    #[test]
    fn summarize_action_output_for_prompt_compacts_primitive_terminal_result() {
        let action = ExecutableAction::Bash(BashAction::new("noop"));
        let output = format!(
            "Browser result: {}",
            json!({
                "success": true,
                "summary": "All visible tests were completed.",
                "terminal_decision": "goal_reached",
                "inner_iterations": 37,
                "inner_objective": {
                    "capability": "browser",
                    "id": "objective-123",
                    "text": "goal: execute every test case"
                },
                "evidence_ledger": {
                    "terminal_evidence": "summary count shows 12 passed",
                    "final_url": "http://localhost:5173/tests/sota-tests/18-dual-range-slider.html"
                },
                "transcript_summary": (0..50)
                    .map(|idx| json!({"iteration": idx, "stdout": "x".repeat(2000)}))
                    .collect::<Vec<_>>()
            })
        );

        let summarized = summarize_action_output_for_prompt(&action, &output);

        assert!(summarized.starts_with("Inner-loop objective completed"));
        assert!(summarized.contains("capability=browser"));
        assert!(summarized.contains("objective_id=objective-123"));
        assert!(summarized.contains("terminal_decision=goal_reached"));
        assert!(summarized.contains("iterations=37"));
        assert!(summarized.contains("summary count shows 12 passed"));
        assert!(!summarized.contains("transcript_summary"));
    }

    #[test]
    fn test_filesystem_state_default() {
        let state = FilesystemState::default();
        assert!(state.last_operation.is_none());
        assert!(state.last_result.is_none());
        assert!(state.error.is_none());
    }

    #[test]
    fn test_agentic_context_builder() {
        let ctx = AgenticContext::new("Complete the task", "Task is marked done")
            .with_max_iterations(20)
            .with_max_tokens_per_cycle(2_000)
            .with_max_repeated_actions(5);

        assert_eq!(ctx.goal, "Complete the task");
        assert_eq!(ctx.max_iterations, 20);
        assert_eq!(ctx.max_tokens_per_cycle, Some(2_000));
        assert_eq!(ctx.max_repeated_actions, 5);
    }

    /// The carrier moves the coding flag and the parent engine across a
    /// spawn as one value, and a run context names its own engine as the
    /// parent whatever the ambient task says.
    #[tokio::test]
    async fn run_task_locals_cross_a_spawn_as_one_value() {
        use crate::magician_v2::execution::coding_engine::{
            coding_context_active, with_coding_context,
        };
        use crate::magician_v2::query_analysis::parent_engine::{
            current_parent_engine, with_parent_engine,
        };

        let probe = || async { (coding_context_active(), current_parent_engine()) };

        let (ambient, for_run) = with_coding_context(
            true,
            with_parent_engine(Some("grok"), async {
                let mut ctx = AgenticContext::new("carry the locals", "carried");
                ctx.harness_engine = Some("codex".to_string());
                ctx.principal = Some("principal".to_string());
                ctx.workspace = Some("workspace".to_string());

                let ambient = CapturedRunTaskLocals::current(None, None);
                let for_run = CapturedRunTaskLocals::for_context(&ctx);
                // A bare spawn sees nothing; the carrier's scope restores it.
                let bare = tokio::spawn(probe()).await.expect("bare task");
                assert_eq!(bare, (false, None));
                (
                    tokio::spawn(ambient.scope(probe()))
                        .await
                        .expect("ambient task"),
                    tokio::spawn(for_run.scope(probe()))
                        .await
                        .expect("run task"),
                )
            }),
        )
        .await;

        assert_eq!(ambient, (true, Some("grok".to_string())));
        assert_eq!(for_run, (true, Some("codex".to_string())));
    }

    #[tokio::test]
    async fn execution_token_meter_blocks_the_call_after_exact_limit() {
        with_execution_token_meter(0, 100, async {
            preflight_execution_token_budget().expect("first call is admitted");
            account_execution_tokens(100).expect("usage at the exact limit is allowed");
            assert_eq!(execution_token_budget_snapshot(), Some((100, 100)));

            let error = preflight_execution_token_budget()
                .expect_err("a subsequent call must be rejected at the exact limit");
            assert_eq!(error.used, 100);
            assert_eq!(error.limit, 100);
            assert!(!error.missing_usage);
        })
        .await;
    }

    #[tokio::test]
    async fn execution_token_meter_rejects_the_response_that_overshoots() {
        with_execution_token_meter(90, 100, async {
            let error = account_execution_tokens(11)
                .expect_err("the response crossing the limit must be rejected");
            assert_eq!(error.used, 101);
            assert_eq!(error.limit, 100);
            assert!(!error.missing_usage);
            assert_eq!(execution_token_budget_snapshot(), Some((101, 100)));
        })
        .await;
    }

    #[tokio::test]
    async fn nested_execution_segments_share_the_outer_meter() {
        with_execution_token_meter(10, 100, async {
            account_execution_tokens(20).expect("first segment usage");
            with_execution_token_meter(0, 1_000, async {
                assert_eq!(execution_token_budget_snapshot(), Some((30, 100)));
                account_execution_tokens(25).expect("nested segment usage");
            })
            .await;
            with_execution_token_meter(0, 1_000, async {
                account_execution_tokens(15).expect("sequential segment usage");
            })
            .await;

            assert_eq!(execution_token_budget_snapshot(), Some((70, 100)));
        })
        .await;
        assert_eq!(execution_token_budget_snapshot(), None);
    }

    #[tokio::test]
    async fn spawned_execution_gets_an_isolated_meter() {
        with_execution_token_meter(10, 100, async {
            let child = tokio::spawn(async {
                assert_eq!(execution_token_budget_snapshot(), None);
                with_execution_token_meter(0, 50, async {
                    account_execution_tokens(25).expect("child usage");
                    assert_eq!(execution_token_budget_snapshot(), Some((25, 50)));
                })
                .await;
                assert_eq!(execution_token_budget_snapshot(), None);
            });
            child.await.expect("child task should complete");

            assert_eq!(execution_token_budget_snapshot(), Some((10, 100)));
        })
        .await;
    }

    #[tokio::test]
    async fn explicit_runtime_handoff_preserves_the_shared_meter() {
        with_execution_token_meter(10, 100, async {
            let captured = CapturedExecutionTokenMeter::current();
            let child = tokio::spawn(async move {
                captured
                    .scope(async {
                        assert_eq!(execution_token_budget_snapshot(), Some((10, 100)));
                        account_execution_tokens(25).expect("handoff usage");
                    })
                    .await;
            });
            child.await.expect("handoff task should complete");

            assert_eq!(
                execution_token_budget_snapshot(),
                Some((35, 100)),
                "the runtime handoff and its caller must share one counter"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn scheduler_root_decision_task_preserves_the_shared_meter() {
        with_execution_token_meter(10, 100, async {
            let mut children = tokio::task::JoinSet::new();
            spawn_with_execution_token_meter_in_set(&mut children, async {
                assert_eq!(execution_token_budget_snapshot(), Some((10, 100)));
                account_execution_tokens(25).expect("decision task usage");
                assert_eq!(execution_token_budget_snapshot(), Some((35, 100)));
            });
            children
                .join_next()
                .await
                .expect("decision task should exist")
                .expect("decision task should complete");

            assert_eq!(
                execution_token_budget_snapshot(),
                Some((35, 100)),
                "the parent and scheduler-root decision task must share one counter"
            );
        })
        .await;
    }

    // ============================================================================
    // Pause/Resume Types Tests
    // ============================================================================

    #[test]
    fn test_user_input_type_text() {
        let input = UserInputType::Text {
            placeholder: Some("Enter your email".to_string()),
            multiline: false,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"placeholder\":\"Enter your email\""));

        let deserialized: UserInputType = serde_json::from_str(&json).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn pause_state_without_cumulative_token_usage_is_backward_compatible() {
        let mut value = serde_json::to_value(AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "history",
            10,
            3,
        ))
        .expect("serialize pause state");
        value
            .as_object_mut()
            .expect("pause state object")
            .remove("llm_tokens_used");

        let restored: AgenticPauseState =
            serde_json::from_value(value).expect("legacy pause state should deserialize");
        assert_eq!(restored.llm_tokens_used, 0);
    }

    #[test]
    fn test_user_input_type_password() {
        let input = UserInputType::Password {
            placeholder: Some("Enter password".to_string()),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"password\""));

        let deserialized: UserInputType = serde_json::from_str(&json).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn test_user_input_type_choice() {
        let input = UserInputType::Choice {
            options: vec![
                ChoiceOption::new("opt1", "Option 1"),
                ChoiceOption::new("opt2", "Option 2"),
            ],
            allow_other: true,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"choice\""));
        assert!(json.contains("\"allow_other\":true"));

        let deserialized: UserInputType = serde_json::from_str(&json).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn test_user_input_type_confirmation() {
        let input = UserInputType::Confirmation {
            confirm_label: Some("Proceed".to_string()),
            deny_label: Some("Cancel".to_string()),
            destructive: true,
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"confirmation\""));
        assert!(json.contains("\"destructive\":true"));

        let deserialized: UserInputType = serde_json::from_str(&json).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn test_user_input_type_external_action() {
        let input = UserInputType::ExternalAction {
            instructions: "Complete the CAPTCHA on the page".to_string(),
            done_label: Some("I've completed it".to_string()),
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"type\":\"external_action\""));
        assert!(json.contains("Complete the CAPTCHA"));

        let deserialized: UserInputType = serde_json::from_str(&json).unwrap();
        assert_eq!(input, deserialized);
    }

    #[test]
    fn test_user_input_value_text() {
        let value = UserInputValue::Text {
            value: "user@example.com".to_string(),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("user@example.com"));

        let deserialized: UserInputValue = serde_json::from_str(&json).unwrap();
        match deserialized {
            UserInputValue::Text { value } => assert_eq!(value, "user@example.com"),
            _ => panic!("Expected Text variant"),
        }
    }

    #[test]
    fn test_user_input_value_choice() {
        let value = UserInputValue::Choice {
            selected_id: "opt1".to_string(),
            other_value: None,
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"type\":\"choice\""));
        assert!(json.contains("\"selected_id\":\"opt1\""));

        let deserialized: UserInputValue = serde_json::from_str(&json).unwrap();
        match deserialized {
            UserInputValue::Choice {
                selected_id,
                other_value,
            } => {
                assert_eq!(selected_id, "opt1");
                assert!(other_value.is_none());
            },
            _ => panic!("Expected Choice variant"),
        }
    }

    #[test]
    fn test_user_input_value_aborted() {
        let value = UserInputValue::Aborted {
            reason: Some("User cancelled".to_string()),
        };
        let json = serde_json::to_string(&value).unwrap();
        assert!(json.contains("\"type\":\"aborted\""));

        let deserialized: UserInputValue = serde_json::from_str(&json).unwrap();
        match deserialized {
            UserInputValue::Aborted { reason } => {
                assert_eq!(reason, Some("User cancelled".to_string()));
            },
            _ => panic!("Expected Aborted variant"),
        }
    }

    #[test]
    fn test_user_input_response_creation() {
        let input_type = UserInputType::Text {
            placeholder: None,
            multiline: false,
        };
        let value = UserInputValue::Text {
            value: "test".to_string(),
        };
        let response = UserInputResponse::new(input_type.clone(), value);

        assert_eq!(response.input_type, input_type);
        match response.value {
            UserInputValue::Text { value } => assert_eq!(value, "test"),
            _ => panic!("Expected Text value"),
        }
        assert!(response.timestamp <= Utc::now());
    }

    #[test]
    fn test_agentic_pause_state_creation() {
        let env_state = EnvironmentState::Shell(ShellState::default());
        let pause_state = AgenticPauseState::new(
            5, // iteration
            "Login to the website".to_string(),
            "User is logged in".to_string(),
            env_state.clone(),
            "Navigated to page, clicked login button".to_string(),
            20, // max_iterations
            3,  // max_repeated_actions
        );

        assert_eq!(pause_state.iteration, 5);
        assert_eq!(pause_state.goal, "Login to the website");
        assert_eq!(pause_state.success_criteria, "User is logged in");
        assert_eq!(pause_state.max_iterations, 20);
        assert_eq!(pause_state.max_repeated_actions, 3);
        assert!(pause_state.execution_id.is_none());
        assert!(pause_state.plan_id.is_none());
        assert!(pause_state.step_id.is_none());
    }

    #[test]
    fn test_agentic_pause_state_with_observability() {
        let env_state = EnvironmentState::Shell(ShellState::default());
        let pause_state = AgenticPauseState::new(
            1,
            "Goal".to_string(),
            "Criteria".to_string(),
            env_state,
            "History".to_string(),
            10,
            3,
        )
        .with_observability(
            "exec-123".to_string(),
            "plan-456".to_string(),
            "step-789".to_string(),
        );

        assert_eq!(pause_state.execution_id, Some("exec-123".to_string()));
        assert_eq!(pause_state.plan_id, Some("plan-456".to_string()));
        assert_eq!(pause_state.step_id, Some("step-789".to_string()));
    }

    #[test]
    fn test_agentic_pause_state_serialization() {
        let env_state = EnvironmentState::Shell(ShellState::default());
        let pause_state = AgenticPauseState::new(
            3,
            "Test goal".to_string(),
            "Test criteria".to_string(),
            env_state,
            "Test history".to_string(),
            15,
            4,
        );

        let json = serde_json::to_string(&pause_state).unwrap();
        assert!(json.contains("\"iteration\":3"));
        assert!(json.contains("\"goal\":\"Test goal\""));
        assert!(!json.contains("\"thread_id\""));

        let deserialized: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.iteration, 3);
        assert_eq!(deserialized.goal, "Test goal");
    }

    #[test]
    fn test_agentic_outcome_waiting_for_user() {
        let env_state = EnvironmentState::Shell(ShellState::default());
        let pause_state = AgenticPauseState::new(
            2,
            "Complete login".to_string(),
            "Logged in".to_string(),
            env_state,
            "History".to_string(),
            10,
            3,
        );

        let pending_inputs = vec![PendingInput::from_agentic_decision(
            "password-input",
            "password",
            None,
            Some("User's password".to_string()),
        )];
        let resolved_inputs = HashMap::new();

        let outcome = AgenticOutcome::WaitingForUser {
            question: "Please enter your password".to_string(),
            input_type: UserInputType::Password { placeholder: None },
            hint: Some("Check your email for 2FA code".to_string()),
            pause_state: Box::new(pause_state.clone()),
            asking_for_parameter: Some("password".to_string()),
            pending_inputs,
            resolved_inputs,
            escalation_trigger: None,
        };

        assert!(outcome.is_waiting_for_user());
        assert!(!outcome.is_terminal());
        assert!(!outcome.is_success());
        assert_eq!(outcome.iterations_used(), 2);
        assert!(outcome.pause_state().is_some());
    }

    #[test]
    fn test_choice_option_creation() {
        let option = ChoiceOption::new("id1", "Label 1");
        assert_eq!(option.id, "id1");
        assert_eq!(option.label, "Label 1");
        assert!(option.description.is_none());

        let option_with_desc = ChoiceOption::with_description("id2", "Label 2", "Description here");
        assert_eq!(
            option_with_desc.description,
            Some("Description here".to_string())
        );
    }

    // ========================================================================
    // truncate_utf8 tests
    // ========================================================================

    #[test]
    fn test_truncate_utf8_short_string() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
        assert_eq!(truncate_utf8("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_utf8_exact_boundary() {
        assert_eq!(truncate_utf8("abcdef", 3), "abc");
    }

    #[test]
    fn test_truncate_utf8_multibyte_boundary() {
        // "café" = [99, 97, 102, 195, 169] — 'é' is 2 bytes
        let s = "café";
        assert_eq!(s.len(), 5); // 5 bytes, 4 chars
                                // Truncate at 4 bytes: lands inside 'é' (byte 195), must back up to 3
        assert_eq!(truncate_utf8(s, 4), "caf");
        // Truncate at 5 bytes: includes full 'é'
        assert_eq!(truncate_utf8(s, 5), "café");
    }

    #[test]
    fn test_truncate_utf8_emoji() {
        // "hi🔥" = [104, 105, 240, 159, 148, 165] — emoji is 4 bytes
        let s = "hi🔥";
        assert_eq!(s.len(), 6);
        // Truncate at 3, 4, 5: all land inside the emoji, back up to 2
        assert_eq!(truncate_utf8(s, 3), "hi");
        assert_eq!(truncate_utf8(s, 4), "hi");
        assert_eq!(truncate_utf8(s, 5), "hi");
        // Truncate at 6: includes full emoji
        assert_eq!(truncate_utf8(s, 6), "hi🔥");
    }

    #[test]
    fn test_truncate_utf8_empty() {
        assert_eq!(truncate_utf8("", 10), "");
        assert_eq!(truncate_utf8("", 0), "");
    }

    // ========================================================================
    // truncate_middle tests
    // ========================================================================

    #[test]
    fn test_truncate_middle_short_string() {
        let s = "short";
        assert_eq!(truncate_middle(s, 100), "short");
    }

    #[test]
    fn test_truncate_middle_preserves_head_and_tail() {
        // separator = " <...redacted for brevity> " = 27 chars
        let sep = " <...redacted for brevity> ";
        assert_eq!(sep.len(), 27);

        // No truncation when within budget
        let s = "A".repeat(34);
        assert_eq!(truncate_middle(&s, 34), s);

        // 100 byte string, limit 50
        // head_budget = 50*2/5 = 20, tail_budget = 50-20-27 = 3
        let long = "A".repeat(40) + &"B".repeat(20) + &"C".repeat(40);
        assert_eq!(long.len(), 100);
        let result = truncate_middle(&long, 50);
        assert!(result.starts_with(&"A".repeat(20))); // 20 A's (head)
        assert!(result.contains("<...redacted for brevity>"));
        assert!(result.ends_with("CCC")); // 3 C's (tail)
    }

    #[test]
    fn test_truncate_middle_40_60_ratio() {
        // 200 byte string, limit 100
        // head_budget = 100*2/5 = 40
        // separator = 27 chars
        // tail_budget = 100 - 40 - 27 = 33
        let s = "H".repeat(80) + &"T".repeat(120);
        assert_eq!(s.len(), 200);
        let result = truncate_middle(&s, 100);
        let head_part = result.split(" <...redacted for brevity> ").next().unwrap();
        let tail_part = result.split(" <...redacted for brevity> ").last().unwrap();
        assert_eq!(head_part.len(), 40); // 40% of budget
        assert_eq!(tail_part.len(), 33); // remainder after separator (60% of non-separator budget)
    }

    #[test]
    fn test_truncate_middle_utf8_safety() {
        // Ensure no panic on multibyte characters at boundaries
        let s = "A".repeat(40) + "café" + &"Z".repeat(56); // 40 + 5 + 56 = 101 bytes
        let result = truncate_middle(&s, 50);
        assert!(result.contains("<...redacted for brevity>"));
        // Just verify no panic and output is valid UTF-8
        assert!(result.len() <= 60); // some slack for separator
    }

    #[test]
    fn test_truncate_middle_constants_applied() {
        // Verify the constants are reasonable values
        assert_eq!(REASONING_LIMIT_MOST_RECENT, 1200);
        assert_eq!(REASONING_LIMIT_OLDER, 700);
        assert!(REASONING_LIMIT_MOST_RECENT > REASONING_LIMIT_OLDER);
    }

    // ============================================================================
    // Agent Routing Tests (TRUE_AGENTS Phase 0)
    // ============================================================================

    #[test]
    fn test_for_agent_sets_correct_fields() {
        let ctx =
            AgenticContext::for_agent("agent-1", "goal-1", "cycle-1", "Buy item", "Item in cart");
        assert_eq!(ctx.agent_id.as_deref(), Some("agent-1"));
        assert_eq!(ctx.goal_id.as_deref(), Some("goal-1"));
        assert_eq!(ctx.cycle_id.as_deref(), Some("cycle-1"));
        assert_eq!(ctx.goal, "Buy item");
        assert_eq!(ctx.success_criteria, "Item in cart");
        assert_eq!(ctx.max_iterations, 4000);
        assert!(ctx.has_agent_routing());
    }

    #[test]
    fn test_routing_key_agent_prefix() {
        let ctx = AgenticContext::for_agent("a1", "g2", "c3", "goal", "criteria");
        assert_eq!(ctx.routing_key(), "agent:a1:g2:c3");
    }

    #[test]
    fn test_routing_key_thread_fallback() {
        let ctx = AgenticContext::new("goal", "criteria").with_observability("tid", "pid", "sid");
        assert_eq!(ctx.routing_key(), "tid:pid:sid");
        assert!(!ctx.has_agent_routing());
    }

    #[test]
    fn test_routing_key_agent_preferred_over_thread() {
        let ctx = AgenticContext::for_agent("a1", "g1", "c1", "goal", "criteria")
            .with_observability("tid", "pid", "sid");
        // Agent routing takes priority
        assert!(ctx.routing_key().starts_with("agent:"));
    }

    #[test]
    fn test_routing_key_uuid_fallback() {
        let ctx = AgenticContext::new("goal", "criteria");
        let key = ctx.routing_key();
        // Neither agent nor thread set — should produce a UUID (36 chars with hyphens)
        assert_eq!(key.len(), 36);
        assert!(!key.starts_with("agent:"));
    }

    #[test]
    fn test_with_agent_routing_builder() {
        let ctx = AgenticContext::new("goal", "criteria").with_agent_routing("a1", "g1", "c1");
        assert!(ctx.has_agent_routing());
        assert_eq!(ctx.routing_key(), "agent:a1:g1:c1");
    }

    #[test]
    fn test_with_artifact_chain_id_builder() {
        let ctx = AgenticContext::new("goal", "criteria").with_artifact_chain_id("exec-1");
        assert_eq!(ctx.artifact_chain_id.as_deref(), Some("exec-1"));
    }

    #[test]
    fn test_default_context_has_no_agent_routing() {
        let ctx = AgenticContext::default();
        assert!(!ctx.has_agent_routing());
        assert!(ctx.agent_id.is_none());
        assert!(ctx.goal_id.is_none());
        assert!(ctx.cycle_id.is_none());
        assert!(ctx.trust_level.is_none());
        assert!(ctx.trust_policies_path.is_none());
        assert!(ctx.preloaded_trust_enforcer.is_none());
        assert!(ctx.approval_rules.is_empty());
        assert!(ctx.approved_confirmation_actions.is_empty());
    }

    #[test]
    fn test_with_trust_policy_sets_context_fields() {
        let ctx = AgenticContext::new("goal", "criteria")
            .with_trust_policy("untrusted", "/tmp/trust.yaml");
        assert_eq!(ctx.trust_level.as_deref(), Some("untrusted"));
        assert_eq!(
            ctx.trust_policies_path.as_deref(),
            Some(std::path::Path::new("/tmp/trust.yaml"))
        );
    }

    #[test]
    fn test_with_preloaded_trust_enforcer_sets_context_field() {
        let enforcer = std::sync::Arc::new(TrustPolicyEnforcer::new(vec![]).unwrap());
        let ctx = AgenticContext::new("goal", "criteria")
            .with_preloaded_trust_enforcer(std::sync::Arc::clone(&enforcer));
        assert!(std::sync::Arc::ptr_eq(
            ctx.preloaded_trust_enforcer
                .as_ref()
                .expect("preloaded enforcer should be set"),
            &enforcer
        ));
    }

    #[test]
    fn test_with_approval_rules_sets_context_field() {
        let ctx = AgenticContext::new("goal", "criteria").with_approval_rules(vec![ApprovalRule {
            tool: "browser".to_string(),
            action: crate::magician_v2::agents::types::ActionPattern::Single("submit".to_string()),
            when: None,
            ttl_secs: None,
        }]);
        assert_eq!(ctx.approval_rules.len(), 1);
        assert_eq!(ctx.approval_rules[0].tool, "browser");
    }

    #[test]
    fn test_with_approved_confirmation_actions_sets_context_field() {
        let json = r#"{"type":"bash","command":"echo hi"}"#.to_string();
        let owner_snapshot = OwnerSnapshot {
            active_owner_agent_id: "owner-1".to_string(),
            owner_stack: vec!["root-owner".to_string()],
        };
        let ctx = AgenticContext::new("goal", "criteria").with_approved_confirmation_actions(vec![
            ApprovedConfirmationAction {
                action_json: json.clone(),
                owner_snapshot: owner_snapshot.clone(),
            },
        ]);
        assert_eq!(
            ctx.approved_confirmation_actions,
            vec![ApprovedConfirmationAction {
                action_json: json,
                owner_snapshot,
            }]
        );
    }

    #[test]
    fn test_pause_state_storage_key_agent() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
        .with_agent_routing("a1", "g2", "c3");
        assert_eq!(ps.storage_key(), "agent:a1:g2:c3");
    }

    #[test]
    fn test_pause_state_storage_key_execution_scoped() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
        .with_observability("tid", "pid", "sid");
        assert_eq!(ps.storage_key(), "tid:pid:sid");
    }

    #[test]
    fn test_pause_state_agent_fields_default_none() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        );
        assert!(ps.agent_id.is_none());
        assert!(ps.goal_id.is_none());
        assert!(ps.cycle_id.is_none());
        assert!(ps.trust_level.is_none());
        assert!(ps.trust_policies_path.is_none());
        assert!(ps.approval_rules.is_empty());
    }

    #[test]
    fn test_pause_state_with_trust_policy_roundtrip() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
        .with_trust_policy("reviewed", "/tmp/trust.yaml");
        let json = serde_json::to_string(&ps).unwrap();
        let deserialized: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.trust_level.as_deref(), Some("reviewed"));
        assert_eq!(
            deserialized.trust_policies_path.as_deref(),
            Some(std::path::Path::new("/tmp/trust.yaml"))
        );
    }

    #[test]
    fn test_pause_state_with_approval_rules_roundtrip() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
        .with_approval_rules(vec![ApprovalRule {
            tool: "shell".to_string(),
            action: crate::magician_v2::agents::types::ActionPattern::Single("execute".to_string()),
            when: None,
            ttl_secs: None,
        }]);
        let json = serde_json::to_string(&ps).unwrap();
        let deserialized: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.approval_rules.len(), 1);
        assert_eq!(deserialized.approval_rules[0].tool, "shell");
    }

    // ── Trusted-store integrity: pause discriminator + authorization-hash locks ──
    //
    // These are UNIT-level locks of the mechanism's building blocks. The FULL
    // end-to-end HTTP regression (stage a real elevation pause → POST the resume
    // to `respond_hitl_handler` → assert forged/tampered/restart all refuse;
    // assert a legit resume restores the authority) is an INTEGRATION test that
    // must be written and RUN once the crate compiles.
    //
    // TODO(trusted-store): end-to-end HTTP stage→approve→apply integration
    // regression — write + run when the crate compiles.

    fn base_pause_state() -> AgenticPauseState {
        AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
    }

    /// `is_elevation()` is the discriminator that decides whether the resume
    /// path binds the pause to the authority at all. A pause carrying ANY
    /// re-authorization surface — approval rules, owner identity/stack, or a
    /// typed protected product invocation — is an elevation; a benign pause
    /// with all of them empty/None is NOT. If this ever mis-classified an elevation as benign, a
    /// forged/tampered pause could re-mint authority with no binding check.
    #[test]
    fn is_elevation_discriminates() {
        // Approval rules alone → elevation.
        let with_rules = base_pause_state().with_approval_rules(vec![ApprovalRule {
            tool: "shell".to_string(),
            action: crate::magician_v2::agents::types::ActionPattern::Single("execute".to_string()),
            when: None,
            ttl_secs: None,
        }]);
        assert!(
            with_rules.is_elevation(),
            "non-empty approval_rules must be an elevation"
        );

        // Owner snapshot (active owner + owner_stack) alone → elevation.
        let with_owner =
            base_pause_state().with_owner_snapshot("owner-a", vec!["owner-b".to_string()]);
        assert!(
            with_owner.is_elevation(),
            "an owner authority chain must be an elevation"
        );

        // active_owner_agent_id set but owner_stack empty → still elevation.
        let mut with_active_owner_only = base_pause_state();
        with_active_owner_only.active_owner_agent_id = Some("owner-solo".to_string());
        assert!(
            with_active_owner_only.is_elevation(),
            "active_owner_agent_id alone must be an elevation"
        );

        // A typed product surface can grant a surface-only owner and is
        // therefore authorization-bearing even without approval/owner fields.
        let mut with_product_surface = base_pause_state();
        with_product_surface.invocation_context_override =
            Some(crate::magician_v2::agents::AgentInvocationContext {
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                source_agent_id: None,
                target_agent_id: "brainstorm-facilitator".to_string(),
                surface: crate::magician_v2::agents::InvocationSurface::ThinkingMap,
                feature_mode: crate::magician_v2::agents::FeatureMode::Brainstorm,
                source_kind: crate::magician_v2::agents::InvocationSourceKind::ProductFeature,
                chat_session_id: Some("map-session".to_string()),
                chat_turn_id: Some("map-turn".to_string()),
            });
        assert!(
            with_product_surface.is_elevation(),
            "a typed product-surface invocation must be an elevation"
        );

        let mut with_suspended_product_surface = base_pause_state();
        with_suspended_product_surface.owner_invocation_stack = vec![Some(
            with_product_surface
                .invocation_context_override
                .clone()
                .expect("product invocation"),
        )];
        assert!(
            with_suspended_product_surface.is_elevation(),
            "a suspended typed owner invocation must be an elevation"
        );

        // All authorization-bearing fields empty/None → NOT an elevation.
        let benign = base_pause_state();
        assert!(benign.approval_rules.is_empty());
        assert!(benign.owner_stack.is_empty());
        assert!(benign.owner_invocation_stack.is_empty());
        assert!(benign.active_owner_agent_id.is_none());
        assert!(benign.invocation_context_override.is_none());
        assert!(
            !benign.is_elevation(),
            "a pause with no re-authorization surface must NOT be an elevation"
        );
    }

    /// Locks the Issue-B canonicalization fix (commit `c5046cc24`):
    /// `authorization_hash()` routes the security-bearing fields through
    /// `serde_json::to_value` (a `BTreeMap`) FIRST, so a nested
    /// `param_matches` HashMap with ≥2 keys — whose direct serialization order
    /// is per-instance — hashes IDENTICALLY before and after a serde round-trip.
    /// Without the fix, a disk-recovered pause could re-emit the same map in a
    /// different key order and spuriously fail closed on a legitimate resume.
    #[test]
    fn authorization_hash_stable_across_serde_roundtrip() {
        // ≥2 keys so map ordering matters. If serialization did NOT canonicalize
        // key order, the round-trip could reorder these and change the hash.
        let param_matches = HashMap::from([
            ("selector".to_string(), vec!["apply".to_string()]),
            ("confirmed".to_string(), vec!["true".to_string()]),
        ]);
        let pause = base_pause_state()
            .with_trust_policy("reviewed", "/tmp/trust.yaml")
            .with_owner_snapshot("owner-a", vec!["owner-b".to_string()])
            .with_approval_rules(vec![ApprovalRule {
                tool: "browser".to_string(),
                action: crate::magician_v2::agents::types::ActionPattern::Single(
                    "navigate".to_string(),
                ),
                when: Some(crate::magician_v2::agents::types::ApprovalCondition {
                    param_matches,
                    url_contains: vec!["example.com".to_string()],
                }),
                ttl_secs: None,
            }]);
        assert!(pause.is_elevation());

        let h1 = pause.authorization_hash();
        let json = serde_json::to_string(&pause).unwrap();
        let back: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.authorization_hash(),
            h1,
            "authorization_hash must survive a serde round-trip even with a \
             multi-key param_matches map (canonicalization fix c5046cc24)"
        );
    }

    /// A pause parked after a tool failure carries the failed-tool ledger in
    /// its authorization hash. The ledger's order must not depend on a hash
    /// seed, or the read-back pause hashes differently from the one that was
    /// sealed and the resume is refused as tampered.
    #[test]
    fn authorization_hash_is_stable_with_a_failed_tool_ledger() {
        let mut pause = base_pause_state();
        let mut protective = LoopProtectiveState::default();
        for (tool, fingerprint) in [
            ("files", "read:a"),
            ("tool_search", "select:files"),
            ("files", "write:b"),
            ("http", "get:c"),
            ("browser__click", "timeout"),
        ] {
            protective
                .agentic_failed_tool_fingerprints
                .insert((tool.to_string(), fingerprint.to_string()));
        }
        pause.loop_protective_state = Some(protective);
        let sealed = pause.authorization_hash();
        for _ in 0..8 {
            let json = serde_json::to_string(&pause).unwrap();
            let back: AgenticPauseState = serde_json::from_str(&json).unwrap();
            assert_eq!(back.authorization_hash(), sealed);
            let again: AgenticPauseState =
                serde_json::from_str(&serde_json::to_string(&back).unwrap()).unwrap();
            assert_eq!(again.authorization_hash(), sealed);
        }
    }

    /// Two elevation pauses differing only in an authorization-bearing field
    /// produce different `authorization_hash()` values. This is the tamper
    /// detection the resume decide site relies on: an on-disk edit to any of
    /// `trust_level` / `approval_rules` / owner identity or invocation stack
    /// changes the hash, so it
    /// no longer matches the value recorded at stage time and the resume fails
    /// closed.
    #[test]
    fn authorization_hash_detects_tamper() {
        let baseline = base_pause_state()
            .with_trust_policy("reviewed", "/tmp/trust.yaml")
            .with_owner_snapshot("owner-a", vec!["owner-b".to_string()]);
        let h1 = baseline.authorization_hash();

        // Tamper 1: escalate trust_level.
        let mut tampered_trust = baseline.clone();
        tampered_trust.trust_level = Some("full".to_string());
        assert_ne!(
            tampered_trust.authorization_hash(),
            h1,
            "changing trust_level MUST change authorization_hash"
        );

        // Tamper 2: inject an approval rule (widen the granted surface).
        let mut tampered_rules = baseline.clone();
        tampered_rules.approval_rules = vec![ApprovalRule {
            tool: "shell".to_string(),
            action: crate::magician_v2::agents::types::ActionPattern::Single("execute".to_string()),
            when: None,
            ttl_secs: None,
        }];
        assert_ne!(
            tampered_rules.authorization_hash(),
            h1,
            "adding an approval_rule MUST change authorization_hash"
        );

        // Tamper 3: extend the owner authority chain.
        let mut tampered_owner = baseline.clone();
        tampered_owner.owner_stack = vec!["owner-b".to_string(), "owner-c".to_string()];
        assert_ne!(
            tampered_owner.authorization_hash(),
            h1,
            "extending owner_stack MUST change authorization_hash"
        );

        // Tamper 4: inject protected authority for a suspended owner.
        let mut tampered_suspended_surface = baseline.clone();
        tampered_suspended_surface.owner_invocation_stack =
            vec![Some(crate::magician_v2::agents::AgentInvocationContext {
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                source_agent_id: None,
                target_agent_id: "personal-assistant".to_string(),
                surface: crate::magician_v2::agents::InvocationSurface::Tutor,
                feature_mode: crate::magician_v2::agents::FeatureMode::Tutor,
                source_kind: crate::magician_v2::agents::InvocationSourceKind::ProductFeature,
                chat_session_id: Some("tutor-session".to_string()),
                chat_turn_id: Some("tutor-turn".to_string()),
            })];
        assert_ne!(
            tampered_suspended_surface.authorization_hash(),
            h1,
            "changing suspended owner invocation authority MUST change authorization_hash"
        );

        // Tamper 5: inject a protected product surface.
        let mut tampered_surface = baseline.clone();
        tampered_surface.invocation_context_override =
            Some(crate::magician_v2::agents::AgentInvocationContext {
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                source_agent_id: None,
                target_agent_id: "brainstorm-facilitator".to_string(),
                surface: crate::magician_v2::agents::InvocationSurface::ThinkingMap,
                feature_mode: crate::magician_v2::agents::FeatureMode::Brainstorm,
                source_kind: crate::magician_v2::agents::InvocationSourceKind::ProductFeature,
                chat_session_id: Some("map-session".to_string()),
                chat_turn_id: Some("map-turn".to_string()),
            });
        assert_ne!(
            tampered_surface.authorization_hash(),
            h1,
            "adding a product invocation MUST change authorization_hash"
        );

        // Tamper 6: redirect an already-protected paused execution to another
        // provider/profile. Routing controls where execution context is sent,
        // so it participates in the same trusted pause binding.
        let mut tampered_routing = baseline;
        tampered_routing.execution_llm_routing_overrides = Some(
            crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides {
                planning: crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingEndpoint::for_profile(
                    "tampered-provider-profile",
                ),
                ..Default::default()
            },
        );
        assert_ne!(
            tampered_routing.authorization_hash(),
            h1,
            "changing protected execution routing MUST change authorization_hash"
        );

        // Tamper 7: widen a deliberately one-pack execution while it is paused.
        let mut narrow_scope = base_pause_state()
            .with_trust_policy("reviewed", "/tmp/trust.yaml")
            .with_owner_snapshot("owner-a", Vec::new());
        narrow_scope.preserve_initial_tool_scope = true;
        narrow_scope.preserved_initial_tools = vec![runtime_core::ToolInfo {
            name: "search_memory".to_string(),
            description: "narrow read-only scope".to_string(),
            category: "memory".to_string(),
            categories: Vec::new(),
            parameters: Vec::new(),
            enhanced_description: None,
            keywords: Vec::new(),
            use_cases: Vec::new(),
            composition_category: None,
            providing_agent_id: None,
        }];
        let narrow_hash = narrow_scope.authorization_hash();
        narrow_scope
            .preserved_initial_tools
            .push(runtime_core::ToolInfo {
                name: "shell".to_string(),
                description: "unauthorized widened scope".to_string(),
                category: "system".to_string(),
                categories: Vec::new(),
                parameters: Vec::new(),
                enhanced_description: None,
                keywords: Vec::new(),
                use_cases: Vec::new(),
                composition_category: None,
                providing_agent_id: None,
            });
        assert_ne!(
            narrow_scope.authorization_hash(),
            narrow_hash,
            "widening a preserved direct-tool scope MUST change authorization_hash"
        );
    }

    #[test]
    fn test_pause_state_serde_without_agent_fields() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
        .with_observability("tid", "pid", "sid");
        let json = serde_json::to_string(&ps).unwrap();
        // agent_id should NOT appear thanks to skip_serializing_if
        assert!(!json.contains("agent_id"));
        // Should round-trip
        let deserialized: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert!(deserialized.agent_id.is_none());
        assert_eq!(deserialized.execution_id.as_deref(), Some("tid"));
        assert_eq!(deserialized.storage_key(), "tid:pid:sid");
    }

    #[test]
    fn test_pause_state_with_task_execution_context_roundtrip() {
        let ps = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            10,
            3,
        )
        .with_stable_goal_hash(Some("goal-hash-1".to_string()))
        .with_task_execution_context(
            Some("task-1".to_string()),
            Some("exec-1".to_string()),
            Some("thread-exec-1--execution-exec-1".to_string()),
        )
        .with_root_execution_id(Some("exec-root-1".to_string()));

        let json = serde_json::to_string(&ps).unwrap();
        let deserialized: AgenticPauseState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.task_id.as_deref(), Some("task-1"));
        assert_eq!(deserialized.execution_id.as_deref(), Some("exec-1"));
        assert_eq!(
            deserialized.root_execution_id.as_deref(),
            Some("exec-root-1")
        );
        assert_eq!(
            deserialized.stable_goal_hash.as_deref(),
            Some("goal-hash-1")
        );
        assert_eq!(
            deserialized.artifact_chain_id.as_deref(),
            Some("thread-exec-1--execution-exec-1")
        );
    }

    #[test]
    fn test_existing_new_unchanged() {
        // Regression: existing new() behavior is unaffected
        let ctx = AgenticContext::new("Do X", "X done")
            .with_max_iterations(10)
            .with_observability("t1", "p1", "s1");
        assert_eq!(ctx.goal, "Do X");
        assert_eq!(ctx.success_criteria, "X done");
        assert_eq!(ctx.max_iterations, 10);
        assert!(ctx.has_observability());
        assert!(!ctx.has_agent_routing());
    }

    fn test_artifact(name: &str, content_type: &str, data: &[u8]) -> Artifact {
        Artifact {
            name: name.into(),
            content_type: content_type.into(),
            data: data.to_vec(),
            artifact_type: None,
            render_hints: None,
            materialized_path: None,
        }
    }

    #[test]
    fn artifact_kind_extracts_from_json() {
        let a = test_artifact(
            "custom",
            "application/json",
            br#"{"artifact_kind":"custom_file"}"#,
        );
        assert_eq!(artifact_kind(&a), "custom_file");
    }

    #[test]
    fn json_artifact_kind_is_read_through_the_bounded_header_without_wire_drift() {
        let artifact = Artifact::json(
            "large",
            &serde_json::json!({
                "artifact_kind": "custom_file",
                "payload": "x".repeat(1024 * 1024),
            }),
        )
        .expect("json artifact");
        assert_eq!(artifact.artifact_type, None);
        assert_eq!(
            artifact_kind_from_json_header(&artifact.data).as_deref(),
            Some("custom_file")
        );
        assert_eq!(artifact_kind(&artifact), "custom_file");
    }

    #[test]
    fn artifact_header_rejects_deep_json_before_serde_deserialization() {
        let mut data = br#"{"artifact_kind":"custom_file","payload":"#.to_vec();
        data.extend(std::iter::repeat_n(b'[', 10_000));
        data.extend(std::iter::repeat_n(b']', 10_000));
        data.push(b'}');

        assert_eq!(artifact_kind_from_json_header(&data), None);
    }

    #[test]
    fn artifact_kind_falls_back_to_other_for_non_json() {
        let a = test_artifact("plain", "text/plain", b"not json");
        assert_eq!(artifact_kind(&a), "other");
    }

    #[test]
    fn artifact_kind_falls_back_to_other_for_missing_field() {
        let a = test_artifact(
            "json-no-kind",
            "application/json",
            br#"{"other_field":"value"}"#,
        );
        assert_eq!(artifact_kind(&a), "other");
    }

    #[test]
    fn compute_artifact_budgets_three_kinds_fits_under_total() {
        let counts = vec![
            ("A".to_string(), 20),
            ("B".to_string(), 20),
            ("C".to_string(), 20),
        ];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        let mut values: Vec<usize> = budgets.values().copied().collect();
        values.sort();
        assert_eq!(values, vec![5, 5, 5]);
        assert_eq!(values.iter().sum::<usize>(), 15);
    }

    #[test]
    fn compute_artifact_budgets_four_kinds_redistributes() {
        let counts = vec![
            ("A".to_string(), 20),
            ("B".to_string(), 20),
            ("C".to_string(), 20),
            ("D".to_string(), 20),
        ];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        let mut values: Vec<usize> = budgets.values().copied().collect();
        values.sort();
        assert_eq!(values, vec![3, 4, 4, 4]);
        assert_eq!(values.iter().sum::<usize>(), 15);
        // Lex-smaller kind name wins the decrement contest → A gets the 3.
        assert_eq!(budgets["A"], 3);
        assert_eq!(budgets["B"], 4);
        assert_eq!(budgets["C"], 4);
        assert_eq!(budgets["D"], 4);
    }

    #[test]
    fn compute_artifact_budgets_respects_per_kind_cap_when_sparse() {
        let counts = vec![
            ("A".to_string(), 20),
            ("B".to_string(), 20),
            ("C".to_string(), 1),
            ("D".to_string(), 1),
        ];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        assert_eq!(budgets["A"], 5);
        assert_eq!(budgets["B"], 5);
        assert_eq!(budgets["C"], 1);
        assert_eq!(budgets["D"], 1);
        assert_eq!(budgets.values().sum::<usize>(), 12);
    }

    #[test]
    fn compute_artifact_budgets_one_kind_stays_under_per_kind_cap() {
        let counts = vec![("A".to_string(), 20)];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        assert_eq!(budgets["A"], 5);
    }

    #[test]
    fn compute_artifact_budgets_two_kinds_both_cap_under_total() {
        let counts = vec![("A".to_string(), 20), ("B".to_string(), 20)];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        assert_eq!(budgets["A"], 5);
        assert_eq!(budgets["B"], 5);
        assert_eq!(budgets.values().sum::<usize>(), 10);
    }

    #[test]
    fn compute_artifact_budgets_larger_available_wins_tiebreak() {
        // Both kinds hit per_kind_max=5 → budgets [5, 5] = 10.
        // total_max=9 forces one decrement.
        // Budgets tied → secondary tiebreak: larger available_count (B=20) wins.
        let counts = vec![("A".to_string(), 10), ("B".to_string(), 20)];
        let budgets = compute_artifact_budgets(counts, 5, 9);
        assert_eq!(
            budgets["A"], 5,
            "A should retain its budget; B has more available"
        );
        assert_eq!(
            budgets["B"], 4,
            "B should take the decrement (larger available)"
        );
        assert_eq!(budgets.values().sum::<usize>(), 9);
    }

    #[test]
    fn compute_artifact_budgets_five_kinds_redistributes() {
        let counts = vec![
            ("A".to_string(), 20),
            ("B".to_string(), 20),
            ("C".to_string(), 20),
            ("D".to_string(), 20),
            ("E".to_string(), 20),
        ];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        let mut values: Vec<usize> = budgets.values().copied().collect();
        values.sort();
        assert_eq!(values, vec![3, 3, 3, 3, 3]);
        assert_eq!(values.iter().sum::<usize>(), 15);
    }

    #[test]
    fn compute_artifact_budgets_six_kinds_redistributes() {
        let counts = vec![
            ("A".to_string(), 20),
            ("B".to_string(), 20),
            ("C".to_string(), 20),
            ("D".to_string(), 20),
            ("E".to_string(), 20),
            ("F".to_string(), 20),
        ];
        let budgets = compute_artifact_budgets(counts, 5, 15);
        let mut values: Vec<usize> = budgets.values().copied().collect();
        values.sort();
        assert_eq!(values, vec![2, 2, 2, 3, 3, 3]);
        assert_eq!(values.iter().sum::<usize>(), 15);
        // Lex-smaller kind names get decremented first → A, B, C → 2; D, E, F → 3.
        assert_eq!(budgets["A"], 2);
        assert_eq!(budgets["B"], 2);
        assert_eq!(budgets["C"], 2);
        assert_eq!(budgets["D"], 3);
        assert_eq!(budgets["E"], 3);
        assert_eq!(budgets["F"], 3);
    }

    #[test]
    fn compute_artifact_budgets_empty_input_returns_empty() {
        let budgets = compute_artifact_budgets(vec![], 5, 15);
        assert!(budgets.is_empty());
    }

    #[test]
    fn compute_artifact_budgets_is_deterministic() {
        let counts = vec![
            ("A".to_string(), 20),
            ("B".to_string(), 20),
            ("C".to_string(), 20),
            ("D".to_string(), 20),
        ];
        let first = compute_artifact_budgets(counts.clone(), 5, 15);
        let second = compute_artifact_budgets(counts, 5, 15);
        assert_eq!(first, second);
    }

    #[test]
    fn combined_artifacts_for_prompt_per_kind_caps_per_kind() {
        // 6 custom_file + 2 downloaded_file -> should return 5+2 = 7
        let mut artifacts: Vec<Artifact> = Vec::new();
        for i in 0..6 {
            artifacts.push(test_artifact(
                &format!("custom_{i}"),
                "application/json",
                format!(r#"{{"artifact_kind":"custom_file","n":{i}}}"#).as_bytes(),
            ));
        }
        for i in 0..2 {
            artifacts.push(test_artifact(
                &format!("dl_{i}"),
                "application/json",
                format!(r#"{{"artifact_kind":"downloaded_file","n":{i}}}"#).as_bytes(),
            ));
        }

        let history = ExecutionHistory {
            artifacts,
            ..Default::default()
        };

        let selected = history.combined_artifacts_for_prompt_per_kind(5, 15);
        assert_eq!(selected.len(), 7);

        let customs = selected
            .iter()
            .filter(|a| a.name.starts_with("custom_"))
            .count();
        let dls = selected
            .iter()
            .filter(|a| a.name.starts_with("dl_"))
            .count();
        assert_eq!(customs, 5);
        assert_eq!(dls, 2);
    }

    #[test]
    fn combined_artifacts_for_prompt_per_kind_keeps_most_recent_within_kind() {
        let mut artifacts: Vec<Artifact> = Vec::new();
        for i in 0..8 {
            artifacts.push(test_artifact(
                &format!("custom_{i}"),
                "application/json",
                format!(r#"{{"artifact_kind":"custom_file","n":{i}}}"#).as_bytes(),
            ));
        }
        let history = ExecutionHistory {
            artifacts,
            ..Default::default()
        };
        let selected = history.combined_artifacts_for_prompt_per_kind(5, 15);
        let names: Vec<&str> = selected.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["custom_3", "custom_4", "custom_5", "custom_6", "custom_7"]
        );
    }

    #[test]
    fn format_artifact_list_per_kind_caps_section() {
        let mut artifacts: Vec<Artifact> = Vec::new();
        for i in 0..8 {
            artifacts.push(test_artifact(
                &format!("custom_{i}"),
                "application/json",
                format!(r#"{{"artifact_kind":"custom_file","absolute_path":"/p/{i}"}}"#).as_bytes(),
            ));
        }
        let history = ExecutionHistory {
            artifacts,
            ..Default::default()
        };
        let out = history.format_artifact_list_for_llm(5, 15);

        // Should contain exactly 5 lines starting with "- custom_"
        let lines_count = out.lines().filter(|l| l.starts_with("- custom_")).count();
        assert_eq!(lines_count, 5);

        // Must keep the most recent 5 (custom_3..custom_7)
        assert!(out.contains("- custom_7"));
        assert!(out.contains("- custom_3"));
        assert!(!out.contains("- custom_0"));
        assert!(!out.contains("- custom_2"));
    }

    #[test]
    fn combined_artifacts_for_prompt_per_kind_preserves_global_order() {
        let artifacts = vec![
            test_artifact(
                "custom_0",
                "application/json",
                br#"{"artifact_kind":"custom_file"}"#,
            ),
            test_artifact(
                "dl_0",
                "application/json",
                br#"{"artifact_kind":"downloaded_file"}"#,
            ),
            test_artifact(
                "custom_1",
                "application/json",
                br#"{"artifact_kind":"custom_file"}"#,
            ),
        ];
        let history = ExecutionHistory {
            artifacts,
            ..Default::default()
        };
        let selected = history.combined_artifacts_for_prompt_per_kind(5, 15);
        let names: Vec<&str> = selected.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["custom_0", "dl_0", "custom_1"]);
    }

    // ========================================================================
    // Resume state loss — see docs/archive/plans/2026-08-25-resume-state-loss-fix.md
    //
    // Before this fix, 25 of the loop's 28 cross-iteration locals were absent
    // from `AgenticPauseState`, so every pause silently reset loop detection,
    // all retry/abort budgets, the accumulated USD spend, and any approval the
    // user had already granted.
    // ========================================================================

    fn populated_loop_protective() -> LoopProtectiveState {
        let mut protective = LoopProtectiveState {
            cumulative_run_cost_usd: 4.25,
            consecutive_parse_failures: 2,
            transient_retry_count: 3,
            yield_transient_retry_count: 1,
            consecutive_goal_reached_rejections: 2,
            consecutive_giveup_rejections: 1,
            consecutive_no_action_iterations: 3,
            skip_iterations: 7,
            pending_step_completed: Some("step-7".to_string()),
            pending_step_failed: Some("step-8".to_string()),
            last_request_hover_discovery: Some(true),
            ..LoopProtectiveState::default()
        };
        protective
            .agentic_tool_repeat_counts
            .insert("browser__click".to_string(), 4);
        protective
            .agentic_failed_tool_fingerprints
            .insert(("browser__click".to_string(), "timeout".to_string()));
        protective.started_step_ids.insert("step-1".to_string());
        protective
            .recent_no_action_summaries
            .push_back("iter 3: no decision recorded".to_string());
        protective
    }

    fn approval(action_json: &str) -> ApprovedConfirmationAction {
        ApprovedConfirmationAction {
            action_json: action_json.to_string(),
            owner_snapshot: OwnerSnapshot {
                active_owner_agent_id: "agent-1".to_string(),
                owner_stack: Vec::new(),
            },
        }
    }

    #[test]
    fn loop_protective_state_round_trips_through_json() {
        let original = populated_loop_protective();
        let encoded = serde_json::to_string(&original).expect("serialize");
        let decoded: LoopProtectiveState = serde_json::from_str(&encoded).expect("deserialize");

        // Every field, individually — a blanket equality assert would pass even
        // if a field were silently dropped by a missing `serde(default)`.
        assert_eq!(decoded.cumulative_run_cost_usd, 4.25);
        assert_eq!(decoded.consecutive_parse_failures, 2);
        assert_eq!(decoded.transient_retry_count, 3);
        assert_eq!(decoded.yield_transient_retry_count, 1);
        assert_eq!(decoded.consecutive_goal_reached_rejections, 2);
        assert_eq!(decoded.consecutive_giveup_rejections, 1);
        assert_eq!(decoded.consecutive_no_action_iterations, 3);
        assert_eq!(decoded.skip_iterations, 7);
        assert_eq!(decoded.pending_step_completed.as_deref(), Some("step-7"));
        assert_eq!(decoded.pending_step_failed.as_deref(), Some("step-8"));
        assert_eq!(decoded.last_request_hover_discovery, Some(true));
        assert_eq!(
            decoded.agentic_tool_repeat_counts.get("browser__click"),
            Some(&4)
        );
        assert!(decoded
            .agentic_failed_tool_fingerprints
            .contains(&("browser__click".to_string(), "timeout".to_string())));
        assert!(decoded.started_step_ids.contains("step-1"));
        assert_eq!(
            decoded
                .recent_no_action_summaries
                .front()
                .map(String::as_str),
            Some("iter 3: no decision recorded")
        );
    }

    #[test]
    fn empty_loop_protective_state_serializes_to_an_empty_object() {
        // Every field is `skip_serializing_if`, so an untouched run must not
        // grow the pause body at all. Pause records are fsynced uncompressed on
        // every pause, so this is a size guarantee, not a cosmetic one.
        let encoded = serde_json::to_string(&LoopProtectiveState::default()).expect("serialize");
        assert_eq!(encoded, "{}", "default state must add no bytes to a pause");
    }

    #[test]
    fn unconsumed_approvals_make_a_pause_an_elevation() {
        // `is_elevation` gates whether `authorization_hash` is verified at all,
        // so an approval that does not trip it would never be tamper-checked.
        let mut pause = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        assert!(
            !pause.is_elevation(),
            "a plain pause with no authority must not be an elevation"
        );

        pause.approved_confirmation_actions = vec![approval("{\"tool\":\"bash\"}")];
        assert!(
            pause.is_elevation(),
            "a pause carrying an unconsumed approval re-grants authority on resume"
        );
    }

    #[test]
    fn the_ceilings_survive_the_disk_round_trip() {
        // §2 of the turn-boundary contract: "anything enforcement-bearing —
        // ceilings, counters, approvals — must be represented, or the
        // enforcement is advisory." These five were not, and all five are
        // empty-means-unrestricted, so losing them widened the resumed run.
        let mut pause = AgenticPauseState::new(
            3,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        pause.allowed_action_types = Some(vec!["browser".to_string()]);
        pause.denied_capability_names = vec!["shell".to_string()];
        pause.browser_transports = vec!["cdp".to_string()];
        pause.denied_tool_params = HashMap::from([(
            "http".to_string(),
            HashMap::from([("method".to_string(), vec!["DELETE".to_string()])]),
        )]);

        let encoded = serde_json::to_string(&pause).expect("serialize");
        let restored: AgenticPauseState = serde_json::from_str(&encoded).expect("read back");

        assert_eq!(
            restored.allowed_action_types,
            Some(vec!["browser".to_string()]),
            "a run pinned to one action type must not resume able to use others"
        );
        assert_eq!(restored.denied_capability_names, vec!["shell".to_string()]);
        assert_eq!(restored.browser_transports, vec!["cdp".to_string()]);
        assert_eq!(restored.denied_tool_params, pause.denied_tool_params);
    }

    /// The stage side hashes the in-memory pause and the resume side hashes
    /// the body read back from disk; a field that does not survive the
    /// serde round trip must not split them. The hash is taken over the
    /// normalised form, so a second round trip changes nothing.
    #[test]
    fn the_authorization_hash_is_the_same_before_and_after_a_disk_round_trip() {
        let mut pause = AgenticPauseState::new(
            7,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        pause.active_owner_agent_id = Some("android-operator".to_string());
        let mut protective = LoopProtectiveState::default();
        protective.cumulative_run_cost_usd = 0.1 + 0.2;
        protective.consecutive_goal_reached_rejections = 1;
        protective
            .agentic_tool_repeat_counts
            .insert("android_act".to_string(), 3);
        protective
            .agentic_failed_tool_fingerprints
            .insert(("android_app".to_string(), "launch".to_string()));
        pause.loop_protective_state = Some(protective);
        let staged = pause.authorization_hash();
        let read_back: AgenticPauseState =
            serde_json::from_str(&serde_json::to_string(&pause).unwrap()).unwrap();
        assert_eq!(staged, read_back.authorization_hash());
        let read_back_twice: AgenticPauseState =
            serde_json::from_str(&serde_json::to_string(&read_back).unwrap()).unwrap();
        assert_eq!(staged, read_back_twice.authorization_hash());
    }

    #[test]
    fn widening_a_ceiling_on_disk_breaks_the_authorization_hash() {
        // Removing a restriction is an escalation, so the hash must cover them
        // even though they do not, by themselves, make a pause an elevation.
        let base = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        let mut restricted = base.clone();
        restricted.allowed_action_types = Some(vec!["browser".to_string()]);
        assert_ne!(
            base.authorization_hash(),
            restricted.authorization_hash(),
            "the action ceiling must be covered, or a rewritten body could drop it"
        );

        let mut widened = restricted.clone();
        widened.allowed_action_types = Some(vec!["browser".to_string(), "tool".to_string()]);
        assert_ne!(
            restricted.authorization_hash(),
            widened.authorization_hash(),
            "the hash must cover the ceiling's CONTENTS, not merely its presence"
        );

        let mut denied = base.clone();
        denied.denied_capability_names = vec!["shell".to_string()];
        assert_ne!(base.authorization_hash(), denied.authorization_hash());
    }

    #[test]
    fn a_restricted_run_is_not_an_elevation() {
        // REGRESSION GUARD. Promoting every restricted run to an elevation would
        // make it fail closed after a restart — the in-process authority entry
        // does not survive one — and discard the user's work. That is the exact
        // trap `approved_confirmation_actions` was careful to avoid, and a
        // ceiling grants nothing, so it must not spring it.
        let mut pause = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        pause.allowed_action_types = Some(vec!["browser".to_string()]);
        pause.denied_capability_names = vec!["shell".to_string()];
        pause.browser_transports = vec!["cdp".to_string()];

        assert!(
            !pause.is_elevation(),
            "a ceiling narrows what a run may do; it grants nothing and must stay \
             resumable across a restart"
        );

        // What this costs, stated because the two properties compose in a way
        // neither test shows alone. `authorization_hash` covers the ceilings, but
        // it is only VERIFIED for a pause that `is_elevation()` — so a pause
        // carrying nothing but ceilings has a hash nobody checks, and an attacker
        // holding write access to the record can widen them.
        //
        // Accepted, for two reasons. It is strictly better than the state this
        // replaced, where the ceilings were absent from the record entirely and a
        // resume was unrestricted with no tampering required at all. And the
        // pauses that actually rest on disk waiting for a human are elevations
        // for other reasons — an approval pause carries `approval_rules` — so the
        // records with the longest exposure are the covered ones.
        //
        // The alternative, verifying every resume, has nothing to verify against:
        // the authority entry the hash is checked against is written only for
        // elevations. Closing this needs that mechanism, not a wider predicate.
    }

    #[test]
    fn authorization_hash_covers_approved_confirmation_actions() {
        let base = AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        let mut tampered = base.clone();
        tampered.approved_confirmation_actions = vec![approval("{\"tool\":\"bash\"}")];

        assert_ne!(
            base.authorization_hash(),
            tampered.authorization_hash(),
            "injecting an approval must change the authorization hash, or a forged \
             on-disk pause could smuggle in a pre-approved action"
        );

        let mut different_action = base.clone();
        different_action.approved_confirmation_actions = vec![approval("{\"tool\":\"http\"}")];
        assert_ne!(
            tampered.authorization_hash(),
            different_action.authorization_hash(),
            "the hash must cover the approval payload, not merely its presence"
        );
    }

    #[test]
    fn an_approval_survives_the_disk_round_trip_still_gated_and_still_tamper_checked() {
        // The plan's fourth verification, at the level this can honestly reach.
        // It asked for "grant a one-time approval, pause, resume, assert the
        // action executes without re-asking". The half already covered was the
        // in-memory one: what `build_full_pause_state` writes and what
        // `restore_context_from_pause` refuses to grant. The half that was not
        // is the disk itself — a pause is serialized, fsynced, and read back by
        // a different call, and an approval that did not survive that would
        // re-ask the user a question they already answered.
        //
        // What this does NOT prove: that the restored approval actually
        // suppresses the confirmation at dispatch. That needs a live loop, and
        // is covered structurally instead — the approval is only handed to the
        // authority-gated resume path, which is asserted separately.
        let mut pause = AgenticPauseState::new(
            7,
            "goal",
            "criteria",
            EnvironmentState::Uninitialized,
            "summary",
            10,
            3,
        );
        pause.approved_confirmation_actions = vec![approval("{\"tool\":\"bash\"}")];
        let staged_hash = pause.authorization_hash();

        let encoded = serde_json::to_string(&pause).expect("serialize pause");
        let restored: AgenticPauseState =
            serde_json::from_str(&encoded).expect("a pause carrying an approval must read back");

        assert_eq!(
            restored.approved_confirmation_actions, pause.approved_confirmation_actions,
            "the approval must survive the disk round trip, or resume re-asks"
        );
        assert!(
            restored.is_elevation(),
            "it must still be an elevation after reading back, or the resume gate \
             stops applying to the very record that carries authority"
        );
        assert_eq!(
            restored.authorization_hash(),
            staged_hash,
            "the hash must be stable across the round trip, or a legitimately \
             staged pause fails its own authority check on resume"
        );

        // And the check still bites on the read-back record: a body edited on
        // disk after staging must not verify against the staged hash.
        let mut tampered = restored;
        tampered.approved_confirmation_actions = vec![approval("{\"tool\":\"http\"}")];
        assert_ne!(
            tampered.authorization_hash(),
            staged_hash,
            "an approval swapped on disk must break the authority check"
        );
    }

    #[test]
    fn legacy_pause_blob_loads_with_defaulted_loop_state() {
        // Records written before this change carry neither new field. They must
        // still load — degrading to the old reset-everything behaviour — rather
        // than failing closed and stranding a paused execution.
        let legacy = json!({
            "iteration": 4,
            "goal": "legacy goal",
            "success_criteria": "legacy criteria",
            "environment_state": { "type": "uninitialized" },
            "action_history_summary": "did some things",
            "paused_at": "2026-08-01T00:00:00Z",
            "max_iterations": 12,
            "max_repeated_actions": 5,
            "storage_base_path": "/tmp/legacy",
            "task_output_mode": "accumulate",
            "on_failure": "fail",
        });

        let decoded: AgenticPauseState =
            serde_json::from_value(legacy).expect("pre-migration blob must still deserialize");
        assert_eq!(decoded.iteration, 4);
        assert!(decoded.loop_protective_state.is_none());
        assert!(decoded.approved_confirmation_actions.is_empty());
        assert!(
            !decoded.is_elevation(),
            "a legacy blob with no authority must not become an elevation"
        );
    }
}
