//! # Plan Lowering: PlanGraph → ExecutableStep Translation
//!
//! This module provides deterministic translation of planner-produced `PlanGraph` steps
//! into executable actions via the capability registry.
//!
//! ## Dispatch
//! - **Registry path** (`lower_step_with_registry`): all tools route through
//!   `CapabilityRegistry` — the production dispatch path since Phase 1.
//! - **Direct path** (`lower_step_to_executable_action`): small built-in
//!   compatibility path used by tests and orphan callers.
//!
//! ## Supported tools
//! - **Browser** as an inner-loop `Pack` action. Browser steps are no longer
//!   lowered to Magicutor direct-action JSON.
//! - **Files**, **Shell**, **Search**, **HTTP** (compiled providers)
//! - **Pack** (YAML-defined composite / JavaScript capabilities)
//!
//! **Unsupported tools** return `ExecutionError::Step` with descriptive message.
//!
//! ## Test Coverage
//! Unit tests for this module are tracked in Phase 2 backlog (see PHASE1_STATUS.md).
//! Current validation relies on lowering tests plus execution-path integration tests.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use serde::Deserialize;

use crate::magician_v2::{
    execution::{
        actions::{BashAction, ExecutableAction, FileAction, HttpAction, HttpMethod},
        capability::ImplementationType,
        types::ExecutableStep,
        ExecutionError,
    },
    strategy::plan::{PlanGraph, PlanStep},
};

#[derive(Debug, Deserialize)]
struct SessionHint {
    #[serde(default)]
    session_id: Option<String>,
}

/// Lower a planner-produced graph into executable steps.
///
/// # Supported Tools
/// - **Browser tool** (`browser`): inner-loop pack execution
/// - **File tool** (`files`): Native tokio::fs execution
/// - **Search tool** (`search`): Native workspace search via `rg`
/// - **Shell tool** (`shell`): Native tokio::process execution
/// - **HTTP tools** (`http_*`): Native reqwest execution (internal)
///
/// # Errors
/// Returns `ExecutionError::Step` if:
/// - Step is missing required `tool` field
/// - Tool is not recognized (not in the supported set)
/// - Native/non-browser tool parameters are invalid
///
/// Browser lowering intentionally emits a pack action only. The browser
/// inner-loop runtime owns primitive browser decisions through the pinned
/// `agent-browser` CLI.
pub fn lower_plan_to_executable_steps(
    plan: &PlanGraph,
) -> Result<Vec<ExecutableStep>, ExecutionError> {
    let order = topological_sort(plan)?;
    let mut steps = Vec::with_capacity(order.len());
    for &idx in &order {
        let step = &plan.steps[idx];
        let action = lower_step_to_executable_action(step)?;

        let mut exec = ExecutableStep::new_bare(step.clone(), action);
        exec.session_id = extract_session_id(step);
        exec.timeout_secs = step.timeout_override_secs;
        steps.push(exec);
    }
    Ok(steps)
}

/// Lower a plan graph to executable steps via the capability registry.
///
/// All tool dispatch goes through the registry, supporting both compiled and
/// pack providers.
pub fn lower_plan_to_executable_steps_with_registry(
    plan: &PlanGraph,
    registry: &super::capability::CapabilityRegistry,
) -> Result<Vec<ExecutableStep>, ExecutionError> {
    let order = topological_sort(plan)?;
    let mut steps = Vec::with_capacity(order.len());
    for &idx in &order {
        let step = &plan.steps[idx];
        let action = lower_step_with_registry(step, registry)?;

        let mut exec = ExecutableStep::new(step.clone(), action);
        exec.session_id = extract_session_id(step);
        exec.timeout_secs = step.timeout_override_secs;
        steps.push(exec);
    }
    Ok(steps)
}

/// Topological sort of plan steps using Kahn's algorithm.
///
/// Returns step indices in dependency order so that every step appears after
/// all steps it `depends_on`.  When no step has `depends_on` set (old plans),
/// returns original array order as a zero-cost fast path.
///
/// # Errors
/// Returns `ExecutionError::CyclicDependency` if a dependency cycle is detected
/// (defensive — the planner's `check_no_cycles` should have caught it earlier).
pub fn topological_sort(plan: &PlanGraph) -> Result<Vec<usize>, ExecutionError> {
    let n = plan.steps.len();
    if n == 0 {
        return Ok(vec![]);
    }

    // Fast path: no dependencies → preserve original order
    if plan.steps.iter().all(|s| s.depends_on.is_empty()) {
        return Ok((0..n).collect());
    }

    let id_to_idx: HashMap<&str, usize> = plan
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();

    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

    for (i, step) in plan.steps.iter().enumerate() {
        for dep_id in &step.depends_on {
            if let Some(&j) = id_to_idx.get(dep_id.as_str()) {
                adj[j].push(i); // j must complete before i
                in_degree[i] += 1;
            }
            // Unknown dep IDs ignored (matches check_no_cycles behavior)
            // but warn so misspelled deps surface in logs
            else {
                tracing::warn!(
                    step_id = %step.id,
                    unknown_dep = %dep_id,
                    "[LOWERING] depends_on references unknown step id; treated as satisfied"
                );
            }
        }
    }

    let mut queue: VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();

    let mut sorted = Vec::with_capacity(n);
    while let Some(u) = queue.pop_front() {
        sorted.push(u);
        for &v in &adj[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    if sorted.len() < n {
        let cyclic: Vec<&str> = plan
            .steps
            .iter()
            .enumerate()
            .filter(|&(i, _)| in_degree[i] > 0)
            .map(|(_, s)| s.id.as_str())
            .collect();
        return Err(ExecutionError::CyclicDependency(format!(
            "cycle in depends_on involving steps: {cyclic:?}"
        )));
    }

    Ok(sorted)
}

fn extract_session_id(step: &PlanStep) -> Option<String> {
    step.metadata.get("session").and_then(|encoded| {
        serde_json::from_str::<SessionHint>(encoded)
            .ok()
            .and_then(|hint| hint.session_id)
    })
}

fn lower_browser_pack_action(step: &PlanStep) -> Result<ExecutableAction, ExecutionError> {
    let raw_tool = step.tool.as_deref().ok_or_else(|| {
        ExecutionError::Step(format!("Plan step '{}' missing tool name", step.id))
    })?;
    if raw_tool != "browser" {
        return Err(ExecutionError::Step(format!(
            "Unsupported browser tool '{}' for plan step '{}'. Expected tool 'browser'.",
            raw_tool, step.id
        )));
    }

    let mut resolved_params = step.parameters.clone();
    resolved_params
        .entry("intent".to_string())
        .or_insert_with(|| serde_json::Value::String(plan_step_browser_intent(step)));

    Ok(ExecutableAction::Pack {
        capability_name: "browser".to_string(),
        implementation: ImplementationType::Compiled {
            provider_name: "browser".to_string(),
        },
        resolved_params,
    })
}

fn plan_step_browser_intent(step: &PlanStep) -> String {
    let mut intent = if step.task.trim().is_empty() {
        format!("Execute browser plan step `{}`.", step.id)
    } else {
        step.task.trim().to_string()
    };

    if let Some(action) = step
        .parameters
        .get("action")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        intent.push_str(&format!(
            "\nRequested browser action hint: {}.",
            action.trim()
        ));
    }
    if let Some(url) = step
        .parameters
        .get("url")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        intent.push_str(&format!("\nTarget URL hint: {}.", url.trim()));
    }

    intent
}

