use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::{
    BotAuthSnapshot, BotAuthStartResponse, BotAuthStateSnapshot, BotConfigStore,
    BotConfigStoreError, BotLogLine, BotManager, BotManagerError, BotStatusSnapshot,
};
use magician::config::BotProcessConfig;
use magician::magician_v2::artifact_v2::service::ArtifactV2Error;
use magician::magician_v2::artifact_v2::{
    workspace::{DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE},
    CapabilityWorkspaceError, CapabilityWorkspaceManager,
};

#[derive(Debug, thiserror::Error)]
pub enum ScopedBotRuntimeError {
    #[error(transparent)]
    ConfigStore(#[from] BotConfigStoreError),
    #[error(transparent)]
    Bot(#[from] BotManagerError),
    #[error(transparent)]
    CapabilityWorkspace(#[from] CapabilityWorkspaceError),
    #[error(transparent)]
    ArtifactWorkspace(#[from] ArtifactV2Error),
}

#[derive(Debug, Clone)]
pub struct ScopedBotRuntime {
    capability_workspace: Arc<CapabilityWorkspaceManager>,
    managers: Arc<RwLock<BTreeMap<(String, String), Arc<BotManager>>>>,
    stores: Arc<RwLock<BTreeMap<(String, String), Arc<BotConfigStore>>>>,
}

impl ScopedBotRuntime {
    pub fn new(capability_workspace: Arc<CapabilityWorkspaceManager>) -> Self {
        Self {
            capability_workspace,
            managers: Arc::new(RwLock::new(BTreeMap::new())),
            stores: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub async fn resolve_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<(Arc<BotManager>, Arc<BotConfigStore>), ScopedBotRuntimeError> {
        let key = (principal.to_string(), workspace.to_string());
        if let (Some(manager), Some(store)) = (
            self.managers.read().await.get(&key).cloned(),
            self.stores.read().await.get(&key).cloned(),
        ) {
            return Ok((manager, store));
        }

        self.capability_workspace
            .materialize_scope(principal, workspace)?;
        let store = Arc::new(BotConfigStore::new(
            self.capability_workspace
                .scoped_bot_configs_path(principal, workspace),
        ));
        let configs = store.load_bots().await?;
        let manager = Arc::new(BotManager::new_scoped(
            configs,
            self.capability_workspace.scope_paths(principal, workspace),
        ));

        let mut managers = self.managers.write().await;
        let manager = managers
            .entry(key.clone())
            .or_insert_with(|| Arc::clone(&manager))
            .clone();
        drop(managers);

        let mut stores = self.stores.write().await;
        let store = stores
            .entry(key)
            .or_insert_with(|| Arc::clone(&store))
            .clone();
        drop(stores);

        Ok((manager, store))
    }

    pub async fn start_enabled_existing_scopes(&self) -> Result<(), ScopedBotRuntimeError> {
        let mut scopes = self
            .capability_workspace
            .workspace_layout()
            .list_scope_segments_sync()?;
        if !scopes.iter().any(|(principal, workspace)| {
            principal == DEFAULT_SCOPE_PRINCIPAL && workspace == DEFAULT_SCOPE_WORKSPACE
        }) {
            scopes.push((
                DEFAULT_SCOPE_PRINCIPAL.to_string(),
                DEFAULT_SCOPE_WORKSPACE.to_string(),
            ));
        }
        scopes.sort();
        scopes.dedup();
        for (principal, workspace) in scopes {
            let (manager, _) = self.resolve_scope(&principal, &workspace).await?;
            manager.start_enabled().await;
        }
        Ok(())
    }

    pub async fn shutdown_all(&self) {
        let managers: Vec<Arc<BotManager>> = self.managers.read().await.values().cloned().collect();
        for manager in managers {
            manager.shutdown().await;
        }
    }

    pub async fn list(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<BotStatusSnapshot>, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        Ok(manager.list().await)
    }

    pub async fn start(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<BotStatusSnapshot, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.start(name).await.map_err(Into::into)
    }

    pub async fn stop(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<BotStatusSnapshot, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.stop(name).await.map_err(Into::into)
    }

    pub async fn restart(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<BotStatusSnapshot, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.restart(name).await.map_err(Into::into)
    }

    pub async fn logs(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
        limit: usize,
    ) -> Result<Vec<BotLogLine>, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.logs(name, limit).await.map_err(Into::into)
    }

    pub async fn qr_code_path(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<Option<std::path::PathBuf>, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.qr_code_path(name).await.map_err(Into::into)
    }

    pub async fn config(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<BotProcessConfig, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.config(name).await.map_err(Into::into)
    }

    pub async fn auth_info(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<BotAuthSnapshot, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.auth_info(name).await.map_err(Into::into)
    }

    pub async fn list_auth(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<BotAuthSnapshot>, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        Ok(manager.list_auth().await)
    }

    pub async fn auth_state(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<BotAuthStateSnapshot, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        Ok(manager.auth_state().await)
    }

    pub async fn start_auth(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
    ) -> Result<BotAuthStartResponse, ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager.start_auth(name).await.map_err(Into::into)
    }

    pub async fn submit_auth_input(
        &self,
        principal: &str,
        workspace: &str,
        name: &str,
        input: &str,
    ) -> Result<(), ScopedBotRuntimeError> {
        let (manager, _) = self.resolve_scope(principal, workspace).await?;
        manager
            .submit_auth_input(name, input)
            .await
            .map_err(Into::into)
    }
}
