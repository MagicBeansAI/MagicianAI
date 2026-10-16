//! Immutable agent, personality and procedure bindings for app workflows.
//!
//! The ordinary agent and skill stores remain the source owners. This module
//! does not create a second loader or mint authority from serialized fields;
//! it records the exact material selected by those owners so launch, retry and
//! resume can prove that they reconstructed the same definition and prompt.

use std::{collections::BTreeSet, fmt, sync::OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    manifest::{AppManifestError, AppManifestInputSchema},
    models::{
        AppDigest, AppHandlingLabels, AppName, AppReference, AppRevision, AppScopeBindingRef,
    },
    records::{AppDataHandlingPolicy, AppResourceCeiling, AppReviewedWorkflowMaterialBinding},
    skill_dependencies::AppProcedureInvocation,
};
use crate::magician_v2::{
    agents::{AgentAppToolContract, AgentDefinition, InvocationSurface},
    execution::actions::{DelegationExpectedArtifact, DelegationTargetRequest},
    json_traversal::{canonical_json_bytes, exact_json_encoded_len},
};

pub const APP_AGENT_BINDING_SCHEMA: &str = "magician.app-agent-binding.v1";
pub const APP_PROMPT_MATERIAL_BINDING_SCHEMA: &str = "magician.app-prompt-material-binding.v1";
const MAX_APP_AGENT_DEFINITION_BYTES: usize = 256 * 1024;
const MAX_APP_AGENT_TOOL_VALUE_BYTES: usize = 256 * 1024;
const MAX_APP_AGENT_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_APP_AGENT_TOOL_SCHEMA_FIELDS: usize = 256;
pub(crate) const APP_AGENT_TOOL_RESULT_ARTIFACT_NAME: &str = "app_agent_tool_result.json";
const APP_AGENT_TOOL_CHILD_INSTRUCTION: &str =
    "Complete only the typed child request. Return exactly one JSON object matching the reviewed \
     result schema in the named result artifact; do not return a transcript or delegate further.";

/// Runtime-only provenance for the one result declaration derived from a
/// sealed callable-agent binding. Generic callers cannot populate or mutate
/// the declaration through task/API state, and the orchestrator uses this
/// owner-minted value instead of retaining ambient artifact declarations.
#[derive(Clone)]
pub struct AppAgentToolResultDeclarationPermit {
    declaration: crate::magician_v2::agents::types::ArtifactDeclaration,
    binding_digest: AppDigest,
}

impl fmt::Debug for AppAgentToolResultDeclarationPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentToolResultDeclarationPermit")
            .field("binding_digest", &self.binding_digest)
            .finish_non_exhaustive()
    }
}

impl AppAgentToolResultDeclarationPermit {
    pub(crate) fn declaration(&self) -> &crate::magician_v2::agents::types::ArtifactDeclaration {
        &self.declaration
    }
}

/// Deterministic canonical child identity for one exact callable-agent action.
/// It intentionally excludes the delegated request's rendered context because
/// that context contains the launch digest, which in turn binds this child id.
/// Hashing the sealed tool/input identities directly avoids a circular or
/// caller-selected idempotency key.
pub(crate) fn agent_tool_child_execution_id(
    parent_execution_id: &str,
    action_invocation_ref: &AppReference,
    tool_binding_digest: &AppDigest,
    input_digest: &AppDigest,
    retry_generation: u64,
) -> Result<String, AppAgentCapabilityError> {
    if parent_execution_id.trim().is_empty() || retry_generation == 0 {
        return Err(AppAgentCapabilityError::CorruptAgentToolContract);
    }
    let digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
        "schema": "magician.app-agent-tool-child-id.v1",
        "parent_execution_id": parent_execution_id,
        "action_invocation_ref": action_invocation_ref,
        "tool_binding_digest": tool_binding_digest,
        "input_digest": input_digest,
        "retry_generation": retry_generation,
    }))?);
    let suffix = digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or(AppAgentCapabilityError::CorruptAgentToolContract)?;
    Ok(format!("exec-deleg-{}", &suffix[..24]))
}

/// Content address of the complete physical owner for the typed child-task
/// path. The source agent definition is a separate argument because it is
/// scope/package material; every load-bearing runtime/reducer boundary is
/// framed here so an implementation edit cannot retain an old reviewed
/// action/effect identity.
pub fn agent_tool_implementation_plan_digest(
    agent_source_digest: &AppDigest,
    declaration: &AgentAppToolContract,
) -> Result<AppDigest, AppAgentCapabilityError> {
    static OWNER_SOURCE_DIGEST: OnceLock<AppDigest> = OnceLock::new();
    let declaration_digest =
        AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(declaration)?)?);
    let owner_source_digest = OWNER_SOURCE_DIGEST.get_or_init(|| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"magician.app-agent-tool-owner-source.v2\0");
        for (path, bytes) in [
            (
                "agents/types.rs",
                include_bytes!("../agents/types.rs").as_slice(),
            ),
            ("apps/manifest.rs", include_bytes!("manifest.rs").as_slice()),
            ("apps/models.rs", include_bytes!("models.rs").as_slice()),
            ("apps/records.rs", include_bytes!("records.rs").as_slice()),
            (
                "apps/effect_kernel.rs",
                include_bytes!("effect_kernel.rs").as_slice(),
            ),
            (
                "apps/resource_authority.rs",
                include_bytes!("resource_authority.rs").as_slice(),
            ),
            (
                "apps/tool_disclosure.rs",
                include_bytes!("tool_disclosure.rs").as_slice(),
            ),
            (
                "apps/processing_boundary.rs",
                include_bytes!("processing_boundary.rs").as_slice(),
            ),
            (
                "apps/agent_capability.rs",
                include_bytes!("agent_capability.rs").as_slice(),
            ),
            (
                "apps/primitive_catalog.rs",
                include_bytes!("primitive_catalog.rs").as_slice(),
            ),
            (
                "apps/installation_review.rs",
                include_bytes!("../../../../magician-apps/src/apps/installation_review.rs")
                    .as_slice(),
            ),
            (
                "apps/workflows.rs",
                include_bytes!("workflows.rs").as_slice(),
            ),
            (
                "artifact_v2/models.rs",
                include_bytes!("../artifact_v2/models.rs").as_slice(),
            ),
            (
                "artifact_v2/app_agent_tool.rs",
                include_bytes!("../artifact_v2/app_agent_tool.rs").as_slice(),
            ),
            (
                "artifact_v2/events.rs",
                include_bytes!("../artifact_v2/events.rs").as_slice(),
            ),
            (
                "artifact_v2/execution_artifacts.rs",
                include_bytes!("../artifact_v2/execution_artifacts.rs").as_slice(),
            ),
            (
                "artifact_v2/workspace.rs",
                include_bytes!("../artifact_v2/workspace.rs").as_slice(),
            ),
            (
                "artifact_v2/io.rs",
                include_bytes!("../artifact_v2/io.rs").as_slice(),
            ),
            (
                "artifact_v2/scheduler.rs",
                include_bytes!("../artifact_v2/scheduler.rs").as_slice(),
            ),
            (
                "artifact_v2/reducer.rs",
                include_bytes!("../artifact_v2/reducer.rs").as_slice(),
            ),
            (
                "artifact_v2/task_writes.rs",
                include_bytes!("../artifact_v2/task_writes.rs").as_slice(),
            ),
            (
                "artifact_v2/service.rs",
                include_bytes!("../artifact_v2/service.rs").as_slice(),
            ),
            (
                "agents/runtime.rs",
                include_bytes!("../agents/runtime.rs").as_slice(),
            ),
            (
                "agents/storage.rs",
                include_bytes!("../agents/storage.rs").as_slice(),
            ),
            (
                "magician-core/json_traversal.rs",
                magician_core::json_traversal::IMPLEMENTATION_SOURCE,
            ),
            (
                "execution/agentic/types.rs",
                include_bytes!("../execution/agentic/types.rs").as_slice(),
            ),
            (
                "execution/agentic/executor.rs",
                include_bytes!("../execution/agentic/executor.rs").as_slice(),
            ),
            (
                "execution/agentic/delegation_dispatch.rs",
                include_bytes!("../execution/agentic/delegation_dispatch.rs").as_slice(),
            ),
            (
                "execution/actions.rs",
                include_bytes!("../execution/actions.rs").as_slice(),
            ),
            (
                "orchestrator/v2_orchestrator.rs",
                include_bytes!("../orchestrator/v2_orchestrator.rs").as_slice(),
            ),
        ] {
            hasher.update(&(path.len() as u64).to_le_bytes());
            hasher.update(path.as_bytes());
            hasher.update(&(bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        AppDigest::parse(format!("blake3:{}", hasher.finalize().to_hex()))
            .expect("BLAKE3 emits the canonical app digest representation")
    });
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": "magician.app-agent-tool-owner.v2",
            "owner_source_digest": owner_source_digest,
            "agent_source_digest": agent_source_digest,
            "declaration_digest": declaration_digest,
        }),
    )?))
}

/// One shared eligibility predicate for installation review and launch.
/// Disabled agents and definitions which do not admit the canonical Task
/// surface must never be advertised as runnable app workflow material.
pub fn agent_definition_permits_app_task(definition: &AgentDefinition) -> bool {
    !definition.disabled
        && definition
            .invocation_policy
            .permits_direct_surface(InvocationSurface::Task)
}

#[derive(Debug, Error)]
pub enum AppAgentCapabilityError {
    #[error("agent definition identity is invalid")]
    InvalidAgentIdentity,
    #[error("agent definition exceeds the app binding byte ceiling")]
    AgentDefinitionTooLarge,
    #[error("agent definition revision is invalid")]
    InvalidAgentRevision,
    #[error("agent definition binding is corrupt")]
    CorruptAgentBinding,
    #[error("app prompt material binding is corrupt")]
    CorruptPromptMaterial,
    #[error("the current agent or personality no longer matches installation review")]
    ReviewMaterialDrift,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Contract(#[from] super::models::AppContractError),
    #[error(transparent)]
    Manifest(#[from] AppManifestError),
    #[error("agent_as_tool input or result exceeds its reviewed byte ceiling")]
    AgentToolValueTooLarge,
    #[error("agent_as_tool contract is corrupt")]
    CorruptAgentToolContract,
    #[error("agent_as_tool declaration is absent or invalid")]
    InvalidAgentToolDeclaration,
    #[error("agent_as_tool result labels exceed the reviewed policy")]
    AgentToolResultPolicyDenied,
    #[error("agent_as_tool terminal observation does not match its sealed child launch")]
    AgentToolTerminalObservationMismatch,
    #[error("agent_as_tool sealed launch deadline has expired")]
    AgentToolLaunchExpired,
}

pub fn seal_reviewed_workflow_material(
    workflow_id: &AppName,
    agent_ref: &AppReference,
    definition: &AgentDefinition,
    personality: Option<(&AppReference, &crate::magician_v2::skills::PersonalitySpec)>,
) -> Result<AppReviewedWorkflowMaterialBinding, AppAgentCapabilityError> {
    if agent_ref.as_str() != format!("agent:{}", definition.agent_id) {
        return Err(AppAgentCapabilityError::InvalidAgentIdentity);
    }
    let definition_value = serde_json::to_value(definition)?;
    if exact_json_encoded_len(&definition_value) > MAX_APP_AGENT_DEFINITION_BYTES {
        return Err(AppAgentCapabilityError::AgentDefinitionTooLarge);
    }
    let agent_definition_revision = AppRevision::new(u64::from(definition.version))
        .map_err(|_| AppAgentCapabilityError::InvalidAgentRevision)?;
    let agent_definition_digest = AppDigest::blake3(&canonical_json_bytes(&definition_value)?);
    let agent_descriptor_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
        "schema": "magician.app-reviewed-agent-descriptor.v1",
        "agent_ref": agent_ref,
        "definition_revision": agent_definition_revision,
        "definition_digest": &agent_definition_digest,
    }))?);
    let (personality_ref, personality_content_digest, personality_descriptor_digest) =
        match personality {
            Some((personality_ref, spec)) => {
                let content_digest =
                    AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(spec)?)?);
                let descriptor_digest =
                    AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
                        "schema": "magician.app-reviewed-personality-descriptor.v1",
                        "personality_ref": personality_ref,
                        "content_digest": &content_digest,
                    }))?);
                (
                    Some(personality_ref.clone()),
                    Some(content_digest),
                    Some(descriptor_digest),
                )
            },
            None => (None, None, None),
        };
    let binding_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
        "schema": "magician.app-reviewed-workflow-material.v1",
        "workflow_id": workflow_id,
        "agent_ref": agent_ref,
        "agent_definition_revision": agent_definition_revision,
        "agent_definition_digest": &agent_definition_digest,
        "agent_descriptor_digest": &agent_descriptor_digest,
        "personality_ref": &personality_ref,
        "personality_content_digest": &personality_content_digest,
        "personality_descriptor_digest": &personality_descriptor_digest,
    }))?);
    Ok(AppReviewedWorkflowMaterialBinding {
        schema: "magician.app-reviewed-workflow-material.v1".to_owned(),
        workflow_id: workflow_id.clone(),
        agent_ref: agent_ref.clone(),
        agent_definition_revision,
        agent_definition_digest,
        agent_descriptor_digest,
        personality_ref,
        personality_content_digest,
        personality_descriptor_digest,
        binding_digest,
    })
}

