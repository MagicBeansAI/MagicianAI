//! Agent update event vocabulary.
//!
//! Typed, internally-tagged enum of every semantic event an agent can emit
//! for the operator-facing feed. Persisted to per-workspace JSONL logs in
//! Phase 1 of the agentic UI semantic-layer plan
//! (`docs/plans/2026-04-23-agentic-ui-semantic-layer.md`).
//!
//! This module defines vocabulary only. No emissions, no wire changes, no UI
//! side effects. Adoption happens in later steps of the plan.
//!
//! ## Wire shape
//!
//! ```json
//! {
//!   "id": "01HX...",
//!   "ts": 1745432100123,
//!   "workspace_id": "ws_a",
//!   "agent_id": "cfo",
//!   "cycle_id": "cyc_42",
//!   "kind": "cycle_completed",
//!   "outcome": "succeeded",
//!   "duration_ms": 1234
//! }
//! ```
//!
//! `kind` is the internal tag for [`AgentUpdateKind`]; variant fields flatten
//! into the top-level object alongside the envelope fields.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::approval_service::ApprovalDecision;
use super::types::{AgentId, AgentKind, CycleId, GoalId};
use crate::magician_v2::realtime_events::RuntimeAgentEventType;

// --------------------------------------------------------------------------
// Id aliases
// --------------------------------------------------------------------------

pub type ApprovalId = String;
pub type ArtifactId = String;
pub type TaskId = String;
pub type WorkspaceId = String;
pub type ThreadId = String;

// --------------------------------------------------------------------------
// Envelope
// --------------------------------------------------------------------------

/// A single operator-facing agent update event.
///
/// Envelope fields describe the scope (workspace, agent, thread, cycle). The
/// [`AgentUpdateKind`] body carries the semantic payload and is flattened
/// into the serialized representation so the wire shape is a flat object with
/// a `kind` discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentUpdate {
    pub id: String,
    pub ts: i64,
    pub workspace_id: WorkspaceId,
    pub agent_id: AgentId,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub thread_id: Option<ThreadId>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cycle_id: Option<CycleId>,
    #[serde(flatten)]
    pub body: AgentUpdateKind,
}

impl AgentUpdate {
    /// Construct a new update with a freshly generated ULID and current
    /// timestamp. Envelope scope (workspace, agent) is required; thread and
    /// cycle are optional via [`with_thread`] / [`with_cycle`].
    pub fn new(workspace_id: WorkspaceId, agent_id: AgentId, body: AgentUpdateKind) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            ts: current_unix_millis(),
            workspace_id,
            agent_id,
            thread_id: None,
            cycle_id: None,
            body,
        }
    }

    pub fn with_thread(mut self, thread_id: ThreadId) -> Self {
        self.thread_id = Some(thread_id);
        self
    }

    pub fn with_cycle(mut self, cycle_id: CycleId) -> Self {
        self.cycle_id = Some(cycle_id);
        self
    }
}

fn current_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --------------------------------------------------------------------------
// Support types
// --------------------------------------------------------------------------

/// Summary outcome of a cycle for feed display.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CycleOutcome {
    Succeeded,
    Failed,
    Paused,
    PartiallySucceeded,
}

/// Reference to an artifact produced or touched by an agent event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub artifact_id: ArtifactId,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub surface_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub label: Option<String>,
}

// --------------------------------------------------------------------------
// AgentUpdateKind
// --------------------------------------------------------------------------

