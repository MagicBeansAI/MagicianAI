//! Agent-level resource bundle for compiled-pack providers.
//!
//! Phase 0.8c. The previous architecture (`AgentBackend` trait,
//! formerly `ChatNativeBridge`) required a hand-written trait method
//! per agent tool that needed access to ChatService-owned state
//! (memory resolver, agent definition store, workspace, chat
//! session). Adding a new compiled-pack tool meant editing the trait,
//! editing the impl on ChatService, AND keeping the autonomous path
//! in sync.
//!
//! This struct centralises those resources in a single neutral type
//! that every compiled provider can hold an `Arc<AgentResources>`
//! reference to. Each provider's `execute()` method does its work
//! using these resources directly — no callback into ChatService, no
//! per-tool trait method.
//!
//! Migration plan (this struct lives alongside the existing
//! `AgentBackend` trait during the transition):
//! - **0.8c-1 (this slice):** introduce `AgentResources`, pilot
//!   migrate `switch_personality`.
//! - **0.8c-2:** migrate the remaining memory / introspection /
//!   workspace-artifact tools off `AgentBackend`.
//! - **0.8c-3:** implement `activate_skill` / `deactivate_skill` as
//!   compiled packs using `AgentResources` + an agent-scope memory
//!   tier for state.
//! - **0.8c-4:** delete the now-empty `AgentBackend` trait.
//!
//! Dynamic skills (CLI-template skills in the workspace `skills/`
//! folder) are NOT touched by this work — they were already fully
//! dynamic, dispatched by name through the existing capability-pack
//! registry. The 0.8c migration only applies to compiled packs that
//! happened to need ChatService-owned resources.

use std::sync::{Arc, RwLock};

use runtime_core::FileSandboxConfig;

use crate::config::MagicianConfig;
use crate::magician_v2::agents::definition_store::AgentDefinitionStore;
use crate::magician_v2::agents::memory::AgentMemoryResolver;
use crate::magician_v2::artifact_v2::service::ArtifactV2Service;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use crate::magician_v2::secrets::SecretStoreResolver;
use crate::magician_v2::user_requests::UserRequestService;

/// Resources every compiled-pack provider may need to do its job.
///
/// Construction happens once at boot (in `bin/magician.rs`) and the
/// `Arc<AgentResources>` is plumbed through `ScopedCapabilityResolver`
/// to every per-scope `CapabilityRegistry` it builds.
///
/// Fields are added as new tool migrations need them. Keep this lean
/// — anything that's chat-session-scoped belongs on the
/// `session_store` slot below rather than as a top-level field, so
/// pure autonomous executions can leave session-state hooks `None`.
#[derive(Clone)]
pub struct AgentResources {
    /// Reloadable process configuration snapshot. Compiled handlers use this for
    /// shared operator policy that is not session-scoped, such as the coding
    /// profile catalog.
    pub magician_config: Arc<RwLock<MagicianConfig>>,

    /// Read/write memory tiers (user, agent, agent_goal,
    /// environment_knowledge, …).
    pub memory_resolver: Arc<AgentMemoryResolver>,

    /// Load agent definitions by (principal, workspace, agent_id).
    /// Needed for tools that touch the agent's declared `tools:`,
    /// `memory_tiers`, etc.
    pub agent_definition_store: Arc<AgentDefinitionStore>,

    /// Workspace layout — paths for skills root, artifacts, etc.
    /// `scope_skills_root(principal, workspace)` returns the per-scope
    /// skills directory used by skill resolvers.
    pub artifact_workspace: ArtifactV2Workspace,

    /// The full Artifact V2 service — task ops, artifact listings,
    /// scheduled-task introspection. Optional because some test /
    /// control-plane boots construct `AgentResources` before
    /// `ArtifactV2Service` exists; handlers that require it should
    /// return a clear error when it's `None`.
    pub artifact_v2_service: Option<Arc<ArtifactV2Service>>,

    /// Best-effort runtime event publisher. Compiled handlers that run
    /// long-lived work outside the normal agentic loop use this to surface
    /// live progress to task cards and operator pages.
    pub event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,