pub(crate) fn revalidate_reviewed_workflow_material(
    reviewed: &AppReviewedWorkflowMaterialBinding,
    definition: &AgentDefinition,
    personality: Option<(&AppReference, &crate::magician_v2::skills::PersonalitySpec)>,
) -> Result<(), AppAgentCapabilityError> {
    let current = seal_reviewed_workflow_material(
        &reviewed.workflow_id,
        &reviewed.agent_ref,
        definition,
        personality,
    )?;
    if &current != reviewed {
        return Err(AppAgentCapabilityError::ReviewMaterialDrift);
    }
    Ok(())
}

/// Exact scoped agent definition selected for one app workflow.
///
/// The complete bounded definition is sealed with its digest so updates or
/// deletion cannot force reconstruction from a mutable friendly name. The
/// runtime consumes this value directly and does not require the mutable owner
/// record to remain byte-equivalent or even present.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAgentDefinitionBinding {
    pub schema: String,
    pub agent_ref: AppReference,
    pub runtime_agent_id: AppReference,
    pub definition_revision: AppRevision,
    pub definition_digest: AppDigest,
    pub sealed_definition: AgentDefinition,
    pub persona_digest: AppDigest,
    pub prompt_pipeline_digest: AppDigest,
    pub constraint_digest: AppDigest,
    pub effective_tools: BTreeSet<AppReference>,
    pub effective_resources: AppResourceCeiling,
    pub delegation_ceiling: BTreeSet<AppReference>,
    pub binding_digest: AppDigest,
}

impl fmt::Debug for AppAgentDefinitionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentDefinitionBinding")
            .field("schema", &self.schema)
            .field("agent_ref", &self.agent_ref)
            .field("runtime_agent_id", &self.runtime_agent_id)
            .field("definition_revision", &self.definition_revision)
            .field("definition_digest", &self.definition_digest)
            .field("persona_digest", &self.persona_digest)
            .field("prompt_pipeline_digest", &self.prompt_pipeline_digest)
            .field("constraint_digest", &self.constraint_digest)
            .field("effective_tools", &self.effective_tools)
            .field("effective_resources", &self.effective_resources)
            .field("delegation_ceiling", &self.delegation_ceiling)
            .field("binding_digest", &self.binding_digest)
            .finish_non_exhaustive()
    }
}

impl AppAgentDefinitionBinding {
    pub fn seal(
        definition: &AgentDefinition,
        effective_tools: BTreeSet<AppReference>,
        effective_resources: AppResourceCeiling,
    ) -> Result<Self, AppAgentCapabilityError> {
        if definition.agent_id.trim().is_empty() {
            return Err(AppAgentCapabilityError::InvalidAgentIdentity);
        }
        let definition_value = serde_json::to_value(definition)?;
        if exact_json_encoded_len(&definition_value) > MAX_APP_AGENT_DEFINITION_BYTES {
            return Err(AppAgentCapabilityError::AgentDefinitionTooLarge);
        }
        let definition_revision = AppRevision::new(u64::from(definition.version))
            .map_err(|_| AppAgentCapabilityError::InvalidAgentRevision)?;
        let agent_ref = AppReference::parse(format!("agent:{}", definition.agent_id))?;
        let runtime_agent_id = AppReference::parse(definition.agent_id.clone())?;
        let definition_digest = AppDigest::blake3(&canonical_json_bytes(&definition_value)?);
        let persona_digest = AppDigest::blake3(definition.persona.as_bytes());
        let prompt_pipeline_digest = AppDigest::blake3(&canonical_json_bytes(
            &serde_json::to_value(&definition.prompt_pipeline)?,
        )?);
        let constraint_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
            &definition.constraints,
        )?)?);
        // App workflows currently disable ambient delegation at the canonical
        // V3 owner (`max_spawned_tasks = 0`). Do not turn definition targets,
        // especially `*`, into callable app authority before agent_as_tool has
        // its own reviewed child contract.
        let delegation_ceiling = BTreeSet::new();
        let binding_digest = agent_binding_digest(
            &agent_ref,
            &runtime_agent_id,
            definition_revision,
            &definition_digest,
            &persona_digest,
            &prompt_pipeline_digest,
            &constraint_digest,
            &effective_tools,
            &effective_resources,
            &delegation_ceiling,
        )?;
        Ok(Self {
            schema: APP_AGENT_BINDING_SCHEMA.to_owned(),
            agent_ref,
            runtime_agent_id,
            definition_revision,
            definition_digest,
            sealed_definition: definition.clone(),
            persona_digest,
            prompt_pipeline_digest,
            constraint_digest,
            effective_tools,
            effective_resources,
            delegation_ceiling,
            binding_digest,
        })
    }

    pub fn validate_integrity(&self) -> Result<(), AppAgentCapabilityError> {
        let sealed_value = serde_json::to_value(&self.sealed_definition)?;
        if exact_json_encoded_len(&sealed_value) > MAX_APP_AGENT_DEFINITION_BYTES
            || AppDigest::blake3(&canonical_json_bytes(&sealed_value)?) != self.definition_digest
            || self.sealed_definition.version as u64 != self.definition_revision.get()
            || self.sealed_definition.agent_id != self.runtime_agent_id.as_str()
            || self.agent_ref.as_str() != format!("agent:{}", self.sealed_definition.agent_id)
            || AppDigest::blake3(self.sealed_definition.persona.as_bytes()) != self.persona_digest
            || AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
                &self.sealed_definition.prompt_pipeline,
            )?)?)
                != self.prompt_pipeline_digest
            || AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
                &self.sealed_definition.constraints,
            )?)?)
                != self.constraint_digest
            || self.schema != APP_AGENT_BINDING_SCHEMA
            || self.binding_digest
                != agent_binding_digest(
                    &self.agent_ref,
                    &self.runtime_agent_id,
                    self.definition_revision,
                    &self.definition_digest,
                    &self.persona_digest,
                    &self.prompt_pipeline_digest,
                    &self.constraint_digest,
                    &self.effective_tools,
                    &self.effective_resources,
                    &self.delegation_ceiling,
                )?
        {
            return Err(AppAgentCapabilityError::CorruptAgentBinding);
        }
        Ok(())
    }

    pub fn sealed_definition(&self) -> &AgentDefinition {
        &self.sealed_definition
    }
}

#[allow(clippy::too_many_arguments)]
fn agent_binding_digest(
    agent_ref: &AppReference,
    runtime_agent_id: &AppReference,
    definition_revision: AppRevision,
    definition_digest: &AppDigest,
    persona_digest: &AppDigest,
    prompt_pipeline_digest: &AppDigest,
    constraint_digest: &AppDigest,
    effective_tools: &BTreeSet<AppReference>,
    effective_resources: &AppResourceCeiling,
    delegation_ceiling: &BTreeSet<AppReference>,
) -> Result<AppDigest, serde_json::Error> {
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": APP_AGENT_BINDING_SCHEMA,
            "agent_ref": agent_ref,
            "runtime_agent_id": runtime_agent_id,
            "definition_revision": definition_revision,
            "definition_digest": definition_digest,
            "persona_digest": persona_digest,
            "prompt_pipeline_digest": prompt_pipeline_digest,
            "constraint_digest": constraint_digest,
            "effective_tools": effective_tools,
            "effective_resources": effective_resources,
            "delegation_ceiling": delegation_ceiling,
        }),
    )?))
}

/// Narrow app resource authority by the selected agent's own limits.
///
/// `max_tokens_per_cycle` is an aggregate ceiling while Apps accounts input
/// and output independently. Split it deterministically so their sum cannot
/// exceed the agent ceiling. This may be conservative but can never widen it.
pub(crate) fn narrow_resources_for_agent(
    granted: &AppResourceCeiling,
    definition: &AgentDefinition,
) -> AppResourceCeiling {
    let token_ceiling = definition.constraints.max_tokens_per_cycle;
    let input_share = token_ceiling / 2;
    let output_share = token_ceiling.saturating_sub(input_share);
    let duration = definition.constraints.max_duration_secs.unwrap_or(u64::MAX);
    AppResourceCeiling {
        max_input_tokens: granted.max_input_tokens.min(input_share),
        max_output_tokens: granted.max_output_tokens.min(output_share),
        max_cost_microusd: granted.max_cost_microusd,
        // Iterations and paid tool invocations are different accounting units:
        // one model iteration may issue multiple calls. The sealed definition
        // retains its iteration ceiling for the agent loop; the app resource
        // owner independently enforces this invocation ceiling.
        max_paid_tool_invocations: granted.max_paid_tool_invocations,
        max_active_seconds: granted.max_active_seconds.min(duration),
        max_lifetime_seconds: granted.max_lifetime_seconds.min(duration),
        max_browser_network_actions: granted.max_browser_network_actions,
        max_concurrent_foreground_runs: granted.max_concurrent_foreground_runs,
        max_concurrent_background_runs: granted.max_concurrent_background_runs,
        max_records: granted.max_records,
        max_payload_bytes: granted.max_payload_bytes,
        max_attachment_bytes: granted.max_attachment_bytes,
        max_monthly_tokens: granted.max_monthly_tokens,
        max_monthly_cost_microusd: granted.max_monthly_cost_microusd,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppInstructionRevisionBinding {
    pub dependency_ref: AppReference,
    pub semantic_version: String,
    pub content_digest: AppDigest,
    pub instructions_digest: AppDigest,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPersonalityMaterialBinding {
    pub name: AppName,
    pub content_digest: AppDigest,
    pub sealed_spec: Value,
}

impl fmt::Debug for AppPersonalityMaterialBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppPersonalityMaterialBinding")
            .field("name", &self.name)
            .field("content_digest", &self.content_digest)
            .finish_non_exhaustive()
    }
}

/// Digest tree for every instruction-bearing input to the model prompt.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPromptMaterialBinding {
    pub schema: String,
    pub workflow: AppName,
    pub workflow_instructions_digest: AppDigest,
    pub procedures: Vec<AppInstructionRevisionBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<AppPersonalityMaterialBinding>,
    pub agent_binding_digest: AppDigest,
    pub effective_tool_ceiling_digest: AppDigest,
    pub reconstruction_digest: AppDigest,
}

impl fmt::Debug for AppPromptMaterialBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppPromptMaterialBinding")
            .field("schema", &self.schema)
            .field("workflow", &self.workflow)
            .field(
                "workflow_instructions_digest",
                &self.workflow_instructions_digest,
            )
            .field("procedures", &self.procedures)
            .field("personality", &self.personality)
            .field("agent_binding_digest", &self.agent_binding_digest)
            .field(
                "effective_tool_ceiling_digest",
                &self.effective_tool_ceiling_digest,
            )
            .field("reconstruction_digest", &self.reconstruction_digest)
            .finish()
    }
}