/// Every operator-facing event an agent can emit. New variants must be
/// deliberate — see the plan doc for the operator/ops split.
///
/// Serialized as internally-tagged on `kind`, flattened into the envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentUpdateKind {
    // ---- Agent lifecycle -------------------------------------------------
    AgentCreated {
        name: String,
        agent_kind: AgentKind,
    },
    AgentUpdated {
        changed_fields: Vec<String>,
    },
    AgentDeleted,
    AgentPaused {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        reason: Option<String>,
    },
    AgentResumed,

    // ---- Cycle -----------------------------------------------------------
    CycleStarted {
        #[serde(skip_serializing_if = "Option::is_none", default)]
        focus_area: Option<String>,
        trigger: String,
    },
    CycleCompleted {
        outcome: CycleOutcome,
        duration_ms: u64,
    },
    CycleFailed {
        error: String,
        duration_ms: u64,
    },
    CyclePaused {
        reason: String,
    },

    // ---- Goal ------------------------------------------------------------
    GoalCompleted {
        goal_id: GoalId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<ArtifactRef>,
    },
    GoalFailed {
        goal_id: GoalId,
        error: String,
    },
    GoalRecovered {
        goal_id: GoalId,
        recovery_strategy: String,
    },

    // ---- Approval --------------------------------------------------------
    ApprovalRequested {
        approval_id: ApprovalId,
        /// Tool name (e.g. `"fs.write"`) when the approval is tied to a
        /// specific tool invocation. Optional because legacy emitters that
        /// only carry approval metadata (not the triggering action) supply
        /// `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        /// Action verb (e.g. `"create"`, `"overwrite"`). Optional for the
        /// same reason as `tool`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        /// Serialized action parameters when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<Value>,
        /// Count of pending approval actions in the originating request.
        /// Carried when the approval is bundled (multiple actions await a
        /// single decision).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_action_count: Option<u32>,
    },
    ApprovalResolved {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        resolver: String,
    },
    ApprovalExpired {
        approval_id: ApprovalId,
    },

    // ---- Artifact --------------------------------------------------------
    ArtifactCreated {
        artifact: ArtifactRef,
    },
    ArtifactCreateFailed {
        attempted_kind: String,
        error: String,
    },

    // ---- Circuit breaker -------------------------------------------------
    CircuitOpened {
        scope: String,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        next_retry_at: Option<i64>,
    },
    CircuitRecovered {
        scope: String,
        recovered_at: i64,
    },

    // ---- Task ------------------------------------------------------------
    TaskCreated {
        task_id: TaskId,
        title: String,
    },
    TaskUpdated {
        task_id: TaskId,
        changed_fields: Vec<String>,
    },
    TaskCompleted {
        task_id: TaskId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<ArtifactRef>,
    },
    TaskFailed {
        task_id: TaskId,
        error: String,
    },

    // ---- Memory ----------------------------------------------------------
    MemoryReport {
        summary: String,
        episodes_processed: u32,
        corrections_applied: u32,
    },

    // ---- Feedback --------------------------------------------------------
    FeedbackGenerated {
        corrections: u32,
        success_patterns: u32,
    },

    // ---- Tier consolidation ---------------------------------------------
    TierConsolidated {
        source: String,
        target: String,
        records_written: u32,
        rule: String,
    },

    // ---- Delegation ------------------------------------------------------
    DelegationIssued {
        from_agent: AgentId,
        to_agent: AgentId,
        task_id: TaskId,
    },
    DelegationResolved {
        from_agent: AgentId,
        to_agent: AgentId,
        task_id: TaskId,
        outcome: String,
    },

    // ---- Health ----------------------------------------------------------
    FeedStalled {
        silent_for_ms: u64,
    },

    // ---- Publication -----------------------------------------------------
    PublishedSurfaceChanged {
        surface_id: String,
        url: String,
    },
}