    /// The shared operation-aware model router. Model-calling compiled
    /// providers must use this path so queue policy, provider-attempt identity,
    /// usage/cost telemetry and cancellation behavior remain the same as other
    /// Magician LLM calls. Optional only for minimal/control-plane test boots.
    pub operation_llm_router: Option<Arc<OperationLlmRouter>>,

    /// Scope-aware secret store resolver. Compiled handlers that need
    /// runtime-owned credentials use this to source values from the
    /// keychain-backed provisioned secret store without exposing them to the
    /// model.
    pub secret_store_resolver: Option<Arc<SecretStoreResolver>>,

    /// Late-bound scope-aware content acquisition resolver. The resolver is
    /// constructed from the fully wired capability registry after this bundle
    /// is created, so compiled `content_search` / `content_read` handlers read
    /// it through a shared slot.
    pub content_acquisition_resolver:
        Arc<RwLock<Option<Arc<crate::magician_v2::content_sources::ContentAcquisitionResolver>>>>,

    /// File sandbox config for filesystem-touching compiled tools
    /// (`read_file`, `write_file`, etc.). Resolved at boot from the
    /// scope's runtime config so paths-allowed / read-only-mode /
    /// allow-delete are honoured. Same config is used by the
    /// existing struct-based `FileCapabilityProvider` (the `files`
    /// multi-action pack) — handlers reuse it for consistency.
    pub file_sandbox: FileSandboxConfig,

    /// Process-level flat-loop tool index, populated at registry-build time
    /// (`build_compiled_registry`). `OnceLock` so it can be set after
    /// `AgentResources` is constructed — the index needs the fully-resolved
    /// pack list, which isn't available until the registry is built — without
    /// `&mut self`. `tool_search` reads it to fetch deferred-tool schemas on
    /// demand. `None`/unset means flat loop is inactive for this scope (every
    /// tool is loaded eagerly), and `tool_search` returns its inactive shape.
    pub tool_index:
        Arc<std::sync::OnceLock<Arc<crate::magician_v2::execution::flat_loop::ToolIndex>>>,

    /// Surface a non-blocking owner request (the chat-facing owner-relay
    /// tools: `notify_owner` + the envoy `ask_owner` / `propose_meeting` /
    /// `request_owner_action`). Optional because some test / control-plane
    /// boots construct `AgentResources` before the request service exists;
    /// handlers that need it return a clear error when it's `None`.
    pub user_request_service: Option<Arc<UserRequestService>>,

    /// The agent runtime — lets compiled handlers trigger goal cycles
    /// (officer mobilization after an approved CEO decomposition). Optional
    /// because minimal boots construct `AgentResources` before the runtime
    /// exists; handlers degrade to "next scheduled cycle" when `None`.
    pub agent_runtime: Option<Arc<crate::magician_v2::agents::runtime::AgentRuntime>>,
}

impl std::fmt::Debug for AgentResources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentResources")
            .field("magician_config", &"<MagicianConfig>")
            .field("agent_runtime", &self.agent_runtime.is_some())
            .field("memory_resolver", &"<AgentMemoryResolver>")
            .field("agent_definition_store", &"<AgentDefinitionStore>")
            .field("artifact_workspace", &"<ArtifactV2Workspace>")
            .field(
                "artifact_v2_service",
                &self.artifact_v2_service.as_ref().map(|_| "<configured>"),
            )
            .field(
                "event_broadcaster",
                &self.event_broadcaster.as_ref().map(|_| "<configured>"),
            )
            .field(
                "operation_llm_router",
                &self.operation_llm_router.as_ref().map(|_| "<configured>"),
            )
            .field(
                "secret_store_resolver",
                &self.secret_store_resolver.as_ref().map(|_| "<configured>"),
            )
            .field(
                "content_acquisition_resolver",
                &self
                    .content_acquisition_resolver
                    .read()
                    .map(|resolver| resolver.is_some())
                    .unwrap_or(false),
            )
            .field("file_sandbox", &"<FileSandboxConfig>")
            .field(
                "tool_index",
                &self
                    .tool_index
                    .get()
                    .map(|_| "<installed>")
                    .unwrap_or("<empty>"),
            )
            .field(
                "user_request_service",
                &self.user_request_service.as_ref().map(|_| "<configured>"),
            )
            .finish()
    }
}

