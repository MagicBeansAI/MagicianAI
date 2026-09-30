//! # Domain Adapters
//!
//! Translate domain-specific records into canonical catalog metadata
//! ([`ArtifactMetadata`]). There is one adapter per artifact domain:
//!
//! | Adapter            | Source type      | Domain      |
//! |--------------------|------------------|-------------|
//! | `PipelineAdapter`  | `AgentArtifact`  | Pipeline    |
//! | `WorkflowAdapter`  | `StepArtifact`   | Workflow    |
//! | `ExecutionAdapter` | `Artifact`       | Execution   |
//! | `EpisodeAdapter`   | `V3EpisodeRecord`| Episode     |
//!
//! Each adapter implements the [`DomainAdapter`] trait for identity/UID
//! generation and provides a domain-specific `adapt_*` method that maps
//! source fields to [`ArtifactMetadata`] with appropriate default policies.

use chrono::{DateTime, Utc};

use super::types::{
    ArtifactDomain, ArtifactMetadata, ArtifactUid, ExposureClass, FreshnessClass, LifecycleState,
    LifecycleTransition, OwnershipScope, PhysicalLocator, PolicyBindings, ProducerInfo,
    ProtectionFlags, RenderHints, RetentionClass, TransitionReason,
};
use crate::magician_v2::agents::types::StepArtifact;
use crate::magician_v2::artifact_v2::memory::V3EpisodeRecord;
use crate::magician_v2::execution::agentic::Artifact as ExecutionArtifact;
use crate::magician_v2::pipeline::artifact::{AgentArtifact, ArtifactType};

// ---------------------------------------------------------------------------
// DomainAdapter trait
// ---------------------------------------------------------------------------

/// Common trait for all domain adapters.
///
/// Provides domain identification and UID generation. Domain-specific
/// adaptation methods live on each concrete adapter because their signatures
/// differ by source type.
pub trait DomainAdapter: Send + Sync {
    /// The domain this adapter handles.
    fn domain(&self) -> ArtifactDomain;

    /// Generate a unique artifact UID for this domain.
    ///
    /// The returned UID is prefixed with the domain name (e.g.
    /// `"pipeline-<domain_id>"`) so UIDs are globally unambiguous.
    fn generate_uid(&self, domain_id: &str) -> ArtifactUid;
}

// ---------------------------------------------------------------------------
// PipelineAdapter
// ---------------------------------------------------------------------------

/// Adapts [`AgentArtifact`] (pipeline coordination artifacts) into catalog
/// metadata.
///
/// Default policy:
/// - Retention: `TaskLifetime`
/// - Freshness: `RevalidateOnResume`
/// - Exposure:  `InternalSanitized`
///
/// `PlanGraph` artifacts receive `replan_safe = true`.
#[derive(Debug, Clone, Default)]
pub struct PipelineAdapter;

impl DomainAdapter for PipelineAdapter {
    fn domain(&self) -> ArtifactDomain {
        ArtifactDomain::Pipeline
    }

    fn generate_uid(&self, domain_id: &str) -> ArtifactUid {
        format!("pipeline-{}", domain_id)
    }
}

impl PipelineAdapter {
    /// Create a new `PipelineAdapter`.
    pub fn new() -> Self {
        Self
    }

