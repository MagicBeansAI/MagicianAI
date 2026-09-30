//! # Lifecycle Integration Bridge
//!
//! Standalone fire-and-forget functions that production code paths call after
//! their existing operations to register artifacts in the lifecycle catalog.
//!
//! Each function adapts a domain-specific record into canonical
//! [`ArtifactMetadata`] via the corresponding domain adapter, then calls
//! [`LifecycleService::try_register`] which logs errors without propagating
//! them — ensuring that lifecycle registration never breaks existing flows.
//!
//! ## Integration Points
//!
//! | Function | Call after | Source location |
//! |----------|-----------|-----------------|
//! | `register_episode` | native episode persistence succeeds | `web_api.rs` post-execution |
//! | `register_pipeline` | `ArtifactStore::put()` | pipeline system agent stages |
//! | `register_execution` | outcome produced | agentic execution loop |

use std::sync::Arc;

use super::adapters::{
    DurableAdapter, EpisodeAdapter, ExecutionAdapter, PipelineAdapter, WorkflowAdapter,
};
use super::service::LifecycleService;
use super::types::OwnershipScope;
use crate::magician_v2::agents::types::StepArtifact;
use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;
use crate::magician_v2::execution::agentic::Artifact as ExecutionArtifact;
use crate::magician_v2::pipeline::artifact::AgentArtifact;

/// Register an episode artifact in the lifecycle catalog.
///
/// Call this after native episode persistence succeeds. Only episodes with a
/// non-None `artifact_output` produce a catalog entry (the adapter returns
/// `None` otherwise).
pub fn register_episode(
    service: &LifecycleService,
    episode: &V3EpisodeRecord,
    ownership: &OwnershipScope,
) {
    let adapter = EpisodeAdapter::new();
    if let Some(metadata) = adapter.adapt_episode_artifact(episode, ownership) {
        service.try_register(metadata);
    }
}

/// Register a pipeline artifact in the lifecycle catalog.
///
/// Call this after `ArtifactStore::put()` succeeds.
pub fn register_pipeline(
    service: &LifecycleService,
    artifact: &AgentArtifact,
    chain_id: &str,
    ownership: &OwnershipScope,
) {
    let adapter = PipelineAdapter::new();
    let metadata = adapter.adapt_pipeline_artifact(artifact, chain_id, ownership);
    service.try_register(metadata);
}

/// Register a workflow step artifact in the lifecycle catalog.
///
/// Call this after `persist_artifacts()` succeeds.
pub fn register_workflow(
    service: &LifecycleService,
    artifact: &StepArtifact,
    instance_id: &str,
    step_name: &str,
    agent_id: &str,
    ownership: &OwnershipScope,
) {
    let adapter = WorkflowAdapter::new();
    let metadata =
        adapter.adapt_step_artifact(artifact, instance_id, step_name, agent_id, ownership);
    service.try_register(metadata);
}

/// Register an execution artifact in the lifecycle catalog.
///
/// Call this after an agentic execution produces artifacts in its outcome.
pub fn register_execution(
    service: &LifecycleService,
    artifact: &ExecutionArtifact,
    agent_id: &str,
    cycle_id: &str,
    ownership: &OwnershipScope,
) {
    let adapter = ExecutionAdapter::new();
    let metadata = adapter.adapt_execution_artifact(artifact, agent_id, cycle_id, ownership);
    service.try_register(metadata);
}

/// Convenience: register all execution artifacts from a successful outcome.
///
/// Iterates the artifact list and registers each one individually.
pub fn register_execution_artifacts(
    service: &LifecycleService,
    artifacts: &[ExecutionArtifact],
    agent_id: &str,
    cycle_id: &str,
    ownership: &OwnershipScope,
) {
    for artifact in artifacts {
        register_execution(service, artifact, agent_id, cycle_id, ownership);
    }
}