/// Translate a legacy stringly-typed agent event (event_type + JSON payload)
/// into an [`AgentUpdate`] envelope suitable for the operator feed.
///
/// Returns `None` for event types handled by inline emission elsewhere
/// (cycle.*, approval.*) — those already fire their own `AgentUpdate` next
/// to the legacy emission. This function covers event types that flow
/// through the derived-events pipeline (goal.*, circuit.*, memory.*,
/// lifecycle.*) where inline emission would require editing many call sites.
///
/// Placed here so the event_type → variant mapping lives next to the enum.
pub fn agent_update_from_legacy_event(
    event_type: &str,
    payload: &serde_json::Value,
    agent_id: &str,
    workspace_id: &str,
) -> Option<AgentUpdate> {
    // Skip event types handled by inline emission to avoid double-firing.
    // `agent.cycle.started` / `agent.cycle.completed` are canonical
    // `ArtifactV2EventType` variants emitted directly by the canonical
    // event registry; `cycle.completed` is a non-typed legacy alias.
    // `hitl.requested` / `hitl.resolved` (canonical HITL lifecycle —
    // every source: approval, user_request, clarification, agentic)
    // are also emitted inline. The 6 legacy projection event_types
    // (approval.requested / approval.resolved / approval.expired etc.)
    // are gone — only the canonical `hitl.*` strings reach this code.
    let typed = RuntimeAgentEventType::from_str(event_type);
    if matches!(
        typed,
        Some(RuntimeAgentEventType::AgentCycleFailed | RuntimeAgentEventType::AgentCyclePaused)
    ) || matches!(
        event_type,
        "agent.cycle.started"
            | "agent.cycle.completed"
            | "cycle.completed"
            | "hitl.requested"
            | "hitl.resolved"
    ) {
        return None;
    }

    let cycle_id = payload
        .get("cycle_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let kind = match typed {
        Some(RuntimeAgentEventType::AgentGoalCompleted) => {
            let goal_id = payload.get("goal_id").and_then(|v| v.as_str())?.to_string();
            AgentUpdateKind::GoalCompleted {
                goal_id,
                artifacts: Vec::new(),
            }
        },
        Some(RuntimeAgentEventType::AgentGoalFailed) => {
            let goal_id = payload.get("goal_id").and_then(|v| v.as_str())?.to_string();
            let error = payload
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("goal_failed")
                .to_string();
            AgentUpdateKind::GoalFailed { goal_id, error }
        },
        Some(RuntimeAgentEventType::AgentGoalRecovered) => {
            let goal_id = payload.get("goal_id").and_then(|v| v.as_str())?.to_string();
            let recovery_strategy = payload
                .get("after_failures")
                .and_then(|v| v.as_i64())
                .map(|n| format!("recovered after {} failure(s)", n))
                .unwrap_or_else(|| "recovered".to_string());
            AgentUpdateKind::GoalRecovered {
                goal_id,
                recovery_strategy,
            }
        },
        Some(RuntimeAgentEventType::AgentCircuitOpened) => {
            let scope = payload
                .get("goal_id")
                .and_then(|v| v.as_str())
                .map(|g| format!("{}:{}", agent_id, g))
                .unwrap_or_else(|| agent_id.to_string());
            let reason = payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| {
                    payload
                        .get("failure_count")
                        .and_then(|v| v.as_i64())
                        .map(|_| "consecutive_failures_threshold")
                        .unwrap_or("circuit_opened")
                })
                .to_string();
            AgentUpdateKind::CircuitOpened {
                scope,
                reason,
                next_retry_at: None,
            }
        },
        Some(RuntimeAgentEventType::AgentCircuitRecovered) => {
            let scope = payload
                .get("goal_id")
                .and_then(|v| v.as_str())
                .map(|g| format!("{}:{}", agent_id, g))
                .unwrap_or_else(|| agent_id.to_string());
            let recovered_at = payload
                .get("recovered_at")
                .and_then(|v| v.as_i64())
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
            AgentUpdateKind::CircuitRecovered {
                scope,
                recovered_at,
            }
        },
        Some(RuntimeAgentEventType::AgentMemoryReport) => {
            let summary = payload
                .get("summary")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let episodes_processed = payload
                .get("episodes_processed")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let corrections_applied = payload
                .get("corrections_applied")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            AgentUpdateKind::MemoryReport {
                summary,
                episodes_processed,
                corrections_applied,
            }
        },
        Some(RuntimeAgentEventType::AgentFeedbackGenerated) => {
            let corrections = payload
                .get("corrections")
                .and_then(|v| v.as_u64())
                .or_else(|| payload.get("corrections_count").and_then(|v| v.as_u64()))
                .unwrap_or(0) as u32;
            let success_patterns = payload
                .get("success_patterns")
                .and_then(|v| v.as_u64())
                .or_else(|| {
                    payload
                        .get("success_patterns_count")
                        .and_then(|v| v.as_u64())
                })
                .unwrap_or(0) as u32;
            AgentUpdateKind::FeedbackGenerated {
                corrections,
                success_patterns,
            }
        },
        Some(RuntimeAgentEventType::AgentCreated) => {
            let name = payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(agent_id)
                .to_string();
            let agent_kind = match payload.get("kind").and_then(|v| v.as_str()) {
                Some("worker") => AgentKind::Worker,
                _ => AgentKind::Personal,
            };
            AgentUpdateKind::AgentCreated { name, agent_kind }
        },
        Some(RuntimeAgentEventType::AgentUpdated) => {
            let changed_fields: Vec<String> = payload
                .get("changed_fields")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            AgentUpdateKind::AgentUpdated { changed_fields }
        },
        Some(RuntimeAgentEventType::AgentDeleted) => AgentUpdateKind::AgentDeleted,
        Some(RuntimeAgentEventType::AgentPaused) => {
            let reason = payload
                .get("reason")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            AgentUpdateKind::AgentPaused { reason }
        },
        Some(RuntimeAgentEventType::AgentResumed) => AgentUpdateKind::AgentResumed,
        _ if event_type == "published_surface.changed" => {
            let surface_id = payload
                .get("surface_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let url = payload
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            AgentUpdateKind::PublishedSurfaceChanged { surface_id, url }
        },
        _ => return None,
    };

    let mut update = AgentUpdate::new(workspace_id.to_string(), agent_id.to_string(), kind);
    if let Some(cid) = cycle_id {
        if !cid.is_empty() {
            update = update.with_cycle(cid);
        }
    }
    Some(update)
}

