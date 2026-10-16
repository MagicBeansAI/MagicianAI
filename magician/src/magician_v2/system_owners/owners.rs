//! Cataloged Task 16 system, device, and secret owners.

use std::path::{Path, PathBuf};

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SystemOwner {
    SecretVault,
    ResourceAuthority,
    Programs,
    CapabilityEvolution,
    TransportLog,
    CompactionMetrics,
    DevicePairing,
    ContentSources,
    SystemTemplates,
    RuntimeConfig,
    DeviceLocalImessage,
    DeviceLocalWhatsapp,
    NotesProvider,
    SilverbulletSpaceRuntime,
    EvalsRuns,
}

impl SystemOwner {
    pub const ALL: [SystemOwner; 15] = [
        Self::SecretVault,
        Self::ResourceAuthority,
        Self::Programs,
        Self::CapabilityEvolution,
        Self::EvalsRuns,
        Self::TransportLog,
        Self::CompactionMetrics,
        Self::DevicePairing,
        Self::ContentSources,
        Self::SystemTemplates,
        Self::RuntimeConfig,
        Self::DeviceLocalImessage,
        Self::DeviceLocalWhatsapp,
        Self::NotesProvider,
        Self::SilverbulletSpaceRuntime,
    ];

    pub const TENANT: [SystemOwner; 7] = [
        Self::SecretVault,
        Self::ResourceAuthority,
        Self::Programs,
        Self::CapabilityEvolution,
        Self::EvalsRuns,
        Self::TransportLog,
        Self::CompactionMetrics,
    ];

    pub const HOST: [SystemOwner; 4] = [
        Self::DevicePairing,
        Self::ContentSources,
        Self::SystemTemplates,
        Self::RuntimeConfig,
    ];

    pub const DEVICE: [SystemOwner; 4] = [
        Self::DeviceLocalImessage,
        Self::DeviceLocalWhatsapp,
        Self::NotesProvider,
        Self::SilverbulletSpaceRuntime,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::SecretVault => "secret_vault",
            Self::ResourceAuthority => "resource_authority",
            Self::Programs => "programs",
            Self::CapabilityEvolution => "capability_evolution",
            Self::EvalsRuns => "evals_runs",
            Self::TransportLog => "transport_log_jsonl",
            Self::CompactionMetrics => "compaction_metrics",
            Self::DevicePairing => "device_pairing",
            Self::ContentSources => "content_sources",
            Self::SystemTemplates => "system_templates",
            Self::RuntimeConfig => "runtime_config",
            Self::DeviceLocalImessage => "device_local_imessage",
            Self::DeviceLocalWhatsapp => "device_local_whatsapp",
            Self::NotesProvider => "notes_provider",
            Self::SilverbulletSpaceRuntime => "silverbullet_space_runtime",
        }
    }

    pub fn sample_rel(self) -> &'static str {
        match self {
            Self::SecretVault => "secrets/secret_audit.jsonl",
            Self::ResourceAuthority => "resource_authority/resource_ledger.jsonl",
            Self::Programs => "programs/state/program-1.json",
            Self::CapabilityEvolution => "capability_evolution/capability_packs.json",
            Self::EvalsRuns => "evals/runs/lane/run-1.json",
            Self::TransportLog => "events.jsonl",
            Self::CompactionMetrics => "storage_governance/compaction_metrics.json",
            Self::DevicePairing => "system/paired-devices.json",
            Self::ContentSources => "content_sources/cache/entry.json",
            Self::SystemTemplates => "system/trust_policies.template.yaml",
            Self::RuntimeConfig => "magician-config.yaml",
            Self::DeviceLocalImessage => "device_local/imessage.marker",
            Self::DeviceLocalWhatsapp => "device_local/whatsapp.marker",
            Self::NotesProvider => "notes/note.md",
            Self::SilverbulletSpaceRuntime => "silverbullet/.magician/runtime/marker",
        }
    }

    pub fn is_host(self) -> bool {
        Self::HOST.contains(&self)
    }

    pub fn is_device(self) -> bool {
        Self::DEVICE.contains(&self)
    }

    pub fn allows(self, rel: &str) -> bool {
        if rel.starts_with('/') || std::path::Path::new(rel).is_absolute() {
            return false;
        }
        let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
        if parts.is_empty()
            || parts
                .iter()
                .any(|part| *part == "." || *part == ".." || part.contains('\0'))
        {
            return false;
        }
        match self {
            Self::SecretVault => prefix(&parts, &["secrets"], 2),
            Self::ResourceAuthority => prefix(&parts, &["resource_authority"], 2),
            Self::Programs => prefix(&parts, &["programs"], 2),
            Self::CapabilityEvolution => capability_pack_locator(&parts),
            Self::EvalsRuns => prefix(&parts, &["evals", "runs"], 3),
            Self::TransportLog => parts == ["events.jsonl"],
            Self::CompactionMetrics => parts == ["storage_governance", "compaction_metrics.json"],
            Self::DevicePairing => {
                parts == ["system", "paired-devices.json"]
                    || parts == ["system", "device-policy.json"]
            },
            Self::ContentSources => prefix(&parts, &["content_sources"], 2),
            Self::SystemTemplates => {
                prefix(&parts, &["system"], 2)
                    && parts != ["system", "paired-devices.json"]
                    && parts != ["system", "device-policy.json"]
            },
            Self::RuntimeConfig => parts == ["magician-config.yaml"],
            Self::DeviceLocalImessage => parts == ["device_local", "imessage.marker"],
            Self::DeviceLocalWhatsapp => parts == ["device_local", "whatsapp.marker"],
            Self::NotesProvider => prefix(&parts, &["notes"], 2),
            Self::SilverbulletSpaceRuntime => {
                prefix(&parts, &["silverbullet", ".magician", "runtime"], 4)
            },
        }
    }

    pub fn claiming(rel: &str) -> Option<SystemOwner> {
        Self::ALL.into_iter().find(|owner| owner.allows(rel))
    }

    pub fn root(
        self,
        workspace: &ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
    ) -> PathBuf {
        if self.is_host() || self.is_device() {
            workspace.base_root().to_path_buf()
        } else {
            workspace.scope_root(principal, workspace_name)
        }
    }
}

fn prefix(parts: &[&str], head: &[&str], min_len: usize) -> bool {
    parts.len() >= min_len && parts.get(..head.len()) == Some(head)
}

fn capability_pack_locator(parts: &[&str]) -> bool {
    if !prefix(parts, &["capability_evolution"], 2) {
        return false;
    }
    matches!(
        parts[1],
        "capability_packs.json" | ".catalog.lock" | "pack_promotion_audit.jsonl"
    ) || prefix(parts, &["capability_evolution", "rollback"], 3)
}

pub fn walk_files(scope_root: &Path, owner: SystemOwner) -> Vec<String> {
    let mut out = Vec::new();
    let _ = collect(scope_root, scope_root, owner, &mut out);
    out.sort();
    out.dedup();
    out.into_iter().filter(|rel| owner.allows(rel)).collect()
}

fn collect(
    scope_root: &Path,
    dir: &Path,
    owner: SystemOwner,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                continue;
            }
        }
        if path.is_dir() {
            collect(scope_root, &path, owner, out)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(scope_root) {
                let rel = rel.to_string_lossy().replace('\\', "/");
                if owner.allows(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    Ok(())
}
