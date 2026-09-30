//! # Multi-Action Type System
//!
//! This module defines the unified action types for the execution engine.
//! Actions can be File, HTTP, Bash, DuckDB, pack, delegation, and scheduler
//! operations. Browser automation is no longer represented as a direct
//! Magicutor action enum; it enters through the browser skill and
//! agent-browser loop.
//!
//! ## Design Rationale
//!
//! - **FileAction vs BashAction**: File operations use `tokio::fs` directly
//!   (10x faster, no shell injection risk). BashAction is for arbitrary commands.
//! - **Native execution**: File/HTTP/Bash execute in Rust, not via MCP
//!   (avoids JSON serialization, process spawning overhead).
//! - **Tool name disambiguation**: The planner's tool choice determines action type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Unified action type that can represent any executable operation.
/// The executor dispatches to the appropriate native handler based on variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "action_type",
    content = "action_data",
    rename_all = "snake_case"
)]
pub enum ExecutableAction {
    /// File system operations via tokio::fs
    File(FileAction),

    /// HTTP requests via reqwest
    Http(HttpAction),

    /// Shell command execution via tokio::process
    Bash(BashAction),

    /// DuckDB SQL query execution via embedded engine
    DuckDb(DuckDbAction),

    /// YAML-defined capability executed via registry dispatch.
    /// Added by the capability provider system (Phase 1).
    Pack {
        /// Name of the capability (matches CapabilityPackDefinition.name).
        capability_name: String,
        /// How the capability is implemented (composite, cdp, js, mcp).
        implementation: super::capability::ImplementationType,
        /// Parameters resolved against the capability's declared schema.
        resolved_params: HashMap<String, serde_json::Value>,
    },

    /// Recursive sub-goal execution
    SpawnSubGoal { goal: String, budget: usize },

    /// Cross-agent delegation: dispatch work to a different registered agent.
    DelegateToAgent {
        targets: Vec<DelegationTargetRequest>,
    },

    /// Same-execution ownership transfer to a specialist agent.
    HandoverToAgent {
        target_agent_id: String,
        context: String,
    },

    /// Signal that the agent wants to sleep until `wake_at`.
    /// The agentic loop returns `AgenticOutcome::Sleeping` immediately
    /// without executing any real side-effecting action.
    SleepUntil {
        wake_at: DateTime<Utc>,
        reason: Option<String>,
    },
}

/// One isolated delegated child-execution request within a DelegateToAgent decision.
///
/// Deserialized raw from model-supplied arguments, so unknown fields are a
/// deserialization error rather than silently ignored: a model-invented
/// authority field (e.g. an engagement ref) must fail loudly instead of
/// appearing accepted (§4.2c row 5 hardening).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DelegationTargetRequest {
    pub target_agent_id: String,
    pub context: String,
    #[serde(default)]
    pub input_artifact_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub spend_token_ids: Vec<String>,
    /// Optional capability this stage needs — a skill/pack name as shown in the
    /// delegation roster (e.g. `comic-strip`). When set, the runtime verifies
    /// the target agent actually owns it and returns a retryable error naming
    /// the real owner(s) on mismatch, so a capability-specific stage lands on an
    /// agent that can do the work. Omit for non-capability-specific work
    /// (fail-open: no check).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_capability: Option<String>,
    /// Optional named text deliverables this stage MUST produce (e.g.
    /// `comic_script.md`). Each becomes an `expected_artifact_declaration` on the
    /// delegated child, so the deterministic refinement gate re-runs the child
    /// until an artifact with that exact name exists — instead of letting the
    /// agent bury a written deliverable inside a tool argument and finish without
    /// it. Omit for stages with no named text deliverable.
    #[serde(default)]
    pub expected_artifacts: Vec<DelegationExpectedArtifact>,
}

/// A named text deliverable a delegated stage is expected to produce, declared by
/// the delegating orchestrator at `delegate_to_agent` time. `name` is matched
/// verbatim against the child's produced artifact names by the refinement gate
/// (`missing_declaration_names`); `content_type` is advisory (enrich/render).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DelegationExpectedArtifact {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

pub const DELEGATION_WORK_BUDGET_NORMAL_SECS: u64 = 300;
pub const DELEGATION_WORK_BUDGET_DEEP_SECS: u64 = 900;
pub const DELEGATION_WORK_BUDGET_THOROUGH_SECS: u64 = 1800;

/// Resolve the active-work budget for a delegated run.
///
/// `timeout_secs` remains the wire name for compatibility, but the resolved
/// value is deliberately *not* a cancellation deadline. The agentic loop uses
/// it at operation boundaries: an in-flight provider/tool call may finish, no
/// new work starts afterwards, and normal result synthesis remains unbounded by
/// this budget. When neither an explicit value nor a depth is present, no
/// delegation-specific budget is created.
///
/// An explicit value only ever extends its tier — the named depth, or the
/// normal tier when none is named. A delegated run is a specialist's whole
/// job, not a tool call, and the value is model-authored: a chat mouth
/// guessed 120 s for a desktop-automation job and 180 s for another, and
/// every one of those runs stopped as "partial" with the entry not yet
/// written, at the budget, on every engine.
pub fn resolve_delegation_work_budget_secs(
    explicit_timeout_secs: Option<u64>,
    depth: Option<&str>,
) -> Result<Option<u64>, &'static str> {
    let depth_budget = match depth {
        Some("normal") => Some(DELEGATION_WORK_BUDGET_NORMAL_SECS),
        Some("deep") => Some(DELEGATION_WORK_BUDGET_DEEP_SECS),
        Some("thorough") => Some(DELEGATION_WORK_BUDGET_THOROUGH_SECS),
        Some(_) => {
            return Err("`depth` must be one of `normal`, `deep`, or `thorough`");
        },
        None => None,
    };

    if let Some(explicit) = explicit_timeout_secs {
        if explicit == 0 {
            return Err("`timeout_secs` must be a positive integer");
        }
        let tier = depth_budget.unwrap_or(DELEGATION_WORK_BUDGET_NORMAL_SECS);
        return Ok(Some(explicit.max(tier)));
    }

    Ok(depth_budget)
}