fn required<'a>(
    params: &'a HashMap<String, serde_json::Value>,
    key: &str,
    tool: &str,
    step_id: &str,
) -> Result<&'a str, ExecutionError> {
    params.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        ExecutionError::Step(format!(
            "Plan step '{}' missing '{}' parameter for tool {}",
            step_id, key, tool
        ))
    })
}

// ============================================================================
// Placeholder Helpers
// ============================================================================
// Placeholders are used for native action parameters.

const PLACEHOLDER_PREFIX: &str = "__RESOLVE::";
const PLACEHOLDER_SUFFIX: &str = "::__";

/// Check if a selector is a placeholder that needs runtime resolution.
pub fn is_placeholder_selector(selector: &str) -> bool {
    selector.starts_with(PLACEHOLDER_PREFIX) && selector.ends_with(PLACEHOLDER_SUFFIX)
}

/// Extract the intent description from a placeholder selector.
pub fn extract_placeholder_intent(selector: &str) -> Option<&str> {
    if is_placeholder_selector(selector) {
        Some(&selector[PLACEHOLDER_PREFIX.len()..selector.len() - PLACEHOLDER_SUFFIX.len()])
    } else {
        None
    }
}

// ============================================================================
// Native Action Placeholder System
// ============================================================================
// For native actions (File, HTTP, Shell), we use placeholders when required
// parameters are missing. The executor will resolve these at runtime by:
// - Deriving from context (previous step outputs, task description)
// - Using LLM to infer values
// - Eliciting from user if truly ambiguous

/// Returns parameter value or a placeholder with intent for native actions.
/// Used when a parameter is "required" but we want tolerant lowering.
fn param_or_placeholder(
    params: &HashMap<String, serde_json::Value>,
    key: &str,
    step: &PlanStep,
) -> String {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Build intent from task description and parameter name
            let intent = if step.task.is_empty() {
                format!("{}:{}", step.id, key)
            } else {
                format!("{}:{}", step.task, key)
            };
            format!("{}{}{}", PLACEHOLDER_PREFIX, intent, PLACEHOLDER_SUFFIX)
        })
}

/// Check if a path/parameter value is a placeholder needing runtime resolution.
pub fn is_placeholder_param(value: &str) -> bool {
    value.starts_with(PLACEHOLDER_PREFIX) && value.ends_with(PLACEHOLDER_SUFFIX)
}

/// Extract the intent description from a placeholder parameter.
pub fn extract_placeholder_param_intent(value: &str) -> Option<&str> {
    if is_placeholder_param(value) {
        Some(&value[PLACEHOLDER_PREFIX.len()..value.len() - PLACEHOLDER_SUFFIX.len()])
    } else {
        None
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_lowercase().as_str() {
        "true" | "1" | "yes" | "y" => Ok(true),
        "false" | "0" | "no" | "n" => Ok(false),
        other => Err(format!("Invalid boolean value '{}'", other)),
    }
}

fn parse_u64(value: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("Invalid integer '{}'", value))
}

fn parse_u32(value: &str) -> Result<u32, String> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("Invalid integer '{}'", value))
}

// ============================================================================
// JIT (Just-In-Time) Multi-Action Lowering
// ============================================================================
// This section provides JIT lowering that supports multiple action types:
// - Browser inner-loop pack actions
// - File actions (via tokio::fs)
// - HTTP actions (via reqwest)
// - Bash actions (via tokio::process)
//
// Preferred tool surface:
// - browser(...) → Pack(browser) inner-loop
// - files(action=...) → File
// - search(query=...) → Bash (rg-based)
// - shell(command=...) → Bash
//
/// Lower a single plan step to an ExecutableAction using only built-in tool types.
///
/// Convenience function for tests and paths that don't have a `CapabilityRegistry`.
/// Production code should use [`lower_step_with_registry`] instead, which dispatches
/// through the registry (including both compiled and pack providers).
///
/// # Supported Tool Names
/// - `browser` → Browser inner-loop pack action
/// - `files` → File action (native, fast)
/// - `search` → Workspace search action (native shell via `rg`)
/// - `http_*` → HTTP action (native, fast)
/// - `shell` → Shell action
pub fn lower_step_to_executable_action(
    step: &PlanStep,
) -> Result<ExecutableAction, ExecutionError> {
    let tool = step.tool.as_deref().ok_or_else(|| {
        ExecutionError::Step(format!("Plan step '{}' missing tool name", step.id))
    })?;

    if tool == "browser" {
        lower_browser_pack_action(step)
    } else if tool == "files" {
        lower_file_action(step)
    } else if tool == "search" {
        lower_search_action(step)
    } else if tool == "http" || tool.starts_with("http_") {
        lower_http_action(step, tool)
    } else if tool == "shell" {
        lower_bash_action(step)
    } else {
        Err(ExecutionError::Step(format!(
            "Unsupported tool '{}' for plan step '{}'. Expected one of: browser, files, search, shell, or http_*.",
            tool, step.id
        )))
    }
}