/// Convenience: register all workflow step artifacts produced by a step.
pub fn register_workflow_artifacts(
    service: &LifecycleService,
    artifacts: &[StepArtifact],
    instance_id: &str,
    step_name: &str,
    agent_id: &str,
    ownership: &OwnershipScope,
) {
    for artifact in artifacts {
        register_workflow(
            service,
            artifact,
            instance_id,
            step_name,
            agent_id,
            ownership,
        );
    }
}

/// Register a durable artifact in the lifecycle catalog.
///
/// Call this after a file write to the durable artifact store succeeds.
/// Fire-and-forget: errors are logged, never propagated.
pub fn register_durable(
    service: &LifecycleService,
    namespace: &str,
    name: &str,
    agent_id: &str,
    ownership: &OwnershipScope,
) {
    let adapter = DurableAdapter::new();
    let metadata = adapter.adapt_durable_artifact(namespace, name, agent_id, ownership);
    service.try_register(metadata);
}

/// Release all references held by a given chain from pipeline artifacts.
///
/// Call this when a chain's plan graph is purged (e.g. during `full_replan`
/// in `from_suspension()`). Iterates pipeline artifacts in the catalog
/// whose UID matches the chain prefix, and removes any reference whose
/// `referrer_id` matches `chain-{chain_id}`.
pub fn release_chain_references(service: &LifecycleService, chain_id: &str) {
    use super::types::{ArtifactDomain, CatalogQuery};
    let referrer_id = format!("chain-{}", chain_id);
    let uid_prefix = format!("pipeline-{}-", chain_id);
    const PAGE_SIZE: usize = 100;
    let mut offset = 0;
    loop {
        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Pipeline),
            limit: Some(PAGE_SIZE),
            offset: Some(offset),
            ..Default::default()
        };
        let artifacts = match service.list(&query) {
            Ok(a) => a,
            Err(_) => break,
        };
        let page_len = artifacts.len();
        for art in artifacts {
            if art.artifact_uid.starts_with(&uid_prefix) {
                let _ = service.release_reference(&art.artifact_uid, &referrer_id);
            }
        }
        if page_len < PAGE_SIZE {
            break;
        }
        offset += PAGE_SIZE;
    }
}

/// Convenience wrapper for optional lifecycle service.
///
/// Production code can hold `Option<Arc<LifecycleService>>` and call this
/// to register without checking the option at every call site.
pub fn try_register_episode(
    service: Option<&Arc<LifecycleService>>,
    episode: &V3EpisodeRecord,
    ownership: &OwnershipScope,
) {
    if let Some(svc) = service {
        register_episode(svc, episode, ownership);
    }
}

