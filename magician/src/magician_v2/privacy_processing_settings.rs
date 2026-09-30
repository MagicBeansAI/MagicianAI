//! The `privacy.processing` settings store: the UI-writable surface of the
//! processing-locality switch (`privacy.processing.mode`).
//!
//! Modeled directly on [`crate::magician_v2::workspace_storage_settings`]: a
//! light serde view over one section of the runtime `magician-config.yaml`
//! (no whole-`MagicianConfig` validity required to read), a locked
//! load-edit-save durable write of that section, and an envelope that reports
//! the effective routing — which profile each switchable operation resolved
//! to under the current mode — so the UI shows real routing rather than
//! echoing the request. The caller (settings API) triggers the existing
//! config-reload path after a successful save; the mode is live without a
//! restart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{PrivacyProcessingSettings, PrivacySettings};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

const CONFIG_FILE: &str = "magician-config.yaml";

/// Light view that pulls just `privacy` out of the big config without
/// requiring the whole `MagicianConfig` to be valid (no `deny_unknown_fields`).
#[derive(Debug, Default, Deserialize)]
struct ConfigPrivacyView {
    #[serde(default)]
    privacy: PrivacySettings,
}

/// Light view of the router config for routing resolution — same philosophy:
/// the envelope must stay readable even when an unrelated section fails full
/// validation.
#[derive(Debug, Default, Deserialize)]
struct ConfigRouterView {
    #[serde(default)]
    llm: ConfigLlmView,
}

#[derive(Debug, Default, Deserialize)]
struct ConfigLlmView {
    #[serde(default)]
    router: Option<magicllm::config::LLMRouterConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivacyOperationRouting {
    /// Operation key carrying a `when_cloud` arm (the switchable set).
    pub operation: String,
    /// Profile the operation resolves to under the effective mode.
    pub profile: String,
    /// Provider kind of that profile (`ollama`, `openai`, …).
    pub provider: String,
    /// True when the `when_cloud` arm is serving this operation.
    pub switched: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivacyProcessingSettingsEnvelope {
    pub settings_path: String,
    /// Effective mode: `local` (on-device model serves local-eligible
    /// operations) or `cloud` (`when_cloud` counterparts serve them).
    pub mode: String,
    /// Per-operation routing the current mode produces (operations with a
    /// `when_cloud` arm only). Embeddings never appear here: they stay local
    /// in both modes and no `embed_*` operation carries a cloud arm.
    pub routing: Vec<PrivacyOperationRouting>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PrivacyProcessingSettingsStore {
    bootstrap_root: PathBuf,
}

impl PrivacyProcessingSettingsStore {
    pub fn new<P: AsRef<Path>>(bootstrap_root: P) -> Self {
        Self {
            bootstrap_root: bootstrap_root.as_ref().to_path_buf(),
        }
    }

    pub fn settings_path(&self) -> PathBuf {
        self.bootstrap_root.join(CONFIG_FILE)
    }

    fn load_settings(&self) -> PrivacyProcessingSettings {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(text) => serde_yaml::from_str::<ConfigPrivacyView>(&text)
                .map(|view| view.privacy.processing)
                .unwrap_or_default(),
            Err(_) => PrivacyProcessingSettings::default(),
        }
    }

    pub async fn load_envelope(&self) -> std::io::Result<PrivacyProcessingSettingsEnvelope> {
        self.envelope_for(self.load_settings())
    }

    /// Durable write of the `privacy:` section, under the config-file lock
    /// shared with every other whole-file config writer.
    pub async fn save(
        &self,
        settings: PrivacyProcessingSettings,
    ) -> std::io::Result<PrivacyProcessingSettingsEnvelope> {
        let write_settings = settings;
        let envelope_settings = write_settings.clone();
        let write_path = self.settings_path();
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
            config.privacy.processing = write_settings.clone();
            if let Some(parent) = write_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let yaml = serde_yaml::to_string(&config).map_err(std::io::Error::other)?;
            write_bytes_durably_sync(&write_path, yaml.as_bytes())
        })
        .await
        .map_err(|error| std::io::Error::other(format!("config write task failed: {error}")))??;
        self.envelope_for(envelope_settings)
    }