    /// Adapt an [`AgentArtifact`] into canonical [`ArtifactMetadata`].
    ///
    /// # Arguments
    /// - `artifact` -- the pipeline artifact to adapt.
    /// - `chain_id` -- the pipeline chain that stores this artifact.
    /// - `ownership` -- ownership scope (thread, task, etc.).
    pub fn adapt_pipeline_artifact(
        &self,
        artifact: &AgentArtifact,
        chain_id: &str,
        ownership: &OwnershipScope,
    ) -> ArtifactMetadata {
        // Include chain_id in the UID so lookups from ArtifactStore
        // (which use `pipeline-{chain_id}-{artifact_id}`) match.
        let domain_id = format!("{}-{}", chain_id, artifact.artifact_id);
        let uid = self.generate_uid(&domain_id);
        let now = Utc::now();

        // Only plan artifacts get replan_safe protection.
        let replan_safe = matches!(artifact.artifact_type, ArtifactType::PlanGraph);

        ArtifactMetadata {
            artifact_uid: uid,
            domain: ArtifactDomain::Pipeline,
            artifact_type: Some(artifact.artifact_type.to_string()),
            physical_locator: PhysicalLocator::PipelineStore {
                chain_id: chain_id.to_string(),
                artifact_id: artifact.artifact_id.clone(),
            },
            route_target: None,
            ownership: ownership.clone(),
            producer: ProducerInfo {
                producer_agent_id: artifact.producer_agent_id.clone(),
                producer_stage: Some(artifact.artifact_type.to_string()),
                produced_at: artifact.produced_at,
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::TaskLifetime,
                freshness_class: FreshnessClass::RevalidateOnResume,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags {
                    replan_safe,
                    ..ProtectionFlags::default()
                },
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: now,
            expires_at: None,
            references: vec![],
            render_hints: artifact.render_hints.clone(),
            transition_log: vec![LifecycleTransition {
                from_state: LifecycleState::Active,
                to_state: LifecycleState::Active,
                reason: TransitionReason::Registered,
                transitioned_at: now,
            }]
            .into(),
        }
    }
}

// ---------------------------------------------------------------------------
// WorkflowAdapter
// ---------------------------------------------------------------------------

/// Adapts [`StepArtifact`] (workflow step outputs) into catalog metadata.
///
/// Default policy:
/// - Retention: `RunLifetime`
/// - Freshness: `Window { fresh_seconds: 300, stale_seconds: 600 }`
/// - Exposure:  `InternalSanitized`
#[derive(Debug, Clone, Default)]
pub struct WorkflowAdapter;

impl DomainAdapter for WorkflowAdapter {
    fn domain(&self) -> ArtifactDomain {
        ArtifactDomain::Workflow
    }

    fn generate_uid(&self, domain_id: &str) -> ArtifactUid {
        format!("workflow-{}", domain_id)
    }
}

impl WorkflowAdapter {
    /// Create a new `WorkflowAdapter`.
    pub fn new() -> Self {
        Self
    }

    /// Adapt a [`StepArtifact`] into canonical [`ArtifactMetadata`].
    ///
    /// # Arguments
    /// - `artifact`    -- the workflow step artifact to adapt.
    /// - `instance_id` -- the workflow instance ID.
    /// - `step_name`   -- the step that produced this artifact.
    /// - `agent_id`    -- the agent that executed the step.
    /// - `ownership`   -- ownership scope (thread, task, etc.).
    pub fn adapt_step_artifact(
        &self,
        artifact: &StepArtifact,
        instance_id: &str,
        step_name: &str,
        agent_id: &str,
        ownership: &OwnershipScope,
    ) -> ArtifactMetadata {
        let composite_id = format!("{}-{}-{}", instance_id, step_name, artifact.name);
        let uid = self.generate_uid(&composite_id);
        let now = Utc::now();

        // Use provenance produced_at if available, otherwise now.
        let produced_at = artifact.provenance.produced_at;

        // Build ownership with workflow instance if not already set.
        let mut effective_ownership = ownership.clone();
        if effective_ownership.workflow_instance_id.is_none() {
            effective_ownership.workflow_instance_id = Some(instance_id.to_string());
        }

        ArtifactMetadata {
            artifact_uid: uid,
            domain: ArtifactDomain::Workflow,
            artifact_type: Some(artifact.artifact_type.clone()),
            physical_locator: PhysicalLocator::WorkflowRun {
                instance_id: instance_id.to_string(),
                step_name: step_name.to_string(),
                artifact_name: artifact.name.clone(),
            },
            route_target: None,
            ownership: effective_ownership,
            producer: ProducerInfo {
                producer_agent_id: agent_id.to_string(),
                producer_stage: Some(step_name.to_string()),
                produced_at,
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::RunLifetime,
                freshness_class: FreshnessClass::Window {
                    fresh_seconds: 300,
                    stale_seconds: 600,
                },
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: now,
            expires_at: None,
            references: vec![],
            render_hints: artifact.render_hints.clone(),
            transition_log: vec![LifecycleTransition {
                from_state: LifecycleState::Active,
                to_state: LifecycleState::Active,
                reason: TransitionReason::Registered,
                transitioned_at: now,
            }]
            .into(),
        }
    }
}

// ---------------------------------------------------------------------------
// ExecutionAdapter
// ---------------------------------------------------------------------------

/// Adapts [`ExecutionArtifact`] (agentic execution outputs) into catalog
/// metadata.
///
/// Default policy:
/// - Retention: `GoalLifetime`
/// - Freshness: `Evergreen`
/// - Exposure:  `InternalSanitized`
#[derive(Debug, Clone, Default)]
pub struct ExecutionAdapter;

impl DomainAdapter for ExecutionAdapter {
    fn domain(&self) -> ArtifactDomain {
        ArtifactDomain::Execution
    }

