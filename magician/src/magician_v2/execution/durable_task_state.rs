//! Typed outer-loop contract for optional durable task-state mutation.
//!
//! The inner loop never sees this contract. It is carried as internal
//! metadata on outer-loop decision tool calls so the outer agent can decide
//! whether durable product/UI state should be created, patched, closed, or
//! left alone at semantic boundaries.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStateActionKind {
    #[serde(rename = "none")]
    NoneAction,
    Create,
    Patch,
    Close,
}

impl TaskStateActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoneAction => "none",
            Self::Create => "create",
            Self::Patch => "patch",
            Self::Close => "close",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStateNextReview {
    NextOuterIteration,
    AfterCapabilityReturn,
    OnResume,
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStateActionEnvelope {
    pub action: TaskStateActionKind,
    pub reason: String,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub next_review: Option<TaskStateNextReview>,
    #[serde(default)]
    pub proposed_taskplan: Option<Value>,
    #[serde(default)]
    pub patch: Option<Value>,
    #[serde(default)]
    pub source_execution_id: Option<String>,
    #[serde(default)]
    pub source_iteration_range: Option<[u64; 2]>,
    #[serde(default)]
    pub evidence_refs: Vec<Value>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl TaskStateActionEnvelope {
    pub fn none(reason: impl Into<String>) -> Self {
        Self {
            action: TaskStateActionKind::NoneAction,
            reason: reason.into(),
            confidence: Some(1.0),
            next_review: Some(TaskStateNextReview::NextOuterIteration),
            proposed_taskplan: None,
            patch: None,
            source_execution_id: None,
            source_iteration_range: None,
            evidence_refs: Vec::new(),
            notes: None,
        }
    }

    pub fn validate_basic(&self) -> Result<(), String> {
        let reason = self.reason.trim();
        if reason.is_empty() {
            return Err("task_state_action.reason is required".to_string());
        }
        if reason.chars().count() > 500 {
            return Err("task_state_action.reason must be <= 500 chars".to_string());
        }
        if let Some(confidence) = self.confidence {
            if !(0.0..=1.0).contains(&confidence) {
                return Err("task_state_action.confidence must be between 0.0 and 1.0".to_string());
            }
        }
        if self.action == TaskStateActionKind::NoneAction {
            if self.proposed_taskplan.is_some() {
                return Err(
                    "task_state_action.proposed_taskplan must be null for action=none".to_string(),
                );
            }
            if self.patch.is_some() {
                return Err("task_state_action.patch must be null for action=none".to_string());
            }
        }
        if let Some([start, end]) = self.source_iteration_range {
            if start > end {
                return Err(
                    "task_state_action.source_iteration_range start must be <= end".to_string(),
                );
            }
        }
        Ok(())
    }
}

impl Default for TaskStateActionEnvelope {
    fn default() -> Self {
        Self::none("Durable task state unchanged.")
    }
}

pub fn parse_task_state_action_value(
    value: Option<&Value>,
) -> Result<TaskStateActionEnvelope, String> {
    let Some(value) = value else {
        return Err("missing required task_state_action envelope".to_string());
    };
    let envelope: TaskStateActionEnvelope = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid task_state_action envelope: {error}"))?;
    envelope.validate_basic()?;
    Ok(envelope)
}

const DURABLE_TASK_STATE_SCHEMA_VERSION: &str = "1.0";

// Durable task state is injected back into later model turns, so it must stay
// small even when an orchestration goal contains a full harness prompt. The
// authoritative goal and execution payload remain in the task journal; this
// checkpoint keeps a bounded head/tail synopsis for continuity.
const STORED_GOAL_MAX_BYTES: usize = 768;
const STORED_SUCCESS_CRITERIA_MAX_BYTES: usize = 768;
const STORED_MICRO_GOAL_DESCRIPTION_MAX_BYTES: usize = 512;
const STORED_DETAIL_MAX_BYTES: usize = 384;
const STORED_PARTIAL_ITEM_MAX_BYTES: usize = 192;
const STORED_PARTIAL_ITEMS_MAX: usize = 4;
const COMPACTION_MARKER: &str = "\n…[compacted in task state]…\n";

fn prefix_boundary(value: &str, mut byte_index: usize) -> usize {
    byte_index = byte_index.min(value.len());
    while byte_index > 0 && !value.is_char_boundary(byte_index) {
        byte_index -= 1;
    }
    byte_index
}

fn suffix_boundary(value: &str, mut byte_index: usize) -> usize {
    byte_index = byte_index.min(value.len());
    while byte_index < value.len() && !value.is_char_boundary(byte_index) {
        byte_index += 1;
    }
    byte_index
}

fn compact_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes <= COMPACTION_MARKER.len() {
        return value[..prefix_boundary(value, max_bytes)].to_string();
    }

    let content_budget = max_bytes - COMPACTION_MARKER.len();
    let head_budget = content_budget.saturating_mul(2) / 3;
    let tail_budget = content_budget.saturating_sub(head_budget);
    let head_end = prefix_boundary(value, head_budget);
    let tail_start = suffix_boundary(value, value.len().saturating_sub(tail_budget));
    format!(
        "{}{}{}",
        &value[..head_end],
        COMPACTION_MARKER,
        &value[tail_start..]
    )
}

fn compact_string_field(map: &mut Map<String, Value>, key: &str, max_bytes: usize) -> bool {
    let Some(Value::String(value)) = map.get_mut(key) else {
        return false;
    };
    if value.len() <= max_bytes {
        return false;
    }
    *value = compact_text(value, max_bytes);
    true
}

fn compact_string_array(value: &mut Value, max_items: usize, max_item_bytes: usize) -> bool {
    let Some(items) = value.as_array_mut() else {
        return false;
    };
    let mut changed = false;
    if items.len() > max_items {
        items.truncate(max_items);
        changed = true;
    }
    for item in items {
        let Value::String(text) = item else {
            continue;
        };
        if text.len() > max_item_bytes {
            *text = compact_text(text, max_item_bytes);
            changed = true;
        }
    }
    changed
}

/// Compact only verbose, human-readable fields in a valid typed task-state
/// checkpoint. Identity, status, timestamps, evidence references, and the
/// micro-goal graph are preserved. Unknown/untyped state retains the provider's
/// existing fail-closed behavior.
pub fn compact_durable_task_state_for_storage(
    state: &Value,
    max_bytes: usize,
) -> Result<(Value, bool), String> {
    let original_json =
        serde_json::to_string_pretty(state).map_err(|error| format!("serialize: {error}"))?;
    if original_json.len() <= max_bytes {
        return Ok((state.clone(), false));
    }

    let expected_task_id = state.get("task_id").and_then(Value::as_str);
    if state.get("schema_version").and_then(Value::as_str)
        != Some(DURABLE_TASK_STATE_SCHEMA_VERSION)
        || validate_durable_task_state(state, expected_task_id).is_err()
    {
        return Ok((state.clone(), false));
    }

    let mut compacted = state.clone();
    let map = object_mut(&mut compacted, "DurableTaskState")?;
    let mut changed = compact_string_field(map, "goal", STORED_GOAL_MAX_BYTES);
    changed |= compact_string_field(map, "success_criteria", STORED_SUCCESS_CRITERIA_MAX_BYTES);

    if let Some(goals) = map.get_mut("micro_goals").and_then(Value::as_array_mut) {
        for goal in goals {
            let Some(goal) = goal.as_object_mut() else {
                continue;
            };
            changed |=
                compact_string_field(goal, "description", STORED_MICRO_GOAL_DESCRIPTION_MAX_BYTES);
            changed |= compact_string_field(goal, "blocked_reason", STORED_DETAIL_MAX_BYTES);
        }
    }

    if let Some(metadata) = map.get_mut("metadata").and_then(Value::as_object_mut) {
        changed |= compact_string_field(metadata, "close_reason", STORED_DETAIL_MAX_BYTES);
        if let Some(latest) = metadata
            .get_mut("latest_partial_yield")
            .and_then(Value::as_object_mut)
        {
            changed |= compact_string_field(latest, "summary", STORED_DETAIL_MAX_BYTES);
            for key in ["open", "blockers"] {
                let original_items = latest
                    .get(key)
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_default();
                if let Some(items) = latest.get_mut(key) {
                    changed |= compact_string_array(
                        items,
                        STORED_PARTIAL_ITEMS_MAX,
                        STORED_PARTIAL_ITEM_MAX_BYTES,
                    );
                }
                if original_items > STORED_PARTIAL_ITEMS_MAX {
                    latest.insert(format!("{key}_total"), json!(original_items));
                }
            }
        }
    }

    if changed {
        let metadata = map
            .entry("metadata".to_string())
            .or_insert_with(|| json!({}));
        let metadata = object_mut(metadata, "DurableTaskState.metadata")?;
        metadata.insert("storage_compacted".to_string(), Value::Bool(true));
        metadata.insert(
            "storage_original_bytes".to_string(),
            json!(original_json.len()),
        );
    }

    if changed {
        validate_durable_task_state(&compacted, expected_task_id)
            .map_err(|error| format!("compacted task state failed validation: {error}"))?;
    }
    let compacted_bytes = serde_json::to_string_pretty(&compacted)
        .map_err(|error| format!("serialize compacted task state: {error}"))?
        .len();
    if compacted_bytes > max_bytes {
        return Err(format!(
            "typed task state remains too large after safe text compaction ({compacted_bytes} bytes, max {max_bytes}); refusing to discard task graph or evidence"
        ));
    }
    Ok((compacted, changed))
}

fn is_valid_task_status(status: &str) -> bool {
    matches!(
        status,
        "open" | "in_progress" | "blocked" | "completed" | "abandoned"
    )
}

fn is_valid_micro_goal_status(status: &str) -> bool {
    matches!(status, "pending" | "in_progress" | "completed" | "blocked")
}

fn micro_goal_rank(status: &str) -> Option<u8> {
    match status {
        "pending" => Some(0),
        "in_progress" => Some(1),
        "completed" => Some(2),
        "blocked" => Some(3),
        _ => None,
    }
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} must be a JSON object"))
}