    fn envelope_for(
        &self,
        settings: PrivacyProcessingSettings,
    ) -> std::io::Result<PrivacyProcessingSettingsEnvelope> {
        let mode = settings.mode;
        let mut warnings = Vec::new();
        let mut routing = Vec::new();
        // Resolve real routing from the runtime config through a light
        // router view: a config whose unrelated sections fail full
        // validation still deserves a readable envelope.
        let router_view = std::fs::read_to_string(self.settings_path())
            .ok()
            .and_then(|text| serde_yaml::from_str::<ConfigRouterView>(&text).ok());
        match router_view {
            Some(view) => {
                if let Some(router) = view.llm.router.as_ref() {
                    let mut operations: Vec<&String> = router
                        .operation_mapping
                        .keys()
                        .filter(|operation| {
                            matches!(
                                router.operation_mapping.get(*operation),
                                Some(magicllm::config::OperationProfileSelector::Conditional {
                                    when_cloud: Some(_),
                                    ..
                                })
                            )
                        })
                        .collect();
                    operations.sort();
                    if let Some(offender) = operations.iter().find(|op| op.starts_with("embed_")) {
                        warnings.push(format!(
                            "embed operation `{offender}` carries a when_cloud arm; embeddings \
                             must stay local in both modes (remove the arm)"
                        ));
                    }
                    for operation in operations {
                        let selector = &router.operation_mapping[operation];
                        let profile_name = selector
                            .profile_for_locality(&magicllm::config::RequestShape::NONE, mode);
                        let switched = selector.selects_cloud_arm(mode);
                        let provider = router
                            .profiles
                            .get(profile_name)
                            .map(|profile| profile.provider.as_str().to_string())
                            .unwrap_or_else(|| {
                                warnings.push(format!(
                                    "operation `{operation}` resolves to missing profile `{profile_name}`"
                                ));
                                "unknown".to_string()
                            });
                        routing.push(PrivacyOperationRouting {
                            operation: operation.clone(),
                            profile: profile_name.to_string(),
                            provider,
                            switched,
                        });
                    }
                } else {
                    warnings.push(
                        "config has no llm.router section; routing is unavailable".to_string(),
                    );
                }
            },
            None => warnings.push(
                "runtime config could not be read or parsed for routing resolution".to_string(),
            ),
        }
        Ok(PrivacyProcessingSettingsEnvelope {
            settings_path: self.settings_path().display().to_string(),
            mode: match mode {
                magicllm::ProcessingLocality::Local => "local".to_string(),
                magicllm::ProcessingLocality::Cloud => "cloud".to_string(),
            },
            routing,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_section_defaults_to_local() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = PrivacyProcessingSettingsStore::new(temp.path());
        let settings = store.load_settings();
        assert_eq!(settings.mode, magicllm::ProcessingLocality::Local);
    }

    #[tokio::test]
    async fn envelope_reads_mode_and_switchable_routing_from_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join(CONFIG_FILE),
            "llm:\n  router:\n    profiles:\n      local-p:\n        provider: ollama\n        model: m\n      remote-p:\n        provider: openai\n        model: r\n    operation_mapping:\n      channel_ingest_distill:\n        default: local-p\n        when_cloud: remote-p\n    default_profile: local-p\n",
        )
        .expect("seed config");

        let store = PrivacyProcessingSettingsStore::new(temp.path());
        let envelope = store.load_envelope().await.expect("envelope");
        assert_eq!(envelope.mode, "local");
        assert_eq!(envelope.routing.len(), 1);
        assert_eq!(envelope.routing[0].operation, "channel_ingest_distill");
        assert_eq!(envelope.routing[0].profile, "local-p");
        assert_eq!(envelope.routing[0].provider, "ollama");
        assert!(!envelope.routing[0].switched);
    }
}
