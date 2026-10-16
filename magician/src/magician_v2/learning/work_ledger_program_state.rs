//! Work-ledger → program-state distiller (P2.1).
//!
//! For HARNESS agents (C-suite / meta-harness coordinators — those whose
//! definition carries `harness` + `autonomous_config.focus_areas`) this distills
//! the terminal work unit's open-loops / next-action / summary into a
//! `ProgramStateUpdate` learning candidate and routes it through the SUPPORTED
//! [`LearningProgramStateBridge`]. Because the candidate is provenance-stamped to
//! the harness agent itself and its patch stays inside the bookkeeping whitelist
//! (`open_loops` / `next_action_hints` / `last_run_summary`), the bridge's
//! Phase-4.1 auto-apply lane writes it into the focus-area program's runtime
//! state — so the next autonomous cycle sees where the last unit left off.
//!
//! Non-harness agents are skipped intentionally: they still get the work-ledger
//! evidence record + tier consolidation elsewhere, but no program-state (there is
//! no program for them to advance).
//!
//! Everything here is additive + fail-soft. `distill_work_unit_to_program_state`
//! returns `Ok(None)` when there is nothing to write (non-harness, no focus area,
//! empty patch) and never surfaces an error that could affect the run.

use anyhow::Result;
use serde_json::json;

use crate::magician_v2::agents::AgentDefinitionStore;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::evidence::WorkOutcomeInput;

use super::{
    CreateLearningCandidateRequest, LearningCandidate, LearningCandidateState,
    LearningCandidateType, LearningEvidenceRef, LearningProgramStateBridge,
    LearningProgramStateRouteOutcome, LearningRiskLevel, LearningScope, LearningStore,
};

/// Outcome of a work-unit → program-state distillation.
#[derive(Debug)]
pub struct WorkUnitProgramStateOutcome {
    /// The persisted candidate.
    pub candidate: LearningCandidate,
    /// The bridge's routing decision (auto-applied for a harness-self agent).
    pub route: LearningProgramStateRouteOutcome,
    /// The focus-area program document path the update targeted.
    pub program_relative_path: String,
}