impl AppPromptMaterialBinding {
    pub fn seal(
        agent: &AppAgentDefinitionBinding,
        invocation: &AppProcedureInvocation,
        personality: Option<(
            &AppName,
            &crate::magician_v2::skills::PersonalitySpec,
            &AppDigest,
        )>,
    ) -> Result<Self, AppAgentCapabilityError> {
        agent.validate_integrity()?;
        let workflow_instructions_digest =
            AppDigest::blake3(invocation.private_instructions().as_bytes());
        let procedures = invocation
            .procedures()
            .iter()
            .map(|procedure| AppInstructionRevisionBinding {
                dependency_ref: procedure.fence().dependency_ref().clone(),
                semantic_version: procedure.fence().semantic_version().to_owned(),
                content_digest: procedure.fence().content_digest().clone(),
                instructions_digest: AppDigest::blake3(procedure.instructions().as_bytes()),
            })
            .collect::<Vec<_>>();
        let personality = personality
            .map(|(name, spec, digest)| {
                let sealed_spec = serde_json::to_value(spec)?;
                if AppDigest::blake3(&canonical_json_bytes(&sealed_spec)?) != *digest {
                    return Err(AppAgentCapabilityError::CorruptPromptMaterial);
                }
                Ok(AppPersonalityMaterialBinding {
                    name: name.clone(),
                    content_digest: digest.clone(),
                    sealed_spec,
                })
            })
            .transpose()?;
        let effective_tool_ceiling_digest = AppDigest::blake3(&canonical_json_bytes(
            &serde_json::to_value(invocation.effective_tools())?,
        )?);
        let reconstruction_digest = prompt_material_digest(
            invocation.workflow(),
            &workflow_instructions_digest,
            &procedures,
            personality.as_ref(),
            &agent.binding_digest,
            &effective_tool_ceiling_digest,
        )?;
        Ok(Self {
            schema: APP_PROMPT_MATERIAL_BINDING_SCHEMA.to_owned(),
            workflow: invocation.workflow().clone(),
            workflow_instructions_digest,
            procedures,
            personality,
            agent_binding_digest: agent.binding_digest.clone(),
            effective_tool_ceiling_digest,
            reconstruction_digest,
        })
    }

    pub fn revalidate(
        &self,
        agent: &AppAgentDefinitionBinding,
        invocation: &AppProcedureInvocation,
        personality: Option<(
            &AppName,
            &crate::magician_v2::skills::PersonalitySpec,
            &AppDigest,
        )>,
    ) -> Result<(), AppAgentCapabilityError> {
        if self.schema != APP_PROMPT_MATERIAL_BINDING_SCHEMA
            || &Self::seal(agent, invocation, personality)? != self
        {
            return Err(AppAgentCapabilityError::CorruptPromptMaterial);
        }
        Ok(())
    }

    pub fn validate_integrity(
        &self,
        agent: &AppAgentDefinitionBinding,
    ) -> Result<(), AppAgentCapabilityError> {
        agent.validate_integrity()?;
        let personality = self
            .personality
            .as_ref()
            .map(|personality| {
                if AppDigest::blake3(&canonical_json_bytes(&personality.sealed_spec)?)
                    != personality.content_digest
                {
                    return Err(AppAgentCapabilityError::CorruptPromptMaterial);
                }
                Ok(personality)
            })
            .transpose()?;
        let effective_tool_ceiling_digest = AppDigest::blake3(&canonical_json_bytes(
            &serde_json::to_value(&agent.effective_tools)?,
        )?);
        if self.schema != APP_PROMPT_MATERIAL_BINDING_SCHEMA
            || self.agent_binding_digest != agent.binding_digest
            || self.effective_tool_ceiling_digest != effective_tool_ceiling_digest
            || self.reconstruction_digest
                != prompt_material_digest(
                    &self.workflow,
                    &self.workflow_instructions_digest,
                    &self.procedures,
                    personality,
                    &self.agent_binding_digest,
                    &self.effective_tool_ceiling_digest,
                )?
        {
            return Err(AppAgentCapabilityError::CorruptPromptMaterial);
        }
        Ok(())
    }

    pub fn sealed_personality(
        &self,
    ) -> Result<
        Option<(
            AppName,
            crate::magician_v2::skills::PersonalitySpec,
            AppDigest,
        )>,
        AppAgentCapabilityError,
    > {
        self.personality
            .as_ref()
            .map(|personality| {
                if AppDigest::blake3(&canonical_json_bytes(&personality.sealed_spec)?)
                    != personality.content_digest
                {
                    return Err(AppAgentCapabilityError::CorruptPromptMaterial);
                }
                let spec = serde_json::from_value(personality.sealed_spec.clone())?;
                Ok((
                    personality.name.clone(),
                    spec,
                    personality.content_digest.clone(),
                ))
            })
            .transpose()
    }
}

/// Non-serializable proof that an app workflow restored its exact sealed
/// definition through the trusted task-sidecar owner. The constructor is
/// restricted to the Apps module tree; generic orchestrator callers cannot
/// wrap arbitrary mutable definitions and enter the immutable app path.
pub(crate) struct RestoredAppAgentDefinition {
    definition: AgentDefinition,
    effective_tools: BTreeSet<AppReference>,
}

impl RestoredAppAgentDefinition {
    pub(super) fn restore(
        agent: &AppAgentDefinitionBinding,
        prompt: &AppPromptMaterialBinding,
        base_authority_digest: &AppDigest,
        expected_workflow_authority_digest: &AppDigest,
        expected_runtime_agent_id: &AppReference,
    ) -> Result<Self, AppAgentCapabilityError> {
        prompt.validate_integrity(agent)?;
        if &agent.runtime_agent_id != expected_runtime_agent_id
            || workflow_authority_material_digest(base_authority_digest, agent, prompt)?
                != *expected_workflow_authority_digest
        {
            return Err(AppAgentCapabilityError::CorruptAgentBinding);
        }
        Ok(Self {
            definition: agent.sealed_definition().clone(),
            effective_tools: agent.effective_tools.clone(),
        })
    }

    pub(crate) fn definition(&self) -> &AgentDefinition {
        &self.definition
    }

    pub(crate) fn effective_tools(&self) -> &BTreeSet<AppReference> {
        &self.effective_tools
    }

    /// Restore the definition captured by a persisted `agent_as_tool` launch.
    /// The child launch has already sealed workflow authority separately; this
    /// constructor exists so the canonical executor can run the exact retained
    /// definition after a crash without reopening the mutable agent catalog.
    pub(crate) fn restore_agent_tool_child(
        agent: &AppAgentDefinitionBinding,
    ) -> Result<Self, AppAgentCapabilityError> {
        agent.validate_integrity()?;
        if !agent_definition_permits_app_task(agent.sealed_definition()) {
            return Err(AppAgentCapabilityError::CorruptAgentBinding);
        }
        Ok(Self {
            definition: agent.sealed_definition().clone(),
            effective_tools: agent.effective_tools.clone(),
        })
    }
}

fn prompt_material_digest(
    workflow: &AppName,
    workflow_instructions_digest: &AppDigest,
    procedures: &[AppInstructionRevisionBinding],
    personality: Option<&AppPersonalityMaterialBinding>,
    agent_binding_digest: &AppDigest,
    effective_tool_ceiling_digest: &AppDigest,
) -> Result<AppDigest, serde_json::Error> {
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": APP_PROMPT_MATERIAL_BINDING_SCHEMA,
            "workflow": workflow,
            "workflow_instructions_digest": workflow_instructions_digest,
            "procedures": procedures,
            "personality": personality,
            "agent_binding_digest": agent_binding_digest,
            "effective_tool_ceiling_digest": effective_tool_ceiling_digest,
        }),
    )?))
}

/// Bind instruction identity to the already canonical app capability
/// authority. The base authority remains independently revalidatable by the
/// existing grant/runtime kernel; this digest prevents an agent/personality
/// change that leaves tool/resource sets unchanged from looking equivalent.
pub(crate) fn workflow_authority_material_digest(
    base_authority_digest: &AppDigest,
    agent: &AppAgentDefinitionBinding,
    prompt: &AppPromptMaterialBinding,
) -> Result<AppDigest, AppAgentCapabilityError> {
    agent.validate_integrity()?;
    if prompt.agent_binding_digest != agent.binding_digest {
        return Err(AppAgentCapabilityError::CorruptPromptMaterial);
    }
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": "magician.app-workflow-authority-material.v1",
            "base_authority_digest": base_authority_digest,
            "agent_binding_digest": &agent.binding_digest,
            "prompt_material_digest": &prompt.reconstruction_digest,
        }),
    )?))
}

/// Typed, bounded contract for one canonical `agent_as_tool` child launch.
/// It remains authority-free descriptor/task material: only the workflow and
/// Artifact V3 owners can consume a separately sealed launch permit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppAgentToolContract {
    schema: String,
    agent_binding_digest: AppDigest,
    input_schema: AppManifestInputSchema,
    result_schema: AppManifestInputSchema,
    max_input_bytes: u64,
    max_result_bytes: u64,
    effective_tools: BTreeSet<AppReference>,
    effective_resources: AppResourceCeiling,
    effective_data_handling_policy: AppDataHandlingPolicy,
    result_artifact_name: String,
    child_prompt_digest: AppDigest,
    contract_digest: AppDigest,
}

