//! Phase C: PlanPatcherAgent — patches refined data back into the PlanGraph.
//!
//! After the step-scoped refinement chain (slot-extractor → elicitor →
//! query-rewriter) completes for a Weak step, PlanPatcherAgent reads the
//! resulting ClarifiedTask and/or SlotGraph artifacts and patches the
//! refined data into the corresponding PlanStep in the PlanGraph.
//!
//! The agent then re-classifies the step's readiness so it can progress
//! from Weak → Ready (or remain Weak for another refinement round).

use async_trait::async_trait;
use chrono::Utc;
use tracing::info;
use uuid::Uuid;

use crate::magician_v2::{
    pipeline::{
        agent::{
            PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext,
            AGENT_ID_PLAN_PATCHER,
        },
        artifact::{AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION},
    },
    strategy::plan::{classify_step_readiness, PlanGraph},
};

/// Lightweight agent that reads refinement artifacts (ClarifiedTask, SlotGraph)
/// and patches them into the PlanGraph for the step currently being refined.
pub struct PlanPatcherAgent;

#[async_trait]
impl PipelineAgent for PlanPatcherAgent {
    fn agent_id(&self) -> &str {
        AGENT_ID_PLAN_PATCHER
    }

    fn required_inputs(&self) -> Vec<ArtifactType> {
        // No hard requirements — the agent gracefully handles missing artifacts.
        vec![]
    }

    fn output_types(&self) -> Vec<ArtifactType> {
        vec![ArtifactType::PlanGraph]
    }