impl ExecutableAction {
    /// Structural parameters a pack action may name in its governed event.
    ///
    /// Strictly an allowlist. A pack's resolved parameters are model- and
    /// config-supplied and can carry credentials (an http pack's auth header, a
    /// browser pack's form values), so the event names only the keys that say
    /// *what was done to what* and never the payload.
    /// `file_path` earns its place beside `path`: the single-purpose file
    /// leaves (`read_file`, `write_file`, `edit_file`) name their file that
    /// way, and without it every one of their rows rendered as a bare verb —
    /// the timeline said `read_file` and no consumer could say which file.
    /// A search `pattern` deliberately stays out: it is model-authored text,
    /// and a grep for a secret would put the secret in the event.
    const PACK_EVENT_PARAM_KEYS: [&'static str; 5] =
        ["action", "path", "file_path", "source", "destination"];

    /// Longest value this will place in an event, per key.
    const PACK_EVENT_VALUE_BYTES: usize = 200;

    /// What a pack action did, for the per-action governed event.
    ///
    /// `FileAction::description` already yields `Read /path` — the action and
    /// its target — but agentic file work routes through the `files` pack, and
    /// the pack arm named only the capability. The durable event therefore said
    /// `files` and nothing more, so no consumer could tell which file a
    /// governed tool touched, or whether it read or wrote. The only events that
    /// carried a path were the goal text, the model's reply and the decision
    /// summary — all model prose, which cannot distinguish work that happened
    /// from work a model merely claimed.
    ///
    /// Renders `files(action="read", path="/abs/path")`, values JSON-quoted so
    /// a consumer can match one exactly and escaping stays unambiguous. A pack
    /// with none of the allowlisted keys renders as its bare name, exactly as
    /// before.
    pub fn pack_event_target(
        capability_name: &str,
        resolved_params: &HashMap<String, serde_json::Value>,
    ) -> String {
        let mut named = Vec::new();
        for key in Self::PACK_EVENT_PARAM_KEYS {
            let Some(value) = resolved_params.get(key) else {
                continue;
            };
            let rendered = match value {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                // Objects, arrays and nulls are payload shapes rather than a
                // name for what was acted on; naming them would be the leak
                // this allowlist exists to prevent.
                _ => continue,
            };
            let rendered = if rendered.len() > Self::PACK_EVENT_VALUE_BYTES {
                let mut cut = Self::PACK_EVENT_VALUE_BYTES;
                while cut > 0 && !rendered.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}…", &rendered[..cut])
            } else {
                rendered
            };
            named.push(format!("{key}={}", serde_json::Value::String(rendered)));
        }
        if named.is_empty() {
            capability_name.to_string()
        } else {
            format!("{capability_name}({})", named.join(", "))
        }
    }

    const MAX_RETAINED_JSON_NODES: usize = 200_000;
    const MAX_RETAINED_JSON_BYTES: usize = 8 * 1024 * 1024;