fn object_mut<'a>(value: &'a mut Value, label: &str) -> Result<&'a mut Map<String, Value>, String> {
    value
        .as_object_mut()
        .ok_or_else(|| format!("{label} must be a JSON object"))
}

fn required_string<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    label: &str,
) -> Result<&'a str, String> {
    map.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{label}.{key} is required"))
}

fn valid_evidence_refs(value: Option<&Value>) -> bool {
    value.and_then(Value::as_array).is_some_and(|refs| {
        !refs.is_empty()
            && refs.iter().all(|item| match item {
                Value::String(s) => observed_evidence_ref(s),
                Value::Object(map) => {
                    let strings = map.values().filter_map(Value::as_str).collect::<Vec<_>>();
                    !strings.is_empty() && strings.iter().all(|value| observed_evidence_ref(value))
                },
                _ => false,
            })
    })
}

fn observed_evidence_ref(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    ![
        "planned:",
        "planned artifact:",
        "todo:",
        "expected:",
        "proposed:",
        "intended:",
        "future:",
        "will create ",
        "will produce ",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
}

fn validate_micro_goal(value: &Value, label: &str) -> Result<(), String> {
    let map = object(value, label)?;
    required_string(map, "id", label)?;
    required_string(map, "description", label)?;
    let status = required_string(map, "status", label)?;
    if !is_valid_micro_goal_status(status) {
        return Err(format!("{label}.status is invalid: {status}"));
    }
    if status == "completed" && !valid_evidence_refs(map.get("evidence_refs")) {
        return Err(format!(
            "{label}.evidence_refs must be non-empty when status=completed"
        ));
    }
    Ok(())
}

pub fn validate_durable_task_state(
    state: &Value,
    expected_task_id: Option<&str>,
) -> Result<(), String> {
    let map = object(state, "DurableTaskState")?;
    let schema_version = required_string(map, "schema_version", "DurableTaskState")?;
    if schema_version != DURABLE_TASK_STATE_SCHEMA_VERSION {
        return Err(format!(
            "DurableTaskState.schema_version must be {DURABLE_TASK_STATE_SCHEMA_VERSION}"
        ));
    }
    let task_id = required_string(map, "task_id", "DurableTaskState")?;
    if let Some(expected_task_id) = expected_task_id {
        if task_id != expected_task_id {
            return Err(format!(
                "DurableTaskState.task_id mismatch: expected {expected_task_id}, got {task_id}"
            ));
        }
    }
    required_string(map, "goal", "DurableTaskState")?;
    required_string(map, "success_criteria", "DurableTaskState")?;
    required_string(map, "created_at", "DurableTaskState")?;
    required_string(map, "updated_at", "DurableTaskState")?;
    let status = required_string(map, "status", "DurableTaskState")?;
    if !is_valid_task_status(status) {
        return Err(format!("DurableTaskState.status is invalid: {status}"));
    }
    let micro_goals = map
        .get("micro_goals")
        .and_then(Value::as_array)
        .ok_or_else(|| "DurableTaskState.micro_goals must be an array".to_string())?;
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (idx, micro_goal) in micro_goals.iter().enumerate() {
        validate_micro_goal(micro_goal, &format!("DurableTaskState.micro_goals[{idx}]"))?;
        let id = micro_goal
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !ids.insert(id.clone()) {
            return Err(format!("duplicate micro_goal id: {id}"));
        }
    }
    if let Some(active_id) = map.get("active_micro_goal_id").and_then(Value::as_str) {
        if !active_id.trim().is_empty() && !ids.contains(active_id) {
            return Err(format!(
                "DurableTaskState.active_micro_goal_id references unknown id: {active_id}"
            ));
        }
    }
    if status == "completed"
        && micro_goals.iter().any(|goal| {
            !matches!(
                goal.get("status").and_then(Value::as_str),
                Some("completed" | "blocked")
            )
        })
    {
        return Err(
            "DurableTaskState.status=completed requires every micro_goal to be completed or blocked"
                .to_string(),
        );
    }
    Ok(())
}

pub fn synthesize_durable_task_state(
    task_id: &str,
    goal: &str,
    success_criteria: &str,
    agent_id: Option<&str>,
) -> Value {
    let now = Utc::now().to_rfc3339();
    json!({
        "schema_version": DURABLE_TASK_STATE_SCHEMA_VERSION,
        "task_id": task_id,
        "goal": goal,
        "success_criteria": success_criteria,
        "status": "in_progress",
        "created_at": now,
        "updated_at": now,
        "micro_goals": [{
            "id": "mg_initial",
            "description": goal,
            "status": "in_progress",
            "evidence_refs": [],
            "blocked_reason": null,
            "created_at": now,
            "updated_at": now
        }],
        "active_micro_goal_id": "mg_initial",
        "metadata": {
            "source_agent_id": agent_id,
            "delegation_chain": []
        }
    })
}

fn micro_goals_mut(state: &mut Value) -> Result<&mut Vec<Value>, String> {
    object_mut(state, "DurableTaskState")?
        .get_mut("micro_goals")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "DurableTaskState.micro_goals must be an array".to_string())
}