#[allow(dead_code)] // Opaque contract inspection is retained for downstream owners/tests.
impl AppAgentToolContract {
    pub(crate) fn seal(
        agent: &AppAgentDefinitionBinding,
        declaration: &AgentAppToolContract,
        effective_data_handling_policy: AppDataHandlingPolicy,
    ) -> Result<Self, AppAgentCapabilityError> {
        agent.validate_integrity()?;
        if !agent_definition_permits_app_task(&agent.sealed_definition)
            || agent.sealed_definition.app_tool.as_ref() != Some(declaration)
        {
            return Err(AppAgentCapabilityError::InvalidAgentToolDeclaration);
        }
        let hard_max = u64::try_from(MAX_APP_AGENT_TOOL_VALUE_BYTES).unwrap_or(u64::MAX);
        let resource_max = agent.effective_resources.max_payload_bytes.min(hard_max);
        let input_schema_bytes = exact_json_encoded_len(&serde_json::to_value(&declaration.input)?);
        let result_schema_bytes =
            exact_json_encoded_len(&serde_json::to_value(&declaration.result)?);
        if declaration.input.fields.len() > MAX_APP_AGENT_TOOL_SCHEMA_FIELDS
            || declaration.result.fields.len() > MAX_APP_AGENT_TOOL_SCHEMA_FIELDS
            || input_schema_bytes > MAX_APP_AGENT_TOOL_SCHEMA_BYTES
            || result_schema_bytes > MAX_APP_AGENT_TOOL_SCHEMA_BYTES
            || !agent_tool_schema_is_closed(&declaration.input)
            || !agent_tool_schema_is_closed(&declaration.result)
        {
            return Err(AppAgentCapabilityError::InvalidAgentToolDeclaration);
        }
        let max_input_bytes = declaration.max_input_bytes;
        let max_result_bytes = declaration.max_result_bytes;
        if max_input_bytes == 0
            || max_result_bytes == 0
            || max_input_bytes > resource_max
            || max_result_bytes > resource_max
        {
            return Err(AppAgentCapabilityError::AgentToolValueTooLarge);
        }
        let result_artifact_name = APP_AGENT_TOOL_RESULT_ARTIFACT_NAME.to_owned();
        let child_prompt_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
            "schema": "magician.app-agent-tool-child-prompt.v1",
            "agent_binding_digest": &agent.binding_digest,
            "input_schema": &declaration.input,
            "result_schema": &declaration.result,
            "result_artifact_name": &result_artifact_name,
            "instruction": APP_AGENT_TOOL_CHILD_INSTRUCTION,
        }))?);
        let mut contract = Self {
            schema: "magician.app-agent-tool-contract.v2".to_owned(),
            agent_binding_digest: agent.binding_digest.clone(),
            input_schema: declaration.input.clone(),
            result_schema: declaration.result.clone(),
            max_input_bytes,
            max_result_bytes,
            effective_tools: agent.effective_tools.clone(),
            effective_resources: agent.effective_resources.clone(),
            effective_data_handling_policy,
            result_artifact_name,
            child_prompt_digest,
            contract_digest: AppDigest::blake3(b"pending-agent-tool-contract"),
        };
        contract.contract_digest = contract.canonical_digest()?;
        Ok(contract)
    }

    pub(crate) fn validate_integrity(&self) -> Result<(), AppAgentCapabilityError> {
        let hard_max = u64::try_from(MAX_APP_AGENT_TOOL_VALUE_BYTES).unwrap_or(u64::MAX);
        let resource_max = self.effective_resources.max_payload_bytes.min(hard_max);
        if self.schema != "magician.app-agent-tool-contract.v2"
            || self.input_schema.fields.len() > MAX_APP_AGENT_TOOL_SCHEMA_FIELDS
            || self.result_schema.fields.len() > MAX_APP_AGENT_TOOL_SCHEMA_FIELDS
            || exact_json_encoded_len(&serde_json::to_value(&self.input_schema)?)
                > MAX_APP_AGENT_TOOL_SCHEMA_BYTES
            || exact_json_encoded_len(&serde_json::to_value(&self.result_schema)?)
                > MAX_APP_AGENT_TOOL_SCHEMA_BYTES
            || !agent_tool_schema_is_closed(&self.input_schema)
            || !agent_tool_schema_is_closed(&self.result_schema)
            || self.result_artifact_name.as_str() != APP_AGENT_TOOL_RESULT_ARTIFACT_NAME
            || self.max_input_bytes == 0
            || self.max_result_bytes == 0
            || self.max_input_bytes > resource_max
            || self.max_result_bytes > resource_max
            || self.contract_digest != self.canonical_digest()?
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        Ok(())
    }

    pub(crate) fn validate_input(&self, value: &Value) -> Result<(), AppAgentCapabilityError> {
        self.validate_integrity()?;
        validate_agent_tool_value(value, &self.input_schema, self.max_input_bytes)
    }

    pub(crate) fn validate_result(&self, value: &Value) -> Result<(), AppAgentCapabilityError> {
        self.validate_integrity()?;
        validate_agent_tool_value(value, &self.result_schema, self.max_result_bytes)
    }

    fn canonical_digest(&self) -> Result<AppDigest, AppAgentCapabilityError> {
        Ok(AppDigest::blake3(&canonical_json_bytes(
            &serde_json::json!({
                "schema": &self.schema,
                "agent_binding_digest": &self.agent_binding_digest,
                "input_schema": &self.input_schema,
                "result_schema": &self.result_schema,
                "max_input_bytes": self.max_input_bytes,
                "max_result_bytes": self.max_result_bytes,
                "effective_tools": &self.effective_tools,
                "effective_resources": &self.effective_resources,
                "effective_data_handling_policy": &self.effective_data_handling_policy,
                "result_artifact_name": &self.result_artifact_name,
                "child_prompt_digest": &self.child_prompt_digest,
            }),
        )?))
    }

    pub(crate) fn digest(&self) -> &AppDigest {
        &self.contract_digest
    }

    pub(crate) fn input_schema(&self) -> &AppManifestInputSchema {
        &self.input_schema
    }

    pub(crate) fn result_schema(&self) -> &AppManifestInputSchema {
        &self.result_schema
    }

    pub(crate) fn max_input_bytes(&self) -> u64 {
        self.max_input_bytes
    }

    pub(crate) fn max_result_bytes(&self) -> u64 {
        self.max_result_bytes
    }

    pub(crate) fn result_artifact_name(&self) -> &str {
        self.result_artifact_name.as_str()
    }

    pub(crate) fn child_prompt_digest(&self) -> &AppDigest {
        &self.child_prompt_digest
    }

    pub(crate) fn effective_policy(&self) -> &AppDataHandlingPolicy {
        &self.effective_data_handling_policy
    }

    pub(crate) fn effective_tools(&self) -> &BTreeSet<AppReference> {
        &self.effective_tools
    }

    pub(crate) fn effective_resources(&self) -> &AppResourceCeiling {
        &self.effective_resources
    }

    fn matches_agent_declaration(&self, agent: &AppAgentDefinitionBinding) -> bool {
        agent_definition_permits_app_task(&agent.sealed_definition)
            && self.effective_tools == agent.effective_tools
            && self.effective_resources == agent.effective_resources
            && agent
                .sealed_definition
                .app_tool
                .as_ref()
                .is_some_and(|declaration| {
                    self.input_schema == declaration.input
                        && self.result_schema == declaration.result
                        && self.max_input_bytes == declaration.max_input_bytes
                        && self.max_result_bytes == declaration.max_result_bytes
                })
    }

    fn validate_value_labels(
        &self,
        schema: &AppManifestInputSchema,
        value: &Value,
        labels: &AppHandlingLabels,
    ) -> Result<(), AppAgentCapabilityError> {
        let expected_policy_digest = AppDigest::blake3(&canonical_json_bytes(
            &serde_json::to_value(&self.effective_data_handling_policy)?,
        )?);
        let (classification_floor, model_processing) = schema.resolve_handling_floor(
            value,
            self.effective_data_handling_policy.classification_floor,
            self.effective_data_handling_policy.model_processing,
        )?;
        if labels.policy_digest != expected_policy_digest
            || labels.classification < classification_floor
            || labels.model_processing > model_processing
        {
            return Err(AppAgentCapabilityError::AgentToolResultPolicyDenied);
        }
        Ok(())
    }

    fn validate_input_labels(
        &self,
        value: &Value,
        labels: &AppHandlingLabels,
    ) -> Result<(), AppAgentCapabilityError> {
        self.validate_value_labels(&self.input_schema, value, labels)
    }

    pub(crate) fn input_labels(
        &self,
        value: &Value,
        provenance_digest: AppDigest,
    ) -> Result<AppHandlingLabels, AppAgentCapabilityError> {
        self.validate_input(value)?;
        let (classification, model_processing) = self.input_schema.resolve_handling_floor(
            value,
            self.effective_data_handling_policy.classification_floor,
            self.effective_data_handling_policy.model_processing,
        )?;
        Ok(AppHandlingLabels {
            classification,
            model_processing,
            policy_digest: AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
                &self.effective_data_handling_policy,
            )?)?),
            provenance_digest,
        })
    }

    pub(crate) fn validate_result_labels(
        &self,
        value: &Value,
        labels: &AppHandlingLabels,
    ) -> Result<(), AppAgentCapabilityError> {
        self.validate_value_labels(&self.result_schema, value, labels)
    }

    fn validate_control_labels(
        &self,
        labels: &AppHandlingLabels,
    ) -> Result<(), AppAgentCapabilityError> {
        let expected_policy_digest = AppDigest::blake3(&canonical_json_bytes(
            &serde_json::to_value(&self.effective_data_handling_policy)?,
        )?);
        if labels.policy_digest != expected_policy_digest
            || labels.classification < self.effective_data_handling_policy.classification_floor
            || labels.model_processing > self.effective_data_handling_policy.model_processing
        {
            return Err(AppAgentCapabilityError::AgentToolResultPolicyDenied);
        }
        Ok(())
    }

    pub(crate) fn result_labels(
        &self,
        value: &Value,
        provenance_digest: AppDigest,
    ) -> Result<AppHandlingLabels, AppAgentCapabilityError> {
        self.validate_result(value)?;
        let (classification, model_processing) = self.result_schema.resolve_handling_floor(
            value,
            self.effective_data_handling_policy.classification_floor,
            self.effective_data_handling_policy.model_processing,
        )?;
        Ok(AppHandlingLabels {
            classification,
            model_processing,
            policy_digest: AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
                &self.effective_data_handling_policy,
            )?)?),
            provenance_digest,
        })
    }

    pub(crate) fn control_labels(
        &self,
        provenance_digest: AppDigest,
    ) -> Result<AppHandlingLabels, AppAgentCapabilityError> {
        self.validate_integrity()?;
        Ok(AppHandlingLabels {
            classification: self.effective_data_handling_policy.classification_floor,
            model_processing: self.effective_data_handling_policy.model_processing,
            policy_digest: AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(
                &self.effective_data_handling_policy,
            )?)?),
            provenance_digest,
        })
    }
}

fn agent_tool_schema_is_closed(schema: &AppManifestInputSchema) -> bool {
    schema.fields.values().all(|field| match field {
        super::manifest::AppManifestField::Enum { values, .. } => !values.is_empty(),
        _ => true,
    })
}

/// Install/task-sealed callable target. Friendly agent/tool names are absent
/// from dispatch authority; every execution must match the locked primitive,
/// action implementation, exact target definition and typed contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppAgentToolBinding {
    schema: String,
    primitive_ref: AppReference,
    primitive_source_digest: AppDigest,
    action_descriptor_digest: AppDigest,
    implementation_plan_digest: AppDigest,
    agent: AppAgentDefinitionBinding,
    contract: AppAgentToolContract,
    binding_digest: AppDigest,
}

impl AppAgentToolBinding {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn seal(
        primitive_ref: AppReference,
        primitive_source_digest: AppDigest,
        action_descriptor_digest: AppDigest,
        implementation_plan_digest: AppDigest,
        agent: AppAgentDefinitionBinding,
        contract: AppAgentToolContract,
    ) -> Result<Self, AppAgentCapabilityError> {
        agent.validate_integrity()?;
        contract.validate_integrity()?;
        if contract.agent_binding_digest != agent.binding_digest
            || !contract.matches_agent_declaration(&agent)
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        let mut binding = Self {
            schema: "magician.app-agent-tool-binding.v1".to_owned(),
            primitive_ref,
            primitive_source_digest,
            action_descriptor_digest,
            implementation_plan_digest,
            agent,
            contract,
            binding_digest: AppDigest::blake3(b"pending-agent-tool-binding"),
        };
        binding.binding_digest = binding.canonical_digest()?;
        Ok(binding)
    }

    pub(crate) fn validate_integrity(&self) -> Result<(), AppAgentCapabilityError> {
        self.agent.validate_integrity()?;
        self.contract.validate_integrity()?;
        if self.schema != "magician.app-agent-tool-binding.v1"
            || self.contract.agent_binding_digest != self.agent.binding_digest
            || !self.contract.matches_agent_declaration(&self.agent)
            || self.binding_digest != self.canonical_digest()?
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        Ok(())
    }

    fn canonical_digest(&self) -> Result<AppDigest, AppAgentCapabilityError> {
        Ok(AppDigest::blake3(&canonical_json_bytes(
            &serde_json::json!({
                "schema": &self.schema,
                "primitive_ref": &self.primitive_ref,
                "primitive_source_digest": &self.primitive_source_digest,
                "action_descriptor_digest": &self.action_descriptor_digest,
                "implementation_plan_digest": &self.implementation_plan_digest,
                "agent_binding_digest": &self.agent.binding_digest,
                "contract_digest": &self.contract.contract_digest,
            }),
        )?))
    }

    pub(crate) fn agent(&self) -> &AppAgentDefinitionBinding {
        &self.agent
    }

    pub(crate) fn contract(&self) -> &AppAgentToolContract {
        &self.contract
    }

    pub(crate) fn primitive_ref(&self) -> &AppReference {
        &self.primitive_ref
    }

    pub(crate) fn primitive_source_digest(&self) -> &AppDigest {
        &self.primitive_source_digest
    }

    pub(crate) fn action_descriptor_digest(&self) -> &AppDigest {
        &self.action_descriptor_digest
    }

    pub(crate) fn implementation_plan_digest(&self) -> &AppDigest {
        &self.implementation_plan_digest
    }

    pub(crate) fn digest(&self) -> &AppDigest {
        &self.binding_digest
    }
}

/// Durable, sealed owner record for one exact child execution. It is stored
/// under the canonical Artifact V3 task/execution tree before the child may
/// start, so a restart reconstructs the same definition, prompt, authority,
/// typed input and retry identity instead of consulting a mutable agent name.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppAgentChildTaskBinding {
    schema: String,
    parent_task_ref: AppReference,
    parent_execution_ref: AppReference,
    child_task_ref: AppReference,
    child_execution_ref: AppReference,
    child_execution_id: String,
    action_invocation_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    workflow_authority_digest: AppDigest,
    tool_binding: AppAgentToolBinding,
    input: Value,
    input_digest: AppDigest,
    input_handling_labels: AppHandlingLabels,
    retry_generation: u64,
    expires_at_ms: i64,
    launch_digest: AppDigest,
}

/// Move-only evidence that the authenticated workflow owner admitted the
/// exact child input and its upstream provenance. Raw JSON or a caller-chosen
/// digest cannot be promoted into labeled child input authority.
pub(crate) struct AppAgentChildInputProvenanceProof {
    action_invocation_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    workflow_authority_digest: AppDigest,
    input_digest: AppDigest,
    provenance_digest: AppDigest,
}

impl fmt::Debug for AppAgentChildInputProvenanceProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildInputProvenanceProof")
            .field("action_invocation_ref", &self.action_invocation_ref)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("workflow_authority_digest", &self.workflow_authority_digest)
            .field("input_digest", &self.input_digest)
            .field("provenance_digest", &self.provenance_digest)
            .finish()
    }
}