    /// Validate every recursive field before the action crosses a durable
    /// history or prepared-execution boundary.
    pub fn retention_admission_error(&self) -> Option<String> {
        let encoded_string_bytes = |value: &str| {
            crate::magician_v2::json_traversal::json_string_encoded_len(value).unwrap_or(usize::MAX)
        };
        let encoded_strings_bytes = |values: &[String]| {
            values
                .iter()
                .map(|value| encoded_string_bytes(value))
                .fold(0usize, usize::saturating_add)
        };
        let mut total_nodes = match self {
            Self::Pack {
                resolved_params, ..
            } => resolved_params.len(),
            Self::DelegateToAgent { targets } => {
                targets.iter().fold(targets.len(), |count, target| {
                    count
                        .saturating_add(target.input_artifact_ids.len())
                        .saturating_add(target.spend_token_ids.len())
                        .saturating_add(target.expected_artifacts.len())
                })
            },
            _ => 0,
        };
        if total_nodes > Self::MAX_RETAINED_JSON_NODES {
            return Some(format!(
                "action metadata exceeds the retained node limit ({})",
                Self::MAX_RETAINED_JSON_NODES
            ));
        }
        let mut total_bytes = match self {
            Self::Pack {
                capability_name,
                resolved_params,
                ..
            } => resolved_params
                .keys()
                .map(|key| encoded_string_bytes(key))
                .fold(encoded_string_bytes(capability_name), usize::saturating_add),
            Self::DelegateToAgent { targets } => targets
                .iter()
                .map(|target| {
                    encoded_string_bytes(&target.target_agent_id)
                        .saturating_add(encoded_string_bytes(&target.context))
                        .saturating_add(encoded_strings_bytes(&target.input_artifact_ids))
                        .saturating_add(encoded_strings_bytes(&target.spend_token_ids))
                        .saturating_add(target.depth.as_deref().map_or(0, encoded_string_bytes))
                        .saturating_add(
                            target
                                .required_capability
                                .as_deref()
                                .map_or(0, encoded_string_bytes),
                        )
                        .saturating_add(target.expected_artifacts.iter().fold(
                            0usize,
                            |bytes, artifact| {
                                bytes
                                    .saturating_add(encoded_string_bytes(&artifact.name))
                                    .saturating_add(
                                        artifact
                                            .content_type
                                            .as_deref()
                                            .map_or(0, encoded_string_bytes),
                                    )
                            },
                        ))
                })
                .fold(0usize, usize::saturating_add),
            _ => 0,
        };
        if total_bytes > Self::MAX_RETAINED_JSON_BYTES {
            return Some(format!(
                "action metadata exceeds the retained byte limit ({})",
                Self::MAX_RETAINED_JSON_BYTES
            ));
        }
        let mut admit = |label: &str, value: &Value| -> Option<String> {
            let remaining = Self::MAX_RETAINED_JSON_NODES.saturating_sub(total_nodes);
            let Some(metrics) =
                crate::magician_v2::json_traversal::inspect_json_bounded(value, remaining)
            else {
                return Some(format!(
                    "{label} exceeds the retained action node limit ({})",
                    Self::MAX_RETAINED_JSON_NODES
                ));
            };
            if metrics.max_depth > crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH {
                return Some(format!(
                    "{label} exceeds the retained action JSON depth ({})",
                    crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH
                ));
            }
            total_nodes = total_nodes.saturating_add(metrics.nodes);
            total_bytes = total_bytes.saturating_add(
                crate::magician_v2::json_traversal::exact_json_encoded_len(value),
            );
            if total_bytes > Self::MAX_RETAINED_JSON_BYTES {
                return Some(format!(
                    "{label} exceeds the retained action byte limit ({})",
                    Self::MAX_RETAINED_JSON_BYTES
                ));
            }
            None
        };

        match self {
            Self::Pack {
                resolved_params, ..
            } => {
                for (key, value) in resolved_params {
                    if let Some(error) = admit(&format!("pack parameter `{key}`"), value) {
                        return Some(error);
                    }
                }
            },
            Self::DelegateToAgent { targets } => {
                for (index, target) in targets.iter().enumerate() {
                    if let Some(value) = target.input_data.as_ref() {
                        if let Some(error) =
                            admit(&format!("delegation target {index} input"), value)
                        {
                            return Some(error);
                        }
                    }
                }
            },
            _ => {},
        }
        None
    }