impl AgentUpdateKind {
    /// Stable wire tag for this variant.
    ///
    /// The compiler enforces exhaustiveness here. Adding a new variant
    /// without adding an arm is a build break — which is the whole point.
    /// If you hit that break, decide whether the new variant belongs in the
    /// operator-facing feed at all (per the plan doc) before extending.
    pub fn variant_tag(&self) -> &'static str {
        match self {
            Self::AgentCreated { .. } => "agent_created",
            Self::AgentUpdated { .. } => "agent_updated",
            Self::AgentDeleted => "agent_deleted",
            Self::AgentPaused { .. } => "agent_paused",
            Self::AgentResumed => "agent_resumed",
            Self::CycleStarted { .. } => "cycle_started",
            Self::CycleCompleted { .. } => "cycle_completed",
            Self::CycleFailed { .. } => "cycle_failed",
            Self::CyclePaused { .. } => "cycle_paused",
            Self::GoalCompleted { .. } => "goal_completed",
            Self::GoalFailed { .. } => "goal_failed",
            Self::GoalRecovered { .. } => "goal_recovered",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::ApprovalExpired { .. } => "approval_expired",
            Self::ArtifactCreated { .. } => "artifact_created",
            Self::ArtifactCreateFailed { .. } => "artifact_create_failed",
            Self::CircuitOpened { .. } => "circuit_opened",
            Self::CircuitRecovered { .. } => "circuit_recovered",
            Self::TaskCreated { .. } => "task_created",
            Self::TaskUpdated { .. } => "task_updated",
            Self::TaskCompleted { .. } => "task_completed",
            Self::TaskFailed { .. } => "task_failed",
            Self::MemoryReport { .. } => "memory_report",
            Self::FeedbackGenerated { .. } => "feedback_generated",
            Self::TierConsolidated { .. } => "tier_consolidated",
            Self::DelegationIssued { .. } => "delegation_issued",
            Self::DelegationResolved { .. } => "delegation_resolved",
            Self::FeedStalled { .. } => "feed_stalled",
            Self::PublishedSurfaceChanged { .. } => "published_surface_changed",
        }
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    /// All variants with representative payloads, used to drive the
    /// roundtrip and wire-tag tests.
    fn all_variants() -> Vec<AgentUpdateKind> {
        vec![
            AgentUpdateKind::AgentCreated {
                name: "CFO".into(),
                agent_kind: AgentKind::Personal,
            },
            AgentUpdateKind::AgentUpdated {
                changed_fields: vec!["persona".into(), "tools".into()],
            },
            AgentUpdateKind::AgentDeleted,
            AgentUpdateKind::AgentPaused {
                reason: Some("manual".into()),
            },
            AgentUpdateKind::AgentResumed,
            AgentUpdateKind::CycleStarted {
                focus_area: Some("daily_review".into()),
                trigger: "cron".into(),
            },
            AgentUpdateKind::CycleCompleted {
                outcome: CycleOutcome::Succeeded,
                duration_ms: 1234,
            },
            AgentUpdateKind::CycleFailed {
                error: "boom".into(),
                duration_ms: 500,
            },
            AgentUpdateKind::CyclePaused {
                reason: "budget exhausted".into(),
            },
            AgentUpdateKind::GoalCompleted {
                goal_id: "g1".into(),
                artifacts: vec![],
            },
            AgentUpdateKind::GoalFailed {
                goal_id: "g1".into(),
                error: "boom".into(),
            },
            AgentUpdateKind::GoalRecovered {
                goal_id: "g1".into(),
                recovery_strategy: "retry".into(),
            },
            AgentUpdateKind::ApprovalRequested {
                approval_id: "ap_1".into(),
                tool: Some("fs.write".into()),
                action: Some("create".into()),
                params: Some(json!({"path": "/tmp/x"})),
                pending_action_count: Some(1),
            },
            AgentUpdateKind::ApprovalResolved {
                approval_id: "ap_1".into(),
                decision: ApprovalDecision::Approve,
                resolver: "user:alice".into(),
            },
            AgentUpdateKind::ApprovalExpired {
                approval_id: "ap_1".into(),
            },
            AgentUpdateKind::ArtifactCreated {
                artifact: ArtifactRef {
                    artifact_id: "art_1".into(),
                    kind: "persistent".into(),
                    surface_url: Some("/surface/123".into()),
                    label: Some("Report".into()),
                },
            },
            AgentUpdateKind::ArtifactCreateFailed {
                attempted_kind: "persistent".into(),
                error: "disk full".into(),
            },
            AgentUpdateKind::CircuitOpened {
                scope: "tool.fs.write".into(),
                reason: "too_many_failures".into(),
                next_retry_at: Some(1_745_432_200_000),
            },
            AgentUpdateKind::CircuitRecovered {
                scope: "tool.fs.write".into(),
                recovered_at: 1_745_432_200_000,
            },
            AgentUpdateKind::TaskCreated {
                task_id: "t_1".into(),
                title: "Draft report".into(),
            },
            AgentUpdateKind::TaskUpdated {
                task_id: "t_1".into(),
                changed_fields: vec!["status".into()],
            },
            AgentUpdateKind::TaskCompleted {
                task_id: "t_1".into(),
                artifacts: vec![],
            },
            AgentUpdateKind::TaskFailed {
                task_id: "t_1".into(),
                error: "timeout".into(),
            },
            AgentUpdateKind::MemoryReport {
                summary: "10 episodes".into(),
                episodes_processed: 10,
                corrections_applied: 2,
            },
            AgentUpdateKind::FeedbackGenerated {
                corrections: 3,
                success_patterns: 7,
            },
            AgentUpdateKind::TierConsolidated {
                source: "episodes".into(),
                target: "knowledge".into(),
                records_written: 5,
                rule: "weekly_summary".into(),
            },
            AgentUpdateKind::DelegationIssued {
                from_agent: "cfo".into(),
                to_agent: "analyst".into(),
                task_id: "t_1".into(),
            },
            AgentUpdateKind::DelegationResolved {
                from_agent: "cfo".into(),
                to_agent: "analyst".into(),
                task_id: "t_1".into(),
                outcome: "completed".into(),
            },
            AgentUpdateKind::FeedStalled {
                silent_for_ms: 600_000,
            },
            AgentUpdateKind::PublishedSurfaceChanged {
                surface_id: "surf_42".into(),
                url: "/surface/42".into(),
            },
        ]
    }

