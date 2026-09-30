//! Local owner snapshots and remote logical exports.
//!
//! Restore order is objects, then datasets, then repositories so a recovered
//! database point still names object generations that exist. Secret *bytes*
//! are not exported.

use serde::{Deserialize, Serialize};

use crate::magician_v2::agent_owners::{AgentAccess, AgentOwner};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::chat_owners::{ChatAccess, ChatOwner};
use crate::magician_v2::dataset_owners::{DatasetAccess, DatasetFamily};
use crate::magician_v2::object_owners::{BlobAccess, ObjectOwner};
use crate::magician_v2::system_owners::{SystemAccess, SystemOwner};
use crate::magician_v2::work_owners::{WorkAccess, WorkOwner};

const RESTORE_ORDER: &[&str] = &[
    "app_attachments",
    "events",
    "programs",
    "task_records",
    "chat_sessions",
    "memory_canonical",
    "agent_runtime",
    "device_pairing",
    "secret_vault",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerSnapshot {
    pub catalog_id: String,
    pub class: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSnapshot {
    pub taken_unix_ms: i64,
    pub principal: String,
    pub workspace: String,
    pub owners: Vec<OwnerSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedSnapshot {
    pub digest: String,
    pub snapshot: ProfileSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEvidence {
    pub taken_unix_ms: i64,
    pub profile: String,
    pub accepted_by: String,
    pub owners: Vec<String>,
    pub classes: Vec<String>,
    pub digest: String,
    pub secret_bytes_excluded: bool,
}

impl ProfileSnapshot {
    pub fn evidence(&self, digest: &str) -> BackupEvidence {
        BackupEvidence {
            taken_unix_ms: self.taken_unix_ms,
            profile: "local_embedded".into(),
            accepted_by: super::gate2::GATE2_OWNER.into(),
            owners: self
                .owners
                .iter()
                .map(|owner| owner.catalog_id.clone())
                .collect(),
            classes: self
                .owners
                .iter()
                .map(|owner| owner.class.clone())
                .collect(),
            digest: digest.to_string(),
            secret_bytes_excluded: true,
        }
    }
}

impl SignedSnapshot {
    pub fn wrap(snapshot: ProfileSnapshot) -> anyhow::Result<Self> {
        let digest = snapshot_digest(&snapshot)?;
        Ok(Self { digest, snapshot })
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        let actual = snapshot_digest(&self.snapshot)?;
        if actual != self.digest {
            anyhow::bail!("backup digest mismatch");
        }
        Ok(())
    }
}

fn snapshot_digest(snapshot: &ProfileSnapshot) -> anyhow::Result<String> {
    let body = serde_json::to_vec(snapshot)?;
    Ok(blake3::hash(&body).to_hex().to_string())
}

pub async fn snapshot_representative_scope(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
) -> anyhow::Result<ProfileSnapshot> {
    let mut owners = Vec::new();
    owners.push(
        snap_object(
            workspace,
            principal,
            workspace_name,
            ObjectOwner::AppAttachments,
            "required_objects",
        )
        .await?,
    );
    owners.push(
        snap_dataset(
            workspace,
            principal,
            workspace_name,
            DatasetFamily::Events,
            "observability_datasets",
        )
        .await?,
    );
    owners.push(
        snap_system(
            workspace,
            principal,
            workspace_name,
            SystemOwner::Programs,
            "canonical_state",
        )
        .await?,
    );
    owners.push(
        snap_work(
            workspace,
            principal,
            workspace_name,
            WorkOwner::TaskRecords,
            "canonical_state",
        )
        .await?,
    );
    owners.push(
        snap_chat(
            workspace,
            principal,
            workspace_name,
            ChatOwner::ChatSessions,
            "canonical_state",
        )
        .await?,
    );
    owners.push(
        snap_agent(
            workspace,
            principal,
            workspace_name,
            AgentOwner::MemoryCanonical,
            "canonical_state",
        )
        .await?,
    );
    owners.push(
        snap_agent(
            workspace,
            principal,
            workspace_name,
            AgentOwner::AgentRuntime,
            "canonical_state",
        )
        .await?,
    );
    owners.push(
        snap_system(
            workspace,
            principal,
            workspace_name,
            SystemOwner::DevicePairing,
            "device",
        )
        .await?,
    );
    owners.push(
        snap_system(
            workspace,
            principal,
            workspace_name,
            SystemOwner::SecretVault,
            "secrets_configuration",
        )
        .await?,
    );
    Ok(ProfileSnapshot {
        taken_unix_ms: chrono::Utc::now().timestamp_millis(),
        principal: principal.to_string(),
        workspace: workspace_name.to_string(),
        owners,
    })
}

pub async fn restore_representative_scope(
    workspace: &ArtifactV2Workspace,
    signed: &SignedSnapshot,
) -> anyhow::Result<()> {
    signed.verify()?;
    let snapshot = &signed.snapshot;
    let mut remaining: Vec<&OwnerSnapshot> = snapshot.owners.iter().collect();
    for id in RESTORE_ORDER {
        if let Some(idx) = remaining.iter().position(|owner| owner.catalog_id == *id) {
            let owner = remaining.remove(idx);
            import_owner(workspace, snapshot, owner).await?;
        }
    }
    if let Some(unknown) = remaining.first() {
        anyhow::bail!("unknown snapshot owner {}", unknown.catalog_id);
    }
    walk_restored_integrity(workspace, snapshot).await?;
    Ok(())
}

async fn import_owner(
    workspace: &ArtifactV2Workspace,
    snapshot: &ProfileSnapshot,
    owner: &OwnerSnapshot,
) -> anyhow::Result<()> {
    match owner.catalog_id.as_str() {
        "app_attachments" => {
            crate::magician_v2::object_owners::open_local_object_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                ObjectOwner::AppAttachments,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "events" => {
            crate::magician_v2::dataset_owners::open_local_dataset_family(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                DatasetFamily::Events,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "programs" => {
            crate::magician_v2::system_owners::open_local_system_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                SystemOwner::Programs,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "task_records" => {
            crate::magician_v2::work_owners::open_local_work_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                WorkOwner::TaskRecords,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "chat_sessions" => {
            crate::magician_v2::chat_owners::open_local_chat_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                ChatOwner::ChatSessions,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "memory_canonical" => {
            crate::magician_v2::agent_owners::open_local_agent_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                AgentOwner::MemoryCanonical,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "agent_runtime" => {
            crate::magician_v2::agent_owners::open_local_agent_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                AgentOwner::AgentRuntime,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "device_pairing" => {
            crate::magician_v2::system_owners::open_local_system_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                SystemOwner::DevicePairing,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        "secret_vault" => {
            crate::magician_v2::system_owners::open_local_system_owner(
                workspace,
                &snapshot.principal,
                &snapshot.workspace,
                SystemOwner::SecretVault,
            )
            .import_all(&owner.bytes)
            .await?;
        },
        other => anyhow::bail!("unknown snapshot owner {other}"),
    }
    Ok(())
}

async fn walk_restored_integrity(
    workspace: &ArtifactV2Workspace,
    snapshot: &ProfileSnapshot,
) -> anyhow::Result<()> {
    use crate::magician_v2::dataset_owners::part_digest;
    use crate::magician_v2::object_owners::blob_digest;
    use crate::magician_v2::recovery::integrity::{RecoveryManifest, ReferencedObject};
    use crate::magician_v2::system_owners::system_digest;

    let mut expected_objects = Vec::new();
    let mut present_objects = Vec::new();
    let mut expected_parts = Vec::new();
    let mut present_parts = Vec::new();
    let mut expected_devices = Vec::new();
    let mut present_devices = Vec::new();
    let mut expected_secrets = Vec::new();
    let mut present_secrets = Vec::new();

    for owner in &snapshot.owners {
        match owner.catalog_id.as_str() {
            "app_attachments" => {
                let records: Vec<crate::magician_v2::object_owners::ExportedBlob> =
                    serde_json::from_slice(&owner.bytes)?;
                let store = crate::magician_v2::object_owners::open_local_object_owner(
                    workspace,
                    &snapshot.principal,
                    &snapshot.workspace,
                    ObjectOwner::AppAttachments,
                );
                for record in records {
                    expected_objects.push(ReferencedObject {
                        key: record.path.clone(),
                        version: "restored".into(),
                        digest: record.digest.clone(),
                    });
                    let raw = store.get(&record.path).await?;
                    present_objects.push(ReferencedObject {
                        key: record.path,
                        version: "restored".into(),
                        digest: blob_digest(&raw),
                    });
                }
            },
            "events" => {
                let records: Vec<crate::magician_v2::dataset_owners::ExportedPart> =
                    serde_json::from_slice(&owner.bytes)?;
                let store = crate::magician_v2::dataset_owners::open_local_dataset_family(
                    workspace,
                    &snapshot.principal,
                    &snapshot.workspace,
                    DatasetFamily::Events,
                );
                for record in records {
                    expected_parts.push(record.path.clone());
                    let raw = store.get(&record.path).await?;
                    if record.digest.is_empty() || record.digest != part_digest(&raw) {
                        anyhow::bail!("restored dataset digest mismatch for {}", record.path);
                    }
                    present_parts.push(record.path);
                }
            },
            "device_pairing" => {
                let records: Vec<crate::magician_v2::system_owners::ExportedSystem> =
                    serde_json::from_slice(&owner.bytes)?;
                let store = crate::magician_v2::system_owners::open_local_system_owner(
                    workspace,
                    &snapshot.principal,
                    &snapshot.workspace,
                    SystemOwner::DevicePairing,
                );
                for record in records {
                    expected_devices.push(record.path.clone());
                    let raw = store.get(&record.path).await?;
                    if record.digest.is_empty() || record.digest != system_digest(&raw) {
                        anyhow::bail!("restored device digest mismatch for {}", record.path);
                    }
                    present_devices.push(record.path);
                }
            },
            "secret_vault" => {
                let records: Vec<crate::magician_v2::system_owners::ExportedSystem> =
                    serde_json::from_slice(&owner.bytes)?;
                let store = crate::magician_v2::system_owners::open_local_system_owner(
                    workspace,
                    &snapshot.principal,
                    &snapshot.workspace,
                    SystemOwner::SecretVault,
                );
                for record in records {
                    expected_secrets.push(record.path.clone());
                    let raw = store.get(&record.path).await?;
                    if record.digest.is_empty() || record.digest != system_digest(&raw) {
                        anyhow::bail!("restored secret digest mismatch for {}", record.path);
                    }
                    present_secrets.push(record.path);
                }
            },
            _ => {},
        }
    }

    let report = RecoveryManifest {
        objects: expected_objects,
        dataset_parts: expected_parts,
        index_watermarks: Vec::new(),
        device_records: expected_devices,
        secret_refs: expected_secrets,
    }
    .walk(
        &present_objects,
        &present_parts,
        &present_devices,
        &present_secrets,
    );
    if !report.missing.is_empty() {
        anyhow::bail!("restore integrity missing {:?}", report.missing);
    }
    Ok(())
}

async fn snap_system(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: SystemOwner,
    class: &str,
) -> anyhow::Result<OwnerSnapshot> {
    let store = crate::magician_v2::system_owners::open_local_system_owner(
        workspace,
        principal,
        workspace_name,
        owner,
    );
    Ok(OwnerSnapshot {
        catalog_id: owner.id().into(),
        class: class.into(),
        bytes: store.export_all().await?,
    })
}

async fn snap_object(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: ObjectOwner,
    class: &str,
) -> anyhow::Result<OwnerSnapshot> {
    let store = crate::magician_v2::object_owners::open_local_object_owner(
        workspace,
        principal,
        workspace_name,
        owner,
    );
    Ok(OwnerSnapshot {
        catalog_id: owner.id().into(),
        class: class.into(),
        bytes: store.export_all().await?,
    })
}

async fn snap_dataset(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    family: DatasetFamily,
    class: &str,
) -> anyhow::Result<OwnerSnapshot> {
    let store = crate::magician_v2::dataset_owners::open_local_dataset_family(
        workspace,
        principal,
        workspace_name,
        family,
    );
    Ok(OwnerSnapshot {
        catalog_id: family.id().into(),
        class: class.into(),
        bytes: store.export_all().await?,
    })
}

async fn snap_work(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: WorkOwner,
    class: &str,
) -> anyhow::Result<OwnerSnapshot> {
    let store = crate::magician_v2::work_owners::open_local_work_owner(
        workspace,
        principal,
        workspace_name,
        owner,
    );
    Ok(OwnerSnapshot {
        catalog_id: owner.id().into(),
        class: class.into(),
        bytes: store.export_all().await?,
    })
}

async fn snap_chat(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: ChatOwner,
    class: &str,
) -> anyhow::Result<OwnerSnapshot> {
    let store = crate::magician_v2::chat_owners::open_local_chat_owner(
        workspace,
        principal,
        workspace_name,
        owner,
    );
    Ok(OwnerSnapshot {
        catalog_id: owner.id().into(),
        class: class.into(),
        bytes: store.export_all().await?,
    })
}

async fn snap_agent(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: AgentOwner,
    class: &str,
) -> anyhow::Result<OwnerSnapshot> {
    let store = crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        workspace_name,
        owner,
    );
    Ok(OwnerSnapshot {
        catalog_id: owner.id().into(),
        class: class.into(),
        bytes: store.export_all().await?,
    })
}