fn find_micro_goal_mut<'a>(
    micro_goals: &'a mut [Value],
    id: &str,
) -> Result<&'a mut Value, String> {
    micro_goals
        .iter_mut()
        .find(|goal| goal.get("id").and_then(Value::as_str) == Some(id))
        .ok_or_else(|| format!("patch references unknown micro_goal id: {id}"))
}

fn ensure_forward_micro_goal_transition(current: &str, next: &str) -> Result<(), String> {
    if next == "blocked" {
        return Ok(());
    }
    let current_rank =
        micro_goal_rank(current).ok_or_else(|| format!("invalid current status: {current}"))?;
    let next_rank = micro_goal_rank(next).ok_or_else(|| format!("invalid next status: {next}"))?;
    if next_rank < current_rank {
        return Err(format!(
            "backward micro_goal transition rejected: {current} -> {next}"
        ));
    }
    Ok(())
}

fn set_updated_at(value: &mut Value, now: &str) {
    if let Some(map) = value.as_object_mut() {
        map.insert("updated_at".to_string(), Value::String(now.to_string()));
    }
}

/// What a model-authored `create` / `patch` may omit and the runtime fills.
///
/// `schema_version`, `task_id` / `expected_task_id`, `created_at` /
/// `updated_at` / `expected_updated_at` are identity and versioning facts the
/// runtime owns; the model was never shown them (the decision prompt renders
/// only the model's half of the contract, and the tool schema is an open
/// object), so from the day the validators required them every model write
/// was rejected — `DurableTaskState.schema_version is required` on every run,
/// and the evidence-gated success contract ran on nothing. The model's own
/// vocabulary is accepted too: the prompt says "one micro-goal per
/// requirement", and the model writes `requirements`. Every fill and mapping
/// is reported so drift stays visible in the log.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TaskStateNormalization {
    pub notes: Vec<String>,
}

impl TaskStateNormalization {
    fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }
}