    fn envelope_with(body: AgentUpdateKind) -> AgentUpdate {
        AgentUpdate {
            id: "01HXTEST000000000000000000".into(),
            ts: 1_745_432_100_123,
            workspace_id: "ws_a".into(),
            agent_id: "cfo".into(),
            thread_id: None,
            cycle_id: None,
            body,
        }
    }

    #[test]
    fn every_variant_roundtrips() {
        for variant in all_variants() {
            let env = envelope_with(variant.clone());
            let json = serde_json::to_string(&env).expect("serialize");
            let parsed: AgentUpdate = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(parsed, env, "roundtrip mismatch for {:?}", variant);
        }
    }

    #[test]
    fn variant_tag_matches_wire_kind() {
        for variant in all_variants() {
            let value = serde_json::to_value(&variant).expect("serialize");
            let wire_kind = value
                .get("kind")
                .and_then(|k| k.as_str())
                .expect("kind field present");
            assert_eq!(
                wire_kind,
                variant.variant_tag(),
                "variant_tag() drifted from serde tag for {:?}",
                variant,
            );
        }
    }

    #[test]
    fn wire_shape_cycle_completed() {
        let update = AgentUpdate {
            id: "01HXTEST000000000000000000".into(),
            ts: 1_745_432_100_123,
            workspace_id: "ws_a".into(),
            agent_id: "cfo".into(),
            thread_id: None,
            cycle_id: Some("cyc_42".into()),
            body: AgentUpdateKind::CycleCompleted {
                outcome: CycleOutcome::Succeeded,
                duration_ms: 1234,
            },
        };
        let expected = json!({
            "id": "01HXTEST000000000000000000",
            "ts": 1_745_432_100_123_i64,
            "workspace_id": "ws_a",
            "agent_id": "cfo",
            "cycle_id": "cyc_42",
            "kind": "cycle_completed",
            "outcome": "succeeded",
            "duration_ms": 1234
        });
        assert_eq!(serde_json::to_value(&update).unwrap(), expected);
    }

