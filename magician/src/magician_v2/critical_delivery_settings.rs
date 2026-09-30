//! The `hitl.critical_delivery` settings store: the UI-writable surface of
//! critical-request delivery (secure HITL plan §6.1, P5).
//!
//! Modelled on [`crate::magician_v2::privacy_processing_settings`]: a light
//! serde view over one section of the runtime `magician-config.yaml`, a
//! locked load-edit-save durable write of that section, and an envelope
//! that reports the effective state — which enabled channels actually have
//! an owner identity, masked owner addresses, the public origin the secure
//! link uses — so the UI shows what a real alert would do rather than
//! echoing the request. The caller (settings API) reloads the live config
//! and hands the coordinator its new policy.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{EnvoyConfig, HitlConfig, HitlCriticalDeliverySettings, MobileAccessConfig};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

const CONFIG_FILE: &str = "magician-config.yaml";

/// Light view: `hitl`, `envoy` and `mobile_access` only.
#[derive(Debug, Default, Deserialize)]
struct ConfigView {
    #[serde(default)]
    hitl: HitlConfig,
    #[serde(default)]
    envoy: EnvoyConfig,
    #[serde(default)]
    mobile_access: MobileAccessConfig,
}

/// One enabled channel as the surface shows it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChannelEnvelope {
    pub channel_type: String,
    /// Owner addresses for the channel, masked to their last two characters.
    pub owner_addresses: Vec<String>,
    /// `false` when the channel is enabled but no owner identity exists:
    /// alerts to it are recorded `unavailable`, never sent.
    pub has_owner: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CriticalDeliverySettingsEnvelope {
    pub settings_path: String,
    pub settings: HitlCriticalDeliverySettings,
    pub channels: Vec<ChannelEnvelope>,
    /// Channel types with an owner identity that are not enabled — what the
    /// owner could add.
    pub available_channels: Vec<String>,
    /// Whether the secure link can be built (`mobile_access.public_origin`
    /// or its env fallback).
    pub public_origin_configured: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CriticalDeliverySettingsStore {
    bootstrap_root: PathBuf,
}

impl CriticalDeliverySettingsStore {
    pub fn new<P: AsRef<Path>>(bootstrap_root: P) -> Self {
        Self {
            bootstrap_root: bootstrap_root.as_ref().to_path_buf(),
        }
    }

    pub fn settings_path(&self) -> PathBuf {
        self.bootstrap_root.join(CONFIG_FILE)
    }

    fn load_view(&self) -> ConfigView {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(text) => serde_yaml::from_str::<ConfigView>(&text).unwrap_or_default(),
            Err(_) => ConfigView::default(),
        }
    }

    pub fn load_settings(&self) -> HitlCriticalDeliverySettings {
        self.load_view().hitl.critical_delivery
    }

    pub async fn load_envelope(&self) -> std::io::Result<CriticalDeliverySettingsEnvelope> {
        Ok(self.envelope_for(self.load_view()))
    }

    /// Reject a shape the coordinator could not honour, before any write.
    pub fn validate(settings: &HitlCriticalDeliverySettings) -> Result<(), String> {
        for channel in &settings.enabled_channels {
            let channel = channel.trim();
            if channel.is_empty()
                || channel.len() > 64
                || !channel
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(format!("`{channel}` is not a channel type name"));
            }
        }
        if settings.staged_fallback_secs == 0 || settings.staged_fallback_secs > 3_600 {
            return Err("staged_fallback_secs must be between 1 and 3600".to_string());
        }
        if let Some(quiet) = &settings.quiet_hours {
            for (name, value) in [("start", &quiet.start), ("end", &quiet.end)] {
                let ok = value
                    .trim()
                    .split_once(':')
                    .and_then(|(h, m)| Some((h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)))
                    .is_some_and(|(h, m)| h < 24 && m < 60);
                if !ok {
                    return Err(format!("quiet_hours.{name} must be HH:MM"));
                }
            }
            if quiet.timezone.trim().parse::<chrono_tz::Tz>().is_err() {
                return Err(format!(
                    "quiet_hours.timezone `{}` is not an IANA timezone",
                    quiet.timezone
                ));
            }
        }
        Ok(())
    }

    /// Durable write of the `hitl.critical_delivery` section, under the
    /// config-file lock shared with every other whole-file config writer.
    pub async fn save(
        &self,
        settings: HitlCriticalDeliverySettings,
    ) -> std::io::Result<CriticalDeliverySettingsEnvelope> {
        Self::validate(&settings).map_err(std::io::Error::other)?;
        let write_settings = settings;
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
            let mut written = write_settings;
            Self::carry_forward_operator_fields(&mut written, &config.hitl.critical_delivery);
            config.hitl.critical_delivery = written;
            if let Some(parent) = write_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let yaml = serde_yaml::to_string(&config).map_err(std::io::Error::other)?;
            write_bytes_durably_sync(&write_path, yaml.as_bytes())
        })
        .await
        .map_err(|error| std::io::Error::other(format!("config write task failed: {error}")))??;
        Ok(self.envelope_for(self.load_view()))
    }

    /// Fields of the section that belong to the deployment, not to the owner,
    /// carried forward from disk when the writer did not name them.
    ///
    /// `save` replaces the whole `hitl.critical_delivery` section, and the
    /// settings panel has no field for the link origin, so its payload always
    /// omits it and `#[serde(default)]` fills `None`. Without this the first
    /// save of Critical alerts would silently drop a configured origin and put
    /// the alert back to "open Magician → Attention". An operator clears it by
    /// editing the YAML.
    fn carry_forward_operator_fields(
        written: &mut HitlCriticalDeliverySettings,
        on_disk: &HitlCriticalDeliverySettings,
    ) {
        if written.owner_ui_origin.is_none() {
            written.owner_ui_origin = on_disk.owner_ui_origin.clone();
        }
    }

    fn envelope_for(&self, view: ConfigView) -> CriticalDeliverySettingsEnvelope {
        let settings = view.hitl.critical_delivery;
        let mut warnings = Vec::new();
        let channels: Vec<ChannelEnvelope> = settings
            .enabled_channels
            .iter()
            .map(|channel| {
                let addresses = view.envoy.owner_identities_for(channel.trim());
                if addresses.is_empty() {
                    warnings.push(format!(
                        "`{channel}` is enabled but has no owner identity (envoy.owner_identities / owner_identity_envs); alerts to it are recorded unavailable"
                    ));
                }
                ChannelEnvelope {
                    channel_type: channel.trim().to_ascii_lowercase(),
                    has_owner: !addresses.is_empty(),
                    owner_addresses: addresses.iter().map(|a| mask(a)).collect(),
                }
            })
            .collect();
        let mut available_channels: Vec<String> = view
            .envoy
            .owner_identities
            .keys()
            .chain(view.envoy.owner_identity_envs.keys())
            .map(|channel| channel.trim().to_ascii_lowercase())
            .filter(|channel| !view.envoy.owner_identities_for(channel).is_empty())
            .filter(|channel| !channels.iter().any(|c| &c.channel_type == channel))
            .collect();
        available_channels.sort();
        available_channels.dedup();
        let public_origin_configured = settings.owner_link_origin(&view.mobile_access).is_some();
        if !public_origin_configured {
            warnings.push(
                "no link origin is configured (hitl.critical_delivery.owner_ui_origin, or mobile_access.public_origin); alerts will say to open Magician → Attention instead of linking the request".to_string(),
            );
        }
        CriticalDeliverySettingsEnvelope {
            settings_path: self.settings_path().display().to_string(),
            settings,
            channels,
            available_channels,
            public_origin_configured,
            warnings,
        }
    }
}