impl AgentResources {
    pub fn magician_config_snapshot(&self) -> MagicianConfig {
        self.magician_config
            .read()
            .map(|config| config.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// Read only the Town Square policy for the social-post handler. Avoids a
    /// deep clone of the process config on an otherwise tiny write path.
    pub fn social_config_snapshot(&self) -> crate::config::SocialConfig {
        self.magician_config
            .read()
            .map(|config| config.social.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().social.clone())
    }

    pub fn content_acquisition_resolver(
        &self,
    ) -> Option<Arc<crate::magician_v2::content_sources::ContentAcquisitionResolver>> {
        self.content_acquisition_resolver
            .read()
            .map(|resolver| resolver.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// Read just the configured VibeDev coding lead (`coding.lead_agent_id`) under the config
    /// lock, cloning only the single `Option<String>` instead of deep-cloning the whole
    /// `MagicianConfig` (this is on the task-create path). `None` = no lead configured.
    pub fn coding_lead_agent_id(&self) -> Option<String> {
        self.magician_config
            .read()
            .map(|config| config.coding.lead_agent_id.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().coding.lead_agent_id.clone())
    }

    /// Whether `contribute_to_project` writes directly into the project repo (default) or stages
    /// behind diff-approval HITL — read under the config lock.
    pub fn coding_contribute_direct(&self) -> bool {
        self.magician_config
            .read()
            .map(|config| config.coding.contribute_direct)
            .unwrap_or_else(|poisoned| poisoned.into_inner().coding.contribute_direct)
    }

    /// Update fields consumed through the live config snapshot while retaining
    /// policy whose owners were constructed only at boot.
    ///
    /// Town Square has three consumers: the worker, HTTP API, and this compiled
    /// handler bundle. The first two intentionally capture policy at startup;
    /// replacing `social` here alone would split authorization until restart.
    pub fn update_magician_config_snapshot(&self, config: MagicianConfig) {
        match self.magician_config.write() {
            Ok(mut guard) => {
                let merged = config_with_boot_bound_social(&guard, config);
                *guard = merged;
            },
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                let merged = config_with_boot_bound_social(&guard, config);
                *guard = merged;
            },
        }
    }

    /// Convenience: the per-scope skills root the agent's procedure
    /// playbooks + personality presets are loaded from.
    pub fn scope_skills_root(&self, principal: &str, workspace: &str) -> std::path::PathBuf {
        self.artifact_workspace
            .scope_skills_root(principal, workspace)
    }

    /// Install the flat-loop tool index (idempotent — first writer wins, so
    /// re-entrant registry builds are safe).
    pub fn set_tool_index(&self, index: Arc<crate::magician_v2::execution::flat_loop::ToolIndex>) {
        let _ = self.tool_index.set(index);
    }

    /// The installed tool index, if any. `None` means flat loop is inactive
    /// for this scope.
    pub fn tool_index(&self) -> Option<Arc<crate::magician_v2::execution::flat_loop::ToolIndex>> {
        self.tool_index.get().cloned()
    }
}

fn config_with_boot_bound_social(
    current: &MagicianConfig,
    mut incoming: MagicianConfig,
) -> MagicianConfig {
    incoming.social = current.social.clone();
    incoming
}

#[cfg(any(test, feature = "test-fixtures"))]
mod live_config_tests {
    use super::config_with_boot_bound_social;
    use crate::config::MagicianConfig;

    #[test]
    fn live_reload_preserves_the_boot_bound_social_policy() {
        let mut current = MagicianConfig::default();
        current.social.enabled = true;
        current.social.scopes[0].principal = "boot-owner".to_string();

        let mut incoming = MagicianConfig::default();
        incoming.social.enabled = false;
        incoming.social.scopes[0].principal = "reloaded-owner".to_string();

        let merged = config_with_boot_bound_social(&current, incoming);
        assert!(merged.social.enabled);
        assert_eq!(merged.social.scopes[0].principal, "boot-owner");
    }
}