/// Lower a plan step via the capability registry.
///
/// All tool dispatch goes through the registry. Browser plans should only reach
/// this path as `implementation.type: primitive`; the runtime should not
/// register a compiled browser provider.
pub fn lower_step_with_registry(
    step: &PlanStep,
    registry: &super::capability::CapabilityRegistry,
) -> Result<crate::magician_v2::resource_authority::gated_action::MaybeGatedAction, ExecutionError>
{
    let tool = step.tool.as_deref().ok_or_else(|| {
        ExecutionError::Step(format!("Plan step '{}' missing tool name", step.id))
    })?;

    if let Some(provider) = registry.get(tool) {
        provider.lower(step)
    } else {
        Err(ExecutionError::Step(format!(
            "No provider registered for tool '{}' in plan step '{}'.",
            tool, step.id
        )))
    }
}

/// Lower a file operation tool to FileAction
///
/// Uses tolerant lowering: missing parameters create placeholders that the
/// executor will resolve at runtime (from context, LLM, or user elicitation).
pub(super) fn lower_file_action(step: &PlanStep) -> Result<ExecutableAction, ExecutionError> {
    let params = &step.parameters;
    let tool = step.tool.as_deref().unwrap_or_default();
    let canonical_tool = canonical_file_tool(tool, params, &step.id)?;
    let tool = canonical_tool.as_str();

    let action = match tool {
        "file_read" => {
            let path = param_or_placeholder(params, "path", step);
            FileAction::Read {
                path: PathBuf::from(path),
                encoding: params
                    .get("encoding")
                    .and_then(|v| v.as_str().map(|s| s.to_string())),
            }
        },
        "file_write" => {
            let path = param_or_placeholder(params, "path", step);
            let content = param_or_placeholder(params, "content", step);
            let create_dirs = params
                .get("create_dirs")
                .and_then(|v| v.as_str())
                .map(parse_bool)
                .transpose()
                .map_err(ExecutionError::Step)?
                .unwrap_or(true);
            FileAction::Write {
                path: PathBuf::from(path),
                content,
                create_dirs,
            }
        },
        "file_append" => {
            let path = param_or_placeholder(params, "path", step);
            let content = param_or_placeholder(params, "content", step);
            FileAction::Append {
                path: PathBuf::from(path),
                content,
            }
        },
        "file_delete" => {
            let path = param_or_placeholder(params, "path", step);
            let recursive = params
                .get("recursive")
                .and_then(|v| v.as_str())
                .map(parse_bool)
                .transpose()
                .map_err(ExecutionError::Step)?
                .unwrap_or(false);
            FileAction::Delete {
                path: PathBuf::from(path),
                recursive,
            }
        },
        "file_copy" => {
            let source = param_or_placeholder(params, "source", step);
            let destination = param_or_placeholder(params, "destination", step);
            FileAction::Copy {
                source: PathBuf::from(source),
                destination: PathBuf::from(destination),
            }
        },
        "file_move" => {
            let source = param_or_placeholder(params, "source", step);
            let destination = param_or_placeholder(params, "destination", step);
            FileAction::Move {
                source: PathBuf::from(source),
                destination: PathBuf::from(destination),
            }
        },
        "file_exists" => {
            let path = param_or_placeholder(params, "path", step);
            FileAction::Exists {
                path: PathBuf::from(path),
            }
        },
        "file_list" => {
            let path = param_or_placeholder(params, "path", step);
            FileAction::List {
                path: PathBuf::from(path),
                pattern: params
                    .get("pattern")
                    .and_then(|v| v.as_str().map(|s| s.to_string())),
            }
        },
        "file_create_dir" => {
            let path = param_or_placeholder(params, "path", step);
            FileAction::CreateDir {
                path: PathBuf::from(path),
            }
        },
        _ => {
            return Err(ExecutionError::Step(format!(
                "Unsupported file tool '{}' for step '{}'",
                tool, step.id
            )));
        },
    };

    Ok(ExecutableAction::File(action))
}

/// Lower an HTTP tool to HttpAction
///
/// Uses tolerant lowering: missing URL creates a placeholder that the
/// executor will resolve at runtime (from context, LLM, or user elicitation).
/// One header per entry: a string value as written, any other scalar in its
/// JSON spelling, and `null` as no header at all. Never all-or-nothing.
fn header_map_from_object(
    map: &serde_json::Map<String, serde_json::Value>,
) -> HashMap<String, String> {
    map.iter()
        .filter_map(|(name, value)| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| match value {
                    serde_json::Value::Null => None,
                    other => Some(other.to_string()),
                })
                .map(|value| (name.clone(), value))
        })
        .collect()
}