/// An owner address as the surface shows it: its last two characters.
pub fn mask(address: &str) -> String {
    let tail: String = address
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuietHoursSettings;

    #[test]
    fn validation_refuses_shapes_the_coordinator_could_not_honour() {
        let mut settings = HitlCriticalDeliverySettings::default();
        assert!(CriticalDeliverySettingsStore::validate(&settings).is_ok());
        settings.enabled_channels = vec!["tele gram".into()];
        assert!(CriticalDeliverySettingsStore::validate(&settings).is_err());
        settings.enabled_channels = vec!["telegram".into()];
        settings.staged_fallback_secs = 0;
        assert!(CriticalDeliverySettingsStore::validate(&settings).is_err());
        settings.staged_fallback_secs = 45;
        settings.quiet_hours = Some(QuietHoursSettings {
            start: "22:00".into(),
            end: "7:30".into(),
            timezone: "Asia/Kolkata".into(),
            interrupt_for_time_bound: true,
        });
        assert!(CriticalDeliverySettingsStore::validate(&settings).is_ok());
        settings.quiet_hours = Some(QuietHoursSettings {
            start: "25:00".into(),
            end: "07:00".into(),
            timezone: "UTC".into(),
            interrupt_for_time_bound: true,
        });
        assert!(CriticalDeliverySettingsStore::validate(&settings).is_err());
        settings.quiet_hours = Some(QuietHoursSettings {
            start: "22:00".into(),
            end: "07:00".into(),
            timezone: "Mars/Olympus".into(),
            interrupt_for_time_bound: true,
        });
        assert!(CriticalDeliverySettingsStore::validate(&settings).is_err());
    }

    #[test]
    fn the_envelope_masks_owner_addresses_and_names_channels_without_one() {
        let dir = std::env::temp_dir().join(format!("critical-delivery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(CONFIG_FILE),
            "hitl:\n  critical_delivery:\n    enabled_channels: [telegram, kapso]\nenvoy:\n  owner_identities:\n    telegram: ['777001']\n    whatsapp: ['1234']\n",
        )
        .unwrap();
        let store = CriticalDeliverySettingsStore::new(&dir);
        let view = store.load_view();
        let envelope = store.envelope_for(view);
        assert_eq!(envelope.channels.len(), 2);
        assert_eq!(
            envelope.channels[0].owner_addresses,
            vec!["…01".to_string()]
        );
        assert!(envelope.channels[0].has_owner);
        assert!(!envelope.channels[1].has_owner);
        assert_eq!(envelope.available_channels, vec!["whatsapp".to_string()]);
        assert!(envelope
            .warnings
            .iter()
            .any(|w| w.contains("`kapso` is enabled but has no owner identity")));
        assert!(!serde_json::to_string(&envelope).unwrap().contains("777001"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_settings_save_keeps_the_owner_link_origin_the_panel_never_sends() {
        // What the panel sends: the fields it owns, and nothing about the
        // deployment's link origin.
        let mut written = HitlCriticalDeliverySettings {
            enabled_channels: vec!["telegram".into(), "kapso".into()],
            ..Default::default()
        };
        let on_disk = HitlCriticalDeliverySettings {
            owner_ui_origin: Some("https://ui.example.ai".into()),
            ..Default::default()
        };
        CriticalDeliverySettingsStore::carry_forward_operator_fields(&mut written, &on_disk);
        assert_eq!(
            written.owner_ui_origin.as_deref(),
            Some("https://ui.example.ai"),
            "a save that says nothing about the link origin must not clear it",
        );
        assert_eq!(
            written.enabled_channels,
            vec!["telegram".to_string(), "kapso".to_string()],
            "the fields the panel does own still write through",
        );

        // An operator who does name one is not overridden by the old value.
        let mut written = HitlCriticalDeliverySettings {
            owner_ui_origin: Some("https://new.example.ai".into()),
            ..Default::default()
        };
        CriticalDeliverySettingsStore::carry_forward_operator_fields(&mut written, &on_disk);
        assert_eq!(
            written.owner_ui_origin.as_deref(),
            Some("https://new.example.ai")
        );
    }
}