impl AppAgentChildInputProvenanceProof {
    /// Minted only by the authenticated workflow owner after it has validated
    /// and labeled the exact canonical JSON input. The proof is move-only and
    /// cannot be reconstructed from an app/model wire payload.
    pub(crate) fn from_workflow_owner(
        action_invocation_ref: AppReference,
        scope_binding_ref: AppScopeBindingRef,
        workflow_authority_digest: AppDigest,
        input: &Value,
        provenance_digest: AppDigest,
    ) -> Result<Self, AppAgentCapabilityError> {
        Ok(Self {
            action_invocation_ref,
            scope_binding_ref,
            workflow_authority_digest,
            input_digest: AppDigest::blake3(&canonical_json_bytes(input)?),
            provenance_digest,
        })
    }
}

impl fmt::Debug for AppAgentChildTaskBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildTaskBinding")
            .field("schema", &self.schema)
            .field("parent_task_ref", &self.parent_task_ref)
            .field("parent_execution_ref", &self.parent_execution_ref)
            .field("child_task_ref", &self.child_task_ref)
            .field("child_execution_ref", &self.child_execution_ref)
            .field("child_execution_id", &self.child_execution_id)
            .field("action_invocation_ref", &self.action_invocation_ref)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("workflow_authority_digest", &self.workflow_authority_digest)
            .field("tool_binding_digest", &self.tool_binding.binding_digest)
            .field("input_digest", &self.input_digest)
            .field("input_handling_labels", &self.input_handling_labels)
            .field("retry_generation", &self.retry_generation)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("launch_digest", &self.launch_digest)
            .finish_non_exhaustive()
    }
}

impl AppAgentChildTaskBinding {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn seal(
        tool_binding: AppAgentToolBinding,
        parent_task_ref: AppReference,
        parent_execution_ref: AppReference,
        child_execution_id: String,
        action_invocation_ref: AppReference,
        scope_binding_ref: AppScopeBindingRef,
        workflow_authority_digest: AppDigest,
        input: Value,
        input_provenance: AppAgentChildInputProvenanceProof,
        retry_generation: u64,
        expires_at_ms: i64,
    ) -> Result<Self, AppAgentCapabilityError> {
        tool_binding.validate_integrity()?;
        tool_binding.contract.validate_input(&input)?;
        let input_digest = AppDigest::blake3(&canonical_json_bytes(&input)?);
        if input_provenance.action_invocation_ref != action_invocation_ref
            || input_provenance.scope_binding_ref != scope_binding_ref
            || input_provenance.workflow_authority_digest != workflow_authority_digest
            || input_provenance.input_digest != input_digest
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        let input_handling_labels = tool_binding
            .contract
            .input_labels(&input, input_provenance.provenance_digest)?;
        if retry_generation == 0 || expires_at_ms <= 0 {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        let parent_execution_id = parent_execution_ref
            .as_str()
            .strip_prefix("execution:")
            .ok_or(AppAgentCapabilityError::CorruptAgentToolContract)?;
        if child_execution_id
            != agent_tool_child_execution_id(
                parent_execution_id,
                &action_invocation_ref,
                tool_binding.digest(),
                &input_digest,
                retry_generation,
            )?
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        let child_task_ref = parent_task_ref.clone();
        let child_execution_ref = AppReference::parse(format!("execution:{child_execution_id}"))?;
        let launch_digest = agent_child_launch_digest(
            &parent_task_ref,
            &parent_execution_ref,
            &child_task_ref,
            &child_execution_ref,
            &action_invocation_ref,
            &scope_binding_ref,
            &workflow_authority_digest,
            &tool_binding.binding_digest,
            &input_digest,
            &input_handling_labels,
            retry_generation,
            expires_at_ms,
        )?;
        Ok(Self {
            schema: "magician.app-agent-child-task-binding.v3".to_owned(),
            parent_task_ref,
            parent_execution_ref,
            child_task_ref,
            child_execution_ref,
            child_execution_id,
            action_invocation_ref,
            scope_binding_ref,
            workflow_authority_digest,
            tool_binding,
            input,
            input_digest,
            input_handling_labels,
            retry_generation,
            expires_at_ms,
            launch_digest,
        })
    }

    pub(crate) fn validate_integrity(&self) -> Result<(), AppAgentCapabilityError> {
        self.tool_binding.validate_integrity()?;
        self.tool_binding.contract.validate_input(&self.input)?;
        self.tool_binding
            .contract
            .validate_input_labels(&self.input, &self.input_handling_labels)?;
        if self.schema != "magician.app-agent-child-task-binding.v3"
            || self.child_task_ref != self.parent_task_ref
            || self.retry_generation == 0
            || self.expires_at_ms <= 0
            || self.child_execution_ref.as_str() != format!("execution:{}", self.child_execution_id)
            || self.child_execution_id
                != agent_tool_child_execution_id(
                    self.parent_execution_ref
                        .as_str()
                        .strip_prefix("execution:")
                        .ok_or(AppAgentCapabilityError::CorruptAgentToolContract)?,
                    &self.action_invocation_ref,
                    self.tool_binding.digest(),
                    &self.input_digest,
                    self.retry_generation,
                )?
            || self.input_digest != AppDigest::blake3(&canonical_json_bytes(&self.input)?)
            || self.launch_digest
                != agent_child_launch_digest(
                    &self.parent_task_ref,
                    &self.parent_execution_ref,
                    &self.child_task_ref,
                    &self.child_execution_ref,
                    &self.action_invocation_ref,
                    &self.scope_binding_ref,
                    &self.workflow_authority_digest,
                    &self.tool_binding.binding_digest,
                    &self.input_digest,
                    &self.input_handling_labels,
                    self.retry_generation,
                    self.expires_at_ms,
                )?
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        Ok(())
    }

    pub(crate) fn tool_binding(&self) -> &AppAgentToolBinding {
        &self.tool_binding
    }

    pub(crate) fn input(&self) -> &Value {
        &self.input
    }

    pub(crate) fn parent_task_ref(&self) -> &AppReference {
        &self.parent_task_ref
    }

    pub(crate) fn parent_execution_ref(&self) -> &AppReference {
        &self.parent_execution_ref
    }

    pub(crate) fn child_execution_ref(&self) -> &AppReference {
        &self.child_execution_ref
    }

    pub(crate) fn child_execution_id(&self) -> &str {
        self.child_execution_id.as_str()
    }

    pub(crate) fn child_task_ref(&self) -> &AppReference {
        &self.child_task_ref
    }

    pub(crate) fn action_invocation_ref(&self) -> &AppReference {
        &self.action_invocation_ref
    }

    pub(crate) fn scope_binding_ref(&self) -> &AppScopeBindingRef {
        &self.scope_binding_ref
    }

    pub(crate) fn workflow_authority_digest(&self) -> &AppDigest {
        &self.workflow_authority_digest
    }

    pub(crate) fn input_digest(&self) -> &AppDigest {
        &self.input_digest
    }

    pub(crate) fn input_handling_labels(&self) -> &AppHandlingLabels {
        &self.input_handling_labels
    }

    pub(crate) fn launch_digest(&self) -> &AppDigest {
        &self.launch_digest
    }

    pub(crate) fn retry_generation(&self) -> u64 {
        self.retry_generation
    }

    pub(crate) fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    pub(crate) fn target_agent_id(&self) -> &str {
        self.tool_binding
            .agent()
            .sealed_definition
            .agent_id
            .as_str()
    }

    pub(crate) fn child_context(&self) -> Result<String, AppAgentCapabilityError> {
        self.validate_integrity()?;
        Ok(format!(
            "{APP_AGENT_TOOL_CHILD_INSTRUCTION}\n\nResult artifact: {}\nContract digest: \
             {}\nLaunch digest: {}",
            self.tool_binding.contract().result_artifact_name(),
            self.tool_binding.contract().digest(),
            self.launch_digest,
        ))
    }

    pub(crate) fn result_declaration_permit(
        &self,
    ) -> Result<AppAgentToolResultDeclarationPermit, AppAgentCapabilityError> {
        self.validate_integrity()?;
        let contract = self.tool_binding().contract();
        let mut declaration = crate::magician_v2::agents::types::ArtifactDeclaration::simple(
            contract.result_artifact_name(),
        )
        .with_content_type("application/json");
        declaration.schema = Some(contract.result_schema().to_primitive_json_schema());
        Ok(AppAgentToolResultDeclarationPermit {
            declaration,
            binding_digest: self.launch_digest.clone(),
        })
    }

    pub(crate) fn timeout_ceiling_seconds(&self) -> u64 {
        self.tool_binding
            .contract()
            .effective_resources
            .max_active_seconds
    }

    /// Canonical delegated-child request derived only from the sealed launch.
    /// Startup adoption uses this same constructor, so it cannot rebuild a
    /// crashed reservation from mutable catalog or model arguments.
    pub(crate) fn delegation_target_request(
        &self,
        now_ms: i64,
    ) -> Result<DelegationTargetRequest, AppAgentCapabilityError> {
        self.validate_integrity()?;
        let remaining_ms = self
            .expires_at_ms
            .checked_sub(now_ms)
            .filter(|remaining| *remaining > 0)
            .ok_or(AppAgentCapabilityError::AgentToolLaunchExpired)?;
        let timeout_secs = u64::try_from(remaining_ms)
            .unwrap_or(u64::MAX)
            .saturating_add(999)
            .checked_div(1_000)
            .unwrap_or(0)
            .min(self.timeout_ceiling_seconds());
        if timeout_secs == 0 {
            return Err(AppAgentCapabilityError::AgentToolLaunchExpired);
        }
        Ok(DelegationTargetRequest {
            target_agent_id: self.target_agent_id().to_owned(),
            context: self.child_context()?,
            input_artifact_ids: Vec::new(),
            input_data: Some(self.input.clone()),
            depth: None,
            timeout_secs: Some(timeout_secs),
            spend_token_ids: Vec::new(),
            required_capability: None,
            expected_artifacts: vec![DelegationExpectedArtifact {
                name: self
                    .tool_binding
                    .contract()
                    .result_artifact_name()
                    .to_owned(),
                content_type: Some("application/json".to_owned()),
            }],
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn agent_child_launch_digest(
    parent_task_ref: &AppReference,
    parent_execution_ref: &AppReference,
    child_task_ref: &AppReference,
    child_execution_ref: &AppReference,
    action_invocation_ref: &AppReference,
    scope_binding_ref: &AppScopeBindingRef,
    workflow_authority_digest: &AppDigest,
    tool_binding_digest: &AppDigest,
    input_digest: &AppDigest,
    input_handling_labels: &AppHandlingLabels,
    retry_generation: u64,
    expires_at_ms: i64,
) -> Result<AppDigest, AppAgentCapabilityError> {
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": "magician.app-agent-child-launch.v2",
            "parent_task_ref": parent_task_ref,
            "parent_execution_ref": parent_execution_ref,
            "child_task_ref": child_task_ref,
            "child_execution_ref": child_execution_ref,
            "action_invocation_ref": action_invocation_ref,
            "scope_binding_ref": scope_binding_ref,
            "workflow_authority_digest": workflow_authority_digest,
            "tool_binding_digest": tool_binding_digest,
            "input_digest": input_digest,
            "input_handling_labels": input_handling_labels,
            "retry_generation": retry_generation,
            "expires_at_ms": expires_at_ms,
        }),
    )?))
}

/// Opaque runtime child handle. It is neither clonable nor serializable and
/// therefore cannot be retained as ambient authority or reconstructed from an
/// app/model payload. Durable replay reopens the sealed task binding and mints
/// a fresh handle after exact validation.
pub(crate) struct AppAgentChildRunHandle {
    parent_task_ref: AppReference,
    parent_execution_ref: AppReference,
    child_execution_ref: AppReference,
    action_invocation_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    launch_digest: AppDigest,
}

impl fmt::Debug for AppAgentChildRunHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildRunHandle")
            .field("parent_task_ref", &self.parent_task_ref)
            .field("parent_execution_ref", &self.parent_execution_ref)
            .field("child_execution_ref", &self.child_execution_ref)
            .field("action_invocation_ref", &self.action_invocation_ref)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("launch_digest", &self.launch_digest)
            .finish()
    }
}