pub(super) fn lower_http_action(
    step: &PlanStep,
    tool: &str,
) -> Result<ExecutableAction, ExecutionError> {
    let params = &step.parameters;

    let method = match tool {
        "http_get" => HttpMethod::Get,
        "http" | "http_request" => {
            let method_str = params
                .get("method")
                .and_then(|s| s.as_str())
                .unwrap_or("GET");
            match method_str.to_uppercase().as_str() {
                "GET" => HttpMethod::Get,
                "POST" => HttpMethod::Post,
                "PUT" => HttpMethod::Put,
                "PATCH" => HttpMethod::Patch,
                "DELETE" => HttpMethod::Delete,
                "HEAD" => HttpMethod::Head,
                "OPTIONS" => HttpMethod::Options,
                _ => {
                    return Err(ExecutionError::Step(format!(
                        "Unsupported HTTP method '{}' for step '{}'",
                        method_str, step.id
                    )));
                },
            }
        },
        "http_post" => HttpMethod::Post,
        "http_put" => HttpMethod::Put,
        "http_patch" => HttpMethod::Patch,
        "http_delete" => HttpMethod::Delete,
        "http_head" => HttpMethod::Head,
        "http_options" => HttpMethod::Options,
        _ => {
            return Err(ExecutionError::Step(format!(
                "Unsupported HTTP tool '{}' for step '{}'",
                tool, step.id
            )));
        },
    };

    let url = param_or_placeholder(params, "url", step);

    // The request's headers, in either shape they arrive in: the JSON string
    // the pack's own parameter coercion produces, or the object a model (or a
    // plan step) writes directly. Reading only the string shape dropped an
    // object's headers silently, which made a pre-dispatch buildability check
    // validate a different request from the one that would be sent; parsing the
    // string straight into a string→string map dropped the WHOLE map when any
    // one value was a number, which could silently drop the header carrying a
    // credential after the code had been spent. Both shapes now go through the
    // same per-header coercion, so one odd value costs only its own header.
    let headers = params
        .get("headers")
        .map(|value| match value {
            serde_json::Value::String(text) => serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .map(header_map_from_object)
                .unwrap_or_default(),
            serde_json::Value::Object(map) => header_map_from_object(map),
            _ => HashMap::new(),
        })
        .unwrap_or_default();

    let timeout_secs = params
        .get("timeout")
        .or_else(|| params.get("timeout_secs"))
        .map(|value| {
            value
                .as_u64()
                .map(Ok)
                .or_else(|| value.as_str().map(parse_u64))
                .unwrap_or_else(|| Err("HTTP timeout must be a positive integer".to_owned()))
        })
        .transpose()
        .map_err(ExecutionError::Step)?;

    let follow_redirects = params
        .get("follow_redirects")
        .map(|value| {
            value
                .as_bool()
                .map(Ok)
                .or_else(|| value.as_str().map(parse_bool))
                .unwrap_or_else(|| Err("HTTP follow_redirects must be a boolean".to_owned()))
        })
        .transpose()
        .map_err(ExecutionError::Step)?
        .unwrap_or(true);

    // Set by the secret-sink lowering when a reference was delivered into
    // this request (secure HITL P4/P7): the adapter then never follows a
    // redirect off the origin. A caller setting it by hand only makes its
    // own request stricter.
    let carries_credential = params
        .get(crate::magician_v2::secrets::sinks::HTTP_PACK_CARRIES_CREDENTIAL)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let action = HttpAction {
        method,
        url, // Already a String from param_or_placeholder
        headers,
        body: params
            .get("body")
            .and_then(|v| v.as_str().map(|s| s.to_string())),
        content_type: params
            .get("content_type")
            .and_then(|v| v.as_str().map(|s| s.to_string())),
        timeout_secs,
        follow_redirects,
        carries_credential,
    };

    Ok(ExecutableAction::Http(action))
}

/// Lower a workspace search tool to a native BashAction (rg-based).
pub(super) fn lower_search_action(step: &PlanStep) -> Result<ExecutableAction, ExecutionError> {
    let params = &step.parameters;
    let query = required(params, "query", "search", &step.id)?;
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .unwrap_or(".")
        .to_string();
    let file_glob = params
        .get("file_glob")
        .or_else(|| params.get("glob"))
        .and_then(|v| v.as_str())
        .filter(|g| !g.is_empty())
        .map(|s| s.to_string());

    let target = params
        .get("target")
        .and_then(|t| t.as_str())
        .map(|t| t.to_lowercase())
        .unwrap_or_else(|| "workspace".to_string());
    if target != "workspace" {
        return Err(ExecutionError::Step(format!(
            "Plan step '{}' uses unsupported search target '{}'. Only 'workspace' is supported.",
            step.id, target
        )));
    }

    let max_results = params
        .get("max_results")
        .and_then(|v| v.as_str())
        .map(parse_u32)
        .transpose()
        .map_err(ExecutionError::Step)?
        .unwrap_or(50)
        .max(1);
    let case_sensitive = params
        .get("case_sensitive")
        .and_then(|v| v.as_str())
        .map(parse_bool)
        .transpose()
        .map_err(ExecutionError::Step)?
        .unwrap_or(false);

    let mut command = format!("rg --line-number --max-count {}", max_results);
    if !case_sensitive {
        command.push_str(" --ignore-case");
    }
    if let Some(glob) = file_glob {
        command.push_str(" --glob ");
        command.push_str(&shell_escape(&glob));
    }
    command.push(' ');
    command.push_str(&shell_escape(query));
    command.push(' ');
    command.push_str(&shell_escape(&path));

    let timeout_secs = params
        .get("timeout")
        .or_else(|| params.get("timeout_secs"))
        .and_then(|v| v.as_str())
        .map(parse_u64)
        .transpose()
        .map_err(ExecutionError::Step)?;

    Ok(ExecutableAction::Bash(BashAction {
        command,
        working_dir: params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from),
        env: HashMap::new(),
        timeout_secs,
        capture_output: true,
        stdin: params
            .get("stdin")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }))
}