    async fn execute(
        &self,
        store: &mut ArtifactStore,
        context: &PipelineContext,
    ) -> Result<PipelineAgentResult, PipelineAgentError> {
        let step_id = context
            .refinement_step_id
            .as_deref()
            .ok_or_else(|| PipelineAgentError::ExecutionFailed("no refinement_step_id".into()))?;

        // Read current PlanGraph
        let plan_artifact = store
            .latest_of_type(&ArtifactType::PlanGraph)
            .ok_or_else(|| PipelineAgentError::ExecutionFailed("no PlanGraph".into()))?;
        let mut plan: PlanGraph = plan_artifact
            .deserialize_content()
            .map_err(|e| PipelineAgentError::ExecutionFailed(format!("bad PlanGraph: {e}")))?;

        // Find the step to patch
        if let Some(step) = plan.steps.iter_mut().find(|s| s.id == step_id) {
            // Patch from ClarifiedTask if available
            if let Some(ct) = store.latest_of_type(&ArtifactType::ClarifiedTask) {
                if let Some(task) = ct.content.get("task").and_then(|v| v.as_str()) {
                    info!(step_id = %step_id, "patching step task from ClarifiedTask");
                    step.task = task.to_string();
                }
            }

            // Patch from SlotGraph — extract resolved params
            if let Some(sg) = store.latest_of_type(&ArtifactType::SlotGraph) {
                if let Some(slots) = sg.content.get("slots").and_then(|v| v.as_array()) {
                    for slot in slots {
                        if let (Some(name), Some(value)) = (
                            slot.get("name").and_then(|v| v.as_str()),
                            slot.get("resolved_value"),
                        ) {
                            if !value.is_null() {
                                info!(step_id = %step_id, param = %name, "patching step param from SlotGraph");
                                step.parameters.insert(name.to_string(), value.clone());
                            }
                        }
                    }
                }
            }

            // Re-classify this step's readiness
            step.readiness = Some(classify_step_readiness(step));
            info!(
                step_id = %step_id,
                readiness = ?step.readiness,
                "step readiness re-classified after refinement"
            );
        } else {
            tracing::warn!(step_id = %step_id, "refinement step not found in PlanGraph");
        }

        // Write updated PlanGraph back
        let content = serde_json::to_value(&plan).map_err(|e| {
            PipelineAgentError::SerializationError(format!("serialize PlanGraph: {e}"))
        })?;
        let artifact_id = Uuid::new_v4().to_string();
        store.put(AgentArtifact {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::PlanGraph,
            producer_agent_id: AGENT_ID_PLAN_PATCHER.to_string(),
            producer_cycle_id: context.cycle_id.clone(),
            content,
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });

        Ok(PipelineAgentResult::Completed {
            artifact_ids: vec![artifact_id],
        })
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::strategy::plan::{PlanStep, StepReadiness};

    fn make_context_with_refinement(step_id: &str) -> PipelineContext {
        PipelineContext {
            chain_id: "test-chain".into(),
            cycle_id: "cycle-1".into(),
            workflow_id: "wf-1".into(),
            query: "refine this step".into(),
            refinement_step_id: Some(step_id.into()),
            ..Default::default()
        }
    }

    fn make_plan_graph_with_weak_step() -> PlanGraph {
        PlanGraph {
            steps: vec![PlanStep {
                id: "s1".into(),
                task: "original vague task".into(),
                tool: None,
                confidence: 0.3,
                readiness: Some(StepReadiness::Weak),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn plan_patcher_patches_clarified_task() {
        let agent = PlanPatcherAgent;
        let context = make_context_with_refinement("s1");
        let mut store = ArtifactStore::new("test-chain".to_string());

        // Seed PlanGraph
        store.put(AgentArtifact {
            artifact_id: "pg-1".into(),
            artifact_type: ArtifactType::PlanGraph,
            producer_agent_id: "system:planner".into(),
            producer_cycle_id: "cycle-0".into(),
            content: serde_json::to_value(make_plan_graph_with_weak_step()).unwrap(),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });

        // Seed ClarifiedTask
        store.put(AgentArtifact {
            artifact_id: "ct-1".into(),
            artifact_type: ArtifactType::ClarifiedTask,
            producer_agent_id: "system:query-rewriter".into(),
            producer_cycle_id: "cycle-1".into(),
            content: serde_json::json!({ "task": "navigate to https://example.com using browser" }),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });

        let result = agent.execute(&mut store, &context).await.unwrap();
        assert!(matches!(result, PipelineAgentResult::Completed { .. }));

        // Verify patched PlanGraph
        let updated = store.latest_of_type(&ArtifactType::PlanGraph).unwrap();
        let plan: PlanGraph = serde_json::from_value(updated.content.clone()).unwrap();
        assert_eq!(
            plan.steps[0].task,
            "navigate to https://example.com using browser"
        );
    }

    #[tokio::test]
    async fn plan_patcher_patches_slot_params() {
        let agent = PlanPatcherAgent;
        let context = make_context_with_refinement("s1");
        let mut store = ArtifactStore::new("test-chain".to_string());

        // Seed PlanGraph
        store.put(AgentArtifact {
            artifact_id: "pg-1".into(),
            artifact_type: ArtifactType::PlanGraph,
            producer_agent_id: "system:planner".into(),
            producer_cycle_id: "cycle-0".into(),
            content: serde_json::to_value(make_plan_graph_with_weak_step()).unwrap(),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });

        // Seed SlotGraph with resolved slots
        store.put(AgentArtifact {
            artifact_id: "sg-1".into(),
            artifact_type: ArtifactType::SlotGraph,
            producer_agent_id: "system:slot-extractor".into(),
            producer_cycle_id: "cycle-1".into(),
            content: serde_json::json!({
                "slots": [
                    { "name": "url", "resolved_value": "https://example.com" },
                    { "name": "timeout", "resolved_value": 30 },
                    { "name": "unresolved_param", "resolved_value": null }
                ]
            }),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });

        let result = agent.execute(&mut store, &context).await.unwrap();
        assert!(matches!(result, PipelineAgentResult::Completed { .. }));

        // Verify patched PlanGraph
        let updated = store.latest_of_type(&ArtifactType::PlanGraph).unwrap();
        let plan: PlanGraph = serde_json::from_value(updated.content.clone()).unwrap();
        assert_eq!(
            plan.steps[0].parameters.get("url").and_then(|v| v.as_str()),
            Some("https://example.com")
        );
        assert_eq!(
            plan.steps[0]
                .parameters
                .get("timeout")
                .and_then(|v| v.as_i64()),
            Some(30)
        );
        // Null values should not be inserted
        assert!(plan.steps[0].parameters.get("unresolved_param").is_none());
    }

    #[tokio::test]
    async fn plan_patcher_fails_without_refinement_step_id() {
        let agent = PlanPatcherAgent;
        let context = PipelineContext {
            refinement_step_id: None,
            ..Default::default()
        };
        let mut store = ArtifactStore::new("test-chain".to_string());

        let result = agent.execute(&mut store, &context).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn plan_patcher_fails_without_plan_graph() {
        let agent = PlanPatcherAgent;
        let context = make_context_with_refinement("s1");
        let mut store = ArtifactStore::new("test-chain".to_string());

        let result = agent.execute(&mut store, &context).await;
        assert!(result.is_err());
    }
}