#[allow(dead_code)] // Opaque handle inspection is retained for downstream owners/tests.
impl AppAgentChildRunHandle {
    pub(crate) fn restore(
        binding: &AppAgentChildTaskBinding,
    ) -> Result<Self, AppAgentCapabilityError> {
        binding.validate_integrity()?;
        Ok(Self {
            parent_task_ref: binding.parent_task_ref.clone(),
            parent_execution_ref: binding.parent_execution_ref.clone(),
            child_execution_ref: binding.child_execution_ref.clone(),
            action_invocation_ref: binding.action_invocation_ref.clone(),
            scope_binding_ref: binding.scope_binding_ref.clone(),
            launch_digest: binding.launch_digest.clone(),
        })
    }

    pub(crate) fn child_execution_ref(&self) -> &AppReference {
        &self.child_execution_ref
    }

    pub(crate) fn launch_digest(&self) -> &AppDigest {
        &self.launch_digest
    }

    fn matches_binding(
        &self,
        binding: &AppAgentChildTaskBinding,
    ) -> Result<(), AppAgentCapabilityError> {
        if self.parent_task_ref != binding.parent_task_ref
            || self.parent_execution_ref != binding.parent_execution_ref
            || self.child_execution_ref != binding.child_execution_ref
            || self.action_invocation_ref != binding.action_invocation_ref
            || self.scope_binding_ref != binding.scope_binding_ref
            || self.launch_digest != binding.launch_digest
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        Ok(())
    }
}

fn validate_agent_tool_value(
    value: &Value,
    schema: &AppManifestInputSchema,
    max_bytes: u64,
) -> Result<(), AppAgentCapabilityError> {
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    if exact_json_encoded_len(value) > max_bytes {
        return Err(AppAgentCapabilityError::AgentToolValueTooLarge);
    }
    schema.validate_value(value)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum AppAgentChildControlStatus {
    Completed,
    Cancelled,
    OutcomeUncertain,
}

/// Server-created labeled child result/control event. It deliberately has no
/// `Deserialize`: wire JSON cannot become completion or cancellation
/// authority. A future runtime adapter must persist it through the canonical
/// V3 task/control owner and reconstruct it with trusted labels.
#[derive(Serialize, PartialEq)]
#[allow(dead_code)]
pub(crate) struct AppAgentChildControlCarrier {
    schema: String,
    parent_task_ref: AppReference,
    parent_execution_ref: AppReference,
    child_task_ref: AppReference,
    child_execution_ref: AppReference,
    action_invocation_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    launch_digest: AppDigest,
    notification_ref: AppReference,
    sequence: u64,
    contract_digest: AppDigest,
    workflow_authority_digest: AppDigest,
    status: AppAgentChildControlStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value_digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_artifact_ref: Option<AppReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_artifact_digest: Option<AppDigest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_code: Option<AppName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_settlement_digest: Option<AppDigest>,
    handling_labels: AppHandlingLabels,
    cancellation_settled: bool,
    effect_uncertain: bool,
    carrier_digest: AppDigest,
}

impl fmt::Debug for AppAgentChildControlCarrier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildControlCarrier")
            .field("schema", &self.schema)
            .field("parent_task_ref", &self.parent_task_ref)
            .field("parent_execution_ref", &self.parent_execution_ref)
            .field("child_task_ref", &self.child_task_ref)
            .field("child_execution_ref", &self.child_execution_ref)
            .field("action_invocation_ref", &self.action_invocation_ref)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("launch_digest", &self.launch_digest)
            .field("notification_ref", &self.notification_ref)
            .field("sequence", &self.sequence)
            .field("contract_digest", &self.contract_digest)
            .field("workflow_authority_digest", &self.workflow_authority_digest)
            .field("status", &self.status)
            .field("value_digest", &self.value_digest)
            .field("result_artifact_ref", &self.result_artifact_ref)
            .field("result_artifact_digest", &self.result_artifact_digest)
            .field("failure_code", &self.failure_code)
            .field(
                "terminal_settlement_digest",
                &self.terminal_settlement_digest,
            )
            .field("handling_labels", &self.handling_labels)
            .field("cancellation_settled", &self.cancellation_settled)
            .field("effect_uncertain", &self.effect_uncertain)
            .field("carrier_digest", &self.carrier_digest)
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)]
impl AppAgentChildControlCarrier {
    /// Mint the one terminal carrier from an Artifact-owned observation.
    ///
    /// The observation has no public constructor and can only be produced
    /// after the canonical child execution and its exact result artifact have
    /// been reconciled. This prevents an ordinary crate caller from turning
    /// arbitrary JSON or a cancellation request into a terminal child result.
    pub(crate) fn from_artifact_observation(
        binding: &AppAgentChildTaskBinding,
        handle: AppAgentChildRunHandle,
        observation: crate::magician_v2::artifact_v2::app_agent_tool::AppAgentChildTerminalObservation,
    ) -> Result<Self, AppAgentCapabilityError> {
        binding.validate_integrity()?;
        handle.matches_binding(binding)?;
        if observation.launch_digest() != binding.launch_digest() {
            return Err(AppAgentCapabilityError::AgentToolTerminalObservationMismatch);
        }
        let (
            status,
            value,
            value_digest,
            result_artifact_ref,
            result_artifact_digest,
            failure_code,
            terminal_settlement_digest,
            handling_labels,
            cancellation_settled,
            effect_uncertain,
        ) = observation.into_parts();
        let contract = binding.tool_binding.contract();
        if status == AppAgentChildControlStatus::Completed {
            let value_ref = value
                .as_ref()
                .ok_or(AppAgentCapabilityError::CorruptAgentToolContract)?;
            let digest = value_digest
                .as_ref()
                .ok_or(AppAgentCapabilityError::CorruptAgentToolContract)?;
            if AppDigest::blake3(&canonical_json_bytes(value_ref)?) != *digest {
                return Err(AppAgentCapabilityError::CorruptAgentToolContract);
            }
            contract.validate_result(value_ref)?;
            contract.validate_result_labels(value_ref, &handling_labels)?;
        }
        let shape_valid = match status {
            AppAgentChildControlStatus::Completed => {
                value.is_some()
                    && value_digest.is_some()
                    && result_artifact_ref.is_some()
                    && result_artifact_digest.is_some()
                    && result_artifact_digest == value_digest
                    && failure_code.is_none()
                    && terminal_settlement_digest.is_some()
                    && !cancellation_settled
                    && !effect_uncertain
            },
            AppAgentChildControlStatus::Cancelled => {
                value.is_none()
                    && value_digest.is_none()
                    && result_artifact_ref.is_none()
                    && result_artifact_digest.is_none()
                    && failure_code.is_none()
                    && terminal_settlement_digest.is_some()
                    && cancellation_settled
                    && !effect_uncertain
            },
            AppAgentChildControlStatus::OutcomeUncertain => {
                value.is_none()
                    && value_digest.is_none()
                    && result_artifact_ref.is_none()
                    && result_artifact_digest.is_none()
                    && failure_code.is_some()
                    && !cancellation_settled
                    && effect_uncertain
            },
        };
        if !shape_valid {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        if status != AppAgentChildControlStatus::Completed {
            contract.validate_control_labels(&handling_labels)?;
        }
        Self::new(
            binding,
            handle,
            status,
            value,
            value_digest,
            result_artifact_ref,
            result_artifact_digest,
            failure_code,
            terminal_settlement_digest,
            handling_labels,
            cancellation_settled,
            effect_uncertain,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        binding: &AppAgentChildTaskBinding,
        handle: AppAgentChildRunHandle,
        status: AppAgentChildControlStatus,
        value: Option<Value>,
        value_digest: Option<AppDigest>,
        result_artifact_ref: Option<AppReference>,
        result_artifact_digest: Option<AppDigest>,
        failure_code: Option<AppName>,
        terminal_settlement_digest: Option<AppDigest>,
        handling_labels: AppHandlingLabels,
        cancellation_settled: bool,
        effect_uncertain: bool,
    ) -> Result<Self, AppAgentCapabilityError> {
        binding.validate_integrity()?;
        handle.matches_binding(binding)?;
        let contract = binding.tool_binding.contract();
        contract.validate_integrity()?;
        let parent_task_ref = binding.parent_task_ref.clone();
        let parent_execution_ref = binding.parent_execution_ref.clone();
        let child_task_ref = binding.child_task_ref.clone();
        let child_execution_ref = binding.child_execution_ref.clone();
        let action_invocation_ref = binding.action_invocation_ref.clone();
        let scope_binding_ref = binding.scope_binding_ref.clone();
        let workflow_authority_digest = binding.workflow_authority_digest.clone();
        let launch_digest = binding.launch_digest.clone();
        let notification_ref = AppReference::parse(format!(
            "agent-child-notification:{}",
            launch_digest.as_str()
        ))?;
        let sequence = 1;
        let carrier_digest = agent_child_control_carrier_digest(
            binding,
            &notification_ref,
            status,
            value_digest.as_ref(),
            result_artifact_ref.as_ref(),
            result_artifact_digest.as_ref(),
            failure_code.as_ref(),
            terminal_settlement_digest.as_ref(),
            &handling_labels,
            cancellation_settled,
            effect_uncertain,
        )?;
        Ok(Self {
            schema: "magician.app-agent-child-control.v3".to_owned(),
            parent_task_ref,
            parent_execution_ref,
            child_task_ref,
            child_execution_ref,
            action_invocation_ref,
            scope_binding_ref,
            launch_digest,
            notification_ref,
            sequence,
            contract_digest: contract.contract_digest.clone(),
            workflow_authority_digest,
            status,
            value,
            value_digest,
            result_artifact_ref,
            result_artifact_digest,
            failure_code,
            terminal_settlement_digest,
            handling_labels,
            cancellation_settled,
            effect_uncertain,
            carrier_digest,
        })
    }

    pub(crate) fn notification_ref(&self) -> &AppReference {
        &self.notification_ref
    }

    pub(crate) fn carrier_digest(&self) -> &AppDigest {
        &self.carrier_digest
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn terminal_material(
        &self,
    ) -> (
        AppAgentChildControlStatus,
        Option<Value>,
        Option<AppDigest>,
        Option<AppReference>,
        Option<AppDigest>,
        Option<AppName>,
        Option<AppDigest>,
        AppHandlingLabels,
        bool,
        bool,
    ) {
        (
            self.status,
            self.value.clone(),
            self.value_digest.clone(),
            self.result_artifact_ref.clone(),
            self.result_artifact_digest.clone(),
            self.failure_code.clone(),
            self.terminal_settlement_digest.clone(),
            self.handling_labels.clone(),
            self.cancellation_settled,
            self.effect_uncertain,
        )
    }

    pub(crate) fn status(&self) -> AppAgentChildControlStatus {
        self.status
    }

    pub(crate) fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }
}

#[allow(clippy::too_many_arguments)]
fn agent_child_control_carrier_digest(
    binding: &AppAgentChildTaskBinding,
    notification_ref: &AppReference,
    status: AppAgentChildControlStatus,
    value_digest: Option<&AppDigest>,
    result_artifact_ref: Option<&AppReference>,
    result_artifact_digest: Option<&AppDigest>,
    failure_code: Option<&AppName>,
    terminal_settlement_digest: Option<&AppDigest>,
    handling_labels: &AppHandlingLabels,
    cancellation_settled: bool,
    effect_uncertain: bool,
) -> Result<AppDigest, AppAgentCapabilityError> {
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": "magician.app-agent-child-control.v3",
            "parent_task_ref": &binding.parent_task_ref,
            "parent_execution_ref": &binding.parent_execution_ref,
            "child_task_ref": &binding.child_task_ref,
            "child_execution_ref": &binding.child_execution_ref,
            "action_invocation_ref": &binding.action_invocation_ref,
            "scope_binding_ref": &binding.scope_binding_ref,
            "launch_digest": &binding.launch_digest,
            "notification_ref": notification_ref,
            "sequence": 1,
            "contract_digest": &binding.tool_binding.contract.contract_digest,
            "workflow_authority_digest": &binding.workflow_authority_digest,
            "status": status,
            "value_digest": value_digest,
            "result_artifact_ref": result_artifact_ref,
            "result_artifact_digest": result_artifact_digest,
            "failure_code": failure_code,
            "terminal_settlement_digest": terminal_settlement_digest,
            "handling_labels": handling_labels,
            "cancellation_settled": cancellation_settled,
            "effect_uncertain": effect_uncertain,
        }),
    )?))
}