/// Lower a bash/shell tool to BashAction
///
/// Uses tolerant lowering: missing command creates a placeholder that the
/// executor will resolve at runtime (from context, LLM, or user elicitation).
pub(super) fn lower_bash_action(step: &PlanStep) -> Result<ExecutableAction, ExecutionError> {
    let params = &step.parameters;

    let command = param_or_placeholder(params, "command", step);

    let working_dir = params
        .get("working_dir")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    // Parse env from JSON string
    let env = params
        .get("env")
        .and_then(|e| e.as_str())
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(s).ok())
        .unwrap_or_default();

    let timeout_secs = params
        .get("timeout")
        .or_else(|| params.get("timeout_secs"))
        .and_then(|v| v.as_str())
        .map(parse_u64)
        .transpose()
        .map_err(ExecutionError::Step)?;

    let capture_output = params
        .get("capture_output")
        .and_then(|v| v.as_str())
        .map(parse_bool)
        .transpose()
        .map_err(ExecutionError::Step)?
        .unwrap_or(true);

    let stdin = params
        .get("stdin")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let action = BashAction {
        command, // Already a String from param_or_placeholder
        working_dir,
        env,
        timeout_secs,
        capture_output,
        stdin,
    };

    Ok(ExecutableAction::Bash(action))
}

/// Build the SQL for the `duckdb.preview` inner action. Pure: no I/O, no
/// state. Escapes single quotes in `source` per DuckDB convention (doubles
/// them) and clamps non-positive `limit` to 1 so agents don't accidentally
/// produce `LIMIT 0`.
pub(super) fn build_duckdb_preview_sql(source: &str, limit: i64) -> String {
    let escaped = source.replace('\'', "''");
    let clamped = if limit < 1 { 1 } else { limit };
    format!("SELECT * FROM '{escaped}' LIMIT {clamped}")
}

/// Build the SQL for `duckdb.describe` against a file path. Wraps the path
/// in single quotes (with DuckDB-style `''` escaping).
pub(super) fn build_duckdb_describe_sql(source: &str) -> String {
    let escaped = source.replace('\'', "''");
    format!("DESCRIBE SELECT * FROM '{escaped}'")
}

/// Build the SQL for `duckdb.list_tables` — a fixed `SHOW TABLES` returning
/// tables and views in the current session (or in the attached database when
/// the `database` parameter is set).
pub(super) fn build_duckdb_list_tables_sql() -> &'static str {
    "SHOW TABLES"
}

/// Build the SQL for `duckdb.describe` against a table identifier. Returns an
/// error if the identifier contains anything other than `[A-Za-z0-9_]` so an
/// agent can't smuggle SQL through the table-name path.
pub(super) fn build_duckdb_describe_table_sql(table: &str) -> Result<String, ExecutionError> {
    if table.is_empty() {
        return Err(ExecutionError::Step(
            "duckdb.describe: empty `source` for table mode".to_string(),
        ));
    }
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ExecutionError::Step(format!(
            "duckdb.describe: table identifier `{table}` must be alphanumeric/underscore only. \
             For quoted identifiers, use the `query` action with raw SQL."
        )));
    }
    Ok(format!("DESCRIBE {table}"))
}

/// Build the SQL for `duckdb.read_parquet`. Composes a SELECT against
/// `read_parquet(path, hive_partitioning = ...)`, optionally adding WHERE
/// and LIMIT clauses. Single quotes in `path` are escaped per DuckDB
/// convention.
pub(super) fn build_duckdb_read_parquet_sql(
    path: &str,
    where_clause: Option<&str>,
    select: &str,
    limit: Option<i64>,
    hive_partitioning: bool,
) -> String {
    let escaped = path.replace('\'', "''");
    let hive = if hive_partitioning { "true" } else { "false" };
    let mut sql =
        format!("SELECT {select} FROM read_parquet('{escaped}', hive_partitioning = {hive})");
    if let Some(w) = where_clause {
        sql.push_str(&format!(" WHERE {w}"));
    }
    if let Some(n) = limit {
        sql.push_str(&format!(" LIMIT {n}"));
    }
    sql
}

/// Build the SQL for `duckdb.export`. Wraps the given SELECT in a `COPY ...
/// TO ... (FORMAT ..., HEADER ...)`. Whitelists csv / parquet / json (case
/// insensitive); anything else returns an `ExecutionError` instead of being
/// passed through to DuckDB's parser (where the failure mode is a confusing
/// syntax error far from the inner-loop's view).
pub(super) fn try_build_duckdb_export_sql(
    select_sql: &str,
    output_path: &str,
    format: &str,
    header: bool,
) -> Result<String, ExecutionError> {
    let format = format.to_lowercase();
    if !matches!(format.as_str(), "csv" | "parquet" | "json") {
        return Err(ExecutionError::Step(format!(
            "duckdb.export: unsupported format `{format}`. Use csv, parquet, or json."
        )));
    }
    let escaped_path = output_path.replace('\'', "''");
    let mut options = format!("FORMAT '{format}'");
    if format == "csv" && header {
        options.push_str(", HEADER true");
    }
    Ok(format!(
        "COPY ({select_sql}) TO '{escaped_path}' ({options})"
    ))
}

/// Build the SQL for `duckdb.attach`. Validates that the alias is a SQL
/// identifier (`[A-Za-z0-9_]`) so agents can't smuggle SQL through it. Empty
/// path is also rejected. Single quotes in the path are escaped per DuckDB
/// convention. When `read_only` is true, appends `(READ_ONLY)`.
pub(super) fn try_build_duckdb_attach_sql(
    database_path: &str,
    alias: &str,
    read_only: bool,
) -> Result<String, ExecutionError> {
    if database_path.is_empty() {
        return Err(ExecutionError::Step(
            "duckdb.attach: empty `attach_path` parameter".to_string(),
        ));
    }
    if alias.is_empty() {
        return Err(ExecutionError::Step(
            "duckdb.attach: empty `alias` parameter".to_string(),
        ));
    }
    if !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ExecutionError::Step(format!(
            "duckdb.attach: alias `{alias}` must be alphanumeric/underscore only."
        )));
    }
    let escaped_path = database_path.replace('\'', "''");
    if read_only {
        Ok(format!("ATTACH '{escaped_path}' AS {alias} (READ_ONLY)"))
    } else {
        Ok(format!("ATTACH '{escaped_path}' AS {alias}"))
    }
}