    /// Produce an exact stack-safe clone after retention admission.
    pub fn try_clone_for_retention(&self) -> Result<Self, String> {
        if let Some(error) = self.retention_admission_error() {
            return Err(error);
        }
        Ok(match self {
            Self::Pack {
                capability_name,
                implementation,
                resolved_params,
            } => Self::Pack {
                capability_name: capability_name.clone(),
                implementation: implementation.clone(),
                resolved_params: resolved_params
                    .iter()
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            crate::magician_v2::json_traversal::clone_json_iteratively(value),
                        )
                    })
                    .collect(),
            },
            Self::DelegateToAgent { targets } => Self::DelegateToAgent {
                targets: targets
                    .iter()
                    .map(|target| DelegationTargetRequest {
                        target_agent_id: target.target_agent_id.clone(),
                        context: target.context.clone(),
                        input_artifact_ids: target.input_artifact_ids.clone(),
                        input_data: target.input_data.as_ref().map(|value| {
                            crate::magician_v2::json_traversal::clone_json_iteratively(value)
                        }),
                        depth: target.depth.clone(),
                        timeout_secs: target.timeout_secs,
                        spend_token_ids: target.spend_token_ids.clone(),
                        required_capability: target.required_capability.clone(),
                        expected_artifacts: target.expected_artifacts.clone(),
                    })
                    .collect(),
            },
            other => other.clone(),
        })
    }

    /// Clone an action for non-authoritative history/debugging. Rejected
    /// recursive input is represented explicitly rather than being retained
    /// and failing later during Serde or destruction. Execution paths must use
    /// [`Self::try_clone_for_retention`] instead.
    pub fn clone_for_retention(&self) -> Self {
        self.try_clone_for_retention()
            .unwrap_or_else(|error| match self {
                Self::Pack {
                    capability_name,
                    implementation,
                    ..
                } => Self::Pack {
                    capability_name: capability_name.clone(),
                    implementation: implementation.clone(),
                    resolved_params: HashMap::from([
                        ("_parameters_omitted".to_string(), Value::Bool(true)),
                        ("_retention_error".to_string(), Value::String(error)),
                    ]),
                },
                Self::DelegateToAgent { targets } => Self::DelegateToAgent {
                    targets: targets
                        .iter()
                        .map(|target| DelegationTargetRequest {
                            target_agent_id: target.target_agent_id.clone(),
                            context: target.context.clone(),
                            input_artifact_ids: target.input_artifact_ids.clone(),
                            input_data: target.input_data.as_ref().map(|_| {
                                serde_json::json!({
                                    "_input_omitted": true,
                                    "_retention_error": error.clone(),
                                })
                            }),
                            depth: target.depth.clone(),
                            timeout_secs: target.timeout_secs,
                            spend_token_ids: target.spend_token_ids.clone(),
                            required_capability: target.required_capability.clone(),
                            expected_artifacts: target.expected_artifacts.clone(),
                        })
                        .collect(),
                },
                other => other.clone(),
            })
    }

    /// Returns true if this is a browser action.
    ///
    /// Browser actions are no longer part of the outer executable-action
    /// model, so this compatibility helper always returns false until callers
    /// are cleaned up.
    pub fn is_browser(&self) -> bool {
        false
    }

    /// Returns true if this is a file action
    pub fn is_file(&self) -> bool {
        matches!(self, ExecutableAction::File(_))
    }

    /// Returns true if this is an HTTP action
    pub fn is_http(&self) -> bool {
        matches!(self, ExecutableAction::Http(_))
    }

    /// Returns true if this is a bash action
    pub fn is_bash(&self) -> bool {
        matches!(self, ExecutableAction::Bash(_))
    }

    /// Returns true if this is a DuckDB action
    pub fn is_duckdb(&self) -> bool {
        matches!(self, ExecutableAction::DuckDb(_))
    }

    /// Returns true if this is a pack (YAML-defined) action
    pub fn is_pack(&self) -> bool {
        matches!(self, ExecutableAction::Pack { .. })
    }

    /// Returns true if this is a sub-goal action
    pub fn is_sub_goal(&self) -> bool {
        matches!(self, ExecutableAction::SpawnSubGoal { .. })
    }

    /// Returns true if this is a cross-agent delegation action
    pub fn is_delegate(&self) -> bool {
        matches!(self, ExecutableAction::DelegateToAgent { .. })
    }

    /// Returns true if this is a same-execution ownership handover action.
    pub fn is_handover(&self) -> bool {
        matches!(self, ExecutableAction::HandoverToAgent { .. })
    }

    /// Returns true if this is a sleep-until action
    pub fn is_sleep_until(&self) -> bool {
        matches!(self, ExecutableAction::SleepUntil { .. })
    }

    /// Get a human-readable description of the action type.
    ///
    pub fn action_type_name(&self) -> &'static str {
        match self {
            ExecutableAction::File(_) => "file",
            ExecutableAction::Http(_) => "http",
            ExecutableAction::Bash(_) => "bash",
            ExecutableAction::DuckDb(_) => "duckdb",
            ExecutableAction::Pack { .. } => "pack",
            ExecutableAction::SpawnSubGoal { .. } => "spawn_sub_goal",
            ExecutableAction::DelegateToAgent { .. } => "delegate_to_agent",
            ExecutableAction::HandoverToAgent { .. } => "handover_to_agent",
            ExecutableAction::SleepUntil { .. } => "sleep_until",
        }
    }

    /// Check if this action targets a cross-origin iframe.
    ///
    /// Returns true for iframe-scoped evaluation/query actions.
    /// These actions require cross-origin verification handling because DOM-based
    /// signals cannot access content inside cross-origin iframes.
    pub fn is_cross_origin_frame_action(&self) -> bool {
        match self {
            ExecutableAction::File(_)
            | ExecutableAction::Http(_)
            | ExecutableAction::Bash(_)
            | ExecutableAction::DuckDb(_)
            | ExecutableAction::Pack { .. }
            | ExecutableAction::SpawnSubGoal { .. }
            | ExecutableAction::DelegateToAgent { .. }
            | ExecutableAction::HandoverToAgent { .. }
            | ExecutableAction::SleepUntil { .. } => false,
        }
    }

    /// Extract the primary selector from browser actions that operate on single elements.
    ///
    /// Returns `Some(&str)` for browser actions with a `selector` field, `None` otherwise.
    /// Non-browser actions always return `None`.
    pub fn get_selector(&self) -> Option<&str> {
        match self {
            ExecutableAction::File(_)
            | ExecutableAction::Http(_)
            | ExecutableAction::Bash(_)
            | ExecutableAction::DuckDb(_)
            | ExecutableAction::Pack { .. }
            | ExecutableAction::SpawnSubGoal { .. }
            | ExecutableAction::DelegateToAgent { .. }
            | ExecutableAction::HandoverToAgent { .. }
            | ExecutableAction::SleepUntil { .. } => None,
        }
    }

    /// Canonical trust-policy routing tuple `(tool, action)`.
    ///
    /// Trust policies match on tool/action patterns before dispatch. This mapping
    /// normalizes rich runtime actions into a stable policy surface (for example,
    /// mutating file actions all map to `files:write`).
    pub fn trust_tool_action(&self) -> (String, String) {
        match self {
            ExecutableAction::File(action) => ("files".to_string(), file_trust_action(action)),
            ExecutableAction::Http(action) => (
                "http".to_string(),
                format!("{:?}", action.method).to_ascii_lowercase(),
            ),
            ExecutableAction::Bash(_) => ("shell".to_string(), "execute".to_string()),
            ExecutableAction::DuckDb(_) => ("duckdb".to_string(), "query".to_string()),
            ExecutableAction::Pack {
                capability_name,
                resolved_params,
                ..
            } => crate::magician_v2::agents::approval::tool_action_for_approval(
                capability_name,
                resolved_params,
            ),
            ExecutableAction::SpawnSubGoal { .. } => {
                ("orchestrator".to_string(), "spawn_sub_goal".to_string())
            },
            ExecutableAction::DelegateToAgent { .. } => {
                ("orchestrator".to_string(), "delegate_to_agent".to_string())
            },
            ExecutableAction::HandoverToAgent { .. } => {
                ("orchestrator".to_string(), "handover_to_agent".to_string())
            },
            ExecutableAction::SleepUntil { .. } => {
                ("scheduler".to_string(), "sleep_until".to_string())
            },
        }
    }
}

fn file_trust_action(action: &FileAction) -> String {
    match action {
        FileAction::Read { .. } | FileAction::List { .. } | FileAction::Exists { .. } => {
            "read".to_string()
        },
        FileAction::Write { .. }
        | FileAction::Append { .. }
        | FileAction::Delete { .. }
        | FileAction::Copy { .. }
        | FileAction::Move { .. }
        | FileAction::CreateDir { .. } => "write".to_string(),
    }
}

// ============================================================================
// File Actions
// ============================================================================

