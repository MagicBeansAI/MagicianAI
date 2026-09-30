use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::magician_v2::agents::memory_tiers::MemoryTierDefinition;
use crate::magician_v2::agents::{
    types::{AgentInvocationPolicy, ApprovalRule, InvocationSurface},
    TrustPolicyEnforcer,
};
use crate::magician_v2::query_analysis::operation_llm_router::OperationRoutingOverrides;

use super::delegation_dispatch::DelegationTarget;
use super::types::PromptIdentityContext;

#[derive(Debug, Clone)]
pub struct OwnerExecutionProfile {
    /// Exact scoped definition that produced this security profile. Decision
    /// catalogs resolve their immutable snapshot from this value rather than
    /// reconstructing policy from partial context fields.
    pub definition: crate::magician_v2::agents::AgentDefinition,
    pub agent_id: String,
    pub trust_level: Option<String>,
    pub trust_policies_path: Option<PathBuf>,
    pub preloaded_trust_enforcer: Option<Arc<TrustPolicyEnforcer>>,
    pub approval_rules: Vec<ApprovalRule>,
    pub llm_model_override: Option<String>,
    pub llm_routing_overrides: Option<OperationRoutingOverrides>,
    pub prompt_identity: Option<PromptIdentityContext>,
    pub delegation_targets: Vec<DelegationTarget>,
    pub merged_agent_tools: Vec<runtime_core::ToolInfo>,
    /// Complete whole-tool exclusion/deny set for this owner. Replaced on
    /// handover together with every other security field.
    pub denied_capability_names: Vec<String>,
    pub denied_tool_params:
        std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>>,
    pub invocation_policy: AgentInvocationPolicy,
    pub allowed_action_types: Option<Vec<String>>,
    pub max_delegation_depth: u8,
    pub delegation_timeout_secs: u64,
    pub tier_definitions: Vec<MemoryTierDefinition>,
    /// Procedure skills the agent has declared in its `tools:` allowlist
    /// (resolved through `skills::agent_procedure_skill_catalog`).
    /// Drives whether the outer-loop `activate_skill` / `deactivate_skill`
    /// tools are exposed, and the `## AVAILABLE PROCEDURE SKILLS` block
    /// the LLM sees in its prompt. Empty when the agent has no skills
    /// allowlisted.
    pub available_procedure_skills: Vec<(String, String)>,
}

#[async_trait]
pub trait OwnershipRuntime: Send + Sync {
    /// Read the shared runtime FSM at a phase boundary. A process-local
    /// cancellation token cannot observe a cancellation committed by another
    /// process; stateless holders use this read to stop before more work.
    async fn execution_is_durably_cancelled(&self, execution_id: &str) -> Result<bool, String>;

    /// Cross-process generation of the currently active stateless execution
    /// epoch. `None` means the runtime FSM is not Executing, so it cannot admit
    /// or consume new operator prompt input.
    async fn active_stateless_control_generation(
        &self,
        _execution_id: &str,
    ) -> Result<Option<String>, String> {
        Err("durable stateless control generation is unavailable".to_owned())
    }

    /// The last HMAC-sealed stateless control generation, even after an
    /// external pause/cancel closed API admission. Terminal cleanup uses this
    /// to supersede rows accepted immediately before that control transition.
    async fn stateless_control_generation(
        &self,
        _execution_id: &str,
    ) -> Result<Option<String>, String> {
        Err("durable stateless control generation is unavailable".to_owned())
    }

    async fn load_owner_execution_profile(
        &self,
        agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<OwnerExecutionProfile, String>;

    async fn update_execution_owner_snapshot(
        &self,
        thread_id: &str,
        active_owner_agent_id: &str,
        owner_stack: &[String],
    ) -> Result<(), String>;

    /// Re-resolve an exact source -> target owner transition against the live
    /// scoped definition set. Used at resume and post-wait launch boundaries;
    /// persisted invocation metadata alone is never sufficient authority.
    async fn validate_owner_transition(
        &self,
        source_agent_id: &str,
        target_agent_id: &str,
        surface: InvocationSurface,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<(), String>;
}