/// Expand `llm_calls@<principal>/<workspace>` shorthand to the full LLM-call
/// lakehouse Parquet glob. Non-matching paths pass through unchanged.
pub(super) fn expand_llm_calls_path(path: &str) -> String {
    if let Some(scope) = path.strip_prefix("llm_calls@") {
        format!("magician_data_v3/scopes/{scope}/analytics/llm_calls/dt=*/*.parquet")
    } else {
        path.to_string()
    }
}

/// Lower a DuckDB tool to DuckDbAction
pub(super) fn lower_duckdb_action(step: &PlanStep) -> Result<ExecutableAction, ExecutionError> {
    let params = &step.parameters;

    // Common parameters (shared across all inner actions).
    let database = params
        .get("database")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let output_format = params
        .get("output_format")
        .and_then(|v| v.as_str())
        .unwrap_or("json")
        .to_string();

    let timeout_secs = params.get("timeout_secs").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });

    // Determine the action name. Defaults to "query" for backward
    // compatibility — direct-dispatch callers (outside the inner loop) won't
    // have the synthetic __action_name parameter injected.
    let action_name = params
        .get("__action_name")
        .and_then(|v| v.as_str())
        .unwrap_or("query");

    let sql = match action_name {
        "query" => param_or_placeholder(params, "sql", step),
        "preview" => {
            let source = params
                .get("source")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ExecutionError::Step("duckdb.preview requires a `source` parameter".to_string())
                })?;
            let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            build_duckdb_preview_sql(source, limit)
        },
        "describe" => {
            let source = params
                .get("source")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ExecutionError::Step(
                        "duckdb.describe requires a `source` parameter".to_string(),
                    )
                })?;
            let is_table = params
                .get("is_table")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if is_table {
                build_duckdb_describe_table_sql(source)?
            } else {
                build_duckdb_describe_sql(source)
            }
        },
        "list_tables" => build_duckdb_list_tables_sql().to_string(),
        "read_parquet" => {
            let raw_path = params.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
                ExecutionError::Step("duckdb.read_parquet requires a `path` parameter".to_string())
            })?;
            let expanded = expand_llm_calls_path(raw_path);
            let where_clause = params
                .get("where_clause")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let select = params.get("select").and_then(|v| v.as_str()).unwrap_or("*");
            let limit = params.get("limit").and_then(|v| v.as_i64());
            let hive_partitioning = params
                .get("hive_partitioning")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            build_duckdb_read_parquet_sql(&expanded, where_clause, select, limit, hive_partitioning)
        },
        "export" => {
            let select_sql = params.get("sql").and_then(|v| v.as_str()).ok_or_else(|| {
                ExecutionError::Step(
                    "duckdb.export requires a `sql` parameter (the SELECT to export)".to_string(),
                )
            })?;
            let output_path = params
                .get("output_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ExecutionError::Step(
                        "duckdb.export requires an `output_path` parameter".to_string(),
                    )
                })?;
            let format = params
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("csv");
            let header = params
                .get("header")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            try_build_duckdb_export_sql(select_sql, output_path, format, header)?
        },
        "attach" => {
            let attach_path = params
                .get("attach_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ExecutionError::Step(
                        "duckdb.attach requires an `attach_path` parameter (path to the .duckdb file)"
                            .to_string(),
                    )
                })?;
            let alias = params
                .get("alias")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ExecutionError::Step("duckdb.attach requires an `alias` parameter".to_string())
                })?;
            let read_only = params
                .get("read_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            try_build_duckdb_attach_sql(attach_path, alias, read_only)?
        },
        other => {
            return Err(ExecutionError::Step(format!(
                "unknown duckdb inner action `{other}`. Known actions: query, preview, describe, list_tables, read_parquet, export, attach."
            )));
        },
    };

    let action = super::actions::DuckDbAction {
        sql,
        database,
        output_format,
        timeout_secs,
    };

    Ok(ExecutableAction::DuckDb(action))
}

fn canonical_file_tool(
    tool: &str,
    params: &HashMap<String, serde_json::Value>,
    step_id: &str,
) -> Result<String, ExecutionError> {
    if tool != "files" {
        return Err(ExecutionError::Step(format!(
            "Plan step '{}' uses unsupported files tool '{}'",
            step_id, tool
        )));
    }

    let action = required(params, "action", tool, step_id)?;
    let normalized = normalize_action_name(action);
    let mapped = match normalized.as_str() {
        "read" => "file_read",
        "write" => "file_write",
        "append" => "file_append",
        "delete" | "remove" => "file_delete",
        "copy" => "file_copy",
        "move" | "rename" => "file_move",
        "exists" => "file_exists",
        "list" => "file_list",
        "mkdir" | "create_dir" | "create_directory" => "file_create_dir",
        other => {
            return Err(ExecutionError::Step(format!(
                "Plan step '{}' has unsupported files action '{}'",
                step_id, other
            )));
        },
    };
    Ok(mapped.to_string())
}

fn normalize_action_name(value: &str) -> String {
    value.trim().to_lowercase().replace('-', "_")
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{}'", escaped)
}

/// Check if a tool name is a browser tool
pub fn is_browser_tool(tool: &str) -> bool {
    tool == "browser"
}

/// Check if a tool name is a file tool
pub fn is_file_tool(tool: &str) -> bool {
    tool == "files"
}