/// Type-safe file system operations.
///
/// Benefits over BashAction for file ops:
/// - 10x faster (no shell spawn overhead)
/// - No shell injection possible
/// - Structured errors (io::Error vs parsing shell output)
/// - Explicit parameters vs string interpolation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum FileAction {
    /// Read file contents
    Read {
        path: PathBuf,
        #[serde(default)]
        encoding: Option<String>,
    },

    /// Write content to file (creates parent dirs if needed)
    Write {
        path: PathBuf,
        content: String,
        #[serde(default = "default_true")]
        create_dirs: bool,
    },

    /// Append content to file
    Append { path: PathBuf, content: String },

    /// Delete file or directory
    Delete {
        path: PathBuf,
        #[serde(default)]
        recursive: bool,
    },

    /// Copy file or directory
    Copy {
        source: PathBuf,
        destination: PathBuf,
    },

    /// Move/rename file or directory
    Move {
        source: PathBuf,
        destination: PathBuf,
    },

    /// Check if path exists
    Exists { path: PathBuf },

    /// List directory contents
    List {
        path: PathBuf,
        #[serde(default)]
        pattern: Option<String>,
    },

    /// Create directory (with parents)
    CreateDir { path: PathBuf },
}

fn default_true() -> bool {
    true
}

impl FileAction {
    /// Get the primary path this action operates on
    pub fn primary_path(&self) -> &PathBuf {
        match self {
            FileAction::Read { path, .. } => path,
            FileAction::Write { path, .. } => path,
            FileAction::Append { path, .. } => path,
            FileAction::Delete { path, .. } => path,
            FileAction::Copy { source, .. } => source,
            FileAction::Move { source, .. } => source,
            FileAction::Exists { path } => path,
            FileAction::List { path, .. } => path,
            FileAction::CreateDir { path } => path,
        }
    }

    /// Get a human-readable description
    pub fn description(&self) -> String {
        match self {
            FileAction::Read { path, .. } => format!("Read {}", path.display()),
            FileAction::Write { path, .. } => format!("Write {}", path.display()),
            FileAction::Append { path, .. } => format!("Append to {}", path.display()),
            FileAction::Delete { path, recursive } => {
                if *recursive {
                    format!("Delete recursively {}", path.display())
                } else {
                    format!("Delete {}", path.display())
                }
            },
            FileAction::Copy {
                source,
                destination,
            } => {
                format!("Copy {} to {}", source.display(), destination.display())
            },
            FileAction::Move {
                source,
                destination,
            } => {
                format!("Move {} to {}", source.display(), destination.display())
            },
            FileAction::Exists { path } => format!("Check exists {}", path.display()),
            FileAction::List { path, pattern } => {
                if let Some(p) = pattern {
                    format!("List {} (pattern: {})", path.display(), p)
                } else {
                    format!("List {}", path.display())
                }
            },
            FileAction::CreateDir { path } => format!("Create directory {}", path.display()),
        }
    }
}

// ============================================================================
// HTTP Actions
// ============================================================================

/// HTTP request action executed via reqwest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpAction {
    /// HTTP method
    pub method: HttpMethod,

    /// Target URL
    pub url: String,

    /// Request headers
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Request body (for POST, PUT, PATCH)
    #[serde(default)]
    pub body: Option<String>,

    /// Content type (convenience, also sets header)
    #[serde(default)]
    pub content_type: Option<String>,

    /// Request timeout in seconds
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Follow redirects
    #[serde(default = "default_true")]
    pub follow_redirects: bool,

    /// Set by the secret lowering when a user-typed credential was written
    /// into this request's headers or body (P4): the adapter then never
    /// follows a redirect, and a cross-origin one ends the attempt with the
    /// credential unforwarded. Never read from the model or persisted.
    #[serde(skip)]
    pub carries_credential: bool,
}

/// HTTP methods
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[derive(Default)]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpAction {
    /// Create a simple GET request
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: HttpMethod::Get,
            url: url.into(),
            headers: HashMap::new(),
            body: None,
            content_type: None,
            timeout_secs: Some(30),
            follow_redirects: true,
            carries_credential: false,
        }
    }

    /// Create a POST request with JSON body
    pub fn post_json(url: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            method: HttpMethod::Post,
            url: url.into(),
            headers: HashMap::new(),
            body: Some(body.into()),
            content_type: Some("application/json".to_string()),
            timeout_secs: Some(30),
            follow_redirects: true,
            carries_credential: false,
        }
    }

    /// Get a human-readable description
    pub fn description(&self) -> String {
        format!("{:?} {}", self.method, self.url)
    }
}

// ============================================================================
// Bash Actions
// ============================================================================

/// Shell command execution via tokio::process.
///
/// Use this for arbitrary commands. For file operations,
/// prefer FileAction (faster, safer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashAction {
    /// The command to execute (passed to sh -c)
    pub command: String,

    /// Working directory for command execution
    #[serde(default)]
    pub working_dir: Option<PathBuf>,

    /// Environment variables to set
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Command timeout in seconds
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Whether to capture stdout/stderr
    #[serde(default = "default_true")]
    pub capture_output: bool,

    /// Optional payload written to the child's stdin once the process
    /// is spawned and stdin is closed afterwards (EOF).
    ///
    /// Use cases:
    /// - Answer simple y/n prompts (`stdin: "y\n"`).
    /// - Pipe a JSON/yaml body into a CLI without shell-escaping
    ///   (`stdin: serialize(body)`, command: `gh pr create --body-file -`).
    /// - Pre-fill multi-line input for tools that read stdin
    ///   (`stdin: "name=X\nemail=Y\n"`).
    ///
    /// Limitations: this writes once at startup. Tools that re-prompt
    /// based on the user's earlier answer (interactive wizards, TUIs)
    /// are out of reach for agents: the PTY-backed `interactive_process`
    /// sessions are operator-only (Developer Mode HTTP/UI lane) and sit
    /// on the `NEVER_ON_THE_PLANE` floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
}