    #[test]
    fn wire_shape_approval_requested() {
        let update = AgentUpdate {
            id: "01HXTEST000000000000000001".into(),
            ts: 1_745_432_100_123,
            workspace_id: "ws_a".into(),
            agent_id: "cfo".into(),
            thread_id: Some("th_1".into()),
            cycle_id: None,
            body: AgentUpdateKind::ApprovalRequested {
                approval_id: "ap_1".into(),
                tool: Some("fs.write".into()),
                action: Some("create".into()),
                params: Some(json!({"path": "/tmp/x"})),
                pending_action_count: Some(1),
            },
        };
        let expected = json!({
            "id": "01HXTEST000000000000000001",
            "ts": 1_745_432_100_123_i64,
            "workspace_id": "ws_a",
            "agent_id": "cfo",
            "thread_id": "th_1",
            "kind": "approval_requested",
            "approval_id": "ap_1",
            "tool": "fs.write",
            "action": "create",
            "params": {"path": "/tmp/x"},
            "pending_action_count": 1
        });
        assert_eq!(serde_json::to_value(&update).unwrap(), expected);
    }

    #[test]
    fn wire_shape_agent_deleted_is_unit_variant() {
        let update = AgentUpdate {
            id: "01HXTEST000000000000000002".into(),
            ts: 1_745_432_100_123,
            workspace_id: "ws_a".into(),
            agent_id: "cfo".into(),
            thread_id: None,
            cycle_id: None,
            body: AgentUpdateKind::AgentDeleted,
        };
        let expected = json!({
            "id": "01HXTEST000000000000000002",
            "ts": 1_745_432_100_123_i64,
            "workspace_id": "ws_a",
            "agent_id": "cfo",
            "kind": "agent_deleted"
        });
        assert_eq!(serde_json::to_value(&update).unwrap(), expected);
    }

    #[test]
    fn new_generates_ulid_and_current_timestamp() {
        let update = AgentUpdate::new("ws_a".into(), "cfo".into(), AgentUpdateKind::AgentResumed);
        assert_eq!(update.id.len(), 26, "ULID is 26 crockford-base32 chars");
        assert!(
            update.ts > 1_600_000_000_000,
            "ts is in plausible millis range"
        );
        assert_eq!(update.workspace_id, "ws_a");
        assert_eq!(update.agent_id, "cfo");
        assert_eq!(update.thread_id, None);
        assert_eq!(update.cycle_id, None);
    }

    #[test]
    fn ulid_ids_sort_monotonically() {
        let a = AgentUpdate::new("ws".into(), "a".into(), AgentUpdateKind::AgentResumed);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = AgentUpdate::new("ws".into(), "a".into(), AgentUpdateKind::AgentResumed);
        assert!(
            a.id < b.id,
            "ULIDs generated sequentially should sort ascending"
        );
    }

    #[test]
    fn with_thread_and_with_cycle_set_scope() {
        let update = AgentUpdate::new(
            "ws".into(),
            "agent".into(),
            AgentUpdateKind::CyclePaused {
                reason: "budget".into(),
            },
        )
        .with_thread("th".into())
        .with_cycle("cyc".into());
        assert_eq!(update.thread_id.as_deref(), Some("th"));
        assert_eq!(update.cycle_id.as_deref(), Some("cyc"));
    }

    #[test]
    fn translator_skips_cycle_and_hitl_kinds() {
        let payload = json!({"cycle_id": "c1", "goal_id": "g1"});
        // `agent.cycle.completed` is a canonical V3 event emitted via
        // the direct path — skip in the legacy translator to avoid
        // double-firing.
        assert!(agent_update_from_legacy_event(
            "agent.cycle.completed",
            &payload,
            "agent-1",
            "ws_a"
        )
        .is_none());
        // `hitl.requested` is the canonical HITL lifecycle event
        // (replaces the retired `approval.requested` / `user_request.pending`
        // projections in v0.6.505). The translator skips it because
        // there's a dedicated inline emit path for canonical HITL.
        assert!(
            agent_update_from_legacy_event("hitl.requested", &payload, "agent-1", "ws_a").is_none()
        );
    }