/// Check if a tool name is an HTTP tool
pub fn is_http_tool(tool: &str) -> bool {
    tool == "http" || tool.starts_with("http_")
}

/// Check if a tool name is a shell tool
pub fn is_shell_tool(tool: &str) -> bool {
    tool == "shell"
}

#[cfg(any(test, feature = "test-fixtures"))]
mod duckdb_action_tests {
    use super::*;

    #[test]
    fn bound_http_lowering_preserves_typed_timeout_and_redirect_policy() {
        let step = PlanStep {
            id: "app-http-get".to_owned(),
            tool: Some("http_get".to_owned()),
            parameters: HashMap::from([
                (
                    "url".to_owned(),
                    serde_json::Value::String("https://example.com/data".to_owned()),
                ),
                ("timeout_secs".to_owned(), serde_json::json!(17)),
                ("follow_redirects".to_owned(), serde_json::json!(false)),
            ]),
            ..Default::default()
        };
        let ExecutableAction::Http(action) = lower_http_action(&step, "http_get").unwrap() else {
            panic!("expected HTTP action");
        };
        assert_eq!(action.timeout_secs, Some(17));
        assert!(!action.follow_redirects);
    }

    #[test]
    fn one_odd_header_value_never_drops_the_rest_of_the_map() {
        // The credential rides a header. A sibling header whose value is a
        // number must not take it down with it — parsing the whole string into
        // a string→string map did exactly that, silently, after the one-time
        // code had already been reserved.
        for headers in [
            serde_json::json!({"Authorization": "Basic secret-canary", "X-Retry": 3}),
            serde_json::Value::String(
                serde_json::json!({"Authorization": "Basic secret-canary", "X-Retry": 3})
                    .to_string(),
            ),
        ] {
            let step = PlanStep {
                id: "post".to_owned(),
                tool: Some("http_post".to_owned()),
                parameters: HashMap::from([
                    (
                        "url".to_owned(),
                        serde_json::Value::String("https://example.com/login".to_owned()),
                    ),
                    ("headers".to_owned(), headers.clone()),
                ]),
                ..Default::default()
            };
            let ExecutableAction::Http(action) = lower_http_action(&step, "http_post").unwrap()
            else {
                panic!("expected HTTP action");
            };
            assert_eq!(
                action.headers.get("Authorization").map(String::as_str),
                Some("Basic secret-canary"),
                "{headers}"
            );
            assert_eq!(
                action.headers.get("X-Retry").map(String::as_str),
                Some("3"),
                "{headers}"
            );
        }
    }

    #[test]
    fn duckdb_preview_action_builds_select_limit() {
        let sql = build_duckdb_preview_sql("data/sales.csv", 10);
        assert_eq!(sql, "SELECT * FROM 'data/sales.csv' LIMIT 10");
    }

    #[test]
    fn duckdb_preview_action_clamps_zero_and_negative_limit() {
        // Agents sometimes pass `0` or `-1`. Clamp to 1 rather than producing
        // `LIMIT 0` (which silently returns no rows and confuses follow-up).
        assert_eq!(
            build_duckdb_preview_sql("data/sales.csv", 0),
            "SELECT * FROM 'data/sales.csv' LIMIT 1"
        );
        assert_eq!(
            build_duckdb_preview_sql("data/sales.csv", -5),
            "SELECT * FROM 'data/sales.csv' LIMIT 1"
        );
    }

    #[test]
    fn duckdb_preview_action_escapes_single_quotes_in_source() {
        let sql = build_duckdb_preview_sql("o'reilly.csv", 5);
        assert_eq!(sql, "SELECT * FROM 'o''reilly.csv' LIMIT 5");
    }

    #[test]
    fn duckdb_describe_action_builds_describe_select_for_file() {
        let sql = build_duckdb_describe_sql("data/sales.csv");
        assert_eq!(sql, "DESCRIBE SELECT * FROM 'data/sales.csv'");
    }

    #[test]
    fn duckdb_describe_action_escapes_single_quotes_in_path() {
        let sql = build_duckdb_describe_sql("o'reilly.csv");
        assert_eq!(sql, "DESCRIBE SELECT * FROM 'o''reilly.csv'");
    }

    #[test]
    fn duckdb_describe_action_builds_describe_for_table() {
        // Plain table identifiers are NOT wrapped in quotes — describe a real table.
        let sql = build_duckdb_describe_table_sql("my_table").expect("valid table name");
        assert_eq!(sql, "DESCRIBE my_table");
    }

    #[test]
    fn duckdb_describe_action_rejects_table_name_with_unsafe_chars() {
        // Reject identifiers that aren't [A-Za-z0-9_] — agents that want a quoted
        // identifier should call `query` directly.
        assert!(build_duckdb_describe_table_sql("bad name").is_err());
        assert!(build_duckdb_describe_table_sql("table;DROP").is_err());
        assert!(build_duckdb_describe_table_sql("").is_err());
    }

    #[test]
    fn duckdb_list_tables_builds_show_tables() {
        assert_eq!(build_duckdb_list_tables_sql(), "SHOW TABLES");
    }

    #[test]
    fn duckdb_read_parquet_builds_basic_select() {
        let sql = build_duckdb_read_parquet_sql("data/2026/*.parquet", None, "*", None, true);
        assert_eq!(
            sql,
            "SELECT * FROM read_parquet('data/2026/*.parquet', hive_partitioning = true)"
        );
    }

    #[test]
    fn duckdb_read_parquet_applies_where_and_limit() {
        let sql = build_duckdb_read_parquet_sql(
            "logs/dt=*/*.parquet",
            Some("level = 'error'"),
            "ts, message",
            Some(100),
            true,
        );
        assert_eq!(
            sql,
            "SELECT ts, message FROM read_parquet('logs/dt=*/*.parquet', hive_partitioning = true) \
             WHERE level = 'error' LIMIT 100"
        );
    }

