use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::workspace::{ArtifactV2Workspace, WorkspaceFileEntry};

use super::store::LearningStore;
use super::types::LearningScope;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LearningAuditCounts {
    pub tasks: usize,
    pub task_executions: usize,
    pub scoped_executions: usize,
    pub chat_sessions: usize,
    pub memory_agents: usize,
    pub memory_episodes: usize,
    pub memory_tier_files: usize,
    pub memory_event_parquet_files: usize,
    pub llm_call_parquet_files: usize,
    pub learning_candidates: usize,
    pub learning_evaluation_backlog_items: usize,
    pub learning_evaluation_run_reports: usize,
    pub learning_growth_evaluation_run_reports: usize,
    pub learning_procedures: usize,
    pub learning_capability_evolution_backlog_items: usize,
    pub learning_capability_evolution_proposals: usize,
    pub learning_capability_evolution_validations: usize,
    pub learning_capability_evolution_implementations: usize,
    pub learning_capability_evolution_applications: usize,
    pub learning_capability_evolution_promotions: usize,
    pub learning_events: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningGapAuditReport {
    pub generated_at: DateTime<Utc>,
    pub scope: LearningScope,
    pub counts: LearningAuditCounts,
    pub existing_signal_sources: Vec<String>,
    pub phase_0_findings: Vec<String>,
    pub phase_1_substrate_status: Vec<String>,
    pub next_actions: Vec<String>,
}

pub fn build_learning_gap_audit(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<LearningGapAuditReport> {
    let scope = LearningScope::new(principal, workspace);
    let store = LearningStore::new(workspace_layout.clone());
    let tasks_root = workspace_layout.tasks_root(principal, workspace);
    let scoped_executions_root = workspace_layout.scoped_executions_root(principal, workspace);
    let chat_sessions_root = workspace_layout.chat_sessions_dir(principal, workspace);
    let memory_agents_root = workspace_layout.memory_agents_root(principal, workspace);
    let memory_events_root = workspace_layout.analytics_memory_events_root(principal, workspace);
    let llm_calls_root = workspace_layout.analytics_llm_calls_root(principal, workspace);

    let counts = LearningAuditCounts {
        tasks: count_child_dirs(workspace_layout, &tasks_root),
        task_executions: count_nested_execution_dirs(workspace_layout, &tasks_root),
        scoped_executions: count_child_dirs(workspace_layout, &scoped_executions_root),
        chat_sessions: count_chat_sessions(workspace_layout, &chat_sessions_root),
        memory_agents: count_child_dirs(workspace_layout, &memory_agents_root),
        memory_episodes: count_agent_memory_files(
            workspace_layout,
            &memory_agents_root,
            "episodes",
        ),
        memory_tier_files: count_agent_memory_files(workspace_layout, &memory_agents_root, "tiers"),
        memory_event_parquet_files: count_files_with_extension(
            workspace_layout,
            &memory_events_root,
            "parquet",
        ),
        llm_call_parquet_files: count_files_with_extension(
            workspace_layout,
            &llm_calls_root,
            "parquet",
        ),
        learning_candidates: store.count_candidates(&scope),
        learning_evaluation_backlog_items: store.count_evaluation_backlog_items(&scope),
        learning_evaluation_run_reports: store.count_evaluation_run_reports(&scope),
        learning_growth_evaluation_run_reports: store.count_growth_evaluation_run_reports(&scope),
        learning_procedures: store.count_procedures(&scope),
        learning_capability_evolution_backlog_items: store
            .count_capability_evolution_backlog_items(&scope),
        learning_capability_evolution_proposals: store.count_capability_evolution_proposals(&scope),
        learning_capability_evolution_validations: store
            .count_capability_evolution_validation_reports(&scope),
        learning_capability_evolution_implementations: store
            .count_capability_evolution_implementation_records(&scope),
        learning_capability_evolution_applications: store
            .count_capability_evolution_application_records(&scope),
        learning_capability_evolution_promotions: store
            .count_capability_evolution_promotion_records(&scope),
        learning_events: store.count_event_records(&scope),
    };

    Ok(LearningGapAuditReport {
        generated_at: Utc::now(),
        scope,
        counts,
        existing_signal_sources: vec![
            "Task manifests, task state, execution events, prompt projections, runtime contexts, and task outputs."
                .to_string(),
            "Scoped chat/pack execution events and chat sessions."
                .to_string(),
            "LLM-call telemetry partitions with model, operation, success, error, token, latency, and cost fields."
                .to_string(),
            "Memory retrieval/consolidation/eval telemetry partitions and tiered memory Markdown/JSON files."
                .to_string(),
            "Workspace transport events and analytics logs.".to_string(),
        ],
        phase_0_findings: vec![
            "The system already records enough raw operational evidence to identify repeated failures, user corrections, useful procedures, and tool/schema gaps.".to_string(),
            "Before Phase 1 there was no durable, typed object that represented a proposed learning separate from the raw run artifacts.".to_string(),
            "Promotion and implementation must remain separate from observation so audits can prove that no skill, prompt, memory, wrapper, or code change happened implicitly.".to_string(),
            "The highest-leverage first bridge is read-only: agents need one stable place to inspect learning candidates and their evidence without knowing filesystem internals.".to_string(),
        ],
        phase_1_substrate_status: vec![
            "Learning root: scopes/<principal>/<workspace>/learning/".to_string(),
            "Events: learning/events/YYYY-MM-DD.jsonl records source observations.".to_string(),
            "Candidates: learning/candidates/<candidate_id>.json stores typed proposals and provenance.".to_string(),
            "Evaluation backlog: learning/evaluations/backlog/<candidate_id>.json stores eval cases waiting for meta-harness review.".to_string(),
            "Evaluation runs: learning/evaluations/runs/<candidate_id>/<run_id>.json stores durable eval-worker reports.".to_string(),
            "Growth evaluation runs: learning/evaluations/growth_runs/<run_id>.json stores Phase 9 rollups across memory, skill, capability, program, teaching, and guardrail evidence.".to_string(),
            "Procedures: learning/procedures/{draft,active,deprecated,archived}/<procedure_id>.yaml stores reusable ways of working separate from semantic memory and executable skills.".to_string(),
            "Procedure decisions: learning/procedures/decisions/<procedure_id>.jsonl stores procedure status changes and review reasons.".to_string(),
            "Capability-evolution backlog: capability_evolution/backlog/<candidate_id>.json stores tool/capability improvement work waiting for review.".to_string(),
            "Capability-evolution proposals: capability_evolution/proposals/<candidate_id>.json stores reviewable fix/eval plans without mutating capability files.".to_string(),
            "Capability-evolution validations: capability_evolution/validations/<candidate_id>/<validation_id>.json stores validation evidence for approved proposals.".to_string(),
            "Capability-evolution implementations: capability_evolution/implementations/<candidate_id>/<implementation_id>.json stores reviewed file/patch bundles before promotion.".to_string(),
            "Capability-evolution applications: capability_evolution/applications/<candidate_id>/<application_id>.json stores dry-run or applied scoped file changes from implementation bundles.".to_string(),
            "Capability-evolution promotion audit: capability_evolution/promotion_audit.jsonl stores reviewed promotion evidence before broader promotion automation.".to_string(),
            "Decisions: learning/decisions/<candidate_id>.jsonl stores state transitions and review reasons.".to_string(),
            "The substrate is inert by design: creation and transition APIs do not apply promoted changes.".to_string(),
        ],
        next_actions: vec![
            "Route explicit reflections, user corrections, repeated tool failures, and memory-quality review outputs into LearningCandidate records.".to_string(),
            "Use procedure records for reusable ways of working that are broader than semantic memory but not yet proven enough to become executable skills.".to_string(),
            "Add reviewer/promotion workers that consume candidates, run evaluation, and only then modify memories, skills, prompts, wrappers, or docs.".to_string(),
            "Back future dashboards with this typed substrate rather than scraping prompt projections.".to_string(),
        ],
    })
}

fn entry_path(
    workspace_layout: &ArtifactV2Workspace,
    entry: &WorkspaceFileEntry,
) -> std::path::PathBuf {
    workspace_layout.base_root().join(&entry.relative_path)
}

fn path_exists(workspace_layout: &ArtifactV2Workspace, root: &Path) -> bool {
    workspace_layout
        .metadata_path_sync(root)
        .ok()
        .flatten()
        .is_some()
}

fn read_dir(workspace_layout: &ArtifactV2Workspace, root: &Path) -> Vec<WorkspaceFileEntry> {
    workspace_layout
        .read_dir_path_sync(root)
        .unwrap_or_default()
}

fn count_child_dirs(workspace_layout: &ArtifactV2Workspace, root: &Path) -> usize {
    read_dir(workspace_layout, root)
        .into_iter()
        .filter(|entry| entry.is_dir)
        .count()
}

fn count_chat_sessions(workspace_layout: &ArtifactV2Workspace, root: &Path) -> usize {
    if !path_exists(workspace_layout, root) {
        return 0;
    }
    read_dir(workspace_layout, root)
        .into_iter()
        .filter(|entry| {
            (entry.is_dir && entry.file_name != ".lifecycle")
                || entry_path(workspace_layout, entry)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    == Some("json")
        })
        .count()
}

fn count_nested_execution_dirs(workspace_layout: &ArtifactV2Workspace, tasks_root: &Path) -> usize {
    if !path_exists(workspace_layout, tasks_root) {
        return 0;
    }
    let mut count = 0usize;
    for task in read_dir(workspace_layout, tasks_root) {
        let executions = entry_path(workspace_layout, &task).join("executions");
        count += count_child_dirs(workspace_layout, &executions);
    }
    count
}

fn count_agent_memory_files(
    workspace_layout: &ArtifactV2Workspace,
    memory_agents_root: &Path,
    child_dir: &str,
) -> usize {
    if !path_exists(workspace_layout, memory_agents_root) {
        return 0;
    }
    let mut count = 0usize;
    for agent in read_dir(workspace_layout, memory_agents_root) {
        count += count_jsonish_files_recursive(
            workspace_layout,
            &entry_path(workspace_layout, &agent).join(child_dir),
        );
    }
    count
}

fn count_jsonish_files_recursive(workspace_layout: &ArtifactV2Workspace, root: &Path) -> usize {
    if !path_exists(workspace_layout, root) {
        return 0;
    }
    let mut count = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in read_dir(workspace_layout, &path) {
            if entry.is_dir {
                stack.push(entry_path(workspace_layout, &entry));
            } else if entry.is_file {
                let entry_path = entry_path(workspace_layout, &entry);
                let extension = entry_path.extension().and_then(|ext| ext.to_str());
                if matches!(extension, Some("json") | Some("jsonl") | Some("md")) {
                    count += 1;
                }
            }
        }
    }
    count
}

fn count_files_with_extension(
    workspace_layout: &ArtifactV2Workspace,
    root: &Path,
    extension: &str,
) -> usize {
    if !path_exists(workspace_layout, root) {
        return 0;
    }
    let mut count = 0usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in read_dir(workspace_layout, &path) {
            if entry.is_dir {
                stack.push(entry_path(workspace_layout, &entry));
            } else if entry_path(workspace_layout, &entry)
                .extension()
                .and_then(|ext| ext.to_str())
                == Some(extension)
            {
                count += 1;
            }
        }
    }
    count
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_authority_directory_is_not_counted_as_a_chat_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let root = workspace.chat_sessions_dir("owner", "default");
        std::fs::create_dir_all(root.join("session-1")).expect("session dir");
        std::fs::create_dir_all(root.join(".lifecycle").join("deleted"))
            .expect("lifecycle authority dir");
        std::fs::write(root.join("legacy-session.json"), b"{}").expect("legacy session fixture");

        assert_eq!(count_chat_sessions(&workspace, &root), 2);
    }
}