impl BashAction {
    /// Create a simple command
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            working_dir: None,
            env: HashMap::new(),
            timeout_secs: Some(60),
            capture_output: true,
            stdin: None,
        }
    }

    /// Set working directory
    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Set timeout
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Add environment variable
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Get a human-readable description (truncated command)
    pub fn description(&self) -> String {
        if self.command.chars().count() > 50 {
            let truncated: String = self.command.chars().take(47).collect();
            format!("{truncated}...")
        } else {
            self.command.clone()
        }
    }
}

// ============================================================================
// DuckDB Actions
// ============================================================================

/// DuckDB SQL query execution via the embedded engine.
///
/// Runs SQL against an in-process DuckDB instance. The connection persists
/// across queries within an agent session, so tables/views created in one
/// query are available in subsequent ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuckDbAction {
    /// SQL query to execute
    pub sql: String,

    /// Optional path to a persistent `.duckdb` database file.
    /// When None, uses the session-scoped in-memory connection.
    #[serde(default)]
    pub database: Option<String>,

    /// Output format: "json" (default), "csv", or "table"
    #[serde(default = "default_json_format")]
    pub output_format: String,

    /// Optional per-query timeout in seconds. Overrides the provider default when set.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn default_json_format() -> String {
    "json".to_string()
}

impl DuckDbAction {
    /// Create a simple query action
    pub fn query(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            database: None,
            output_format: "json".to_string(),
            timeout_secs: None,
        }
    }

    /// Get a human-readable description (truncated SQL)
    pub fn description(&self) -> String {
        let sql = if self.sql.chars().count() > 60 {
            let truncated: String = self.sql.chars().take(57).collect();
            format!("{truncated}...")
        } else {
            self.sql.clone()
        };
        if let Some(ref db) = self.database {
            format!("DuckDB [{}]: {}", db, sql)
        } else {
            format!("DuckDB: {}", sql)
        }
    }
}

// ============================================================================
// Action Results
// ============================================================================

/// Result of executing an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActionResult {
    /// Simple success with no data
    Success,

    /// Text content (file read, command output, etc.)
    Text { content: String },

    /// Binary content (base64 encoded)
    Binary {
        data: String,
        mime_type: Option<String>,
    },

    /// HTTP response
    Http {
        status: u16,
        headers: HashMap<String, String>,
        body: String,
    },

    /// Boolean result (exists check, etc.)
    Bool { value: bool },

    /// List of items (directory listing, etc.)
    List { items: Vec<String> },

    /// Browser action result (forwarded from Magicutor)
    Browser { data: serde_json::Value },
}

impl ActionResult {
    /// Create a success result
    pub fn success() -> Self {
        ActionResult::Success
    }

    /// Create a text result
    pub fn text(content: impl Into<String>) -> Self {
        ActionResult::Text {
            content: content.into(),
        }
    }

    /// Create an HTTP result
    pub fn http(status: u16, body: impl Into<String>) -> Self {
        ActionResult::Http {
            status,
            headers: HashMap::new(),
            body: body.into(),
        }
    }

    /// Create a bool result
    pub fn bool(value: bool) -> Self {
        ActionResult::Bool { value }
    }

    /// Create a list result
    pub fn list(items: Vec<String>) -> Self {
        ActionResult::List { items }
    }

    /// Check if the result indicates success
    pub fn is_success(&self) -> bool {
        match self {
            ActionResult::Success => true,
            ActionResult::Http { status, .. } => *status >= 200 && *status < 300,
            ActionResult::Browser { data, .. } => data
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
            _ => true, // Other results are successful if we got them
        }
    }

    /// Get text content if available
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ActionResult::Text { content } => Some(content),
            ActionResult::Http { body, .. } => Some(body),
            _ => None,
        }
    }

    /// Return the browser download payload when this result represents a completed download.
    pub fn download_payload(&self) -> Option<&serde_json::Value> {
        let ActionResult::Browser { data, .. } = self else {
            return None;
        };
        data.get("download")
    }

    /// Return the downloaded file path when this result includes an explicit
    /// browser download payload.
    pub fn completed_download_path(&self) -> Option<&str> {
        self.download_payload()
            .and_then(|download| download.get("path"))
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.trim().is_empty())
    }
}

