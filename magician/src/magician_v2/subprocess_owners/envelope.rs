//! Versioned subprocess storage envelope at the process launcher boundary.
//!
//! A closed subprocess receives no engine root and no database or object-store
//! credentials. Unaccepted scratch is never canonical.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::local::workdirs_root;
use super::owners::SubprocessOwner;
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

pub const ENVELOPE_VERSION: u32 = 1;
pub const ENVELOPE_FILE_NAME: &str = ".magician-subprocess-envelope.json";
pub const DEFAULT_LEASE_BYTES: u64 = 128 * 1024 * 1024;

thread_local! {
    static ACTIVE_SKILL_LEASE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

const FORBIDDEN_CHILD_ENV: [&str; 6] = [
    "MAGICIAN_ROOT_DIR",
    "MAGICIAN_STORAGE_PATH",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "DATABASE_URL",
    "DUCKDB_PATH",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScratchCleanupPolicy {
    DeleteOnComplete,
    RetainUntilDeadline,
    Scavenge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchLeaseSpec {
    pub root: String,
    pub owner: String,
    pub max_bytes: u64,
    #[serde(default)]
    pub deadline_unix_ms: Option<i64>,
    pub cleanup: ScratchCleanupPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputManifestEntry {
    pub logical_id: String,
    pub rel: String,
    pub digest: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub source_owner: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretDelivery {
    Env,
    Fd,
    TempFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequest {
    pub secret_ref: String,
    pub purpose: String,
    pub delivery: SecretDelivery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSlot {
    pub name: String,
    pub rel: String,
    pub media_type: String,
    pub max_bytes: u64,
    pub intended_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub owner_id: String,
    pub locator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultManifestEntry {
    pub slot: String,
    pub digest: String,
    pub size_bytes: u64,
    pub accepted: bool,
    #[serde(default)]
    pub publication: Option<PublicationReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubprocessStorageEnvelope {
    pub version: u32,
    pub lease: ScratchLeaseSpec,
    #[serde(default)]
    pub inputs: Vec<InputManifestEntry>,
    #[serde(default)]
    pub secret_requests: Vec<SecretRequest>,
    #[serde(default)]
    pub outputs: Vec<OutputSlot>,
    #[serde(default)]
    pub result: Vec<ResultManifestEntry>,
    /// Closed children must not inherit engine roots or store credentials.
    pub closed: bool,
    /// Compatibility batches may still resolve the old root via the reviewed shim.
    pub compat: bool,
}

impl SubprocessStorageEnvelope {
    pub fn closed_skill(lease_root: &Path, call_id: &str) -> Self {
        let _ = lease_root;
        Self {
            version: ENVELOPE_VERSION,
            lease: ScratchLeaseSpec {
                root: format!("workdirs/skill-working/{}", sanitize_call_id(call_id)),
                owner: SubprocessOwner::SkillWorking.id().to_string(),
                max_bytes: DEFAULT_LEASE_BYTES,
                deadline_unix_ms: None,
                cleanup: ScratchCleanupPolicy::DeleteOnComplete,
            },
            inputs: Vec::new(),
            secret_requests: Vec::new(),
            outputs: vec![OutputSlot {
                name: "accepted".into(),
                rel: "outputs/accepted.bin".into(),
                media_type: "application/octet-stream".into(),
                max_bytes: DEFAULT_LEASE_BYTES,
                intended_owner: "execution_outputs".into(),
            }],
            result: Vec::new(),
            closed: true,
            compat: false,
        }
    }

    pub fn path_in(lease_root: &Path) -> PathBuf {
        lease_root.join(ENVELOPE_FILE_NAME)
    }
}

pub fn forbidden_child_env_names() -> &'static [&'static str] {
    &FORBIDDEN_CHILD_ENV
}

pub fn set_active_skill_working_lease(path: Option<PathBuf>) {
    ACTIVE_SKILL_LEASE.with(|slot| *slot.borrow_mut() = path);
}

pub fn active_skill_working_lease() -> Option<PathBuf> {
    ACTIVE_SKILL_LEASE.with(|slot| slot.borrow().clone())
}

pub fn env_is_closed<S: AsRef<str>>(keys: impl IntoIterator<Item = S>) -> bool {
    keys.into_iter().all(|key| {
        !FORBIDDEN_CHILD_ENV
            .iter()
            .any(|forbidden| forbidden.eq_ignore_ascii_case(key.as_ref()))
    })
}

pub fn envelope_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn sanitize_call_id(call_id: &str) -> String {
    let mut out = String::new();
    for ch in call_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
        if out.len() >= 64 {
            break;
        }
    }
    if out.is_empty() {
        "call".into()
    } else {
        out
    }
}

/// Create the classified skill-working lease and write the envelope there.
pub fn install_skill_working_envelope(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    call_id: &str,
) -> Result<PathBuf> {
    let lease = workdirs_root(workspace, principal, workspace_name)
        .join("skill-working")
        .join(sanitize_call_id(call_id));
    std::fs::create_dir_all(lease.join("inputs"))
        .with_context(|| format!("create {}", lease.display()))?;
    std::fs::create_dir_all(lease.join("outputs"))?;
    let envelope = SubprocessStorageEnvelope::closed_skill(&lease, call_id);
    let bytes = serde_json::to_vec_pretty(&envelope)?;
    write_bytes_durably_sync(&SubprocessStorageEnvelope::path_in(&lease), &bytes)
        .with_context(|| format!("write envelope under {}", lease.display()))?;
    set_active_skill_working_lease(Some(lease.clone()));
    Ok(lease)
}

/// Launcher hook. Scope-blind calls skip the tenant lease.
pub fn install_skill_working_envelope_from_exec(
    storage_base: &Path,
    principal: Option<&str>,
    workspace_name: Option<&str>,
    call_id: &str,
) -> Result<Option<PathBuf>> {
    let Some(principal) = principal.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some(workspace_name) = workspace_name.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let workspace = ArtifactV2Workspace::new(storage_base);
    Ok(Some(install_skill_working_envelope(
        &workspace,
        principal,
        workspace_name,
        call_id,
    )?))
}

/// Materialize a read-only input into the lease after size and digest checks.
pub fn materialize_input(
    lease_root: &Path,
    entry: &InputManifestEntry,
    bytes: &[u8],
) -> Result<PathBuf> {
    if bytes.len() as u64 != entry.size_bytes {
        bail!(
            "input {} size mismatch: expected {} got {}",
            entry.logical_id,
            entry.size_bytes,
            bytes.len()
        );
    }
    if envelope_digest(bytes) != entry.digest {
        bail!("input {} digest mismatch", entry.logical_id);
    }
    if Path::new(&entry.rel).is_absolute()
        || Path::new(&entry.rel).components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        })
    {
        bail!("input {} rejects traversal", entry.logical_id);
    }
    let dest = lease_root.join(&entry.rel);
    if !dest.starts_with(lease_root) {
        bail!("input {} escaped lease root", entry.logical_id);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_bytes_durably_sync(&dest, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&dest, perms)?;
    }
    Ok(dest)
}

/// Publish accepted slot bytes through a Task 10 object owner. Unaccepted
/// bytes are not written here.
pub async fn publish_accepted_output(
    workspace: &ArtifactV2Workspace,
    dest: &Path,
    slot: &OutputSlot,
    bytes: &[u8],
) -> Result<PublicationReceipt> {
    if bytes.len() as u64 > slot.max_bytes {
        bail!(
            "output slot {} exceeds bound {} > {}",
            slot.name,
            bytes.len(),
            slot.max_bytes
        );
    }
    let (store, rel) = crate::magician_v2::object_owners::store_for_any_owner(workspace, dest)
        .ok_or_else(|| anyhow::anyhow!("accepted output is outside a Task 10 object owner"))?;
    crate::magician_v2::object_owners::BlobAccess::put(&store, &rel, bytes).await?;
    Ok(PublicationReceipt {
        owner_id: slot.intended_owner.clone(),
        locator: rel,
    })
}

/// Delete a lease tree. Unaccepted scratch must not remain as canonical truth.
pub fn scavenge_lease(lease_root: &Path) -> Result<()> {
    if lease_root.exists() {
        std::fs::remove_dir_all(lease_root)
            .with_context(|| format!("scavenge {}", lease_root.display()))?;
    }
    Ok(())
}

/// Read the envelope a launcher wrote under `lease_root` (the counterpart of
/// [`install_skill_working_envelope`]; the file format is versioned by
/// [`ENVELOPE_VERSION`]). Any Rust-side consumer of a lease reads it through
/// here rather than re-parsing [`ENVELOPE_FILE_NAME`].
pub fn load_envelope(lease_root: &Path) -> Result<SubprocessStorageEnvelope> {
    let bytes = std::fs::read(SubprocessStorageEnvelope::path_in(lease_root))?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_envelope_loads_back_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let lease = install_skill_working_envelope(&workspace, "alice", "home", "call-1").unwrap();
        let loaded = load_envelope(&lease).unwrap();
        let expected = SubprocessStorageEnvelope::closed_skill(&lease, "call-1");
        assert_eq!(loaded.version, ENVELOPE_VERSION);
        assert_eq!(loaded.lease.root, expected.lease.root);
        assert_eq!(loaded.lease.owner, expected.lease.owner);
        assert_eq!(loaded.lease.max_bytes, expected.lease.max_bytes);
        assert_eq!(loaded.outputs.len(), 1);
        assert_eq!(
            loaded.outputs[0].intended_owner,
            expected.outputs[0].intended_owner
        );
        assert!(loaded.closed);
        assert!(!loaded.compat);
        set_active_skill_working_lease(None);
    }
}