/// Durable outbox record for the labeled child terminal/control carrier.
/// Artifact V3 consumes `notification_ref` idempotently, then the owner marks
/// this record delivered. A crash before the mark replays the same ref/digest;
/// a crash after it cannot produce a second parent wake.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppAgentChildControlRecord {
    schema: String,
    launch_digest: AppDigest,
    notification_ref: AppReference,
    sequence: u64,
    status: AppAgentChildControlStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_artifact_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_artifact_digest: Option<AppDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_code: Option<AppName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_settlement_digest: Option<AppDigest>,
    handling_labels: AppHandlingLabels,
    cancellation_settled: bool,
    effect_uncertain: bool,
    carrier_digest: AppDigest,
    delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivered_parent_event_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivered_parent_event_digest: Option<AppDigest>,
    record_digest: AppDigest,
}

impl fmt::Debug for AppAgentChildControlRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildControlRecord")
            .field("schema", &self.schema)
            .field("launch_digest", &self.launch_digest)
            .field("notification_ref", &self.notification_ref)
            .field("sequence", &self.sequence)
            .field("status", &self.status)
            .field("value_digest", &self.value_digest)
            .field("result_artifact_ref", &self.result_artifact_ref)
            .field("result_artifact_digest", &self.result_artifact_digest)
            .field("failure_code", &self.failure_code)
            .field(
                "terminal_settlement_digest",
                &self.terminal_settlement_digest,
            )
            .field("handling_labels", &self.handling_labels)
            .field("cancellation_settled", &self.cancellation_settled)
            .field("effect_uncertain", &self.effect_uncertain)
            .field("carrier_digest", &self.carrier_digest)
            .field("delivered", &self.delivered)
            .field(
                "delivered_parent_event_ref",
                &self.delivered_parent_event_ref,
            )
            .field(
                "delivered_parent_event_digest",
                &self.delivered_parent_event_digest,
            )
            .field("record_digest", &self.record_digest)
            .finish_non_exhaustive()
    }
}

impl AppAgentChildControlRecord {
    pub(crate) fn pending(
        carrier: AppAgentChildControlCarrier,
    ) -> Result<Self, AppAgentCapabilityError> {
        let mut record = Self {
            schema: "magician.app-agent-child-control-record.v2".to_owned(),
            launch_digest: carrier.launch_digest,
            notification_ref: carrier.notification_ref,
            sequence: carrier.sequence,
            status: carrier.status,
            value: carrier.value,
            value_digest: carrier.value_digest,
            result_artifact_ref: carrier.result_artifact_ref,
            result_artifact_digest: carrier.result_artifact_digest,
            failure_code: carrier.failure_code,
            terminal_settlement_digest: carrier.terminal_settlement_digest,
            handling_labels: carrier.handling_labels,
            cancellation_settled: carrier.cancellation_settled,
            effect_uncertain: carrier.effect_uncertain,
            carrier_digest: carrier.carrier_digest,
            delivered: false,
            delivered_parent_event_ref: None,
            delivered_parent_event_digest: None,
            record_digest: AppDigest::blake3(b"pending-agent-child-control-record"),
        };
        record.record_digest = record.canonical_digest()?;
        Ok(record)
    }

    pub(crate) fn validate_integrity(
        &self,
        binding: &AppAgentChildTaskBinding,
    ) -> Result<(), AppAgentCapabilityError> {
        binding.validate_integrity()?;
        if self.schema != "magician.app-agent-child-control-record.v2"
            || self.launch_digest != binding.launch_digest
            || self.sequence != 1
            || !matches!(
                (
                    self.delivered,
                    self.delivered_parent_event_ref.is_some(),
                    self.delivered_parent_event_digest.is_some(),
                ),
                (false, false, false) | (true, true, true)
            )
            || self.record_digest != self.canonical_digest()?
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        match self.status {
            AppAgentChildControlStatus::Completed => {
                let value = self
                    .value
                    .as_ref()
                    .ok_or(AppAgentCapabilityError::CorruptAgentToolContract)?;
                binding.tool_binding.contract.validate_result(value)?;
                binding
                    .tool_binding
                    .contract
                    .validate_result_labels(value, &self.handling_labels)?;
                if self.value_digest.as_ref()
                    != Some(&AppDigest::blake3(&canonical_json_bytes(value)?))
                    || self.result_artifact_ref.is_none()
                    || self.result_artifact_digest.is_none()
                    || self.result_artifact_digest != self.value_digest
                    || self.failure_code.is_some()
                    || self.terminal_settlement_digest.is_none()
                    || self.cancellation_settled
                    || self.effect_uncertain
                {
                    return Err(AppAgentCapabilityError::CorruptAgentToolContract);
                }
            },
            AppAgentChildControlStatus::Cancelled => {
                if self.value.is_some()
                    || self.value_digest.is_some()
                    || self.result_artifact_ref.is_some()
                    || self.result_artifact_digest.is_some()
                    || self.failure_code.is_some()
                    || self.terminal_settlement_digest.is_none()
                    || !self.cancellation_settled
                    || self.effect_uncertain
                {
                    return Err(AppAgentCapabilityError::CorruptAgentToolContract);
                }
                binding
                    .tool_binding
                    .contract
                    .validate_control_labels(&self.handling_labels)?;
            },
            AppAgentChildControlStatus::OutcomeUncertain => {
                if self.value.is_some()
                    || self.value_digest.is_some()
                    || self.result_artifact_ref.is_some()
                    || self.result_artifact_digest.is_some()
                    || self.failure_code.is_none()
                    || self.cancellation_settled
                    || !self.effect_uncertain
                {
                    return Err(AppAgentCapabilityError::CorruptAgentToolContract);
                }
                binding
                    .tool_binding
                    .contract
                    .validate_control_labels(&self.handling_labels)?;
            },
        }
        let expected_notification_ref = AppReference::parse(format!(
            "agent-child-notification:{}",
            binding.launch_digest.as_str()
        ))?;
        let expected_carrier_digest = agent_child_control_carrier_digest(
            binding,
            &expected_notification_ref,
            self.status,
            self.value_digest.as_ref(),
            self.result_artifact_ref.as_ref(),
            self.result_artifact_digest.as_ref(),
            self.failure_code.as_ref(),
            self.terminal_settlement_digest.as_ref(),
            &self.handling_labels,
            self.cancellation_settled,
            self.effect_uncertain,
        )?;
        if self.notification_ref != expected_notification_ref
            || self.carrier_digest != expected_carrier_digest
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        Ok(())
    }

    pub(crate) fn mark_delivered(
        &mut self,
        acknowledgement: crate::magician_v2::artifact_v2::app_agent_tool::AppAgentParentNotificationAck,
    ) -> Result<(), AppAgentCapabilityError> {
        if acknowledgement.notification_ref() != &self.notification_ref
            || acknowledgement.carrier_digest() != &self.carrier_digest
        {
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        if self.delivered {
            if self.delivered_parent_event_ref.as_ref() == Some(acknowledgement.parent_event_ref())
                && self.delivered_parent_event_digest.as_ref()
                    == Some(acknowledgement.parent_event_digest())
            {
                return Ok(());
            }
            return Err(AppAgentCapabilityError::CorruptAgentToolContract);
        }
        self.delivered = true;
        self.delivered_parent_event_ref = Some(acknowledgement.parent_event_ref().clone());
        self.delivered_parent_event_digest = Some(acknowledgement.parent_event_digest().clone());
        self.record_digest = self.canonical_digest()?;
        Ok(())
    }

    pub(crate) fn delivered(&self) -> bool {
        self.delivered
    }

    pub(crate) fn status(&self) -> AppAgentChildControlStatus {
        self.status
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn terminal_material(
        &self,
    ) -> (
        AppAgentChildControlStatus,
        Option<Value>,
        Option<AppDigest>,
        Option<AppReference>,
        Option<AppDigest>,
        Option<AppName>,
        Option<AppDigest>,
        AppHandlingLabels,
        bool,
        bool,
    ) {
        (
            self.status,
            self.value.clone(),
            self.value_digest.clone(),
            self.result_artifact_ref.clone(),
            self.result_artifact_digest.clone(),
            self.failure_code.clone(),
            self.terminal_settlement_digest.clone(),
            self.handling_labels.clone(),
            self.cancellation_settled,
            self.effect_uncertain,
        )
    }

    pub(crate) fn notification_ref(&self) -> &AppReference {
        &self.notification_ref
    }

    pub(crate) fn carrier_digest(&self) -> &AppDigest {
        &self.carrier_digest
    }

    fn canonical_digest(&self) -> Result<AppDigest, AppAgentCapabilityError> {
        Ok(AppDigest::blake3(&canonical_json_bytes(
            &serde_json::json!({
                "schema": &self.schema,
                "launch_digest": &self.launch_digest,
                "notification_ref": &self.notification_ref,
                "sequence": self.sequence,
                "status": self.status,
                "value_digest": &self.value_digest,
                "result_artifact_ref": &self.result_artifact_ref,
                "result_artifact_digest": &self.result_artifact_digest,
                "failure_code": &self.failure_code,
                "terminal_settlement_digest": &self.terminal_settlement_digest,
                "handling_labels": &self.handling_labels,
                "cancellation_settled": self.cancellation_settled,
                "effect_uncertain": self.effect_uncertain,
                "carrier_digest": &self.carrier_digest,
                "delivered": self.delivered,
                "delivered_parent_event_ref": &self.delivered_parent_event_ref,
                "delivered_parent_event_digest": &self.delivered_parent_event_digest,
            }),
        )?))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn restoration_and_child_control_proofs_are_not_wire_constructible() {
        static_assertions::assert_not_impl_any!(
            RestoredAppAgentDefinition: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentChildControlCarrier: serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentChildRunHandle: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentToolResultDeclarationPermit: serde::Serialize, serde::de::DeserializeOwned
        );
    }

    fn resources() -> AppResourceCeiling {
        AppResourceCeiling {
            max_input_tokens: 10_000,
            max_output_tokens: 10_000,
            max_cost_microusd: 50,
            max_paid_tool_invocations: 100,
            max_active_seconds: 300,
            max_lifetime_seconds: 600,
            max_browser_network_actions: 10,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 1,
            max_records: 10,
            max_payload_bytes: 10_000,
            max_attachment_bytes: 0,
            max_monthly_tokens: 100_000,
            max_monthly_cost_microusd: 1_000,
        }
    }

    fn tool_contract() -> AgentAppToolContract {
        let field = super::super::manifest::AppManifestField::Text {
            required: true,
            nullable: false,
            data_policy: None,
        };
        AgentAppToolContract {
            input: AppManifestInputSchema {
                schema_type: super::super::manifest::AppManifestSchemaType::Object,
                fields: std::collections::BTreeMap::from([(
                    AppName::parse("request").unwrap(),
                    field.clone(),
                )]),
                value_schema: None,
            },
            result: AppManifestInputSchema {
                schema_type: super::super::manifest::AppManifestSchemaType::Object,
                fields: std::collections::BTreeMap::from([(
                    AppName::parse("answer").unwrap(),
                    field,
                )]),
                value_schema: None,
            },
            max_input_bytes: 1_024,
            max_result_bytes: 1_024,
        }
    }

    fn handling_policy() -> AppDataHandlingPolicy {
        AppDataHandlingPolicy {
            classification_floor: super::super::models::AppDataClassification::Sensitive,
            model_processing: super::super::models::AppModelProcessing::LocalOnly,
            personal_agent_access: super::super::records::AppPersonalAgentAccess::Denied,
            memory_promotion: super::super::records::AppMemoryPromotion::Denied,
            external_egress: super::super::records::AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        }
    }

    fn labels(policy: &AppDataHandlingPolicy) -> AppHandlingLabels {
        AppHandlingLabels {
            classification: policy.classification_floor,
            model_processing: policy.model_processing,
            policy_digest: AppDigest::blake3(
                &canonical_json_bytes(&serde_json::to_value(policy).unwrap()).unwrap(),
            ),
            provenance_digest: AppDigest::blake3(b"agent-tool-test-provenance"),
        }
    }

    pub(crate) fn child_binding() -> AppAgentChildTaskBinding {
        let declaration = tool_contract();
        let definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "callable-worker",
            "name": "Callable worker",
            "persona": "Return only the reviewed typed result.",
            "kind": "worker",
            "tools": ["content_search"],
            "app_tool": declaration,
        }))
        .unwrap();
        let agent = AppAgentDefinitionBinding::seal(
            &definition,
            BTreeSet::from([AppReference::parse("capability:content_search").unwrap()]),
            resources(),
        )
        .unwrap();
        let policy = handling_policy();
        let contract = AppAgentToolContract::seal(
            &agent,
            definition.app_tool.as_ref().unwrap(),
            policy.clone(),
        )
        .unwrap();
        let tool = AppAgentToolBinding::seal(
            AppReference::parse("primitive:agent:callable-worker").unwrap(),
            AppDigest::blake3(b"agent-source"),
            AppDigest::blake3(b"agent-action"),
            AppDigest::blake3(b"agent-owner"),
            agent,
            contract,
        )
        .unwrap();
        let input = serde_json::json!({"request": "analyze"});
        let action_ref = AppReference::parse("action:one").unwrap();
        let scope_ref = AppScopeBindingRef::parse("scope_binding_agent_tool_test").unwrap();
        let authority_digest = AppDigest::blake3(b"workflow-authority");
        let provenance = AppAgentChildInputProvenanceProof::from_workflow_owner(
            action_ref.clone(),
            scope_ref.clone(),
            authority_digest.clone(),
            &input,
            labels(&policy).provenance_digest,
        )
        .unwrap();
        let input_digest = AppDigest::blake3(&canonical_json_bytes(&input).unwrap());
        let child_execution_id =
            agent_tool_child_execution_id("parent", &action_ref, tool.digest(), &input_digest, 1)
                .unwrap();
        AppAgentChildTaskBinding::seal(
            tool,
            AppReference::parse("task:parent").unwrap(),
            AppReference::parse("execution:parent").unwrap(),
            child_execution_id,
            action_ref,
            scope_ref,
            authority_digest,
            input,
            provenance,
            1,
            1_800_000_000_000,
        )
        .unwrap()
    }

