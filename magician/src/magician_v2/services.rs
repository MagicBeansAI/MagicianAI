//! Dependency registry passed into Magician V2 components.

use std::path::PathBuf;
use std::sync::Arc;

use crate::magician_v2::{
    artifact_v2::{workspace::ArtifactV2Workspace, CapabilityScopePaths},
    ask_loop::SessionManager,
    elicitation::{
        AutonomousDiscoveryService, ParameterInferenceService, SensibleElicitationOrchestrator,
    },
    execution::{CapabilityPackDefinition, MagicutorClient},
    prompts::PromptManager,
    storage::V2ConversationStore,
};
use runtime_core::{RuntimeConfig, SemanticSearch, ToolCatalog, ToolMatching};

pub struct MagicianV2Services {
    pub runtime_config: Arc<dyn RuntimeConfig>,
    pub conversation_store: Arc<dyn V2ConversationStore>,
    pub prompt_manager: Arc<PromptManager>,
    pub tool_catalog: Arc<dyn ToolCatalog>,
    pub tool_matching: Arc<dyn ToolMatching>,
    pub semantic_search: Option<Arc<dyn SemanticSearch>>,
    pub magicutor_client: Arc<MagicutorClient>,

    /// Pre-loaded capability pack definitions (loaded once from disk, consumed by orchestrator).
    pub capability_pack_defs: Vec<CapabilityPackDefinition>,

    /// Authoritative repo root derived from cli.config.parent().
    /// Used by DelegationShellProvider for MAGICIAN_ROOT env var injection.
    pub repo_root: PathBuf,
    /// Scope paths used by the startup-scoped capability registry/catalog.
    pub capability_scope_paths: Option<CapabilityScopePaths>,
    /// Resolved runtime workspace provider selected from API settings.
    ///
    /// When present, orchestration storage must use this layout directly instead
    /// of reinterpreting `RuntimeConfig::storage_path()` as a legacy project
    /// root. This keeps SilverBullet/custom workspace providers from gaining an
    /// accidental nested `magician_data_v3` child.
    pub workspace_layout: Option<ArtifactV2Workspace>,

    // Progressive elicitation services (optional for backward compatibility)
    pub sensible_orchestrator: Option<Arc<dyn SensibleElicitationOrchestrator>>,
    pub parameter_inference: Option<Arc<dyn ParameterInferenceService>>,
    pub autonomous_discovery: Option<Arc<dyn AutonomousDiscoveryService>>,
    pub session_manager: Arc<dyn SessionManager>,
}

impl MagicianV2Services {
    pub fn new(
        runtime_config: Arc<dyn RuntimeConfig>,
        conversation_store: Arc<dyn V2ConversationStore>,
        prompt_manager: Arc<PromptManager>,
        tool_catalog: Arc<dyn ToolCatalog>,
        tool_matching: Arc<dyn ToolMatching>,
        semantic_search: Option<Arc<dyn SemanticSearch>>,
        session_manager: Arc<dyn SessionManager>,
        magicutor_client: Arc<MagicutorClient>,
        capability_pack_defs: Vec<CapabilityPackDefinition>,
        repo_root: PathBuf,
        capability_scope_paths: Option<CapabilityScopePaths>,
    ) -> Self {
        Self {
            runtime_config,
            conversation_store,
            prompt_manager,
            tool_catalog,
            tool_matching,
            semantic_search,
            magicutor_client,
            capability_pack_defs,
            repo_root,
            capability_scope_paths,
            workspace_layout: None,
            // Progressive elicitation services default to None for backward compatibility
            sensible_orchestrator: None,
            parameter_inference: None,
            autonomous_discovery: None,
            session_manager,
        }
    }

    pub fn with_workspace_layout(mut self, workspace_layout: ArtifactV2Workspace) -> Self {
        self.workspace_layout = Some(workspace_layout);
        self
    }

    /// Builder method to set progressive elicitation services
    pub fn with_progressive_elicitation(
        mut self,
        sensible_orchestrator: Arc<dyn SensibleElicitationOrchestrator>,
        parameter_inference: Arc<dyn ParameterInferenceService>,
        autonomous_discovery: Arc<dyn AutonomousDiscoveryService>,
    ) -> Self {
        self.sensible_orchestrator = Some(sensible_orchestrator);
        self.parameter_inference = Some(parameter_inference);
        self.autonomous_discovery = Some(autonomous_discovery);
        self
    }
}
