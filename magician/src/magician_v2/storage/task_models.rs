//! Task Storage Models for TODO-style task management
//! Separate from ExecutionRun - Tasks are user-created TODOs that trigger execution on demand

use serde::{Deserialize, Serialize};

use crate::magician_v2::pipeline::agent::{
    AgentScheduleKind, ConcurrentExecutionPolicy, MissedFirePolicy,
};

pub fn default_ui_thread_id() -> String {
    "general".to_string()
}
// ---------------------------------------------------------------------------
// Schedule types (re-exported from pipeline::agent for task-level use)
// ---------------------------------------------------------------------------

/// Which schedule variant drives this task's trigger.
/// Re-exported from `pipeline::agent::AgentScheduleKind`.
pub type TaskScheduleKind = AgentScheduleKind;

/// Optional recurring schedule for a task (cron, interval, one-time, or event-triggered).
/// Moved here from `AgentDefinition.triggers` so scheduling is a first-class Task concept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSchedule {
    /// The schedule kind (cron expression, interval, etc.)
    pub kind: TaskScheduleKind,
    /// Timezone for cron schedules
    #[serde(default)]
    pub timezone: Option<String>,
    /// What to do when fires were missed
    #[serde(default)]
    pub missed_fire_policy: MissedFirePolicy,
    /// What to do when previous execution is still running
    #[serde(default)]
    pub concurrent_execution_policy: ConcurrentExecutionPolicy,
    /// Execution history retention policy.
    /// When set, limits how many past execution records are kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_history_retention: Option<ExecutionHistoryRetention>,
    /// Maximum number of times this schedule should fire before
    /// automatically retiring. When `None` (default) the schedule runs
    /// indefinitely. The API surface here is the declared limit, not the
    /// running counter (`TaskState.schedule_fire_count`).
    ///
    /// ENFORCED at three layers: boot hydration skips re-registering a
    /// task already at its limit (`artifact_v2/service.rs`
    /// `list_scheduled_tasks_across_scopes`), and the dispatch loop checks
    /// `schedule_fire_count` pre-fire and unregisters the task post-fire
    /// once the durable count reaches the limit (`api/web_api.rs`
    /// scheduler loop). Retirement checks consult the same counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runs: Option<u32>,
    /// User-controlled pause flag. When `Some(true)`, the scheduler skips
    /// this task at hydration time (it never registers, or is unregistered
    /// on the next sync). Set back to `Some(false)` / `None` to resume.
    /// Independent of task `status` — "schedule paused" and "execution
    /// paused" are different concepts intentionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
}

/// Policy for trimming old execution-history records.
///
/// `max_records` takes precedence: if both `max_age_days` and `max_records`
/// are set, records beyond `max_records` are removed first, then anything
/// older than `max_age_days` is pruned from the remainder.
///
/// Default (when absent): 3 days if the task fires fewer than 15 times/day,
/// otherwise cap at 50 records in legacy task-document storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionHistoryRetention {
    /// Maximum number of records to keep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_records: Option<u32>,
    /// Maximum age in days — records older than this are pruned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_days: Option<u32>,
}

// ---------------------------------------------------------------------------
// Task provenance
// ---------------------------------------------------------------------------

/// How this task was created — tracks provenance.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskCreatedBy {
    #[default]
    User,
    Agent {
        agent_id: String,
    },
    Delegation {
        parent_task_id: String,
        delegating_agent_id: String,
    },
}

// ---------------------------------------------------------------------------
// Default helpers
// ---------------------------------------------------------------------------

/// Default value for `Task::approved` — true so user-created tasks are
/// immediately eligible for execution.
fn default_approved() -> bool {
    true
}

/// Task status - tracks the lifecycle of a TODO
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskStatus {
    /// Task created, no action taken yet
    #[default]
    Pending,
    /// AI is generating a plan for this task
    Planning,
    /// Plan generated, waiting for user to review/approve
    Ready,
    /// Task is currently executing
    Running,
    /// Execution paused by user
    Paused,
    /// Task completed successfully
    Completed,
    /// Task failed during execution
    Failed,
    /// Task was cancelled by user
    Cancelled,
    /// Execution precondition not met; retry scheduled at `Task.retry_at`
    Deferred,
}