    fn generate_uid(&self, domain_id: &str) -> ArtifactUid {
        format!("execution-{}", domain_id)
    }
}

impl ExecutionAdapter {
    /// Create a new `ExecutionAdapter`.
    pub fn new() -> Self {
        Self
    }

    /// Adapt an [`ExecutionArtifact`] into canonical [`ArtifactMetadata`].
    ///
    /// # Arguments
    /// - `artifact`  -- the execution artifact to adapt.
    /// - `agent_id`  -- the agent that produced this artifact.
    /// - `cycle_id`  -- the execution cycle that produced it.
    /// - `ownership` -- ownership scope (thread, task, etc.).
    pub fn adapt_execution_artifact(
        &self,
        artifact: &ExecutionArtifact,
        agent_id: &str,
        cycle_id: &str,
        ownership: &OwnershipScope,
    ) -> ArtifactMetadata {
        let composite_id = format!("{}-{}-{}", agent_id, cycle_id, artifact.name);
        let uid = self.generate_uid(&composite_id);
        let now = Utc::now();

        // Build ownership with cycle if not already set.
        let mut effective_ownership = ownership.clone();
        if effective_ownership.cycle_id.is_none() {
            effective_ownership.cycle_id = Some(cycle_id.to_string());
        }

        // Use declared artifact_type if present, otherwise fall back to name.
        let effective_type = artifact
            .artifact_type
            .clone()
            .unwrap_or_else(|| artifact.name.clone());

        // Use DurableStore locator when a materialized path exists.
        let physical_locator = match &artifact.materialized_path {
            Some(path) => PhysicalLocator::DurableStore {
                namespace: "execution_artifacts".to_string(),
                name: path.clone(),
            },
            None => PhysicalLocator::InMemory {
                key: artifact.name.clone(),
            },
        };

        ArtifactMetadata {
            artifact_uid: uid,
            domain: ArtifactDomain::Execution,
            artifact_type: Some(effective_type),
            physical_locator,
            route_target: None,
            ownership: effective_ownership,
            producer: ProducerInfo {
                producer_agent_id: agent_id.to_string(),
                producer_stage: None,
                produced_at: now,
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::GoalLifetime,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: now,
            expires_at: None,
            references: vec![],
            render_hints: artifact.render_hints.clone(),
            transition_log: vec![LifecycleTransition {
                from_state: LifecycleState::Active,
                to_state: LifecycleState::Active,
                reason: TransitionReason::Registered,
                transitioned_at: now,
            }]
            .into(),
        }
    }
}

// ---------------------------------------------------------------------------
// EpisodeAdapter
// ---------------------------------------------------------------------------

/// Adapts [`V3EpisodeRecord`] (agent episode outputs) into catalog metadata.
///
/// Returns `None` if the episode has no `artifact_output`.
///
/// Default policy:
/// - Retention: `Permanent`
/// - Freshness: `Evergreen`
/// - Exposure:  `GauiSanitized`
#[derive(Debug, Clone, Default)]
pub struct EpisodeAdapter;

impl DomainAdapter for EpisodeAdapter {
    fn domain(&self) -> ArtifactDomain {
        ArtifactDomain::Episode
    }

    fn generate_uid(&self, domain_id: &str) -> ArtifactUid {
        format!("episode-{}", domain_id)
    }
}

impl EpisodeAdapter {
    /// Create a new `EpisodeAdapter`.
    pub fn new() -> Self {
        Self
    }