/// Distill a completed HARNESS agent's work unit into a program-state update and
/// route it through the supported bridge.
///
/// Returns:
/// - `Ok(None)` when the agent is NOT a harness agent (no `harness` or no
///   `autonomous_config.focus_areas`), or when there is nothing to write (no
///   focus-area program, or the whitelist patch would be empty). This is the
///   intentional skip for non-harness agents.
/// - `Ok(Some(outcome))` when a whitelisted `ProgramStateUpdate` candidate was
///   persisted and routed (auto-applied for a harness-self agent).
///
/// The `agent_id` is the ledger-write's own agent (the run's agent); the
/// candidate is provenance-stamped to it so the bridge's `candidate_is_
/// harness_self_owned` gate recognises it and auto-applies the bookkeeping.
pub async fn distill_work_unit_to_program_state(
    workspace_layout: &ArtifactV2Workspace,
    scope: &LearningScope,
    agent_id: &str,
    input: &WorkOutcomeInput,
) -> Result<Option<WorkUnitProgramStateOutcome>> {
    // 1. Load the agent definition; skip unless it is a harness agent with at
    //    least one focus area. Non-harness agents write no program-state here.
    let definition_store = AgentDefinitionStore::with_workspace_layout(workspace_layout.clone())
        .for_scope(&scope.principal, &scope.workspace);
    let Some(record) = definition_store.get_definition(agent_id).await? else {
        return Ok(None);
    };
    let definition = &record.definition;
    if definition.harness.is_none() {
        return Ok(None);
    }
    let Some(autonomous) = definition.autonomous_config.as_ref() else {
        return Ok(None);
    };

    // 2. Pick a deterministic focus-area program document path. A focus area may
    //    omit `program` (inheriting the harness default `program.md`); the first
    //    focus area with a usable path wins, else the default doc. The chosen
    //    path MUST resolve back to a focus area the bridge's provenance gate
    //    recognises — which it does, since we pick from this agent's own areas.
    let Some(program_relative_path) = resolve_program_relative_path(autonomous) else {
        return Ok(None);
    };

    // 3. Build the whitelist-only patch. ONLY the three bookkeeping keys that the
    //    bridge's `HARNESS_AUTO_APPLY_FIELDS` accepts and that a work unit
    //    produces: `open_loops`, `next_action_hints`, `last_run_summary`. Any
    //    field absent from the work unit is omitted so the patch never carries an
    //    empty/meaningless key.
    let mut patch = serde_json::Map::new();
    if !input.open_loops.is_empty() {
        patch.insert("open_loops".to_string(), json!(input.open_loops));
    }
    if let Some(hint) = input
        .next_step_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        patch.insert("next_action_hints".to_string(), json!([hint]));
    }
    let summary = input.summary.trim();
    if !summary.is_empty() {
        patch.insert(
            "last_run_summary".to_string(),
            json!({
                "outcome": input.outcome,
                "summary": summary,
                "root_execution_id": input.root_execution_id,
            }),
        );
    }
    if patch.is_empty() {
        // Nothing to advance — no open loops, no next step, no summary.
        return Ok(None);
    }

    let source_ref = format!("evd:run:{}", input.root_execution_id);
    let reason = format!(
        "Work-ledger distilled the terminal work unit ({outcome}) into program-state bookkeeping.",
        outcome = input.outcome
    );

    // 4. Persist the candidate and route it through the bridge. The candidate is
    //    Low-risk, review NOT required, provenance = this harness agent, so the
    //    bridge's auto-apply lane advances the focus-area program's bookkeeping.
    let request = CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type: LearningCandidateType::ProgramStateUpdate,
        state: LearningCandidateState::Proposed,
        title: format!(
            "Program-state bookkeeping from run {}",
            input.root_execution_id
        ),
        summary: reason.clone(),
        rationale: reason.clone(),
        proposed_change: json!({
            "program_state_update": {
                "program_relative_path": program_relative_path,
                "patch": patch,
                "reason": reason,
                "source_refs": [source_ref.clone()],
            }
        }),
        proposed_target: Some(program_relative_path.clone()),
        confidence: Some(1.0),
        source_agent_id: Some(agent_id.to_string()),
        source_task_id: input.task_id.clone(),
        source_execution_id: Some(input.root_execution_id.clone()),
        source_chat_session_id: None,
        event_refs: Vec::new(),
        evidence_refs: vec![LearningEvidenceRef {
            kind: "work_outcome".to_string(),
            id: None,
            path: None,
            uri: None,
            summary: Some(source_ref),
        }],
        risk_level: LearningRiskLevel::Low,
        review_required: false,
        review_reason: None,
        review_policy: json!({}),
        promotion_target: None,
        promotion_policy: json!({}),
    };

    let store = LearningStore::new(workspace_layout.clone());
    let candidate = store.create_candidate(scope.clone(), request)?;

    let bridge = LearningProgramStateBridge::new(workspace_layout.clone());
    let route = bridge.route_candidate(&store, scope, &candidate).await?;

    Ok(Some(WorkUnitProgramStateOutcome {
        candidate,
        route,
        program_relative_path,
    }))
}