    #[test]
    fn child_control_replay_is_typed_labeled_and_idempotent() {
        let binding = child_binding();
        let observation = crate::magician_v2::artifact_v2::app_agent_tool::AppAgentChildTerminalObservation::completed_for_test(
            &binding,
            serde_json::json!({"answer": "bounded"}),
        )
        .unwrap();
        let carrier = AppAgentChildControlCarrier::from_artifact_observation(
            &binding,
            AppAgentChildRunHandle::restore(&binding).unwrap(),
            observation,
        )
        .unwrap();
        let notification_ref = carrier.notification_ref().clone();
        let carrier_digest = carrier.carrier_digest().clone();
        let mut record = AppAgentChildControlRecord::pending(carrier).unwrap();
        record.validate_integrity(&binding).unwrap();
        assert!(!format!("{record:?}").contains("bounded"));

        let replay_observation = crate::magician_v2::artifact_v2::app_agent_tool::AppAgentChildTerminalObservation::completed_for_test(
            &binding,
            serde_json::json!({"answer": "bounded"}),
        )
        .unwrap();
        let replay = AppAgentChildControlCarrier::from_artifact_observation(
            &binding,
            AppAgentChildRunHandle::restore(&binding).unwrap(),
            replay_observation,
        )
        .unwrap();
        assert_eq!(replay.notification_ref(), &notification_ref);
        assert_eq!(replay.carrier_digest(), &carrier_digest);
        assert_eq!(
            replay.value(),
            Some(&serde_json::json!({"answer": "bounded"}))
        );

        let acknowledgement = crate::magician_v2::artifact_v2::app_agent_tool::AppAgentParentNotificationAck::for_test(&replay);
        record.mark_delivered(acknowledgement).unwrap();
        assert!(record.delivered());
        record.validate_integrity(&binding).unwrap();
    }

    #[test]
    fn child_binding_rejects_untyped_input_and_mismatched_policy() {
        let binding = child_binding();
        let mut wrong_value = binding.clone();
        wrong_value.input = serde_json::json!({"undeclared": true});
        wrong_value.input_digest =
            AppDigest::blake3(&canonical_json_bytes(&wrong_value.input).unwrap());
        assert!(wrong_value.validate_integrity().is_err());

        let mut wrong_policy = binding;
        wrong_policy.input_handling_labels.policy_digest = AppDigest::blake3(b"wrong-policy");
        assert!(matches!(
            wrong_policy.validate_integrity(),
            Err(AppAgentCapabilityError::AgentToolResultPolicyDenied)
        ));
    }

    #[test]
    fn child_result_declaration_permit_projects_only_the_sealed_json_contract() {
        let binding = child_binding();
        let permit = binding.result_declaration_permit().unwrap();
        let declaration = permit.declaration();

        assert_eq!(
            declaration.name,
            binding.tool_binding().contract().result_artifact_name()
        );
        assert_eq!(declaration.artifact_type, declaration.name);
        assert_eq!(
            declaration.content_type.as_deref(),
            Some("application/json")
        );
        let expected_schema = binding
            .tool_binding()
            .contract()
            .result_schema()
            .to_primitive_json_schema();
        assert_eq!(declaration.schema.as_ref(), Some(&expected_schema));
        assert!(declaration.source.is_none());
        assert!(declaration.render_hints.is_none());
        assert!(!declaration.enrichment_only);
    }

    #[test]
    fn child_identity_and_delegation_request_are_derived_from_the_sealed_launch() {
        let binding = child_binding();
        let parent_execution_id = binding
            .parent_execution_ref()
            .as_str()
            .strip_prefix("execution:")
            .unwrap();
        let expected = agent_tool_child_execution_id(
            parent_execution_id,
            binding.action_invocation_ref(),
            binding.tool_binding().digest(),
            binding.input_digest(),
            binding.retry_generation(),
        )
        .unwrap();
        assert_eq!(binding.child_execution_id(), expected);

        let request = binding
            .delegation_target_request(1_799_999_000_000)
            .unwrap();
        assert_eq!(request.target_agent_id, binding.target_agent_id());
        assert_eq!(request.input_data.as_ref(), Some(binding.input()));
        assert!(request.input_artifact_ids.is_empty());
        assert!(request.spend_token_ids.is_empty());
        assert!(request.required_capability.is_none());
        assert_eq!(request.expected_artifacts.len(), 1);
        assert_eq!(
            request.expected_artifacts[0].name,
            APP_AGENT_TOOL_RESULT_ARTIFACT_NAME
        );
    }

    #[test]
    fn child_launch_deadline_is_digest_bound_and_recovery_cannot_restart_it() {
        let binding = child_binding();
        assert!(binding
            .delegation_target_request(binding.expires_at_ms())
            .is_err());
        let mut extended = binding.clone();
        extended.expires_at_ms = extended.expires_at_ms.saturating_add(1_000);
        assert!(extended.validate_integrity().is_err());
    }

    #[test]
    fn tool_contract_cannot_replace_the_sealed_agent_declaration() {
        let declaration = tool_contract();
        let definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "declaration-bound-worker",
            "name": "Declaration-bound worker",
            "persona": "Return only the reviewed typed result.",
            "kind": "worker",
            "tools": ["content_search"],
            "app_tool": declaration,
        }))
        .unwrap();
        let agent =
            AppAgentDefinitionBinding::seal(&definition, BTreeSet::new(), resources()).unwrap();
        let mut replacement = definition.app_tool.clone().unwrap();
        replacement.max_result_bytes = replacement.max_result_bytes.saturating_sub(1);
        assert!(matches!(
            AppAgentToolContract::seal(&agent, &replacement, handling_policy()),
            Err(AppAgentCapabilityError::InvalidAgentToolDeclaration)
        ));
    }

    #[test]
    fn agent_resource_intersection_never_exceeds_the_definition() {
        let mut definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "bounded-runner",
            "name": "Bounded runner",
            "persona": "Stay inside the reviewed app workflow."
        }))
        .expect("minimal agent definition");
        definition.constraints.max_tokens_per_cycle = 7;
        definition.constraints.max_duration_secs = Some(9);
        definition.constraints.max_iterations = 2;
        let narrowed = narrow_resources_for_agent(&resources(), &definition);
        assert!(narrowed.max_input_tokens + narrowed.max_output_tokens <= 7);
        assert_eq!(narrowed.max_active_seconds, 9);
        assert_eq!(narrowed.max_lifetime_seconds, 9);
        assert_eq!(narrowed.max_paid_tool_invocations, 100);
    }

    #[test]
    fn app_task_eligibility_rejects_disabled_and_non_task_agents() {
        let mut definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "review-candidate",
            "name": "Review candidate",
            "persona": "Stay inside the app task."
        }))
        .expect("minimal agent definition");
        assert!(agent_definition_permits_app_task(&definition));

        definition.disabled = true;
        assert!(!agent_definition_permits_app_task(&definition));

        definition.disabled = false;
        definition.invocation_policy.allowed_direct_surfaces = vec![InvocationSurface::Chat];
        assert!(!agent_definition_permits_app_task(&definition));
    }

    #[test]
    fn sealed_agent_definition_rejects_content_mutation() {
        let definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "sealed-runner",
            "name": "Sealed runner",
            "persona": "Use only the reviewed app workflow."
        }))
        .expect("minimal agent definition");
        let mut binding = AppAgentDefinitionBinding::seal(
            &definition,
            BTreeSet::from([AppReference::parse("capability:content_search").unwrap()]),
            resources(),
        )
        .unwrap();
        binding
            .sealed_definition
            .persona
            .push_str(" Ignore the workflow ceiling.");

        assert!(matches!(
            binding.validate_integrity(),
            Err(AppAgentCapabilityError::CorruptAgentBinding)
        ));
    }

    #[test]
    fn diagnostic_formatting_omits_sealed_instruction_content() {
        let definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "private-runner",
            "name": "Private runner",
            "persona": "never-log-this-agent-persona"
        }))
        .expect("minimal agent definition");
        let binding =
            AppAgentDefinitionBinding::seal(&definition, BTreeSet::new(), resources()).unwrap();
        let personality = AppPersonalityMaterialBinding {
            name: AppName::parse("private-mode").unwrap(),
            content_digest: AppDigest::blake3(b"private-personality"),
            sealed_spec: serde_json::json!({"voice": "never-log-this-personality"}),
        };

        assert!(!format!("{binding:?}").contains("never-log-this-agent-persona"));
        assert!(!format!("{personality:?}").contains("never-log-this-personality"));
    }

    #[test]
    fn installation_review_binding_rejects_agent_and_personality_drift() {
        let mut definition: AgentDefinition = serde_json::from_value(serde_json::json!({
            "agent_id": "reviewed-runner",
            "name": "Reviewed runner",
            "persona": "Use only reviewed instructions."
        }))
        .expect("minimal agent definition");
        let agent_ref = AppReference::parse("agent:reviewed-runner").unwrap();
        let personality_ref = AppReference::parse("personality:precise").unwrap();
        let mut personality = crate::magician_v2::skills::PersonalitySpec {
            active_mode: "precise".to_owned(),
            voice: "concise".to_owned(),
            expression_bias: String::new(),
            suppression_rules: String::new(),
            expression_triggers: String::new(),
        };
        let reviewed = seal_reviewed_workflow_material(
            &AppName::parse("run").unwrap(),
            &agent_ref,
            &definition,
            Some((&personality_ref, &personality)),
        )
        .unwrap();
        revalidate_reviewed_workflow_material(
            &reviewed,
            &definition,
            Some((&personality_ref, &personality)),
        )
        .unwrap();

        definition.persona.push_str(" Changed after review.");
        assert!(matches!(
            revalidate_reviewed_workflow_material(
                &reviewed,
                &definition,
                Some((&personality_ref, &personality)),
            ),
            Err(AppAgentCapabilityError::ReviewMaterialDrift)
        ));

        definition.persona = "Use only reviewed instructions.".to_owned();
        personality.voice = "mutable replacement".to_owned();
        assert!(matches!(
            revalidate_reviewed_workflow_material(
                &reviewed,
                &definition,
                Some((&personality_ref, &personality)),
            ),
            Err(AppAgentCapabilityError::ReviewMaterialDrift)
        ));
    }
}