/// Mark all pipeline artifacts for a chain as stale in the lifecycle catalog.
///
/// Queries the catalog directly (not the ArtifactStore) so this works even
/// after the ArtifactStore has already purged the artifacts. Only marks
/// artifacts whose UID matches the chain prefix `pipeline-{chain_id}-`.
pub fn mark_chain_artifacts_stale(service: &LifecycleService, chain_id: &str, reason: &str) {
    use super::types::{ArtifactDomain, CatalogQuery};
    let uid_prefix = format!("pipeline-{}-", chain_id);
    const PAGE_SIZE: usize = 100;
    let mut offset = 0;
    loop {
        let query = CatalogQuery {
            domain: Some(ArtifactDomain::Pipeline),
            limit: Some(PAGE_SIZE),
            offset: Some(offset),
            ..Default::default()
        };
        let artifacts = match service.list(&query) {
            Ok(a) => a,
            Err(_) => break,
        };
        let page_len = artifacts.len();
        for art in artifacts {
            if art.artifact_uid.starts_with(&uid_prefix) {
                if let Err(e) = service.mark_stale(&art.artifact_uid) {
                    tracing::debug!(
                        uid = %art.artifact_uid,
                        error = %e,
                        "mark_chain_artifacts_stale: {reason} (non-fatal)"
                    );
                }
            }
        }
        if page_len < PAGE_SIZE {
            break;
        }
        offset += PAGE_SIZE;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifacts::types::OwnershipScope;
    use chrono::Utc;
    use serde_json::json;

    fn make_episode(with_artifact: bool) -> V3EpisodeRecord {
        let now = Utc::now();
        V3EpisodeRecord::new_memory_episode(
            Some(("principal-a", "workspace-a")),
            "agent-1",
            "ep-1",
            "goal-1",
            "manual",
            1,
            now,
            None,
            now,
            now,
            &crate::magician_v2::agents::memory::EpisodeOutcome::GoalAchieved {
                summary: "test".to_string(),
            },
            vec![],
            vec![],
            vec![],
            None,
            None,
            if with_artifact {
                Some(json!({"emails": [1, 2, 3]}))
            } else {
                None
            },
        )
    }

    #[test]
    fn register_episode_with_artifact_output() {
        let service = LifecycleService::new();
        let episode = make_episode(true);
        let ownership = OwnershipScope::default();

        register_episode(&service, &episode, &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 1);
    }

    #[test]
    fn register_episode_without_artifact_output_is_noop() {
        let service = LifecycleService::new();
        let episode = make_episode(false);
        let ownership = OwnershipScope::default();

        register_episode(&service, &episode, &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 0);
    }

    #[test]
    fn register_pipeline_artifact() {
        let service = LifecycleService::new();
        let artifact = AgentArtifact {
            artifact_id: "art-1".to_string(),
            artifact_type: crate::magician_v2::pipeline::artifact::ArtifactType::QueryAnalysis,
            producer_agent_id: "agent-1".to_string(),
            producer_cycle_id: "cycle-1".to_string(),
            content: json!({"query": "test"}),
            schema_version: 1,
            produced_at: Utc::now(),
            render_hints: None,
        };
        let ownership = OwnershipScope {
            execution_id: Some("exec-1".to_string()),
            ..Default::default()
        };

        register_pipeline(&service, &artifact, "chain-1", &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 1);
        assert_eq!(stats.by_domain.get("pipeline"), Some(&1));
    }

    #[test]
    fn register_execution_artifact() {
        let service = LifecycleService::new();
        let artifact = ExecutionArtifact {
            name: "output".to_string(),
            content_type: "application/json".to_string(),
            data: b"{\"result\":true}".to_vec(),
            artifact_type: None,
            render_hints: None,
            materialized_path: None,
        };
        let ownership = OwnershipScope::default();

        register_execution(&service, &artifact, "agent-1", "cycle-1", &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 1);
        assert_eq!(stats.by_domain.get("execution"), Some(&1));
    }

    #[test]
    fn try_register_episode_with_none_service_is_noop() {
        let episode = make_episode(true);
        let ownership = OwnershipScope::default();
        // Should not panic with None service.
        try_register_episode(None, &episode, &ownership);
    }

    #[test]
    fn try_register_episode_with_some_service_registers() {
        let service = Arc::new(LifecycleService::new());
        let episode = make_episode(true);
        let ownership = OwnershipScope::default();

        try_register_episode(Some(&service), &episode, &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 1);
    }

    #[test]
    fn register_execution_artifacts_batch() {
        let service = LifecycleService::new();
        let artifacts = vec![
            ExecutionArtifact {
                name: "out-1".to_string(),
                content_type: "application/json".to_string(),
                data: b"{}".to_vec(),
                artifact_type: None,
                render_hints: None,
                materialized_path: None,
            },
            ExecutionArtifact {
                name: "out-2".to_string(),
                content_type: "text/plain".to_string(),
                data: b"hello".to_vec(),
                artifact_type: None,
                render_hints: None,
                materialized_path: None,
            },
        ];
        let ownership = OwnershipScope::default();

        register_execution_artifacts(&service, &artifacts, "agent-1", "cycle-1", &ownership);

        let stats = service.stats().unwrap();
        assert_eq!(stats.total_artifacts, 2);
    }
}