/// A micro-goal as the model may write it — a bare string, or an object with
/// `description` (or `title` / `text` / `requirement`), an optional `id`,
/// `status`, `evidence_refs`, `blocked_reason` — lifted into the durable
/// shape. `completed` without evidence is not honest yet: it lands as
/// `in_progress` (the validator would reject the whole write; the model can
/// complete it with evidence on a later step).
fn lift_model_micro_goal(
    value: &Value,
    index: usize,
    now: &str,
    label: &str,
    normalization: &mut TaskStateNormalization,
) -> Option<Value> {
    let fallback_id = format!("mg_{}", index + 1);
    let (id, description, status, evidence_refs, blocked_reason) = match value {
        Value::String(text) => (
            fallback_id.clone(),
            text.trim().to_string(),
            "pending".to_string(),
            Vec::new(),
            None,
        ),
        Value::Object(map) => {
            let description = ["description", "title", "text", "requirement", "goal"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let id = map
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    normalization.note(format!("{label}: assigned id {fallback_id}"));
                    fallback_id.clone()
                });
            let status = map
                .get("status")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|status| is_valid_micro_goal_status(status))
                .unwrap_or("pending")
                .to_string();
            let evidence_refs = map
                .get("evidence_refs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let blocked_reason = map
                .get("blocked_reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            (id, description, status, evidence_refs, blocked_reason)
        },
        _ => return None,
    };
    if description.is_empty() {
        normalization.note(format!("{label}: dropped a micro-goal with no description"));
        return None;
    }
    let status = if status == "completed" && evidence_refs.is_empty() {
        normalization.note(format!(
            "{label} ({id}): completed without evidence_refs lands as in_progress"
        ));
        "in_progress".to_string()
    } else {
        status
    };
    Some(json!({
        "id": id,
        "description": description,
        "status": status,
        "evidence_refs": evidence_refs,
        "blocked_reason": blocked_reason,
        "created_at": now,
        "updated_at": now,
    }))
}

/// The model's micro-goal list under either name.
fn model_micro_goal_list(map: &Map<String, Value>) -> Option<(&'static str, &Vec<Value>)> {
    ["micro_goals", "requirements"].into_iter().find_map(|key| {
        map.get(key)
            .and_then(Value::as_array)
            .map(|list| (key, list))
    })
}

/// Lift a model-authored `proposed_taskplan` into a valid `DurableTaskState`:
/// the runtime skeleton (`synthesize_durable_task_state`) carrying the
/// model's micro-goals, status, and notes. Runtime-owned fields win over
/// anything the model wrote for them. A plan with no usable micro-goals keeps
/// the skeleton's single goal.
pub fn normalize_model_proposed_task_state(
    proposed: &Value,
    task_id: &str,
    goal: &str,
    success_criteria: &str,
    agent_id: Option<&str>,
) -> (Value, TaskStateNormalization) {
    let mut normalization = TaskStateNormalization::default();
    let mut state = synthesize_durable_task_state(task_id, goal, success_criteria, agent_id);
    let Some(map) = proposed.as_object() else {
        normalization.note("proposed_taskplan is not an object; runtime skeleton used");
        return (state, normalization);
    };
    let now = state["created_at"].as_str().unwrap_or_default().to_string();
    for key in ["schema_version", "task_id", "created_at", "updated_at"] {
        if let Some(value) = map.get(key) {
            if value != &state[key] {
                normalization.note(format!("{key}: runtime value replaced the model's"));
            }
        }
    }
    if let Some((key, list)) = model_micro_goal_list(map) {
        let lifted: Vec<Value> = list
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                lift_model_micro_goal(
                    item,
                    index,
                    &now,
                    &format!("proposed_taskplan.{key}[{index}]"),
                    &mut normalization,
                )
            })
            .collect();
        if !lifted.is_empty() {
            if key == "requirements" {
                normalization.note("proposed_taskplan.requirements mapped to micro_goals");
            }
            let active = lifted
                .iter()
                .find(|goal| goal["status"] == "in_progress")
                .or_else(|| lifted.first())
                .and_then(|goal| goal["id"].as_str())
                .map(str::to_string);
            state["micro_goals"] = Value::Array(lifted);
            state["active_micro_goal_id"] = active.map(Value::String).unwrap_or(Value::Null);
        }
    }
    if let Some(status) = map.get("status").and_then(Value::as_str) {
        if is_valid_task_status(status) && status != "completed" {
            state["status"] = Value::String(status.to_string());
        }
    }
    if let Some(notes) = map.get("notes") {
        state["metadata"]["notes"] = notes.clone();
    }
    (state, normalization)
}

