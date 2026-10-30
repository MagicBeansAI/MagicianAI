use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use crate::magician_v2::agents::types::AgentKind;
use crate::magician_v2::{
    agents::{AgentDefinitionStore, AgentMemoryResolver, AgentRuntime, DefinitionRecord},
    artifact_v2::{models::TaskRecord, service::ArtifactV2Service},
    user_requests::UserRequestService,
};

use super::{scope::HarnessScope, ProgramLoader};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessRuntimeStatus {
    pub enabled: bool,
    pub paused: bool,
    pub configured_paused: Option<bool>,
    pub env_override: Option<bool>,
    pub effective_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
}

pub fn parse_harness_pause_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Shared by admission/control and read-only inspection. Reading cannot
/// unpause the harness or recover/refresh its scheduler.
pub fn load_harness_runtime_status() -> HarnessRuntimeStatus {
    let env_override = std::env::var("MAGICIAN_HARNESS_PAUSED")
        .ok()
        .as_deref()
        .and_then(parse_harness_pause_override);
    let (configured_paused, config_error) =
        match crate::config::load_magician_config_from_path(&crate::config::magician_config_path())
        {
            Ok(config) => (Some(config.harness.paused), None),
            Err(error) => (None, Some(error.to_string())),
        };
    let (paused, effective_source) = match (env_override, configured_paused) {
        (Some(value), _) => (value, "environment"),
        (_, Some(value)) => (value, "config"),
        _ => (true, "config_error"),
    };
    HarnessRuntimeStatus {
        enabled: !paused,
        paused,
        configured_paused,
        env_override,
        effective_source: effective_source.into(),
        config_error,
    }
}

pub type ScopedPausedAgentsIndex =
    Arc<RwLock<HashMap<(String, String), Arc<RwLock<HashSet<String>>>>>>;

#[derive(Clone)]
pub struct HarnessServices {
    pub definition_store: Arc<AgentDefinitionStore>,
    pub memory_resolver: AgentMemoryResolver,
    pub artifact_service: Arc<ArtifactV2Service>,
    pub runtime: Arc<AgentRuntime>,
    pub user_request_service: Option<Arc<UserRequestService>>,
    pub control_gate: Option<Arc<Mutex<()>>>,
    pub scoped_paused_agents: Option<ScopedPausedAgentsIndex>,
}

#[derive(Debug, Clone)]
pub struct HarnessExecutionContext {
    pub principal: String,
    pub workspace: String,
    pub owner_record: DefinitionRecord,
    pub goal_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub task: Option<TaskRecord>,
    pub scope: HarnessScope,
}

impl HarnessExecutionContext {
    pub async fn from_params(
        services: &HarnessServices,
        params: &HashMap<String, Value>,
    ) -> Result<Self, String> {
        Self::resolve(services, params, false).await
    }

    /// Only the metadata-only branches of inspect_agent/system_status may use
    /// this context without an autonomous harness. It grants no control scope.
    pub async fn for_runtime_inspection(
        services: &HarnessServices,
        params: &HashMap<String, Value>,
        tool: &str,
    ) -> Result<Self, String> {
        let context = Self::resolve(services, params, true).await?;
        // These tools are also advertised as universal backend utilities.
        // Broader metadata access requires a definition grant, not merely
        // discovery in that catalog; denial rules continue to win.
        if context.owner_record.definition.harness.is_none()
            && !context
                .owner_record
                .definition
                .resolved_tools(&[tool.to_string()])
                .iter()
                .any(|granted| granted == tool)
        {
            return Err(format!(
                "runtime metadata inspection requires a `{tool}` tool grant"
            ));
        }
        Ok(context)
    }

    async fn resolve(
        services: &HarnessServices,
        params: &HashMap<String, Value>,
        allow_runtime_inspection: bool,
    ) -> Result<Self, String> {
        let principal = required_string(
            params,
            "__principal",
            "harness tools require a scoped principal",
        )?;
        let workspace = required_string(
            params,
            "__workspace",
            "harness tools require a scoped workspace",
        )?;
        let owner_agent_id = required_string(
            params,
            "__agent_id",
            "harness tools require an active agent id",
        )?;

        let scoped_store = services.definition_store.for_scope(&principal, &workspace);
        let owner_record = scoped_store
            .get_definition(&owner_agent_id)
            .await
            .map_err(|error| format!("failed to load harness owner `{owner_agent_id}`: {error}"))?
            .ok_or_else(|| format!("harness owner `{owner_agent_id}` was not found in scope"))?;

        if owner_record.definition.kind != AgentKind::Personal {
            return Err(format!(
                "harness tools are only available to personal agents; `{owner_agent_id}` is {:?}",
                owner_record.definition.kind
            ));
        }
        if owner_record.definition.harness.is_none() && !allow_runtime_inspection {
            return Err(format!(
                "agent `{owner_agent_id}` is not harness-enabled in this scope"
            ));
        }

        let task_id = params
            .get("__task_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let execution_id = params
            .get("__execution_id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let task = if let Some(task_id) = task_id.as_deref() {
            match services
                .artifact_service
                .get_task_by_id(task_id)
                .await
                .map_err(|error| {
                    format!("failed to load current harness task `{task_id}`: {error}")
                })? {
                Some((scope, task)) => {
                    if scope.principal() != principal || scope.workspace() != workspace {
                        return Err(format!(
                            "current task `{task_id}` resolved outside the active scope `{principal}/{workspace}`"
                        ));
                    }
                    Some(task)
                },
                None => None,
            }
        } else {
            None
        };

        let goal_id = task.as_ref().and_then(|task| task.manifest.goal_id.clone());
        let scope =
            HarnessScope::resolve(&scoped_store, &owner_record.definition, goal_id.as_deref())
                .await?;

        Ok(Self {
            principal,
            workspace,
            owner_record,
            goal_id,
            task_id,
            execution_id,
            task,
            scope,
        })
    }

    pub fn scoped_definition_store(&self, services: &HarnessServices) -> AgentDefinitionStore {
        services
            .definition_store
            .for_scope(&self.principal, &self.workspace)
    }

    pub fn memory_service(
        &self,
        services: &HarnessServices,
    ) -> crate::magician_v2::agents::AgentMemoryService {
        services
            .memory_resolver
            .resolve_for_scope(&self.principal, &self.workspace)
            .expect("harness context is always scoped to a valid principal/workspace")
    }

    pub async fn load_program(
        &self,
        services: &HarnessServices,
    ) -> Result<Option<crate::magician_v2::harness::LoadedProgram>, String> {
        ProgramLoader::new(services.artifact_service.workspace().clone())
            .load_for_goal(
                &self.principal,
                &self.workspace,
                &self.owner_record.definition,
                self.goal_id.as_deref(),
            )
            .await
    }

    pub async fn load_program_runtime_state(
        &self,
        services: &HarnessServices,
        loaded: &crate::magician_v2::harness::LoadedProgram,
    ) -> Result<crate::magician_v2::harness::ProgramRuntimeState, String> {
        ProgramLoader::new(services.artifact_service.workspace().clone())
            .load_or_create_runtime_state(
                &self.principal,
                &self.workspace,
                loaded,
                self.goal_id.as_deref(),
            )
            .await
    }
}

fn required_string(
    params: &HashMap<String, Value>,
    key: &str,
    message: &str,
) -> Result<String, String> {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| message.to_string())
}