    #[test]
    fn translator_maps_goal_recovered() {
        let payload = json!({
            "goal_id": "g1",
            "cycle_id": "c1",
            "after_failures": 3,
        });
        let update =
            agent_update_from_legacy_event("agent.goal.recovered", &payload, "agent-1", "ws_a")
                .expect("goal.recovered should translate");
        assert_eq!(update.agent_id, "agent-1");
        assert_eq!(update.workspace_id, "ws_a");
        assert_eq!(update.cycle_id.as_deref(), Some("c1"));
        match update.body {
            AgentUpdateKind::GoalRecovered {
                goal_id,
                recovery_strategy,
            } => {
                assert_eq!(goal_id, "g1");
                assert!(recovery_strategy.contains('3'));
            },
            other => panic!("expected GoalRecovered, got {:?}", other),
        }
    }

    #[test]
    fn translator_maps_circuit_recovered() {
        let payload = json!({
            "goal_id": "g1",
            "cycle_id": "c1",
            "recovered_at": 1_700_000_000_000_i64,
        });
        let update =
            agent_update_from_legacy_event("agent.circuit.recovered", &payload, "agent-1", "ws_a")
                .expect("circuit.recovered should translate");
        match update.body {
            AgentUpdateKind::CircuitRecovered {
                scope,
                recovered_at,
            } => {
                assert_eq!(scope, "agent-1:g1");
                assert_eq!(recovered_at, 1_700_000_000_000);
            },
            other => panic!("expected CircuitRecovered, got {:?}", other),
        }
    }

    #[test]
    fn translator_maps_circuit_opened() {
        let payload = json!({
            "goal_id": "g1",
            "cycle_id": "c1",
            "failure_count": 5,
        });
        let update =
            agent_update_from_legacy_event("agent.circuit.opened", &payload, "agent-1", "ws_a")
                .expect("circuit.opened should translate");
        match update.body {
            AgentUpdateKind::CircuitOpened { scope, .. } => {
                assert_eq!(scope, "agent-1:g1");
            },
            other => panic!("expected CircuitOpened, got {:?}", other),
        }
    }

    #[test]
    fn translator_maps_agent_lifecycle_events() {
        // created
        let update = agent_update_from_legacy_event(
            "agent.created",
            &json!({"name": "CFO", "kind": "personal"}),
            "agent-1",
            "ws_a",
        )
        .unwrap();
        assert!(matches!(update.body, AgentUpdateKind::AgentCreated { .. }));

        // paused with reason
        let update = agent_update_from_legacy_event(
            "agent.paused",
            &json!({"reason": "manual"}),
            "agent-1",
            "ws_a",
        )
        .unwrap();
        match update.body {
            AgentUpdateKind::AgentPaused { reason } => {
                assert_eq!(reason.as_deref(), Some("manual"))
            },
            other => panic!("expected AgentPaused, got {:?}", other),
        }

        // resumed (unit variant)
        let update =
            agent_update_from_legacy_event("agent.resumed", &json!({}), "agent-1", "ws_a").unwrap();
        assert!(matches!(update.body, AgentUpdateKind::AgentResumed));

        // deleted (unit variant)
        let update =
            agent_update_from_legacy_event("agent.deleted", &json!({}), "agent-1", "ws_a").unwrap();
        assert!(matches!(update.body, AgentUpdateKind::AgentDeleted));
    }

    #[test]
    fn translator_returns_none_for_unknown_event_types() {
        assert!(agent_update_from_legacy_event("tool.succeeded", &json!({}), "a", "ws").is_none());
        assert!(agent_update_from_legacy_event("llm.failed", &json!({}), "a", "ws").is_none());
    }

    #[test]
    fn omits_none_scope_fields_from_wire() {
        let update = AgentUpdate {
            id: "01HXTEST000000000000000003".into(),
            ts: 0,
            workspace_id: "ws".into(),
            agent_id: "a".into(),
            thread_id: None,
            cycle_id: None,
            body: AgentUpdateKind::AgentResumed,
        };
        let value = serde_json::to_value(&update).unwrap();
        assert!(
            value.get("thread_id").is_none(),
            "None thread_id must be omitted"
        );
        assert!(
            value.get("cycle_id").is_none(),
            "None cycle_id must be omitted"
        );
    }
}