/// Lift a model-authored patch into a valid `DurableTaskStatePatch` against
/// `current`: stamp the runtime-owned fields when absent, and when the model
/// wrote `micro_goals` / `requirements` / `status` instead of `ops`, translate
/// them — an item whose id exists becomes `set_micro_goal_status` (or an
/// `upsert_micro_goal` when it also rewrites the description), a new item an
/// `upsert_micro_goal`, and a task `status` a `set_status`.
pub fn normalize_model_task_state_patch(
    patch: &Value,
    current: &Value,
    task_id: &str,
) -> (Value, TaskStateNormalization) {
    let mut normalization = TaskStateNormalization::default();
    let mut out = match patch.as_object() {
        Some(map) => map.clone(),
        None => {
            normalization.note("patch is not an object");
            return (patch.clone(), normalization);
        },
    };
    let stamp = |out: &mut Map<String, Value>,
                 key: &str,
                 value: Value,
                 normalization: &mut TaskStateNormalization| {
        let present = out
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.trim().is_empty());
        if !present {
            out.insert(key.to_string(), value);
            normalization.note(format!("{key}: stamped by the runtime"));
        }
    };
    stamp(
        &mut out,
        "schema_version",
        Value::String(DURABLE_TASK_STATE_SCHEMA_VERSION.to_string()),
        &mut normalization,
    );
    stamp(
        &mut out,
        "expected_task_id",
        Value::String(task_id.to_string()),
        &mut normalization,
    );
    if let Some(updated_at) = current.get("updated_at").and_then(Value::as_str) {
        stamp(
            &mut out,
            "expected_updated_at",
            Value::String(updated_at.to_string()),
            &mut normalization,
        );
    }
    let has_ops = out
        .get("ops")
        .and_then(Value::as_array)
        .is_some_and(|ops| !ops.is_empty());
    if !has_ops {
        let now = Utc::now().to_rfc3339();
        let existing: Vec<(String, String)> = current
            .get("micro_goals")
            .and_then(Value::as_array)
            .map(|goals| {
                goals
                    .iter()
                    .filter_map(|goal| {
                        Some((
                            goal.get("id")?.as_str()?.to_string(),
                            goal.get("description")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut ops: Vec<Value> = Vec::new();
        if let Some((key, list)) = model_micro_goal_list(&out) {
            for (index, item) in list.iter().enumerate() {
                let label = format!("patch.{key}[{index}]");
                let Some(lifted) =
                    lift_model_micro_goal(item, index, &now, &label, &mut normalization)
                else {
                    continue;
                };
                let id = lifted["id"].as_str().unwrap_or_default().to_string();
                let description = lifted["description"].as_str().unwrap_or_default();
                match existing.iter().find(|(known, _)| known == &id) {
                    Some((_, known_description)) if known_description == description => {
                        let mut op = json!({
                            "op": "set_micro_goal_status",
                            "id": id,
                            "value": lifted["status"],
                        });
                        if !lifted["evidence_refs"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                        {
                            op["evidence_refs"] = lifted["evidence_refs"].clone();
                        }
                        ops.push(op);
                    },
                    _ => ops.push(json!({ "op": "upsert_micro_goal", "value": lifted })),
                }
            }
            normalization.note(format!("patch.{key} translated to {} op(s)", ops.len()));
        }
        if let Some(status) = out.get("status").and_then(Value::as_str) {
            if is_valid_task_status(status) {
                ops.push(json!({ "op": "set_status", "value": status }));
                normalization.note("patch.status translated to set_status");
            }
        }
        if let Some(reason) = out.get("blocked_reason").and_then(Value::as_str) {
            if let Some(active) = current.get("active_micro_goal_id").and_then(Value::as_str) {
                ops.push(json!({ "op": "set_blocked_reason", "id": active, "value": reason }));
                normalization.note("patch.blocked_reason translated to set_blocked_reason");
            }
        }
        if !ops.is_empty() {
            out.insert("ops".to_string(), Value::Array(ops));
        }
    }
    (Value::Object(out), normalization)
}

pub fn apply_durable_task_state_patch(
    current: &Value,
    patch: &Value,
    expected_task_id: &str,
) -> Result<Value, String> {
    validate_durable_task_state(current, Some(expected_task_id))?;
    let patch_map = object(patch, "DurableTaskStatePatch")?;
    let schema_version = required_string(patch_map, "schema_version", "DurableTaskStatePatch")?;
    if schema_version != DURABLE_TASK_STATE_SCHEMA_VERSION {
        return Err(format!(
            "DurableTaskStatePatch.schema_version must be {DURABLE_TASK_STATE_SCHEMA_VERSION}"
        ));
    }
    let patch_task_id = required_string(patch_map, "expected_task_id", "DurableTaskStatePatch")?;
    if patch_task_id != expected_task_id {
        return Err(format!(
            "DurableTaskStatePatch.expected_task_id mismatch: expected {expected_task_id}, got {patch_task_id}"
        ));
    }
    let current_updated_at = current
        .get("updated_at")
        .and_then(Value::as_str)
        .ok_or_else(|| "current DurableTaskState.updated_at is required".to_string())?;
    let expected_updated_at =
        required_string(patch_map, "expected_updated_at", "DurableTaskStatePatch")?;
    if expected_updated_at != current_updated_at {
        return Err(format!(
            "DurableTaskStatePatch.expected_updated_at mismatch: expected current {current_updated_at}, got {expected_updated_at}"
        ));
    }
    let ops = patch_map
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| "DurableTaskStatePatch.ops must be an array".to_string())?;
    if ops.is_empty() {
        return Err("DurableTaskStatePatch.ops must not be empty".to_string());
    }

    let mut next = current.clone();
    let now = Utc::now().to_rfc3339();
    for (idx, op_value) in ops.iter().enumerate() {
        let label = format!("DurableTaskStatePatch.ops[{idx}]");
        let op = object(op_value, &label)?;
        let op_name = required_string(op, "op", &label)?;
        match op_name {
            "set_status" => {
                let status = required_string(op, "value", &label)?;
                if !is_valid_task_status(status) {
                    return Err(format!("{label}.value is invalid task status: {status}"));
                }
                if status == "completed" {
                    let all_done = next
                        .get("micro_goals")
                        .and_then(Value::as_array)
                        .is_some_and(|goals| {
                            goals.iter().all(|goal| {
                                matches!(
                                    goal.get("status").and_then(Value::as_str),
                                    Some("completed" | "blocked")
                                )
                            })
                        });
                    if !all_done {
                        return Err(
                            "set_status completed requires all micro_goals completed or blocked"
                                .to_string(),
                        );
                    }
                }
                object_mut(&mut next, "DurableTaskState")?
                    .insert("status".to_string(), Value::String(status.to_string()));
            },
            "upsert_micro_goal" => {
                let value = op
                    .get("value")
                    .ok_or_else(|| format!("{label}.value is required"))?
                    .clone();
                validate_micro_goal(&value, &format!("{label}.value"))?;
                let next_id = value.get("id").and_then(Value::as_str).unwrap_or_default();
                let next_status = value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let goals = micro_goals_mut(&mut next)?;
                if let Some(existing) = goals
                    .iter_mut()
                    .find(|goal| goal.get("id").and_then(Value::as_str) == Some(next_id))
                {
                    let current_status = existing
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("pending");
                    ensure_forward_micro_goal_transition(current_status, next_status)?;
                    *existing = value;
                    set_updated_at(existing, &now);
                } else {
                    let mut value = value;
                    set_updated_at(&mut value, &now);
                    goals.push(value);
                }
            },
            "set_micro_goal_status" => {
                let id = required_string(op, "id", &label)?;
                let status = required_string(op, "value", &label)?;
                if !is_valid_micro_goal_status(status) {
                    return Err(format!(
                        "{label}.value is invalid micro_goal status: {status}"
                    ));
                }
                if status == "completed" && !valid_evidence_refs(op.get("evidence_refs")) {
                    return Err(format!(
                        "{label}.evidence_refs must be non-empty when value=completed"
                    ));
                }
                let goals = micro_goals_mut(&mut next)?;
                let goal = find_micro_goal_mut(goals, id)?;
                let current_status = goal
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                ensure_forward_micro_goal_transition(current_status, status)?;
                let goal_map = object_mut(goal, "MicroGoal")?;
                goal_map.insert("status".to_string(), Value::String(status.to_string()));
                if let Some(evidence_refs) = op.get("evidence_refs") {
                    goal_map.insert("evidence_refs".to_string(), evidence_refs.clone());
                }
                goal_map.insert("updated_at".to_string(), Value::String(now.clone()));
            },
            "set_active_micro_goal" => {
                let id = op.get("id").and_then(Value::as_str).map(str::trim);
                if let Some(id) = id {
                    if id.is_empty() {
                        object_mut(&mut next, "DurableTaskState")?
                            .insert("active_micro_goal_id".to_string(), Value::Null);
                        continue;
                    }
                    let known = next
                        .get("micro_goals")
                        .and_then(Value::as_array)
                        .is_some_and(|goals| {
                            goals
                                .iter()
                                .any(|goal| goal.get("id").and_then(Value::as_str) == Some(id))
                        });
                    if !known {
                        return Err(format!(
                            "set_active_micro_goal references unknown micro_goal id: {id}"
                        ));
                    }
                    object_mut(&mut next, "DurableTaskState")?.insert(
                        "active_micro_goal_id".to_string(),
                        Value::String(id.to_string()),
                    );
                } else {
                    object_mut(&mut next, "DurableTaskState")?
                        .insert("active_micro_goal_id".to_string(), Value::Null);
                }
            },
            "set_blocked_reason" => {
                let id = required_string(op, "id", &label)?;
                let reason = required_string(op, "value", &label)?;
                let goals = micro_goals_mut(&mut next)?;
                let goal = find_micro_goal_mut(goals, id)?;
                let goal_map = object_mut(goal, "MicroGoal")?;
                goal_map.insert(
                    "blocked_reason".to_string(),
                    Value::String(reason.to_string()),
                );
                goal_map.insert("status".to_string(), Value::String("blocked".to_string()));
                goal_map.insert("updated_at".to_string(), Value::String(now.clone()));
            },
            other => return Err(format!("{label}.op is unsupported: {other}")),
        }
    }
    object_mut(&mut next, "DurableTaskState")?.insert("updated_at".to_string(), Value::String(now));
    validate_durable_task_state(&next, Some(expected_task_id))?;
    Ok(next)
}

pub fn close_durable_task_state(
    current: &Value,
    expected_task_id: &str,
    terminal_status: &str,
    evidence_refs: &[Value],
    reason: &str,
) -> Result<Value, String> {
    validate_durable_task_state(current, Some(expected_task_id))?;
    if !matches!(terminal_status, "completed" | "blocked" | "abandoned") {
        return Err(format!(
            "close terminal_status must be completed, blocked, or abandoned; got {terminal_status}"
        ));
    }
    let mut next = current.clone();
    let now = Utc::now().to_rfc3339();
    let goals = micro_goals_mut(&mut next)?;
    for goal in goals {
        let status = goal
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if !matches!(status, "pending" | "in_progress") {
            continue;
        }
        let goal_map = object_mut(goal, "MicroGoal")?;
        match terminal_status {
            "completed" => {
                if evidence_refs.is_empty() {
                    return Err(
                        "close completed requires non-empty observed evidence_refs".to_string()
                    );
                }
                goal_map.insert("status".to_string(), Value::String("completed".to_string()));
                goal_map.insert(
                    "evidence_refs".to_string(),
                    Value::Array(evidence_refs.to_vec()),
                );
                goal_map.insert("blocked_reason".to_string(), Value::Null);
            },
            "blocked" | "abandoned" => {
                goal_map.insert("status".to_string(), Value::String("blocked".to_string()));
                goal_map.insert(
                    "blocked_reason".to_string(),
                    Value::String(reason.trim().to_string()),
                );
            },
            _ => unreachable!("terminal status validated above"),
        }
        goal_map.insert("updated_at".to_string(), Value::String(now.clone()));
    }
    let map = object_mut(&mut next, "DurableTaskState")?;
    map.insert(
        "status".to_string(),
        Value::String(terminal_status.to_string()),
    );
    map.insert("active_micro_goal_id".to_string(), Value::Null);
    map.insert("updated_at".to_string(), Value::String(now));
    let metadata = map
        .entry("metadata".to_string())
        .or_insert_with(|| json!({}));
    let metadata_map = object_mut(metadata, "DurableTaskState.metadata")?;
    metadata_map.insert(
        "close_reason".to_string(),
        Value::String(reason.trim().to_string()),
    );
    metadata_map.insert(
        "close_evidence_refs".to_string(),
        Value::Array(evidence_refs.to_vec()),
    );
    validate_durable_task_state(&next, Some(expected_task_id))?;
    Ok(next)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_none_action_envelope() {
        let parsed = parse_task_state_action_value(Some(&json!({
            "action": "none",
            "reason": "Short self-contained step.",
            "confidence": 0.9,
            "next_review": "next_outer_iteration"
        })))
        .expect("valid envelope");

        assert_eq!(parsed.action, TaskStateActionKind::NoneAction);
        assert_eq!(parsed.reason, "Short self-contained step.");
    }

    #[test]
    fn rejects_missing_required_envelope() {
        let err = parse_task_state_action_value(None).expect_err("missing envelope rejected");

        assert!(err.contains("missing required task_state_action"));
    }

    #[test]
    fn rejects_blank_reason() {
        let err = parse_task_state_action_value(Some(&json!({
            "action": "patch",
            "reason": " "
        })))
        .expect_err("blank reason rejected");

        assert!(err.contains("reason is required"));
    }

    #[test]
    fn rejects_payload_on_none_action() {
        let err = parse_task_state_action_value(Some(&json!({
            "action": "none",
            "reason": "No durable update.",
            "patch": {"ops": []}
        })))
        .expect_err("none cannot carry patch");

        assert!(err.contains("patch must be null"));
    }

    #[test]
    fn synthesized_state_matches_durable_schema() {
        let state = synthesize_durable_task_state(
            "task-1",
            "Finish all tests",
            "All tests pass",
            Some("agent-a"),
        );

        validate_durable_task_state(&state, Some("task-1")).unwrap();
        assert_eq!(state["metadata"]["source_agent_id"], json!("agent-a"));
    }

    #[test]
    fn oversized_harness_goal_compacts_without_losing_task_graph_identity() {
        let goal = format!(
            "USER INTENT: explain the repository. {} GOAL TAIL",
            "g".repeat(2_800)
        );
        let success = format!(
            "SUCCESS: return a read-only explanation. {} SUCCESS TAIL",
            "s".repeat(2_700)
        );
        let mut state = synthesize_durable_task_state("task-1", &goal, &success, Some("cto"));
        state["metadata"]["latest_partial_yield"] = json!({
            "summary": format!("{} SUMMARY TAIL", "p".repeat(900)),
            "open": [format!("{} OPEN TAIL", "o".repeat(500))],
            "blockers": [format!("{} BLOCKER TAIL", "b".repeat(500))],
            "recorded_at": "2026-08-15T22:27:00Z"
        });

        let original_bytes = serde_json::to_string_pretty(&state).unwrap().len();
        assert!(original_bytes > 4_096);
        let (compacted, changed) = compact_durable_task_state_for_storage(&state, 4_096).unwrap();
        let encoded = serde_json::to_string_pretty(&compacted).unwrap();

        assert!(changed);
        assert!(encoded.len() <= 4_096, "compacted bytes={}", encoded.len());
        validate_durable_task_state(&compacted, Some("task-1")).unwrap();
        assert_eq!(compacted["task_id"], json!("task-1"));
        assert_eq!(compacted["status"], json!("in_progress"));
        assert_eq!(compacted["active_micro_goal_id"], json!("mg_initial"));
        assert_eq!(compacted["micro_goals"].as_array().unwrap().len(), 1);
        assert_eq!(compacted["micro_goals"][0]["id"], json!("mg_initial"));
        assert!(compacted["goal"].as_str().unwrap().contains("USER INTENT"));
        assert!(compacted["goal"].as_str().unwrap().contains("GOAL TAIL"));
        assert_eq!(compacted["metadata"]["storage_compacted"], json!(true));
        assert_eq!(
            compacted["metadata"]["storage_original_bytes"],
            json!(original_bytes)
        );
    }

    #[test]
    fn structural_overflow_fails_closed_instead_of_dropping_micro_goals() {
        let mut state = synthesize_durable_task_state("task-1", "goal", "done", Some("cto"));
        let template = state["micro_goals"][0].clone();
        let goals = state["micro_goals"].as_array_mut().unwrap();
        for index in 1..80 {
            let mut goal = template.clone();
            goal["id"] = json!(format!("mg_{index}"));
            goal["description"] = json!(format!("independent preserved graph node {index}"));
            goal["status"] = json!("pending");
            goals.push(goal);
        }

        validate_durable_task_state(&state, Some("task-1")).unwrap();
        let error = compact_durable_task_state_for_storage(&state, 4_096)
            .expect_err("graph nodes must never be trimmed to satisfy storage");
        assert!(error.contains("refusing to discard task graph or evidence"));
        assert_eq!(state["micro_goals"].as_array().unwrap().len(), 80);
    }

    #[test]
    fn applies_micro_goal_completion_patch() {
        let current = synthesize_durable_task_state("task-1", "Finish all tests", "Done", None);
        let updated_at = current["updated_at"].as_str().unwrap();
        let patch = json!({
            "schema_version": "1.0",
            "expected_task_id": "task-1",
            "expected_updated_at": updated_at,
            "ops": [{
                "op": "set_micro_goal_status",
                "id": "mg_initial",
                "value": "completed",
                "evidence_refs": ["runtime_ledger:iter-3"]
            }]
        });

        let next = apply_durable_task_state_patch(&current, &patch, "task-1").unwrap();

        assert_eq!(next["micro_goals"][0]["status"], json!("completed"));
        assert!(next["updated_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
    }

    #[test]
    fn rejects_completed_micro_goal_without_evidence() {
        let current = synthesize_durable_task_state("task-1", "Finish all tests", "Done", None);
        let updated_at = current["updated_at"].as_str().unwrap();
        let patch = json!({
            "schema_version": "1.0",
            "expected_task_id": "task-1",
            "expected_updated_at": updated_at,
            "ops": [{
                "op": "set_micro_goal_status",
                "id": "mg_initial",
                "value": "completed"
            }]
        });

        let err = apply_durable_task_state_patch(&current, &patch, "task-1")
            .expect_err("completion evidence is required");

        assert!(err.contains("evidence_refs"));
    }

    #[test]
    fn rejects_prospective_evidence_for_completed_micro_goal() {
        let current = synthesize_durable_task_state("task-1", "Ship the digest", "Done", None);
        let updated_at = current["updated_at"].as_str().unwrap();
        let patch = json!({
            "schema_version": "1.0",
            "expected_task_id": "task-1",
            "expected_updated_at": updated_at,
            "ops": [{
                "op": "set_micro_goal_status",
                "id": "mg_initial",
                "value": "completed",
                "evidence_refs": ["planned artifact: weekly_digest.md"]
            }]
        });

        let err = apply_durable_task_state_patch(&current, &patch, "task-1")
            .expect_err("planned work is not completion evidence");
        assert!(err.contains("evidence_refs"));
    }

    #[test]
    fn close_completed_resolves_open_micro_goals_with_observed_evidence() {
        let current = synthesize_durable_task_state("task-1", "Finish", "Done", None);

        let next = close_durable_task_state(
            &current,
            "task-1",
            "completed",
            &[json!("terminal-yield:exec-1:iteration-3")],
            "Accepted terminal yield",
        )
        .expect("completed close should resolve the synthetic baseline");

        assert_eq!(next["status"], json!("completed"));
        assert_eq!(next["micro_goals"][0]["status"], json!("completed"));
        assert_eq!(
            next["micro_goals"][0]["evidence_refs"][0],
            json!("terminal-yield:exec-1:iteration-3")
        );
        assert!(next["active_micro_goal_id"].is_null());
    }

    #[test]
    fn close_completed_rejects_open_micro_goals_without_evidence() {
        let current = synthesize_durable_task_state("task-1", "Finish", "Done", None);

        let error =
            close_durable_task_state(&current, "task-1", "completed", &[], "Unsupported close")
                .expect_err("an open goal cannot be completed without observed evidence");

        assert!(error.contains("non-empty observed evidence_refs"));
        assert_eq!(current["micro_goals"][0]["status"], json!("in_progress"));
    }

    #[test]
    fn accepts_observed_canonical_evidence_references() {
        for reference in [
            "runtime_ledger:iter-3",
            "artifact:weekly_digest.md",
            "tool_call:create_task:call-7",
        ] {
            assert!(
                valid_evidence_refs(Some(&json!([reference]))),
                "{reference} should be accepted as observed evidence"
            );
        }
    }

    // ---- model-authored envelopes, as run 7 (2026-09-20) actually sent them ----

    #[test]
    fn a_models_requirements_create_becomes_a_valid_durable_state() {
        // The prompt says "one micro-goal per requirement"; the model wrote
        // `requirements`, and no schema_version / task_id / timestamps.
        let proposed = json!({
            "requirements": [
                "Set route BLR to DEL and date 10 Oct 2026, one way, 1 adult",
                {"description": "Read the three lowest fares", "status": "in_progress"},
                {"id": "mg_report", "description": "Report airline, times, stops, fare", "status": "completed"}
            ],
            "status": "in_progress",
            "notes": "Google Flights"
        });
        let (state, normalization) = normalize_model_proposed_task_state(
            &proposed,
            "task_1",
            "goal",
            "criteria",
            Some("personal-assistant"),
        );
        validate_durable_task_state(&state, Some("task_1")).expect("valid after normalization");
        assert_eq!(state["schema_version"], json!("1.0"));
        assert_eq!(state["task_id"], json!("task_1"));
        let goals = state["micro_goals"].as_array().expect("array");
        assert_eq!(goals.len(), 3);
        assert_eq!(goals[0]["id"], json!("mg_1"));
        assert_eq!(goals[0]["status"], json!("pending"));
        assert_eq!(goals[1]["status"], json!("in_progress"));
        // completed without evidence is not honest yet: lands as in_progress
        assert_eq!(goals[2]["id"], json!("mg_report"));
        assert_eq!(goals[2]["status"], json!("in_progress"));
        assert_eq!(state["active_micro_goal_id"], json!("mg_2"));
        assert_eq!(state["metadata"]["notes"], json!("Google Flights"));
        assert!(normalization
            .notes
            .iter()
            .any(|n| n.contains("requirements mapped")));
        assert!(normalization
            .notes
            .iter()
            .any(|n| n.contains("completed without evidence_refs")));
    }

    #[test]
    fn a_models_bare_micro_goals_patch_becomes_ops_and_is_stamped() {
        let current = synthesize_durable_task_state("task_1", "goal", "criteria", None);
        // The model wrote `patch: { micro_goals: [...] }` — no schema_version,
        // no expected_task_id / expected_updated_at, no ops.
        let patch = json!({
            "micro_goals": [
                {"id": "mg_initial", "description": "goal", "status": "completed", "evidence_refs": ["tool_call_evidence:abc"]},
                {"id": "mg_fares", "description": "Read the three lowest fares", "status": "pending"}
            ]
        });
        let (normalized, normalization) =
            normalize_model_task_state_patch(&patch, &current, "task_1");
        assert_eq!(normalized["schema_version"], json!("1.0"));
        assert_eq!(normalized["expected_task_id"], json!("task_1"));
        assert_eq!(normalized["expected_updated_at"], current["updated_at"]);
        let ops = normalized["ops"].as_array().expect("ops");
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0]["op"], json!("set_micro_goal_status"));
        assert_eq!(ops[0]["id"], json!("mg_initial"));
        assert_eq!(ops[0]["value"], json!("completed"));
        assert_eq!(ops[1]["op"], json!("upsert_micro_goal"));
        assert_eq!(ops[1]["value"]["id"], json!("mg_fares"));
        assert!(normalization.notes.iter().any(|n| n.contains("stamped")));
        // And the stamped patch applies.
        let next =
            apply_durable_task_state_patch(&current, &normalized, "task_1").expect("applies");
        let goals = next["micro_goals"].as_array().unwrap();
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0]["status"], json!("completed"));
    }

    #[test]
    fn a_patch_that_already_carries_ops_and_stamps_is_left_alone() {
        let current = synthesize_durable_task_state("task_1", "goal", "criteria", None);
        let patch = json!({
            "schema_version": "1.0",
            "expected_task_id": "task_1",
            "expected_updated_at": current["updated_at"],
            "ops": [{"op": "set_status", "value": "in_progress"}]
        });
        let (normalized, normalization) =
            normalize_model_task_state_patch(&patch, &current, "task_1");
        assert_eq!(normalized, patch);
        assert!(normalization.notes.is_empty());
    }

    #[test]
    fn a_create_with_no_usable_goals_keeps_the_runtime_skeleton() {
        let (state, _) = normalize_model_proposed_task_state(
            &json!({"requirements": [{"status": "pending"}]}),
            "task_1",
            "goal",
            "criteria",
            None,
        );
        validate_durable_task_state(&state, Some("task_1")).expect("valid");
        assert_eq!(state["micro_goals"][0]["id"], json!("mg_initial"));
    }
}