// NOTE: ExecutableStep was removed from this module.
// The main execution flow uses `types::ExecutableStep` with `ExecutableAction`.
// For native (non-browser) actions, JIT lowering returns `ExecutableAction` directly,
// which is stored in `StepExecutionResult.executed_native_action` for observability.
// See MULTI_ACTION_JIT_LOWERING.md for the design rationale.

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_file_action_description() {
        let action = FileAction::Write {
            path: PathBuf::from("/tmp/test.txt"),
            content: "hello".to_string(),
            create_dirs: true,
        };
        assert_eq!(action.description(), "Write /tmp/test.txt");
    }

    #[test]
    fn test_http_action_get() {
        let action = HttpAction::get("https://api.example.com/data");
        assert_eq!(action.method, HttpMethod::Get);
        assert_eq!(action.url, "https://api.example.com/data");
    }

    #[test]
    fn test_bash_action_builder() {
        let action = BashAction::new("echo hello")
            .with_working_dir("/tmp")
            .with_timeout(30)
            .with_env("FOO", "bar");

        assert_eq!(action.command, "echo hello");
        assert_eq!(action.working_dir, Some(PathBuf::from("/tmp")));
        assert_eq!(action.timeout_secs, Some(30));
        assert_eq!(action.env.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_action_result_is_success() {
        assert!(ActionResult::success().is_success());
        assert!(ActionResult::text("hello").is_success());
        assert!(ActionResult::http(200, "OK").is_success());
        assert!(!ActionResult::http(404, "Not Found").is_success());
        assert!(ActionResult::Browser {
            data: serde_json::json!({"ok": true}),
        }
        .is_success());
        assert!(!ActionResult::Browser {
            data: serde_json::json!({"success": false}),
        }
        .is_success());
    }

    #[test]
    fn test_action_result_completed_download_path() {
        let result = ActionResult::Browser {
            data: serde_json::json!({
                "download": {
                    "path": "/tmp/report.pdf"
                }
            }),
        };

        assert_eq!(result.completed_download_path(), Some("/tmp/report.pdf"));
    }

    #[test]
    fn test_executable_action_type_checks() {
        let file = ExecutableAction::File(FileAction::Exists {
            path: PathBuf::from("/tmp"),
        });

        assert!(file.is_file());
        assert!(!file.is_browser());
    }

    #[test]
    fn retained_pack_action_rejects_deep_json_before_exact_clone() {
        std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let mut value = Value::Null;
                for _ in 0..10_000 {
                    value = Value::Array(vec![value]);
                }
                let action = ExecutableAction::Pack {
                    capability_name: "deep-test".to_string(),
                    implementation:
                        crate::magician_v2::execution::capability::ImplementationType::Compiled {
                            provider_name: "deep-test".to_string(),
                        },
                    resolved_params: HashMap::from([("payload".to_string(), value)]),
                };
                assert!(action.try_clone_for_retention().is_err());
                let history = action.clone_for_retention();
                let ExecutableAction::Pack {
                    resolved_params, ..
                } = history
                else {
                    unreachable!();
                };
                assert_eq!(resolved_params["_parameters_omitted"], Value::Bool(true));
                let ExecutableAction::Pack {
                    mut resolved_params,
                    ..
                } = action
                else {
                    unreachable!();
                };
                crate::magician_v2::json_traversal::discard_json_iteratively(
                    resolved_params.remove("payload").expect("payload"),
                );
            })
            .expect("small-stack action worker")
            .join()
            .expect("retained action clone must remain stack safe");
    }

    #[test]
    fn retained_delegation_counts_all_metadata_and_json_escape_expansion() {
        let action = ExecutableAction::DelegateToAgent {
            targets: vec![DelegationTargetRequest {
                target_agent_id: "worker".to_string(),
                context: "\\".repeat(ExecutableAction::MAX_RETAINED_JSON_BYTES / 2),
                input_artifact_ids: Vec::new(),
                input_data: None,
                depth: Some("normal".to_string()),
                timeout_secs: None,
                spend_token_ids: Vec::new(),
                required_capability: Some("research".to_string()),
                expected_artifacts: vec![DelegationExpectedArtifact {
                    name: "answer.md".to_string(),
                    content_type: Some("text/markdown".to_string()),
                }],
            }],
        };
        assert!(action.retention_admission_error().is_some());
        assert!(action.try_clone_for_retention().is_err());
    }

    #[test]
    fn test_action_type_name_returns_native_action_categories() {
        let file = ExecutableAction::File(FileAction::Read {
            path: PathBuf::from("/tmp/test.txt"),
            encoding: None,
        });
        assert_eq!(file.action_type_name(), "file");

        let bash = ExecutableAction::Bash(BashAction::new("echo hello"));
        assert_eq!(bash.action_type_name(), "bash");
    }

    #[test]
    fn trust_route_maps_file_actions_to_read_write() {
        let read = ExecutableAction::File(FileAction::Read {
            path: PathBuf::from("foo.txt"),
            encoding: None,
        });
        let write = ExecutableAction::File(FileAction::Append {
            path: PathBuf::from("foo.txt"),
            content: "x".to_string(),
        });
        assert_eq!(
            read.trust_tool_action(),
            ("files".to_string(), "read".to_string())
        );
        assert_eq!(
            write.trust_tool_action(),
            ("files".to_string(), "write".to_string())
        );
    }

    #[test]
    fn trust_route_maps_pack_discriminator_and_leaf_names() {
        let report = ExecutableAction::Pack {
            capability_name: "report".to_string(),
            resolved_params: HashMap::from([(
                "action".to_string(),
                serde_json::Value::String("email".to_string()),
            )]),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "report".to_string(),
                },
        };
        assert_eq!(
            report.trust_tool_action(),
            ("report".to_string(), "email".to_string())
        );

        let browser_leaf = ExecutableAction::Pack {
            capability_name: "browser__click".to_string(),
            resolved_params: HashMap::new(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "browser".to_string(),
                },
        };
        assert_eq!(
            browser_leaf.trust_tool_action(),
            ("browser".to_string(), "click".to_string())
        );
    }

    #[test]
    fn an_explicit_budget_only_ever_extends_its_tier() {
        assert_eq!(
            resolve_delegation_work_budget_secs(Some(42), Some("thorough")),
            Ok(Some(DELEGATION_WORK_BUDGET_THOROUGH_SECS)),
            "below the named tier: the tier"
        );
        assert_eq!(
            resolve_delegation_work_budget_secs(Some(2400), Some("thorough")),
            Ok(Some(2400)),
            "above it: the explicit value"
        );
        assert_eq!(
            resolve_delegation_work_budget_secs(Some(120), None),
            Ok(Some(DELEGATION_WORK_BUDGET_NORMAL_SECS)),
            "no depth named: the normal tier is the floor"
        );
    }

    #[test]
    fn resolve_delegation_work_budget_uses_depth_tiers() {
        assert_eq!(
            resolve_delegation_work_budget_secs(None, Some("normal")),
            Ok(Some(DELEGATION_WORK_BUDGET_NORMAL_SECS))
        );
        assert_eq!(
            resolve_delegation_work_budget_secs(None, Some("deep")),
            Ok(Some(DELEGATION_WORK_BUDGET_DEEP_SECS))
        );
        assert_eq!(
            resolve_delegation_work_budget_secs(None, Some("thorough")),
            Ok(Some(DELEGATION_WORK_BUDGET_THOROUGH_SECS))
        );
    }

    #[test]
    fn resolve_delegation_work_budget_does_not_invent_a_default() {
        assert_eq!(resolve_delegation_work_budget_secs(None, None), Ok(None));
    }

    #[test]
    fn resolve_delegation_work_budget_rejects_invalid_values() {
        assert!(resolve_delegation_work_budget_secs(Some(0), None).is_err());
        assert!(resolve_delegation_work_budget_secs(None, Some("extended")).is_err());
        assert!(resolve_delegation_work_budget_secs(Some(60), Some("extended")).is_err());
    }
}

