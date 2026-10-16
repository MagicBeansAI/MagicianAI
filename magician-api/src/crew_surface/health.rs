//! Crew health summarization — the pure mapping the health endpoints layer
//! on top of the durable read model (plan workstream 3.5).
//!
//! [`crew_health_state`] decides which `AgentHealthState` band a crew member
//! occupies from its hydrated runtime status plus its scoped task list, and
//! [`crew_health_task_needs_attention`] decides whether one task alone
//! forces the `needs_attention` band. Extracted verbatim from `web_api`; the
//! scoring, caching, and durable history stay in [`crate::crew_health_api`].

use magician::magician_v2::artifact_v2::models::TaskListItemV3;

use crate::crew_health_api::AgentHealthState;

/// Map a hydrated runtime status plus the agent's scoped tasks onto the
/// health band the read model scores.
///
/// Precedence (first match wins): offline (disabled/offline status) →
/// needs attention (attention-demanding task or failed status) → paused →
/// working → idle.
pub fn crew_health_state(runtime_status: &str, tasks: &[&TaskListItemV3]) -> AgentHealthState {
    let status = runtime_status.trim().to_ascii_lowercase();
    if status == "disabled" || status == "offline" {
        return AgentHealthState::Offline;
    }
    if tasks
        .iter()
        .any(|task| crew_health_task_needs_attention(task))
        || matches!(status.as_str(), "error" | "failed" | "needs_attention")
    {
        return AgentHealthState::NeedsAttention;
    }
    if status == "paused"
        || tasks.iter().any(|task| {
            task.active_root_execution_id.is_some()
                && task.status.trim().eq_ignore_ascii_case("paused")
        })
    {
        return AgentHealthState::Paused;
    }
    if matches!(status.as_str(), "running" | "triggered" | "planning")
        || tasks.iter().any(|task| {
            task.active_root_execution_id.is_some()
                && matches!(
                    task.status.trim().to_ascii_lowercase().as_str(),
                    "planning" | "running" | "in_progress" | "active"
                )
        })
    {
        return AgentHealthState::Working;
    }
    AgentHealthState::Idle
}

/// Whether one task alone forces the `needs_attention` band: a pending
/// question, or a blocked task that is live (active root execution) or in a
/// waiting/blocked/paused status.
pub fn crew_health_task_needs_attention(task: &TaskListItemV3) -> bool {
    if task.pending_question.is_some() || !task.pending_questions.is_empty() {
        return true;
    }
    let status = task.status.trim().to_ascii_lowercase();
    task.is_blocked
        && (task.active_root_execution_id.is_some()
            || matches!(
                status.as_str(),
                "blocked" | "waiting" | "waiting_for_user" | "paused"
            ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(agent_id: &str, task_id: &str, status: &str) -> TaskListItemV3 {
        TaskListItemV3 {
            id: task_id.to_string(),
            title: format!("{agent_id} task"),
            description: "Task-backed execution".to_string(),
            status: status.to_string(),
            agent_id: agent_id.to_string(),
            ui_thread_id: magician::magician_v2::artifact_v2::models::default_ui_thread_id(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: magician::magician_v2::artifact_v2::models::default_task_created_by(),
            depends_on: Vec::new(),
            approved: true,
            is_blocked: false,
            schedule: None,
            output_mode: magician::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
            active_root_execution_id: Some("exec-1".to_string()),
            latest_root_execution_id: Some("exec-1".to_string()),
            last_completed_root_execution_id: None,
            current_step_title: None,
            current_substep_title: None,
            completion_summary: None,
            completion_outcome: None,
            completion_artifact_names: Vec::new(),
            has_plan: false,
            latest_plan_id: None,
            approved_plan_id: None,
            plan_updated_at: None,
            plan_status: None,
            pending_question: None,
            pending_questions: Vec::new(),
            chat_session_id: None,
            lifecycle: magician::magician_v2::artifact_v2::models::TaskLifecycle::default(),
            sync_mode: magician::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            synthesis_pending: false,
            synthesis_failed_execution_id: None,
            monitor_revision: 0,
            awaiting_diff_approval: false,
            last_progress_at: None,
            created_at: "2026-08-26T10:00:00Z".to_string(),
            updated_at: "2026-08-26T10:00:00Z".to_string(),
        }
    }

    fn recommended_question(
    ) -> magician::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion {
        use magician::magician_v2::ask_loop::budget::Channel;
        use magician::magician_v2::ask_loop::clarifier::BlockerType;
        use magician::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion;
        use magician::magician_v2::state_tracker::types::StageContext;
        RecommendedQuestion {
            id: "q1".to_string(),
            blocker_type: BlockerType::LowConfidenceSlot,
            question_text: "which scope?".to_string(),
            context_snippets: Vec::new(),
            urgency: 0.5,
            channel: Channel::InApp,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: None,
            created_at: None,
            options: None,
            batch_id: None,
            batch_total: None,
            slot_confidence: None,
            related_slots: Vec::new(),
        }
    }

    #[test]
    fn offline_and_failed_statuses_win_over_task_evidence() {
        assert_eq!(
            crew_health_state("disabled", &[]),
            AgentHealthState::Offline
        );
        assert_eq!(
            crew_health_state(" Offline ", &[]),
            AgentHealthState::Offline
        );
        assert_eq!(
            crew_health_state("error", &[]),
            AgentHealthState::NeedsAttention
        );
        assert_eq!(
            crew_health_state("needs_attention", &[]),
            AgentHealthState::NeedsAttention
        );
        // Offline outranks a running task.
        let running = task("a", "t1", "running");
        assert_eq!(
            crew_health_state("disabled", &[&running]),
            AgentHealthState::Offline
        );
    }

    #[test]
    fn pending_question_or_blocked_live_task_forces_needs_attention() {
        let mut pending = task("a", "t1", "running");
        pending.pending_question = Some(recommended_question());
        assert_eq!(
            crew_health_state("idle", &[&pending]),
            AgentHealthState::NeedsAttention
        );

        let mut blocked = task("a", "t2", "blocked");
        blocked.pending_question = None;
        blocked.is_blocked = true;
        assert_eq!(
            crew_health_state("idle", &[&blocked]),
            AgentHealthState::NeedsAttention
        );

        // A blocked task with no active execution and a non-waiting status
        // (e.g. "completed") does not demand attention on its own.
        let mut done = task("a", "t3", "completed");
        done.is_blocked = true;
        done.active_root_execution_id = None;
        assert!(!crew_health_task_needs_attention(&done));
        assert_eq!(crew_health_state("idle", &[&done]), AgentHealthState::Idle);
    }

    #[test]
    fn paused_outranks_working_and_working_outranks_idle() {
        let paused_task = task("a", "t1", "paused");
        assert_eq!(
            crew_health_state("idle", &[&paused_task]),
            AgentHealthState::Paused
        );

        let running = task("a", "t2", "running");
        assert_eq!(
            crew_health_state("idle", &[&running]),
            AgentHealthState::Working
        );
        // Runtime status alone can mint working/paused without tasks.
        assert_eq!(crew_health_state("running", &[]), AgentHealthState::Working);
        assert_eq!(
            crew_health_state("planning", &[]),
            AgentHealthState::Working
        );
        assert_eq!(crew_health_state("paused", &[]), AgentHealthState::Paused);
        assert_eq!(crew_health_state("idle", &[]), AgentHealthState::Idle);
    }
}
