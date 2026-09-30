use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use serde_json::Value;
use tokio::sync::{Mutex, RwLock};

use crate::magician_v2::agents::types::AgentKind;
use crate::magician_v2::{
    agents::{AgentDefinitionStore, AgentMemoryResolver, AgentRuntime, DefinitionRecord},
    artifact_v2::{models::TaskRecord, service::ArtifactV2Service},
    user_requests::UserRequestService,
};

use super::{scope::HarnessScope, ProgramLoader};

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
        if owner_record.definition.harness.is_none() {
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