impl TaskStatus {
    /// Check if this is a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    /// Check if task is executable (can start execution).
    /// `Deferred` is executable so retry wakes pass the gate.
    pub fn can_execute(&self) -> bool {
        matches!(self, Self::Ready | Self::Paused | Self::Deferred)
    }

    /// Check if execution is in progress
    pub fn is_executing(&self) -> bool {
        matches!(self, Self::Running)
    }
}

/// Task priority levels (P1 = highest, P4 = lowest)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum TaskPriority {
    P1, // Urgent
    P2, // High
    P3, // Medium
    #[default]
    P4, // Low
}

/// Task filter categories for UI
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskFilter {
    #[default]
    All,
    Inbox,
    Today,
    Overdue,
    Running,
    Completed,
    Custom(String), // Custom tag filter
}

/// Task tag for organization
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskTag {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// A TODO task - the core entity for the new UI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    #[serde(default = "default_ui_thread_id")]
    pub ui_thread_id: String,
    pub title: String,
    /// Task description (required) — drives planning and execution
    #[serde(default)]
    pub description: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub due_date: Option<String>, // ISO 8601 date string
    #[serde(default)]
    pub tags: Vec<TaskTag>,

    /// Compulsory agent assignment. Every task must have an agent executor.
    #[serde(default)]
    pub agent_id: String,

    /// Optional recurring schedule (cron, interval, one-time, or event-triggered).
    /// Moved here from AgentDefinition.triggers.
    #[serde(default)]
    pub schedule: Option<TaskSchedule>,

    /// How this task was created — tracks provenance.
    #[serde(default)]
    pub created_by: TaskCreatedBy,

    /// Task IDs that must complete before this task can execute.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Immutable record of original depends_on — used for artifact linking.
    /// Unlike depends_on (which is cleared by cascade_unblock_dependents),
    /// this field preserves the full dependency list for the orchestrator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_task_ids: Vec<String>,

    /// Whether this task is approved for execution. Defaults to true for user-created tasks.
    /// Autonomously-created tasks default to false (require user approval).
    #[serde(default = "default_approved")]
    pub approved: bool,

    /// When true, archive the task out of the active task list after it reaches
    /// a terminal state. Intended for lightweight direct-run tasks created from
    /// Auto-surface publication policy. When set, overrides the agent-level
    /// policy. Inferred from goal text at task creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_surface_policy: Option<crate::magician_v2::agents::types::AutoSurfacePolicy>,

    /// Whether this task has a plan persisted for the active execution.
    /// Single source of truth flag — actual plan content lives in execution storage.
    #[serde(default)]
    pub has_plan: bool,

    /// Root execution currently active for this task, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_root_execution_id: Option<String>,

    /// Most recent root execution created for this task, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_root_execution_id: Option<String>,

    /// Most recent root execution that completed successfully, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_root_execution_id: Option<String>,

    /// Error message if task failed
    #[serde(default)]
    pub error_message: Option<String>,

    /// When to retry a deferred task (millis since epoch).
    /// Set when status is `Deferred`; cleared on reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<i64>,

    /// Current step being executed (0-indexed)
    #[serde(default)]
    pub current_step: Option<u32>,

    /// Execution progress percentage (0-100)
    #[serde(default)]
    pub progress: Option<u8>,

    /// Persisted completion result — survives browser closes.
    /// Populated when `AgenticExecutionCompleted` fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_artifact_names: Option<Vec<String>>,

    pub created_at: i64,
    pub updated_at: i64,
}

impl Task {
    /// Check if this task can be executed (basic gate — status + deps).
    /// The full 4-condition gate (including has_plan + approved) is in WakeUpQueue.
    pub fn is_executable(&self) -> bool {
        self.status.can_execute() && self.depends_on.is_empty()
    }
}

/// Per-step status snapshot captured for a specific execution record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskExecutionStepRecord {
    pub number: usize,
    pub name: String,
    pub status: String,
    pub progress: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_agent_id: Option<String>,
}

/// Linked task artifacts pinned for a specific execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskExecutionLinkedInputRecord {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_chain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_names: Vec<String>,
}

