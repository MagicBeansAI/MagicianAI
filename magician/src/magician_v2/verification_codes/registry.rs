//! The runtime's source registry: which sources this scope permitted for
//! verification codes, read fresh every time.
//!
//! Authority is a *purpose* the owner granted per source, on top of
//! enablement: an Observe channel entry that is enabled and carries the
//! `verification_codes` purpose (email → Gmail, `agentmail`, `imessage`); a
//! paired device the device policy lists, and that is connected to the hub
//! right now. Observation consent alone authorises nothing.
use std::sync::Arc;

use async_trait::async_trait;

use super::matching::SourceKind;
use super::sources::{AuthorizedSource, SourceRegistry};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::observe_connectors::{read_channel_observe, VERIFICATION_CODES_PURPOSE};

pub struct RuntimeSourceRegistry {
    workspace_layout: Arc<ArtifactV2Workspace>,
}

impl RuntimeSourceRegistry {
    pub fn new(workspace_layout: Arc<ArtifactV2Workspace>) -> Self {
        Self { workspace_layout }
    }
}

/// The source kind an Observe channel maps to, when it is one this path reads.
pub fn source_kind_for_channel(channel: &str) -> Option<SourceKind> {
    match channel {
        "email" => Some(SourceKind::Gmail),
        "agentmail" => Some(SourceKind::AgentMail),
        "imessage" => Some(SourceKind::Messages),
        _ => None,
    }
}

/// An account alias as status shows it: its last two characters.
pub fn masked(label: &str) -> String {
    let tail: String = label
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

#[async_trait]
impl SourceRegistry for RuntimeSourceRegistry {
    async fn authorized_sources(&self, principal: &str, workspace: &str) -> Vec<AuthorizedSource> {
        let mut sources = Vec::new();
        if let Some(config) =
            read_channel_observe(&self.workspace_layout, principal, workspace).await
        {
            for entry in config
                .channels
                .iter()
                .filter(|e| e.enabled && e.has_purpose(VERIFICATION_CODES_PURPOSE))
            {
                if let Some(kind) = source_kind_for_channel(&entry.channel) {
                    sources.push(AuthorizedSource {
                        kind,
                        account: entry.account.clone(),
                        label: format!("{} {}", kind.as_str(), masked(&entry.account)),
                    });
                }
            }
        }
        if let Some(policy) = crate::magician_v2::device_governance::global_device_policy() {
            let permitted = policy.verification_code_devices().await;
            if !permitted.is_empty() {
                if let Some(hub) = crate::magician_v2::device_bridge::global_hub() {
                    for key in hub.connected_devices() {
                        if key.principal == principal
                            && key.workspace == workspace
                            && permitted.iter().any(|id| *id == key.device_id)
                        {
                            sources.push(AuthorizedSource {
                                kind: SourceKind::AndroidNotification,
                                account: key.device_id.clone(),
                                label: format!("device {}", masked(&key.device_id)),
                            });
                        }
                    }
                }
            }
        }
        sources
    }
}
