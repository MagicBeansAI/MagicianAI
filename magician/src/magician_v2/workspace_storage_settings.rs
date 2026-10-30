use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, LOCAL_FILE_WORKSPACE_PROVIDER_ID,
};

/// The retired notes-folder storage backend. Its store behaved exactly like
/// `local_file` rooted at its folder, so a config that still selects it reads
/// as `local_file` over that same folder: nothing moves.
const LEGACY_NOTES_FOLDER_PROVIDER_IDS: &[&str] = &["silverbullet_space", "silverbullet"];

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
    /// Read-only back-compat for the retired notes-folder backend (the YAML
    /// key is its old name). Kept only while `provider` still names that
    /// backend, where it keeps rooting the store at the same folder; under
    /// `local_file` it was inert and is dropped on normalize.
    #[serde(
        default,
        rename = "silverbullet",
        skip_serializing_if = "LegacyNotesFolderStorage::is_empty"
    )]
    pub legacy_notes_folder: LegacyNotesFolderStorage,
}

impl Default for WorkspaceStorageSettings {
    fn default() -> Self {
        Self {
            provider: default_workspace_storage_provider(),
            legacy_notes_folder: LegacyNotesFolderStorage::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LegacyNotesFolderStorage {
    /// Root the retired backend stored runtime state under. (A legacy
    /// `runtime_root` field is accepted and ignored, as before.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_path: Option<String>,
}

impl LegacyNotesFolderStorage {
    pub fn is_empty(&self) -> bool {
        self.space_path
            .as_deref()
            .is_none_or(|path| path.trim().is_empty())
    }
}

impl WorkspaceStorageSettings {
    /// A config that selected the retired notes-folder backend with a folder
    /// of its own: the store stays rooted in that folder.
    fn roots_in_legacy_notes_folder(&self) -> bool {
        LEGACY_NOTES_FOLDER_PROVIDER_IDS.contains(&self.provider.trim())
            && !self.legacy_notes_folder.is_empty()
    }
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
        let mut settings = settings.normalize();
        // Clients no longer send the legacy folder. A stored config that roots
        // the store in one is already `local_file` over that folder, so saving
        // plain `local_file` must not quietly re-root the deployment.
        let stored = self.load_settings();
        if stored.roots_in_legacy_notes_folder()
            && settings.provider == LOCAL_FILE_WORKSPACE_PROVIDER_ID
            && settings.legacy_notes_folder.is_empty()
        {
            settings = stored;
        }
        let path = self.settings_path();
        // Load-edit-save: load the full config, change its
        // `workspace_storage` section, and write back only the lines of the
        // values that changed (`apply_yaml_section`) — the owner's comments
        // and every other section stay byte for byte. Prefer the runtime-root
        // copy; fall back to the resolved config path (seed template) so a
        // fresh runtime root still materializes a complete config on first
        // write. The whole load-edit-write runs on the blocking pool under the
        // config file's lock (`config_file_lock`), shared with every other
        // settings writer, so two sections changed at once both land.
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
            let config = crate::config::load_magician_config_from_path(&base_path)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let original = std::fs::read_to_string(&base_path)?;
            let yaml = crate::magician_v2::runtime_settings::apply_yaml_section(
                &original,
                &["workspace_storage"],
                &config.workspace_storage,
                &write_settings,
            )?;
            if yaml == original && base_path == write_path {
                return Ok(());
            }
            if let Some(parent) = write_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
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
            LOCAL_FILE_WORKSPACE_PROVIDER_ID => {
                ArtifactV2Workspace::new(self.active_runtime_root(&envelope.settings))
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

    /// The runtime root: the bootstrap root, or the folder a config that still
    /// selects the retired notes-folder backend points at.
    fn active_runtime_root(&self, settings: &WorkspaceStorageSettings) -> PathBuf {
        if settings.roots_in_legacy_notes_folder() {
            if let Some(path) = settings.legacy_notes_folder.space_path.as_deref() {
                return expand_tilde(path.trim());
            }
        }
        self.bootstrap_root.clone()
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
        let active_runtime_root = self.active_runtime_root(&settings);
        let mut warnings = Vec::new();
        if normalize_workspace_provider_id(&settings.provider).is_none() {
            warnings.push(format!(
                "Unknown workspace storage provider `{}`; local_file will be used.",
                settings.provider
            ));
        }
        Ok(WorkspaceStorageSettingsEnvelope {
            settings_path: self.settings_path().display().to_string(),
            settings,
            resolved: ResolvedWorkspaceStorageSettings {
                provider,
                local_root: local_root.display().to_string(),
                active_runtime_root: active_runtime_root.display().to_string(),
            },
            warnings,
        })
    }
}

impl WorkspaceStorageSettings {
    pub fn normalize(mut self) -> Self {
        // The legacy id is kept only while it carries a folder: that pair is
        // what roots the store there, and rewriting the id to `local_file`
        // would make the next load treat the folder as inert. Without a folder
        // the retired backend was `local_file` at the runtime root.
        if self.roots_in_legacy_notes_folder() {
            self.provider = self.provider.trim().to_string();
            return self;
        }
        self.provider = normalize_workspace_provider_id(&self.provider)
            .unwrap_or_else(default_workspace_storage_provider);
        self.legacy_notes_folder = LegacyNotesFolderStorage::default();
        self
    }
}

fn normalize_workspace_provider_id(provider: &str) -> Option<String> {
    let provider = provider.trim();
    if provider == LOCAL_FILE_WORKSPACE_PROVIDER_ID
        || LEGACY_NOTES_FOLDER_PROVIDER_IDS.contains(&provider)
    {
        Some(LOCAL_FILE_WORKSPACE_PROVIDER_ID.to_string())
    } else {
        None
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

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saving_storage_settings_changes_only_its_own_line() {
        let temp = tempfile::tempdir().expect("tempdir");
        let contents = crate::magician_v2::runtime_settings::replace_top_level_yaml_block(
            &crate::config::shipped_repo_config_yaml(),
            "workspace_storage",
            "workspace_storage:\n  # owner: notes folder until the laptop move\n  provider: silverbullet_space  # notes\n",
        );
        std::fs::write(temp.path().join(CONFIG_FILE), &contents).expect("seed config");
        let store = WorkspaceStorageSettingsStore::new(temp.path());
        let mut settings = store.load_settings();
        settings.provider = LOCAL_FILE_WORKSPACE_PROVIDER_ID.to_string();
        store.save(settings).await.expect("save");
        let written = std::fs::read_to_string(temp.path().join(CONFIG_FILE)).expect("read");
        assert_eq!(
            written,
            contents.replace(
                "  provider: silverbullet_space  # notes\n",
                "  provider: local_file  # notes\n"
            )
        );
    }

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
    fn legacy_notes_folder_backend_reads_as_local_file_without_moving_anything() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("data");
        let store = WorkspaceStorageSettingsStore::new(&root);

        // The shipped shape: the retired backend, no folder of its own. It
        // always rooted at the runtime root, exactly like local_file.
        let yaml = "workspace_storage:\n  provider: silverbullet_space\n";
        let view: ConfigWorkspaceStorageView = serde_yaml::from_str(yaml).expect("parse");
        let envelope = store
            .envelope_for(view.workspace_storage.normalize())
            .expect("resolve");
        assert_eq!(envelope.resolved.provider, LOCAL_FILE_WORKSPACE_PROVIDER_ID);
        assert_eq!(
            envelope.resolved.active_runtime_root,
            root.display().to_string()
        );
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
        let workspace = store_with_config(&root, yaml)
            .resolve_workspace_sync()
            .expect("workspace");
        assert_eq!(
            workspace.file_provider_id(),
            LOCAL_FILE_WORKSPACE_PROVIDER_ID
        );
        assert_eq!(workspace.base_root(), root.as_path());

        // A legacy folder of its own keeps rooting the store there.
        let space = temp.path().join("MagicianNotes");
        let yaml = format!(
            "workspace_storage:\n  provider: silverbullet\n  silverbullet:\n    space_path: {}\n    runtime_root: .magician/runtime\n",
            space.display()
        );
        let view: ConfigWorkspaceStorageView = serde_yaml::from_str(&yaml).expect("parse");
        let settings = view.workspace_storage.normalize();
        let envelope = store.envelope_for(settings.clone()).expect("resolve");
        assert_eq!(envelope.resolved.provider, LOCAL_FILE_WORKSPACE_PROVIDER_ID);
        assert_eq!(
            envelope.resolved.active_runtime_root,
            space.display().to_string()
        );
        let workspace = store_with_config(&root, &yaml)
            .resolve_workspace_sync()
            .expect("workspace");
        assert_eq!(workspace.base_root(), space.as_path());
        // Round-trips under the config's own serializer: a reload after a
        // save still roots in the same folder.
        let reparsed: WorkspaceStorageSettings =
            serde_yaml::from_str(&serde_yaml::to_string(&settings).unwrap()).unwrap();
        assert_eq!(reparsed.clone().normalize(), settings);
        assert_eq!(
            store
                .envelope_for(reparsed.normalize())
                .expect("resolve")
                .resolved
                .active_runtime_root,
            space.display().to_string()
        );

        // Under local_file a stray legacy folder was inert and stays inert.
        let yaml = format!(
            "workspace_storage:\n  provider: local_file\n  silverbullet:\n    space_path: {}\n",
            space.display()
        );
        let view: ConfigWorkspaceStorageView = serde_yaml::from_str(&yaml).expect("parse");
        let envelope = store
            .envelope_for(view.workspace_storage.normalize())
            .expect("resolve");
        assert_eq!(
            envelope.resolved.active_runtime_root,
            root.display().to_string()
        );
    }

    fn store_with_config(root: &Path, yaml: &str) -> WorkspaceStorageSettingsStore {
        std::fs::create_dir_all(root).expect("root");
        std::fs::write(root.join(CONFIG_FILE), yaml).expect("config");
        WorkspaceStorageSettingsStore::new(root)
    }

    #[tokio::test]
    async fn saving_plain_local_file_keeps_a_legacy_folder_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("data");
        let space = temp.path().join("MagicianNotes");
        let contents = crate::magician_v2::runtime_settings::replace_top_level_yaml_block(
            &crate::config::shipped_repo_config_yaml(),
            "workspace_storage",
            &format!(
                "workspace_storage:\n  provider: silverbullet_space\n  silverbullet:\n    space_path: {}\n",
                space.display()
            ),
        );
        std::fs::create_dir_all(&root).expect("root");
        std::fs::write(root.join(CONFIG_FILE), &contents).expect("seed config");
        let store = WorkspaceStorageSettingsStore::new(&root);
        let envelope = store
            .save(WorkspaceStorageSettings::default())
            .await
            .expect("save");
        assert_eq!(
            envelope.resolved.active_runtime_root,
            space.display().to_string()
        );
        assert_eq!(
            store
                .load_envelope_sync()
                .expect("reload")
                .resolved
                .active_runtime_root,
            space.display().to_string()
        );
    }
}