    #[test]
    fn duckdb_read_parquet_disables_hive_partitioning_when_requested() {
        let sql = build_duckdb_read_parquet_sql("flat/file.parquet", None, "*", None, false);
        assert_eq!(
            sql,
            "SELECT * FROM read_parquet('flat/file.parquet', hive_partitioning = false)"
        );
    }

    #[test]
    fn duckdb_read_parquet_escapes_single_quotes_in_path() {
        let sql = build_duckdb_read_parquet_sql("o'reilly/file.parquet", None, "*", None, true);
        assert_eq!(
            sql,
            "SELECT * FROM read_parquet('o''reilly/file.parquet', hive_partitioning = true)"
        );
    }

    #[test]
    fn duckdb_expand_llm_calls_shorthand_to_full_glob() {
        let path = expand_llm_calls_path("llm_calls@anonymous/default");
        assert_eq!(
            path,
            "magician_data_v3/scopes/anonymous/default/analytics/llm_calls/dt=*/*.parquet"
        );
    }

    #[test]
    fn duckdb_expand_llm_calls_passthrough_for_normal_paths() {
        assert_eq!(
            expand_llm_calls_path("data/raw/*.parquet"),
            "data/raw/*.parquet"
        );
    }

    #[test]
    fn duckdb_expand_llm_calls_handles_scope_with_multiple_workspaces() {
        let path = expand_llm_calls_path("llm_calls@principal_a/workspace_b");
        assert_eq!(
            path,
            "magician_data_v3/scopes/principal_a/workspace_b/analytics/llm_calls/dt=*/*.parquet"
        );
    }

    #[test]
    fn duckdb_export_builds_copy_csv_with_header() {
        let sql = try_build_duckdb_export_sql("SELECT * FROM cleaned", "/tmp/out.csv", "csv", true)
            .expect("csv supported");
        assert_eq!(
            sql,
            "COPY (SELECT * FROM cleaned) TO '/tmp/out.csv' (FORMAT 'csv', HEADER true)"
        );
    }

    #[test]
    fn duckdb_export_builds_copy_csv_without_header() {
        let sql =
            try_build_duckdb_export_sql("SELECT * FROM cleaned", "/tmp/out.csv", "csv", false)
                .expect("csv supported");
        assert_eq!(
            sql,
            "COPY (SELECT * FROM cleaned) TO '/tmp/out.csv' (FORMAT 'csv')"
        );
    }

    #[test]
    fn duckdb_export_builds_copy_parquet_ignoring_header_flag() {
        // Parquet doesn't use HEADER — confirm we don't emit it even when header=true.
        let sql = try_build_duckdb_export_sql(
            "SELECT * FROM cleaned",
            "/tmp/out.parquet",
            "parquet",
            true,
        )
        .expect("parquet supported");
        assert_eq!(
            sql,
            "COPY (SELECT * FROM cleaned) TO '/tmp/out.parquet' (FORMAT 'parquet')"
        );
    }

    #[test]
    fn duckdb_export_escapes_single_quotes_in_output_path() {
        let sql = try_build_duckdb_export_sql("SELECT 1", "/tmp/o'reilly.csv", "csv", false)
            .expect("csv supported");
        assert_eq!(
            sql,
            "COPY (SELECT 1) TO '/tmp/o''reilly.csv' (FORMAT 'csv')"
        );
    }

    #[test]
    fn duckdb_export_rejects_unsupported_format() {
        // Only csv / parquet / json are accepted — anything else gets a typed
        // error at build time instead of confusing DuckDB syntax error later.
        let err = try_build_duckdb_export_sql("SELECT 1", "/tmp/x.weird", "xml", true)
            .expect_err("xml not supported");
        assert!(format!("{:?}", err).contains("unsupported format"));
    }

    #[test]
    fn duckdb_export_normalises_format_case() {
        // CSV / Parquet / JSON (any case) should be accepted and lowercased.
        let sql = try_build_duckdb_export_sql("SELECT 1", "/tmp/out.parquet", "PARQUET", false)
            .expect("parquet supported case-insensitively");
        assert!(sql.contains("FORMAT 'parquet'"));
    }

    #[test]
    fn duckdb_attach_builds_attach_readonly() {
        let sql = try_build_duckdb_attach_sql("/tmp/finance.duckdb", "finance", true)
            .expect("valid alias");
        assert_eq!(sql, "ATTACH '/tmp/finance.duckdb' AS finance (READ_ONLY)");
    }

    #[test]
    fn duckdb_attach_builds_attach_readwrite() {
        let sql = try_build_duckdb_attach_sql("/tmp/finance.duckdb", "finance", false)
            .expect("valid alias");
        assert_eq!(sql, "ATTACH '/tmp/finance.duckdb' AS finance");
    }

    #[test]
    fn duckdb_attach_escapes_single_quotes_in_path() {
        let sql =
            try_build_duckdb_attach_sql("/tmp/o'reilly.duckdb", "x", true).expect("valid alias");
        assert_eq!(sql, "ATTACH '/tmp/o''reilly.duckdb' AS x (READ_ONLY)");
    }

    #[test]
    fn duckdb_attach_rejects_invalid_alias() {
        // Aliases are SQL identifiers — only [A-Za-z0-9_].
        assert!(try_build_duckdb_attach_sql("/tmp/x.duckdb", "bad alias", true).is_err());
        assert!(try_build_duckdb_attach_sql("/tmp/x.duckdb", "alias;DROP", true).is_err());
        assert!(try_build_duckdb_attach_sql("/tmp/x.duckdb", "", true).is_err());
    }

    #[test]
    fn duckdb_attach_rejects_empty_path() {
        assert!(try_build_duckdb_attach_sql("", "good_alias", true).is_err());
    }
}
