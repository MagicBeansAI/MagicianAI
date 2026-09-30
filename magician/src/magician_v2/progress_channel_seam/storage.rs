use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};
use anyhow::Context;

use crate::magician_v2::progress_channel_seam::types::{ExecutionLineage, Subscription};

#[derive(Debug, Clone)]
pub struct ProgressChannelStorage {
    base_root: PathBuf,
    workspace_layout: ArtifactV2Workspace,
}

impl ProgressChannelStorage {
    pub async fn new(base_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let base_root = base_root.into();
        Self::with_workspace_layout(ArtifactV2Workspace::new(&base_root)).await
    }

    pub async fn with_workspace_layout(
        workspace_layout: ArtifactV2Workspace,
    ) -> anyhow::Result<Self> {
        let base_root = workspace_layout.base_root().to_path_buf();
        workspace_layout
            .ensure_root()
            .await
            .with_context(|| format!("failed to create progress storage at {:?}", base_root))?;
        Ok(Self {
            base_root,
            workspace_layout,
        })
    }

    pub fn base_path(&self) -> &PathBuf {
        &self.base_root
    }

    pub async fn load_subscriptions(&self) -> anyhow::Result<HashMap<String, Subscription>> {
        let mut merged = HashMap::new();
        for (principal, workspace) in self.workspace_layout.list_scope_segments().await? {
            let path = self
                .workspace_layout
                .progress_subscriptions_path(&principal, &workspace);
            for (id, subscription) in read_json_map(&self.workspace_layout, &path).await? {
                merged.insert(id, subscription);
            }
        }
        Ok(merged)
    }

    pub async fn save_subscriptions(
        &self,
        subscriptions: &HashMap<String, Subscription>,
    ) -> anyhow::Result<()> {
        let mut by_scope: HashMap<(String, String), HashMap<String, Subscription>> = HashMap::new();
        for (id, subscription) in subscriptions {
            by_scope
                .entry((
                    subscription.principal.clone(),
                    subscription.workspace.clone(),
                ))
                .or_default()
                .insert(id.clone(), subscription.clone());
        }
        self.write_partitioned_maps(&by_scope, |principal, workspace| {
            self.workspace_layout
                .progress_subscriptions_path(principal, workspace)
        })
        .await
    }

    pub async fn load_lineage(&self) -> anyhow::Result<HashMap<String, ExecutionLineage>> {
        let mut merged = HashMap::new();
        for (principal, workspace) in self.workspace_layout.list_scope_segments().await? {
            let path = self
                .workspace_layout
                .progress_lineage_path(&principal, &workspace);
            for (id, lineage) in read_json_map(&self.workspace_layout, &path).await? {
                merged.insert(id, lineage);
            }
        }
        Ok(merged)
    }

    pub async fn save_lineage(
        &self,
        lineage: &HashMap<String, ExecutionLineage>,
    ) -> anyhow::Result<()> {
        let mut by_scope: HashMap<(String, String), HashMap<String, ExecutionLineage>> =
            HashMap::new();
        for (id, entry) in lineage {
            by_scope
                .entry((entry.principal.clone(), entry.workspace.clone()))
                .or_default()
                .insert(id.clone(), entry.clone());
        }
        self.write_partitioned_maps(&by_scope, |principal, workspace| {
            self.workspace_layout
                .progress_lineage_path(principal, workspace)
        })
        .await
    }

    async fn write_partitioned_maps<T, F>(
        &self,
        maps: &HashMap<(String, String), HashMap<String, T>>,
        path_for_scope: F,
    ) -> anyhow::Result<()>
    where
        T: serde::Serialize,
        F: Fn(&str, &str) -> PathBuf,
    {
        let mut existing_paths = HashSet::new();
        for (principal, workspace) in self.workspace_layout.list_scope_segments().await? {
            existing_paths.insert(path_for_scope(&principal, &workspace));
        }

        for ((principal, workspace), entries) in maps {
            let path = path_for_scope(principal, workspace);
            if entries.is_empty() {
                delete_if_exists(&self.workspace_layout, &path).await?;
                continue;
            }
            existing_paths.remove(&path);
            // Compact, not pretty. Both maps this writes — the lineage index
            // and the subscription set — are rebuildable machine-read indexes
            // reloaded through `serde_json`, which does not care about
            // indentation. On the live store the whitespace is 59 KB of every
            // lineage write and 38 KB of every subscription write, serialized
            // and fsynced for nothing. Parsing is format-agnostic, so files
            // already on disk in the pretty form keep loading unchanged.
            let bytes = serde_json::to_vec(entries)?;
            if crate::magician_v2::chat_owners::store_for_any_owner(&self.workspace_layout, &path)
                .is_some()
            {
                crate::magician_v2::chat_owners::persist_chat_file(
                    &self.workspace_layout,
                    &path,
                    &bytes,
                )
                .await
                .with_context(|| format!("failed to write progress map {:?}", path))?;
            } else {
                self.workspace_layout
                    .write_json_compact_atomic_path(&path, entries)
                    .await
                    .with_context(|| format!("failed to write progress map {:?}", path))?;
            }
        }

        for path in existing_paths {
            delete_if_exists(&self.workspace_layout, &path).await?;
        }

        Ok(())
    }
}

async fn delete_if_exists(
    workspace_layout: &ArtifactV2Workspace,
    path: &PathBuf,
) -> anyhow::Result<()> {
    match workspace_layout.remove_file_path(path).await {
        Ok(()) => Ok(()),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to delete {:?}", path)),
    }
}

async fn read_json_map<T>(
    workspace_layout: &ArtifactV2Workspace,
    path: &PathBuf,
) -> anyhow::Result<HashMap<String, T>>
where
    T: serde::de::DeserializeOwned,
{
    match workspace_layout.read_to_string_path(path).await {
        Ok(content) => Ok(serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {:?}", path))?),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(HashMap::new())
        },
        Err(error) => Err(error).with_context(|| format!("failed to read {:?}", path)),
    }
}
