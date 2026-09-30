use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, LOCAL_FILE_WORKSPACE_PROVIDER_ID, SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID,
};

/// The storage-provider selector is folded into magician-config.yaml (the
/// `workspace_storage:` section) so the runtime root carries a single config
/// file. The store reads + mutates that section in `<bootstrap_root>/CONFIG_FILE`.
const CONFIG_FILE: &str = "magician-config.yaml";

/// Light view that pulls just `workspace_storage` out of the big config without
/// requiring the whole MagicianConfig to be valid (no `deny_unknown_fields`).
#[derive(Debug, Default, Deserialize)]
struct ConfigWorkspaceStorageView {
    #[serde(default)]
    workspace_storage: WorkspaceStorageSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceStorageSettings {
    #[serde(default = "default_workspace_storage_provider")]
    pub provider: String,
    #[serde(default)]
    pub silverbullet: WorkspaceStorageSilverBulletSettings,
}

impl Default for WorkspaceStorageSettings {
    fn default() -> Self {
        Self {
            provider: default_workspace_storage_provider(),
            silverbullet: WorkspaceStorageSilverBulletSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceStorageSilverBulletSettings {
    /// Where the SilverBullet Space lives. Optional: defaults to the runtime root
    /// (`MAGICIAN_ROOT_DIR` / bootstrap root) so a deployment never hard-codes its
    /// machine path in the seed. (A legacy `runtime_root` field in older settings
    /// files is accepted-and-ignored — the provider no longer re-roots.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStorageSettingsEnvelope {
    pub settings_path: String,
    pub settings: WorkspaceStorageSettings,
    pub resolved: ResolvedWorkspaceStorageSettings,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedWorkspaceStorageSettings {
    pub provider: String,
    pub local_root: String,
    pub silverbullet_space_path: String,
    pub active_runtime_root: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceStorageSettingsStore {
    bootstrap_root: PathBuf,
    seed_root: Option<PathBuf>,
}

impl WorkspaceStorageSettingsStore {
    pub fn new<P: AsRef<Path>>(bootstrap_root: P) -> Self {
        Self {
            bootstrap_root: bootstrap_root.as_ref().to_path_buf(),
            seed_root: None,
        }
    }

    /// Attach the read-only seed root (repo `magician_data_v3`) so the resolved
    /// workspace reads bootstrap templates from the seed while keeping runtime
    /// state under the (possibly different) runtime store root.
    pub fn with_seed_root<P: AsRef<Path>>(mut self, seed_root: P) -> Self {
        self.seed_root = Some(seed_root.as_ref().to_path_buf());
        self
    }

    pub async fn load_envelope(&self) -> std::io::Result<WorkspaceStorageSettingsEnvelope> {
        self.envelope_for(self.load_settings())
    }

    pub fn load_envelope_sync(&self) -> std::io::Result<WorkspaceStorageSettingsEnvelope> {
        self.envelope_for(self.load_settings())
    }

    /// Read just the `workspace_storage:` section from the runtime-root config.
    /// Missing file or absent section → defaults (local_file). A small, rare,
    /// boot/API-time read, so it stays synchronous.
    fn load_settings(&self) -> WorkspaceStorageSettings {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(text) => serde_yaml::from_str::<ConfigWorkspaceStorageView>(&text)
                .map(|view| view.workspace_storage.normalize())
                .unwrap_or_default(),
            Err(_) => WorkspaceStorageSettings::default(),
        }
    }

    pub async fn save(
        &self,
        settings: WorkspaceStorageSettings,
    ) -> std::io::Result<WorkspaceStorageSettingsEnvelope> {
        let settings = settings.normalize();
        let path = self.settings_path();
        // Load-edit-save: deserialize the full config, replace its
        // `workspace_storage` section, re-serialize. Prefer the runtime-root
        // copy; fall back to the resolved config path (seed template) so a fresh
        // runtime root still materializes a complete config on first write.
        // The whole load-edit-write runs on the blocking pool under the config
        // file's lock, shared with `write_top_level_yaml_block`.
        //
        // `magician-config.yaml` has four writers — this one plus three
        // top-level block replacements — and every one rewrites the entire file
        // from its own snapshot. Unlocked, a settings change and, say, a media
        // or vibedev_deploy block edit each read the same file and each wrote
        // their whole version back; the later write took the file and the other
        // section reverted with no error.
        //
        // Blocking rather than async so both writers can share one
        // `std::sync::Mutex`: the alternative was a tokio mutex the synchronous
        // block writer cannot take. It also gets a full YAML parse of the
        // runtime config off the reactor, which it had no business doing.
        let write_settings = settings.clone();
        let write_path = path.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let lock = crate::magician_v2::runtime_settings::config_file_lock(&write_path);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

            let base_path = if write_path.exists() {
                write_path.clone()
            } else {
                crate::config::magician_config_path()
            };
            let mut config = crate::config::load_magician_config_from_path(&base_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            config.workspace_storage = write_settings;
            if let Some(parent) = write_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let yaml = serde_yaml::to_string(&config).map_err(std::io::Error::other)?;
            // Durable publish: this is the whole runtime root's config file, and
            // a torn write of it fails `load_magician_config_from_path` on the
            // next boot rather than costing one setting.
            write_bytes_durably_sync(&write_path, yaml.as_bytes())
        })
        .await
        .map_err(|error| std::io::Error::other(format!("config write task failed: {error}")))??;
        self.envelope_for(settings)
    }

    pub fn resolve_workspace_sync(&self) -> Result<ArtifactV2Workspace> {
        let envelope = self
            .load_envelope_sync()
            .context("loading workspace storage settings")?;
        let workspace = match envelope.resolved.provider.as_str() {
            LOCAL_FILE_WORKSPACE_PROVIDER_ID => ArtifactV2Workspace::new(&self.bootstrap_root),
            SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID => {
                ArtifactV2Workspace::with_silverbullet_space_provider(
                    &envelope.resolved.silverbullet_space_path,
                )
                .context("configuring SilverBullet workspace provider from API settings")?
            },
            other => {
                anyhow::bail!("unsupported workspace storage provider `{other}` in API settings")
            },
        };
        Ok(match &self.seed_root {
            Some(seed) => workspace.with_seed_root(seed),
            None => workspace,
        })
    }

    pub fn settings_path(&self) -> PathBuf {
        // Folded into magician-config.yaml at the runtime-root top level — the
        // store reads + mutates the `workspace_storage:` section there.
        self.bootstrap_root.join(CONFIG_FILE)
    }

    fn envelope_for(
        &self,
        settings: WorkspaceStorageSettings,
    ) -> std::io::Result<WorkspaceStorageSettingsEnvelope> {
        let provider = normalize_workspace_provider_id(&settings.provider)
            .unwrap_or_else(default_workspace_storage_provider);
        let local_root = self.bootstrap_root.clone();
        // space_path defaults to the runtime root (bootstrap_root) so the seed never
        // hard-codes a machine path. The SB provider roots directly at it (no
        // `.magician/runtime` redirect), so the active runtime root IS the space path.
        let silverbullet_space_path = settings
            .silverbullet
            .space_path
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(|| local_root.clone());
        let active_runtime_root = if provider == SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID {
            silverbullet_space_path.clone()
        } else {
            local_root.clone()
        };
        let mut warnings = Vec::new();
        if normalize_workspace_provider_id(&settings.provider).is_none() {
            warnings.push(format!(
                "Unknown workspace storage provider `{}`; local_file will be used.",
                settings.provider
            ));
        }
        if provider == SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID {
            warnings.push(
                "silverbullet_space workspace storage is experimental and stores canonical runtime state under the notes Space. Prefer local_file for canonical runtime storage and use the Notes Provider for SilverBullet projections unless you are intentionally testing this backend."
                    .to_string(),
            );
            if runtime_root_has_data(&local_root) && !runtime_root_has_data(&active_runtime_root) {
                warnings.push(format!(
                    "Active SilverBullet runtime root `{}` appears empty while local runtime root `{}` contains data. Workspace storage settings do not migrate task, chat, execution, memory, or artifact state automatically.",
                    active_runtime_root.display(),
                    local_root.display()
                ));
            }
        }
        Ok(WorkspaceStorageSettingsEnvelope {
            settings_path: self.settings_path().display().to_string(),
            settings,
            resolved: ResolvedWorkspaceStorageSettings {
                provider,
                local_root: local_root.display().to_string(),
                silverbullet_space_path: silverbullet_space_path.display().to_string(),
                active_runtime_root: active_runtime_root.display().to_string(),
            },
            warnings,
        })
    }
}

impl WorkspaceStorageSettings {
    pub fn normalize(mut self) -> Self {
        self.provider = normalize_workspace_provider_id(&self.provider)
            .unwrap_or_else(default_workspace_storage_provider);
        self
    }
}

fn normalize_workspace_provider_id(provider: &str) -> Option<String> {
    match provider.trim() {
        LOCAL_FILE_WORKSPACE_PROVIDER_ID => Some(LOCAL_FILE_WORKSPACE_PROVIDER_ID.to_string()),
        SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID | "silverbullet" => {
            Some(SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID.to_string())
        },
        _ => None,
    }
}

fn default_workspace_storage_provider() -> String {
    LOCAL_FILE_WORKSPACE_PROVIDER_ID.to_string()
}

fn expand_tilde(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(value)
}

fn runtime_root_has_data(root: &Path) -> bool {
    directory_has_entries(&root.join("scopes")) || directory_has_entries(&root.join("system"))
}

fn directory_has_entries(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn default_settings_resolve_local_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = WorkspaceStorageSettingsStore::new(temp.path());
        let envelope = store.load_envelope_sync().expect("load defaults");
        assert_eq!(envelope.resolved.provider, LOCAL_FILE_WORKSPACE_PROVIDER_ID);
        assert_eq!(
            envelope.resolved.active_runtime_root,
            temp.path().display().to_string()
        );
    }

    #[test]
    fn silverbullet_settings_resolve_to_space_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let space = temp.path().join("MagicianNotes");
        let store = WorkspaceStorageSettingsStore::new(temp.path().join("data"));
        let envelope = store
            .envelope_for(WorkspaceStorageSettings {
                provider: "silverbullet".to_string(),
                silverbullet: WorkspaceStorageSilverBulletSettings {
                    space_path: Some(space.display().to_string()),
                },
            })
            .expect("resolve envelope");
        assert_eq!(
            envelope.resolved.provider,
            SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID
        );
        assert_eq!(
            envelope.resolved.active_runtime_root,
            space.display().to_string()
        );
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning
                    .contains("silverbullet_space workspace storage is experimental")),
            "silverbullet workspace storage should surface an experimental warning"
        );
    }

    #[test]
    fn warns_when_switching_to_empty_silverbullet_runtime_from_populated_local_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("scopes/anonymous/default"))
            .expect("local runtime data");
        let space = temp.path().join("MagicianNotes");
        let store = WorkspaceStorageSettingsStore::new(temp.path());
        let envelope = store
            .envelope_for(WorkspaceStorageSettings {
                provider: SILVERBULLET_SPACE_WORKSPACE_PROVIDER_ID.to_string(),
                silverbullet: WorkspaceStorageSilverBulletSettings {
                    space_path: Some(space.display().to_string()),
                },
            })
            .expect("resolve envelope");

        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("appears empty while local runtime root")),
            "switching to a cold provider root should not be silent"
        );
    }
}