    /// Adapt a [`V3EpisodeRecord`] into canonical [`ArtifactMetadata`].
    ///
    /// Returns `None` if `record.artifact_output` is `None` (the episode
    /// did not produce an artifact).
    ///
    /// # Arguments
    /// - `record`    -- the episode record to adapt.
    /// - `ownership` -- ownership scope (thread, task, etc.).
    pub fn adapt_episode_artifact(
        &self,
        record: &V3EpisodeRecord,
        ownership: &OwnershipScope,
    ) -> Option<ArtifactMetadata> {
        // Episodes without artifact output have nothing to register.
        record.artifact_output.as_ref()?;

        let uid = self.generate_uid(&record.episode_id);
        let now = Utc::now();

        // Extract envelope keys if the artifact_output is an object with
        // `_artifact_type` and/or `_render_hints` fields.
        let (declared_type, declared_hints) = record
            .artifact_output
            .as_ref()
            .and_then(|v| v.as_object())
            .map(|obj| {
                let at = obj
                    .get("_artifact_type")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let rh = obj
                    .get("_render_hints")
                    .and_then(|v| serde_json::from_value::<RenderHints>(v.clone()).ok());
                (at, rh)
            })
            .unwrap_or((None, None));

        Some(ArtifactMetadata {
            artifact_uid: uid,
            domain: ArtifactDomain::Episode,
            artifact_type: Some(declared_type.unwrap_or_else(|| "artifact_output".to_string())),
            physical_locator: PhysicalLocator::EpisodeFile {
                agent_id: record.agent_id.clone(),
                episode_id: record.episode_id.clone(),
            },
            route_target: None,
            ownership: ownership.clone(),
            producer: ProducerInfo {
                producer_agent_id: record.agent_id.clone(),
                producer_stage: None,
                produced_at: DateTime::parse_from_rfc3339(&record.completed_at)
                    .map(|value| value.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now()),
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::Permanent,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::GauiSanitized,
                protection_flags: ProtectionFlags::default(),
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: now,
            expires_at: None,
            references: vec![],
            render_hints: declared_hints,
            transition_log: vec![LifecycleTransition {
                from_state: LifecycleState::Active,
                to_state: LifecycleState::Active,
                reason: TransitionReason::Registered,
                transitioned_at: now,
            }]
            .into(),
        })
    }
}

// ---------------------------------------------------------------------------
// DurableAdapter
// ---------------------------------------------------------------------------

/// Adapts durable artifacts (cross-run persistent files) into catalog metadata.
///
/// Default policy:
/// - Retention: `Permanent`
/// - Freshness: `Evergreen`
/// - Exposure:  `InternalSanitized`
#[derive(Debug, Clone, Default)]
pub struct DurableAdapter;

impl DomainAdapter for DurableAdapter {
    fn domain(&self) -> ArtifactDomain {
        ArtifactDomain::Durable
    }