/// Record of a past execution attempt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionRecord {
    pub execution_id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: TaskStatus,
    pub error_message: Option<String>,
    /// Completion result captured per-execution (persists across scheduled resets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_artifact_names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_chain_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_task_inputs: Vec<TaskExecutionLinkedInputRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_statuses: Vec<TaskExecutionStepRecord>,
}

#[derive(Debug, Clone, Default)]
pub struct TaskExecutionSnapshot {
    pub current_step: Option<u32>,
    pub progress: Option<u8>,
    pub plan_id: Option<String>,
    pub artifact_chain_id: Option<String>,
    pub step_statuses: Vec<TaskExecutionStepRecord>,
}

pub fn task_execution_artifact_chain_id(task_id: &str, execution_id: &str) -> String {
    format!("task-{task_id}--execution-{execution_id}")
}

/// Task summary for listings (minimal info)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    #[serde(default = "default_ui_thread_id")]
    pub ui_thread_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub status: TaskStatus,
    pub priority: Option<TaskPriority>,
    pub due_date: Option<String>,
    pub tags: Vec<TaskTag>,
    /// Agent assigned to this task
    #[serde(default)]
    pub agent_id: String,
    /// Whether this task has a recurring schedule
    #[serde(default)]
    pub has_schedule: bool,
    /// Full schedule object (if present) — lets the frontend render cron details
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<TaskSchedule>,
    pub has_plan: bool,
    #[serde(default)]
    pub is_blocked: bool,
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed_root_execution_id: Option<String>,
    /// When to retry a deferred task (millis since epoch)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<i64>,
    /// Persisted completion result
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_artifact_names: Option<Vec<String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<&Task> for TaskSummary {
    fn from(task: &Task) -> Self {
        Self {
            id: task.id.clone(),
            principal: task.principal.clone(),
            workspace: task.workspace.clone(),
            ui_thread_id: task.ui_thread_id.clone(),
            title: task.title.clone(),
            description: task.description.clone(),
            status: task.status.clone(),
            priority: task.priority.clone(),
            due_date: task.due_date.clone(),
            tags: task.tags.clone(),
            agent_id: task.agent_id.clone(),
            has_schedule: task.schedule.is_some(),
            schedule: task.schedule.clone(),
            has_plan: task.has_plan,
            is_blocked: !task.depends_on.is_empty(),
            progress: task.progress,
            active_root_execution_id: task.active_root_execution_id.clone(),
            latest_root_execution_id: task.latest_root_execution_id.clone(),
            last_completed_root_execution_id: task.last_completed_root_execution_id.clone(),
            retry_at: task.retry_at,
            completion_summary: task.completion_summary.clone(),
            completion_outcome: task.completion_outcome.clone(),
            completion_artifact_names: task.completion_artifact_names.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Dependency cycle detection
// ---------------------------------------------------------------------------

/// Check if adding `new_dep` to `task_id`'s depends_on would create a cycle.
/// `get_deps` is a closure that returns the depends_on list for a given task_id.
pub fn would_create_cycle<F>(task_id: &str, new_dep: &str, get_deps: F) -> bool
where
    F: Fn(&str) -> Vec<String>,
{
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![new_dep.to_string()];
    while let Some(current) = stack.pop() {
        if current == task_id {
            return true;
        }
        if visited.insert(current.clone()) {
            stack.extend(get_deps(&current));
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// Helper: build a minimal `Task` suitable for unit tests.
    fn make_task(id: &str, status: TaskStatus, depends_on: Vec<String>) -> Task {
        Task {
            id: id.to_string(),
            principal: "test-principal".to_string(),
            workspace: "test-ws".to_string(),
            ui_thread_id: default_ui_thread_id(),
            title: format!("Task {id}"),
            description: format!("Description for task {id}"),
            status,
            priority: None,
            due_date: None,
            tags: vec![],
            agent_id: "personal-assistant".to_string(),
            schedule: None,
            created_by: TaskCreatedBy::default(),
            linked_task_ids: depends_on.clone(),
            depends_on,
            approved: true,
            auto_surface_policy: None,
            has_plan: false,
            active_root_execution_id: None,
            latest_root_execution_id: None,
            last_completed_root_execution_id: None,
            error_message: None,
            retry_at: None,
            current_step: None,
            progress: None,
            completion_summary: None,
            completion_outcome: None,
            completion_artifact_names: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn test_depends_on_serde_roundtrip() {
        let task = make_task("t1", TaskStatus::Ready, vec!["dep1".into(), "dep2".into()]);
        let json = serde_json::to_string(&task).unwrap();
        let deserialized: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.depends_on,
            vec!["dep1".to_string(), "dep2".to_string()]
        );
    }

    #[test]
    fn test_depends_on_defaults_empty() {
        // Minimal JSON without `depends_on`
        let json = serde_json::json!({
            "id": "t1",
            "principal": "p",
            "workspace": "w",
            "title": "T",
            "status": "pending",
            "created_at": 0,
            "updated_at": 0
        });
        let task: Task = serde_json::from_value(json).unwrap();
        assert!(task.depends_on.is_empty());
    }

    #[test]
    fn test_approved_defaults_true() {
        let json = serde_json::json!({
            "id": "t1",
            "principal": "p",
            "workspace": "w",
            "title": "T",
            "status": "pending",
            "created_at": 0,
            "updated_at": 0
        });
        let task: Task = serde_json::from_value(json).unwrap();
        assert!(task.approved);
    }

    #[test]
    fn test_is_executable_ready_no_deps() {
        let task = make_task("t1", TaskStatus::Ready, vec![]);
        assert!(task.is_executable());
    }

    #[test]
    fn test_is_executable_blocked_by_deps() {
        let task = make_task("t1", TaskStatus::Ready, vec!["dep1".into()]);
        assert!(!task.is_executable());
    }

    #[test]
    fn test_would_create_cycle_rejects() {
        // Existing graph: A depends_on [B], B depends_on [C].
        // Attempting to add dep A to task C (C depends_on A) would create C -> A -> B -> C.
        let get_deps = |id: &str| -> Vec<String> {
            match id {
                "A" => vec!["B".to_string()],
                "B" => vec!["C".to_string()],
                _ => vec![],
            }
        };
        assert!(would_create_cycle("C", "A", get_deps));
    }

    #[test]
    fn test_would_create_cycle_allows_valid() {
        // A -> B, adding A -> C (no cycle via C)
        let get_deps = |id: &str| -> Vec<String> {
            match id {
                "B" => vec![],
                "C" => vec![],
                _ => vec![],
            }
        };
        assert!(!would_create_cycle("A", "C", get_deps));
    }

    #[test]
    fn test_task_summary_is_blocked() {
        // Task with non-empty depends_on -> TaskSummary.is_blocked = true
        let blocked = make_task("t1", TaskStatus::Pending, vec!["dep1".into()]);
        let summary = TaskSummary::from(&blocked);
        assert!(summary.is_blocked);

        // Task with empty depends_on -> TaskSummary.is_blocked = false
        let unblocked = make_task("t2", TaskStatus::Pending, vec![]);
        let summary = TaskSummary::from(&unblocked);
        assert!(!summary.is_blocked);
    }

    #[test]
    fn test_agent_id_defaults_empty_string() {
        // Deserialize a Task JSON without agent_id — should default to ""
        let json = serde_json::json!({
            "id": "t1",
            "principal": "p",
            "workspace": "w",
            "title": "T",
            "status": "pending",
            "created_at": 0,
            "updated_at": 0
        });
        let task: Task = serde_json::from_value(json).unwrap();
        assert_eq!(task.agent_id, "");
    }

    #[test]
    fn test_created_by_serde_roundtrip() {
        // User variant
        let user = TaskCreatedBy::User;
        let json = serde_json::to_string(&user).unwrap();
        let deser: TaskCreatedBy = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, TaskCreatedBy::User);

        // Agent variant
        let agent = TaskCreatedBy::Agent {
            agent_id: "agent-1".to_string(),
        };
        let json = serde_json::to_string(&agent).unwrap();
        let deser: TaskCreatedBy = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deser,
            TaskCreatedBy::Agent {
                agent_id: "agent-1".to_string()
            }
        );

        // Delegation variant
        let delegation = TaskCreatedBy::Delegation {
            parent_task_id: "parent-1".to_string(),
            delegating_agent_id: "agent-2".to_string(),
        };
        let json = serde_json::to_string(&delegation).unwrap();
        let deser: TaskCreatedBy = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deser,
            TaskCreatedBy::Delegation {
                parent_task_id: "parent-1".to_string(),
                delegating_agent_id: "agent-2".to_string(),
            }
        );
    }

    #[test]
    fn test_task_schedule_serde_roundtrip() {
        use crate::magician_v2::pipeline::agent::AgentScheduleKind;

        let schedule = TaskSchedule {
            kind: AgentScheduleKind::Cron {
                expression: "0 9 * * *".to_string(),
                timezone: None,
            },
            timezone: Some("America/New_York".to_string()),
            missed_fire_policy: Default::default(),
            execution_history_retention: None,
            concurrent_execution_policy: Default::default(),
            max_runs: None,
            paused: None,
        };
        let json = serde_json::to_string(&schedule).unwrap();
        let deser: TaskSchedule = serde_json::from_str(&json).unwrap();
        assert_eq!(deser.timezone, Some("America/New_York".to_string()));
    }

    #[test]
    fn test_created_by_defaults_to_user() {
        let json = serde_json::json!({
            "id": "t1",
            "principal": "p",
            "workspace": "w",
            "title": "T",
            "status": "pending",
            "created_at": 0,
            "updated_at": 0
        });
        let task: Task = serde_json::from_value(json).unwrap();
        assert_eq!(task.created_by, TaskCreatedBy::User);
    }

    #[test]
    fn test_completion_fields_default_to_none_on_deserialize() {
        // Old JSON without completion fields should deserialize with None defaults
        let json = serde_json::json!({
            "id": "t1",
            "principal": "p",
            "workspace": "w",
            "title": "Legacy task",
            "status": "completed",
            "agent_id": "personal-assistant",
            "created_at": 1000,
            "updated_at": 2000
        });
        let task: Task = serde_json::from_value(json).unwrap();
        assert!(task.completion_summary.is_none());
        assert!(task.completion_outcome.is_none());
        assert!(task.completion_artifact_names.is_none());
    }

    #[test]
    fn test_completion_fields_serde_roundtrip() {
        let json = serde_json::json!({
            "id": "t2",
            "principal": "p",
            "workspace": "w",
            "title": "With completion",
            "status": "completed",
            "agent_id": "personal-assistant",
            "created_at": 1000,
            "updated_at": 2000,
            "completion_summary": "Goal achieved: find data. Found at row 42",
            "completion_outcome": "goal_achieved",
            "completion_artifact_names": ["results (application/json): {\"row\":42}"]
        });
        let task: Task = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            task.completion_summary.as_deref(),
            Some("Goal achieved: find data. Found at row 42")
        );
        assert_eq!(task.completion_outcome.as_deref(), Some("goal_achieved"));
        assert_eq!(
            task.completion_artifact_names,
            Some(vec!["results (application/json): {\"row\":42}".to_string()])
        );

        // Roundtrip: serialize back and verify
        let reserialized = serde_json::to_value(&task).unwrap();
        assert_eq!(
            reserialized["completion_summary"],
            json["completion_summary"]
        );
        assert_eq!(
            reserialized["completion_outcome"],
            json["completion_outcome"]
        );
        assert_eq!(
            reserialized["completion_artifact_names"],
            json["completion_artifact_names"]
        );
    }

    #[test]
    fn test_completion_fields_omitted_when_none() {
        // When None, fields should be skipped in serialization (skip_serializing_if)
        let task = make_task("t-skip", TaskStatus::Pending, vec![]);
        let json = serde_json::to_value(&task).unwrap();
        assert!(
            json.get("completion_summary").is_none(),
            "None completion_summary should be omitted from JSON"
        );
        assert!(
            json.get("completion_outcome").is_none(),
            "None completion_outcome should be omitted from JSON"
        );
        assert!(
            json.get("completion_artifact_names").is_none(),
            "None completion_artifact_names should be omitted from JSON"
        );
    }
}