#[cfg(test)]
mod pack_event_target_tests {
    use super::*;

    /// A governed event has to say what was done to what. Before this the pack
    /// arm emitted only `files`, so nothing in the durable stream could tell a
    /// read from a write, or name the file — and the only events carrying a
    /// path were model prose.
    #[test]
    fn a_pack_event_target_names_the_action_and_its_path() {
        let mut params = std::collections::HashMap::new();
        params.insert("action".to_string(), serde_json::json!("read"));
        params.insert("path".to_string(), serde_json::json!("/tmp/fixture.txt"));
        assert_eq!(
            ExecutableAction::pack_event_target("files", &params),
            r#"files(action="read", path="/tmp/fixture.txt")"#
        );
    }

    /// The single-purpose file leaves (`read_file`, `write_file`, `edit_file`)
    /// name their file `file_path`, not `path`, so the allowlist that fixed the
    /// multi-action `files` pack left every one of their rows as a bare verb:
    /// the timeline said `read_file` and no consumer — UI, analytics, or a
    /// conformance gate reading the durable stream — could say which file was
    /// read. Measured on the delegation case, where a child that read the
    /// fixture and reported it perfectly still could not be proved to have
    /// touched it.
    #[test]
    fn a_file_leaf_event_target_names_the_file_it_touched() {
        for capability in ["read_file", "write_file", "edit_file"] {
            let mut params = std::collections::HashMap::new();
            params.insert(
                "file_path".to_string(),
                serde_json::json!("/tmp/fixture/input.txt"),
            );
            assert_eq!(
                ExecutableAction::pack_event_target(capability, &params),
                format!(r#"{capability}(file_path="/tmp/fixture/input.txt")"#)
            );
        }
    }

    /// `grep` and `glob` carry a model-authored `pattern`, and a search for a
    /// secret puts that secret in the pattern. The row names where the search
    /// ran, never what was searched for.
    #[test]
    fn a_search_pattern_never_reaches_the_timeline() {
        let mut params = std::collections::HashMap::new();
        params.insert("pattern".to_string(), serde_json::json!("sk-ant-secret"));
        params.insert("path".to_string(), serde_json::json!("/repo"));
        let target = ExecutableAction::pack_event_target("grep", &params);
        assert!(target.contains("/repo"), "{target}");
        assert!(!target.contains("sk-ant-secret"), "{target}");
    }

    /// Resolved parameters are model- and config-supplied and can carry
    /// credentials, so anything outside the allowlist must not reach an event.
    #[test]
    fn a_pack_event_target_names_only_allowlisted_keys() {
        let mut params = std::collections::HashMap::new();
        params.insert("action".to_string(), serde_json::json!("write"));
        params.insert(
            "authorization".to_string(),
            serde_json::json!("Bearer sk-secret"),
        );
        params.insert(
            "body".to_string(),
            serde_json::json!({"password": "hunter2"}),
        );
        let target = ExecutableAction::pack_event_target("http", &params);
        assert!(!target.contains("sk-secret"), "{target}");
        assert!(!target.contains("hunter2"), "{target}");
        assert!(!target.contains("authorization"), "{target}");
        assert_eq!(target, r#"http(action="write")"#);
    }

    /// A pack with nothing worth naming keeps the previous rendering, so this
    /// adds evidence without changing every other pack's timeline entry.
    #[test]
    fn a_pack_event_target_without_allowlisted_keys_is_the_bare_name() {
        let mut params = std::collections::HashMap::new();
        params.insert("query".to_string(), serde_json::json!("anything"));
        assert_eq!(
            ExecutableAction::pack_event_target("browser", &params),
            "browser"
        );
    }

    /// One oversized value must not turn a timeline entry into a payload dump,
    /// and the cut must land on a character boundary.
    #[test]
    fn a_pack_event_target_bounds_each_value() {
        let mut params = std::collections::HashMap::new();
        params.insert("path".to_string(), serde_json::json!("é".repeat(400)));
        let target = ExecutableAction::pack_event_target("files", &params);
        assert!(target.len() < 500, "len {}", target.len());
        assert!(target.ends_with(r#"…")"#), "{target}");
    }
}