/// Deterministically choose the focus-area `program` document path: the first
/// focus area with a non-empty `program`, else — when at least one focus area
/// exists — the harness default `program.md` (which the bridge's provenance gate
/// also accepts for a focus area that omits `program`). `None` only when there
/// are no focus areas at all.
fn resolve_program_relative_path(
    autonomous: &crate::magician_v2::agents::types::AutonomousConfig,
) -> Option<String> {
    if autonomous.focus_areas.is_empty() {
        return None;
    }
    let explicit = autonomous.focus_areas.iter().find_map(|focus_area| {
        focus_area
            .program
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    });
    Some(explicit.unwrap_or_else(|| "program.md".to_string()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::AgentDefinition;
    use crate::magician_v2::harness::ProgramLoader;
    use tempfile::TempDir;
    use tokio::fs;

    /// A harness agent (harness + autonomous_config.focus_areas) whose sole focus
    /// area targets `revenue_strategy.md`.
    fn harness_definition() -> AgentDefinition {
        AgentDefinition::from_yaml_str(
            r#"
agent_id: "cro"
name: "CRO"
persona: "Chief Revenue Officer"
kind: personal
principal: "anonymous"
workspace: "default"
is_primary: true
autonomous_config:
  schedule: "0 7 * * *"
  focus_areas:
    - name: "Revenue Strategy"
      description: "Grow revenue"
      priority: high
      program: "revenue_strategy.md"
harness: {}
"#,
        )
        .expect("harness definition")
    }

    /// A plain agent with neither harness nor autonomous_config — the intended
    /// skip case.
    fn plain_definition() -> AgentDefinition {
        AgentDefinition::from_yaml_str(
            r#"
agent_id: "worker"
name: "Worker"
persona: "Just a worker"
tools: []
"#,
        )
        .expect("plain definition")
    }

    fn work_input(root_execution_id: &str) -> WorkOutcomeInput {
        WorkOutcomeInput {
            root_execution_id: root_execution_id.to_string(),
            task_id: Some("task-1".to_string()),
            agent_id: "cro".to_string(),
            outcome: "success".to_string(),
            summary: "Closed the Q3 pipeline review.".to_string(),
            artifacts: vec!["artifact:report.md".to_string()],
            open_loops: vec!["Follow up with finance on the forecast".to_string()],
            next_step_hint: Some("Draft the board update".to_string()),
            entity_keys: Vec::new(),
            timestamp_ms: 1_760_000_000_000,
        }
    }

    async fn seed_program_doc(workspace: &ArtifactV2Workspace, principal: &str, wkspace: &str) {
        let programs_root = workspace.program_specs_root(principal, wkspace);
        fs::create_dir_all(&programs_root)
            .await
            .expect("programs root");
        fs::write(
            programs_root.join("revenue_strategy.md"),
            "# Revenue Strategy\n\nGrow revenue.\n",
        )
        .await
        .expect("program write");
    }

    /// A HARNESS agent's work unit → a whitelisted `ProgramStateUpdate` candidate
    /// whose patch is EXACTLY the three bookkeeping keys, and `route_candidate`
    /// auto-applies it into the focus-area program's runtime state.
    #[tokio::test]
    async fn harness_work_unit_produces_whitelisted_candidate_and_applies() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LearningScope::new("anonymous", "default");

        // Persist the harness agent definition into the scoped runtime root — the
        // same scope + path the bridge's provenance gate reads.
        let store = AgentDefinitionStore::with_workspace_layout(workspace.clone())
            .for_scope(&scope.principal, &scope.workspace);
        store
            .create_definition(harness_definition())
            .await
            .expect("create harness definition");
        seed_program_doc(&workspace, &scope.principal, &scope.workspace).await;

        let input = work_input("exec-harness-1");
        let outcome = distill_work_unit_to_program_state(&workspace, &scope, "cro", &input)
            .await
            .expect("distill ok")
            .expect("harness agent produces a candidate");

        // The candidate is a ProgramStateUpdate targeting the focus-area program.
        assert_eq!(
            outcome.candidate.candidate_type,
            LearningCandidateType::ProgramStateUpdate
        );
        assert_eq!(outcome.program_relative_path, "revenue_strategy.md");

        // The patch is EXACTLY the three whitelisted bookkeeping keys.
        let patch = outcome
            .candidate
            .proposed_change
            .get("program_state_update")
            .and_then(|value| value.get("patch"))
            .and_then(|value| value.as_object())
            .expect("patch object");
        let mut keys = patch.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "last_run_summary".to_string(),
                "next_action_hints".to_string(),
                "open_loops".to_string(),
            ]
        );
        assert_eq!(
            patch
                .get("open_loops")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            patch
                .get("next_action_hints")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );

        // The bridge auto-applied it (harness-self provenance + whitelist patch).
        assert!(
            outcome.route.applied,
            "expected auto-apply, got reason: {}",
            outcome.route.reason
        );

        // The focus-area program's runtime state now carries the bookkeeping.
        let loader = ProgramLoader::new(workspace.clone());
        let loaded = loader
            .load_by_reference(
                &scope.principal,
                &scope.workspace,
                "revenue_strategy.md",
                None,
            )
            .await
            .expect("load")
            .expect("program");
        let state = loader
            .load_runtime_state(&scope.principal, &scope.workspace, &loaded, None)
            .await
            .expect("state load")
            .expect("state exists");
        assert_eq!(state.open_loops.len(), 1);
        assert_eq!(state.next_action_hints, vec!["Draft the board update"]);
        assert!(state.last_run_summary.is_some());
    }

    /// A NON-harness agent's work unit produces no program-state candidate.
    #[tokio::test]
    async fn non_harness_work_unit_produces_no_candidate() {
        let temp = TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path());
        let scope = LearningScope::new("anonymous", "default");

        let store = AgentDefinitionStore::with_workspace_layout(workspace.clone())
            .for_scope(&scope.principal, &scope.workspace);
        store
            .create_definition(plain_definition())
            .await
            .expect("create plain definition");

        let mut input = work_input("exec-plain-1");
        input.agent_id = "worker".to_string();
        let outcome = distill_work_unit_to_program_state(&workspace, &scope, "worker", &input)
            .await
            .expect("distill ok");
        assert!(
            outcome.is_none(),
            "non-harness agent must not produce a program-state candidate"
        );

        // And no candidate was persisted for the scope.
        let learning_store = LearningStore::new(workspace.clone());
        let candidates = learning_store
            .list_candidates(&scope, Default::default())
            .expect("list candidates");
        assert!(
            candidates.is_empty(),
            "expected no persisted candidates, found {}",
            candidates.len()
        );
    }
}