    fn generate_uid(&self, domain_id: &str) -> ArtifactUid {
        format!("durable-{}", domain_id)
    }
}

impl DurableAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Adapt a durable artifact into canonical [`ArtifactMetadata`].
    pub fn adapt_durable_artifact(
        &self,
        namespace: &str,
        name: &str,
        agent_id: &str,
        ownership: &OwnershipScope,
    ) -> ArtifactMetadata {
        let composite_id = format!("{}-{}", namespace, name);
        let uid = self.generate_uid(&composite_id);
        let now = Utc::now();

        ArtifactMetadata {
            artifact_uid: uid,
            domain: ArtifactDomain::Durable,
            artifact_type: None,
            physical_locator: PhysicalLocator::DurableStore {
                namespace: namespace.to_string(),
                name: name.to_string(),
            },
            route_target: None,
            ownership: ownership.clone(),
            producer: ProducerInfo {
                producer_agent_id: agent_id.to_string(),
                producer_stage: None,
                produced_at: now,
            },
            policy: PolicyBindings {
                retention_class: RetentionClass::Permanent,
                freshness_class: FreshnessClass::Evergreen,
                exposure_class: ExposureClass::InternalSanitized,
                protection_flags: ProtectionFlags::default(),
            },
            lifecycle_state: LifecycleState::Active,
            last_validated_at: now,
            expires_at: None,
            references: vec![],
            render_hints: None,
            transition_log: vec![LifecycleTransition {
                from_state: LifecycleState::Active,
                to_state: LifecycleState::Active,
                reason: TransitionReason::Registered,
                transitioned_at: now,
            }]
            .into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::ArtifactProvenance;
    use serde_json::json;

    // -- helpers ----------------------------------------------------------

    fn sample_ownership() -> OwnershipScope {
        OwnershipScope {
            execution_id: Some("exec-42".to_string()),
            task_id: Some("task-7".to_string()),
            ..Default::default()
        }
    }

    fn sample_pipeline_artifact(artifact_type: ArtifactType) -> AgentArtifact {
        AgentArtifact {
            artifact_id: "art-001".to_string(),
            artifact_type,
            producer_agent_id: "planner-agent".to_string(),
            producer_cycle_id: "cycle-1".to_string(),
            content: json!({"plan": "do stuff"}),
            schema_version: 1,
            produced_at: Utc::now(),
            render_hints: None,
        }
    }

    fn sample_step_artifact() -> StepArtifact {
        StepArtifact {
            name: "extract-output".to_string(),
            artifact_type: "extraction".to_string(),
            content: json!({"data": [1, 2, 3]}),
            provenance: ArtifactProvenance {
                workflow_instance_id: Some("wf-99".to_string()),
                step_name: Some("extract".to_string()),
                agent_id: "extractor-agent".to_string(),
                source_agent_ids: vec![],
                produced_at: Utc::now(),
            },
            content_type: Some("application/json".to_string()),
            render_hints: None,
        }
    }

    fn sample_execution_artifact() -> ExecutionArtifact {
        ExecutionArtifact {
            name: "browser-screenshot".to_string(),
            content_type: "image/png".to_string(),
            data: b"fake-png-data".to_vec(),
            artifact_type: None,
            render_hints: None,
            materialized_path: None,
        }
    }

    fn sample_episode_record(with_output: bool) -> V3EpisodeRecord {
        let now = Utc::now();
        V3EpisodeRecord::new_memory_episode(
            Some(("principal-a", "workspace-a")),
            "research-agent",
            "ep-100",
            "goal-5",
            "manual",
            1,
            now,
            None,
            now,
            now,
            &crate::magician_v2::agents::memory::EpisodeOutcome::GoalAchieved {
                summary: "completed".to_string(),
            },
            vec![],
            vec![],
            vec![],
            None,
            None,
            if with_output {
                Some(json!({"result": "found 42"}))
            } else {
                None
            },
        )
    }

    // -- UID prefix tests -------------------------------------------------

    #[test]
    fn pipeline_uid_is_domain_prefixed() {
        let adapter = PipelineAdapter::new();
        let uid = adapter.generate_uid("abc-123");
        assert!(
            uid.starts_with("pipeline-"),
            "pipeline UID must start with 'pipeline-', got: {uid}"
        );
        assert_eq!(uid, "pipeline-abc-123");
    }

    #[test]
    fn workflow_uid_is_domain_prefixed() {
        let adapter = WorkflowAdapter::new();
        let uid = adapter.generate_uid("step-out");
        assert!(
            uid.starts_with("workflow-"),
            "workflow UID must start with 'workflow-', got: {uid}"
        );
        assert_eq!(uid, "workflow-step-out");
    }

    #[test]
    fn execution_uid_is_domain_prefixed() {
        let adapter = ExecutionAdapter::new();
        let uid = adapter.generate_uid("screenshot-1");
        assert!(
            uid.starts_with("execution-"),
            "execution UID must start with 'execution-', got: {uid}"
        );
        assert_eq!(uid, "execution-screenshot-1");
    }

    #[test]
    fn episode_uid_is_domain_prefixed() {
        let adapter = EpisodeAdapter::new();
        let uid = adapter.generate_uid("ep-42");
        assert!(
            uid.starts_with("episode-"),
            "episode UID must start with 'episode-', got: {uid}"
        );
        assert_eq!(uid, "episode-ep-42");
    }

    // -- domain() trait method tests --------------------------------------

    #[test]
    fn adapters_report_correct_domain() {
        assert_eq!(PipelineAdapter::new().domain(), ArtifactDomain::Pipeline);
        assert_eq!(WorkflowAdapter::new().domain(), ArtifactDomain::Workflow);
        assert_eq!(ExecutionAdapter::new().domain(), ArtifactDomain::Execution);
        assert_eq!(EpisodeAdapter::new().domain(), ArtifactDomain::Episode);
    }

    // -- PipelineAdapter tests --------------------------------------------

    #[test]
    fn pipeline_adapter_produces_correct_metadata() {
        let adapter = PipelineAdapter::new();
        let artifact = sample_pipeline_artifact(ArtifactType::QueryAnalysis);
        let ownership = sample_ownership();

        let meta = adapter.adapt_pipeline_artifact(&artifact, "chain-abc", &ownership);

        assert_eq!(meta.artifact_uid, "pipeline-chain-abc-art-001");
        assert_eq!(meta.domain, ArtifactDomain::Pipeline);

        // Physical locator is PipelineStore.
        match &meta.physical_locator {
            PhysicalLocator::PipelineStore {
                chain_id,
                artifact_id,
            } => {
                assert_eq!(chain_id, "chain-abc");
                assert_eq!(artifact_id, "art-001");
            },
            PhysicalLocator::WorkflowRun { .. }
            | PhysicalLocator::EpisodeFile { .. }
            | PhysicalLocator::InMemory { .. }
            | PhysicalLocator::DurableStore { .. } => {
                panic!(
                    "expected PipelineStore locator, got: {:?}",
                    meta.physical_locator
                )
            },
        }

        // Producer info.
        assert_eq!(meta.producer.producer_agent_id, "planner-agent");
        assert_eq!(meta.producer.produced_at, artifact.produced_at);

        // Lifecycle state.
        assert_eq!(meta.lifecycle_state, LifecycleState::Active);

        // Transition log has registration entry.
        assert_eq!(meta.transition_log.len(), 1);
        assert!(matches!(
            meta.transition_log[0].reason,
            TransitionReason::Registered
        ));
    }

    #[test]
    fn pipeline_adapter_default_policy() {
        let adapter = PipelineAdapter::new();
        let artifact = sample_pipeline_artifact(ArtifactType::SlotGraph);
        let ownership = sample_ownership();

        let meta = adapter.adapt_pipeline_artifact(&artifact, "chain-1", &ownership);

        assert_eq!(meta.policy.retention_class, RetentionClass::TaskLifetime);
        assert_eq!(
            meta.policy.freshness_class,
            FreshnessClass::RevalidateOnResume
        );
        assert_eq!(meta.policy.exposure_class, ExposureClass::InternalSanitized);
    }

    #[test]
    fn pipeline_plan_graph_gets_replan_safe() {
        let adapter = PipelineAdapter::new();
        let artifact = sample_pipeline_artifact(ArtifactType::PlanGraph);
        let ownership = sample_ownership();

        let meta = adapter.adapt_pipeline_artifact(&artifact, "chain-1", &ownership);

        assert!(
            meta.policy.protection_flags.replan_safe,
            "PlanGraph must have replan_safe=true"
        );
        assert!(meta.policy.protection_flags.is_protected());
    }

    #[test]
    fn pipeline_non_plan_artifact_no_replan_safe() {
        let adapter = PipelineAdapter::new();
        let artifact = sample_pipeline_artifact(ArtifactType::Custom("observation".to_string()));
        let ownership = sample_ownership();

        let meta = adapter.adapt_pipeline_artifact(&artifact, "chain-1", &ownership);

        assert!(
            !meta.policy.protection_flags.replan_safe,
            "Non-plan pipeline artifacts must NOT have replan_safe"
        );
        assert!(!meta.policy.protection_flags.is_protected());
    }

    // -- WorkflowAdapter tests --------------------------------------------

    #[test]
    fn workflow_adapter_produces_correct_metadata() {
        let adapter = WorkflowAdapter::new();
        let artifact = sample_step_artifact();
        let ownership = sample_ownership();

        let meta = adapter.adapt_step_artifact(
            &artifact,
            "wf-instance-1",
            "extract-step",
            "extractor-agent",
            &ownership,
        );

        assert_eq!(
            meta.artifact_uid,
            "workflow-wf-instance-1-extract-step-extract-output"
        );
        assert_eq!(meta.domain, ArtifactDomain::Workflow);

        // Physical locator is WorkflowRun.
        match &meta.physical_locator {
            PhysicalLocator::WorkflowRun {
                instance_id,
                step_name,
                artifact_name,
            } => {
                assert_eq!(instance_id, "wf-instance-1");
                assert_eq!(step_name, "extract-step");
                assert_eq!(artifact_name, "extract-output");
            },
            PhysicalLocator::PipelineStore { .. }
            | PhysicalLocator::EpisodeFile { .. }
            | PhysicalLocator::InMemory { .. }
            | PhysicalLocator::DurableStore { .. } => {
                panic!(
                    "expected WorkflowRun locator, got: {:?}",
                    meta.physical_locator
                )
            },
        }

        // Producer info.
        assert_eq!(meta.producer.producer_agent_id, "extractor-agent");
        assert_eq!(
            meta.producer.producer_stage.as_deref(),
            Some("extract-step")
        );

        // Ownership includes workflow instance.
        assert_eq!(
            meta.ownership.workflow_instance_id.as_deref(),
            Some("wf-instance-1")
        );

        // Lifecycle state.
        assert_eq!(meta.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn workflow_adapter_default_policy() {
        let adapter = WorkflowAdapter::new();
        let artifact = sample_step_artifact();
        let ownership = sample_ownership();

        let meta = adapter.adapt_step_artifact(&artifact, "wf-1", "step-a", "agent-x", &ownership);

        assert_eq!(meta.policy.retention_class, RetentionClass::RunLifetime);
        assert_eq!(
            meta.policy.freshness_class,
            FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600
            }
        );
        assert_eq!(meta.policy.exposure_class, ExposureClass::InternalSanitized);
        assert!(!meta.policy.protection_flags.is_protected());
    }

    // -- ExecutionAdapter tests -------------------------------------------

    #[test]
    fn execution_adapter_produces_correct_metadata() {
        let adapter = ExecutionAdapter::new();
        let artifact = sample_execution_artifact();
        let ownership = sample_ownership();

        let meta =
            adapter.adapt_execution_artifact(&artifact, "browser-agent", "cycle-5", &ownership);

        assert_eq!(
            meta.artifact_uid,
            "execution-browser-agent-cycle-5-browser-screenshot"
        );
        assert_eq!(meta.domain, ArtifactDomain::Execution);

        // Physical locator is InMemory.
        match &meta.physical_locator {
            PhysicalLocator::InMemory { key } => {
                assert_eq!(key, "browser-screenshot");
            },
            PhysicalLocator::PipelineStore { .. }
            | PhysicalLocator::WorkflowRun { .. }
            | PhysicalLocator::EpisodeFile { .. }
            | PhysicalLocator::DurableStore { .. } => {
                panic!(
                    "expected InMemory locator, got: {:?}",
                    meta.physical_locator
                )
            },
        }

        // Producer info.
        assert_eq!(meta.producer.producer_agent_id, "browser-agent");

        // Ownership includes cycle_id.
        assert_eq!(meta.ownership.cycle_id.as_deref(), Some("cycle-5"));

        // Lifecycle state.
        assert_eq!(meta.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn execution_adapter_default_policy() {
        let adapter = ExecutionAdapter::new();
        let artifact = sample_execution_artifact();
        let ownership = sample_ownership();

        let meta = adapter.adapt_execution_artifact(&artifact, "agent-1", "cycle-1", &ownership);

        assert_eq!(meta.policy.retention_class, RetentionClass::GoalLifetime);
        assert_eq!(meta.policy.freshness_class, FreshnessClass::Evergreen);
        assert_eq!(meta.policy.exposure_class, ExposureClass::InternalSanitized);
        assert!(!meta.policy.protection_flags.is_protected());
    }

    // -- EpisodeAdapter tests ---------------------------------------------

    #[test]
    fn episode_adapter_returns_none_when_no_artifact_output() {
        let adapter = EpisodeAdapter::new();
        let record = sample_episode_record(false);
        let ownership = sample_ownership();

        let result = adapter.adapt_episode_artifact(&record, &ownership);

        assert!(
            result.is_none(),
            "must return None when artifact_output is None"
        );
    }

    #[test]
    fn episode_adapter_produces_correct_metadata() {
        let adapter = EpisodeAdapter::new();
        let record = sample_episode_record(true);
        let ownership = sample_ownership();

        let meta = adapter
            .adapt_episode_artifact(&record, &ownership)
            .expect("should return Some when artifact_output exists");

        assert_eq!(meta.artifact_uid, "episode-ep-100");
        assert_eq!(meta.domain, ArtifactDomain::Episode);

        // Physical locator is EpisodeFile.
        match &meta.physical_locator {
            PhysicalLocator::EpisodeFile {
                agent_id,
                episode_id,
            } => {
                assert_eq!(agent_id, "research-agent");
                assert_eq!(episode_id, "ep-100");
            },
            PhysicalLocator::PipelineStore { .. }
            | PhysicalLocator::WorkflowRun { .. }
            | PhysicalLocator::InMemory { .. }
            | PhysicalLocator::DurableStore { .. } => {
                panic!(
                    "expected EpisodeFile locator, got: {:?}",
                    meta.physical_locator
                )
            },
        }

        // Producer info.
        assert_eq!(meta.producer.producer_agent_id, "research-agent");
        let completed_at = DateTime::parse_from_rfc3339(&record.completed_at)
            .map(|value| value.with_timezone(&Utc))
            .expect("sample episode should use RFC3339 timestamps");
        assert_eq!(meta.producer.produced_at, completed_at);

        // Lifecycle state.
        assert_eq!(meta.lifecycle_state, LifecycleState::Active);
    }

    #[test]
    fn episode_adapter_default_policy() {
        let adapter = EpisodeAdapter::new();
        let record = sample_episode_record(true);
        let ownership = sample_ownership();

        let meta = adapter
            .adapt_episode_artifact(&record, &ownership)
            .expect("should return Some");

        assert_eq!(meta.policy.retention_class, RetentionClass::Permanent);
        assert_eq!(meta.policy.freshness_class, FreshnessClass::Evergreen);
        assert_eq!(meta.policy.exposure_class, ExposureClass::GauiSanitized);
        assert!(!meta.policy.protection_flags.is_protected());
    }

    // -- Cross-domain default policy tests --------------------------------

    #[test]
    fn default_policies_match_plan_specification() {
        // Pipeline: TaskLifetime, RevalidateOnResume, InternalSanitized
        let pipeline_meta = PipelineAdapter::new().adapt_pipeline_artifact(
            &sample_pipeline_artifact(ArtifactType::QueryAnalysis),
            "c1",
            &sample_ownership(),
        );
        assert_eq!(
            pipeline_meta.policy.retention_class,
            RetentionClass::TaskLifetime
        );
        assert_eq!(
            pipeline_meta.policy.freshness_class,
            FreshnessClass::RevalidateOnResume
        );
        assert_eq!(
            pipeline_meta.policy.exposure_class,
            ExposureClass::InternalSanitized
        );

        // Workflow: RunLifetime, Window{300,600}, InternalSanitized
        let workflow_meta = WorkflowAdapter::new().adapt_step_artifact(
            &sample_step_artifact(),
            "wf-1",
            "s1",
            "a1",
            &sample_ownership(),
        );
        assert_eq!(
            workflow_meta.policy.retention_class,
            RetentionClass::RunLifetime
        );
        assert_eq!(
            workflow_meta.policy.freshness_class,
            FreshnessClass::Window {
                fresh_seconds: 300,
                stale_seconds: 600
            }
        );
        assert_eq!(
            workflow_meta.policy.exposure_class,
            ExposureClass::InternalSanitized
        );

        // Execution: GoalLifetime, Evergreen, InternalSanitized
        let exec_meta = ExecutionAdapter::new().adapt_execution_artifact(
            &sample_execution_artifact(),
            "a1",
            "c1",
            &sample_ownership(),
        );
        assert_eq!(
            exec_meta.policy.retention_class,
            RetentionClass::GoalLifetime
        );
        assert_eq!(exec_meta.policy.freshness_class, FreshnessClass::Evergreen);
        assert_eq!(
            exec_meta.policy.exposure_class,
            ExposureClass::InternalSanitized
        );

        // Episode: Permanent, Evergreen, GauiSanitized
        let episode_meta = EpisodeAdapter::new()
            .adapt_episode_artifact(&sample_episode_record(true), &sample_ownership())
            .expect("has artifact_output");
        assert_eq!(
            episode_meta.policy.retention_class,
            RetentionClass::Permanent
        );
        assert_eq!(
            episode_meta.policy.freshness_class,
            FreshnessClass::Evergreen
        );
        assert_eq!(
            episode_meta.policy.exposure_class,
            ExposureClass::GauiSanitized
        );
    }
}
