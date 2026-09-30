//! Automatic verified VibeDev artifact handoff.
//!
//! A coding model may classify product requirements and write the bounded
//! handoff claim into the repository, but it cannot assert an artifact kind or
//! publication authority. After the repository is green, Magician reloads the
//! claim from the exact attested snapshot, runs the deterministic selector and
//! routes procedure, app, and executable-capability artifacts into the same
//! inert reviewed publication boundaries used by external authors.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    artifact_selection::{
        select_authoring_artifact, AppArtifactRequirement, AppArtifactSelectionError,
        AppArtifactSelectionInput, AppAuthoringArtifactKind,
    },
    authoring::read_bounded_file,
    authority::AuthenticatedAppScope,
    candidate_publication::{
        normalize_package_relative_path, validate_verified_vibedev_evidence,
        AppCandidatePublicationError, AppCandidatePublicationReceipt,
        AppCandidatePublicationService,
    },
    capability_publication::{
        AppCapabilityPublicationError, AppCapabilityPublicationReceipt,
        AppCapabilityPublicationService, APP_CAPABILITY_SKILL_MAX_BYTES,
    },
    models::{AppDigest, AppReference},
    procedure_publication::{
        AppProcedurePublicationError, AppProcedurePublicationReceipt,
        AppProcedurePublicationService, APP_PROCEDURE_SKILL_MAX_BYTES,
    },
};
use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    execution::{
        coding_engine::resolve_coding_repo_binding,
        file_edit::transaction::TransactionScope,
        verification::{
            ids::GateId,
            snapshot::{EntryKind, SourceSnapshot},
            store::VerificationStore,
        },
    },
    json_traversal::{json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded},
};

pub const VIBEDEV_ARTIFACT_HANDOFF_DIR: &str = ".magician/app-artifact-handoffs";
const VIBEDEV_ARTIFACT_HANDOFF_VERSION: u8 = 1;
const VIBEDEV_ARTIFACT_HANDOFF_MAX_BYTES: usize = 32 * 1024;
const VIBEDEV_ARTIFACT_HANDOFF_MAX_DEPTH: usize = 8;
const VIBEDEV_ARTIFACT_HANDOFF_MAX_NODES: usize = 256;
const VIBEDEV_ARTIFACT_PATH_MAX_BYTES: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VibeDevArtifactHandoffClaim {
    schema_version: u8,
    requirements: BTreeSet<AppArtifactRequirement>,
    artifact_path: String,
}

struct AdmittedVibeDevArtifactHandoff {
    root_task_id: String,
    root_execution_id: String,
    verification_attestation_ref: AppReference,
    artifact_path: PathBuf,
    selected_kind: AppAuthoringArtifactKind,
    procedure_bytes: Option<Vec<u8>>,
    capability_bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "artifact_kind", rename_all = "snake_case")]
pub enum VibeDevArtifactPublicationReceipt {
    ProcedureSkill {
        verification_attestation_ref: AppReference,
        publication: AppProcedurePublicationReceipt,
    },
    App {
        verification_attestation_ref: AppReference,
        publication: AppCandidatePublicationReceipt,
        companion_capability: Option<AppCapabilityPublicationReceipt>,
    },
    ExecutableCapability {
        verification_attestation_ref: AppReference,
        publication: AppCapabilityPublicationReceipt,
        companion_procedure: Option<AppProcedurePublicationReceipt>,
    },
}

#[derive(Debug, Error)]
pub enum VibeDevArtifactHandoffError {
    #[error("VibeDev artifact handoff is invalid: {0}")]
    InvalidClaim(String),
    #[error("VibeDev artifact handoff selected an unavailable artifact path: {0:?}")]
    UnsupportedSelection(AppAuthoringArtifactKind),
    #[error(transparent)]
    Selection(#[from] AppArtifactSelectionError),
    #[error(transparent)]
    Candidate(#[from] AppCandidatePublicationError),
    #[error(transparent)]
    Procedure(#[from] AppProcedurePublicationError),
    #[error(transparent)]
    Capability(#[from] AppCapabilityPublicationError),
}

#[derive(Clone)]
pub struct VibeDevArtifactHandoffService {
    workspace: ArtifactV2Workspace,
    candidate_publications: AppCandidatePublicationService,
    procedure_publications: AppProcedurePublicationService,
    capability_publications: AppCapabilityPublicationService,
}

impl VibeDevArtifactHandoffService {
    pub fn new(
        workspace: ArtifactV2Workspace,
        candidate_publications: AppCandidatePublicationService,
        procedure_publications: AppProcedurePublicationService,
        capability_publications: AppCapabilityPublicationService,
    ) -> Self {
        Self {
            workspace,
            candidate_publications,
            procedure_publications,
            capability_publications,
        }
    }

    /// Publish the exact artifact named by the verified repository handoff, or
    /// return `None` for an ordinary VibeDev build with no handoff claim.
    pub async fn publish_if_present(
        &self,
        authenticated: &AuthenticatedAppScope,
        gate_id: GateId,
        now: DateTime<Utc>,
    ) -> Result<Option<VibeDevArtifactPublicationReceipt>, VibeDevArtifactHandoffError> {
        authenticated
            .ensure_live_at(&now)
            .map_err(AppCandidatePublicationError::from)?;
        let workspace = self.workspace.clone();
        let transaction_scope = TransactionScope {
            principal: authenticated.scope().principal.as_str().to_owned(),
            workspace: authenticated.scope().workspace.as_str().to_owned(),
        };
        let admission_gate_id = gate_id.clone();
        let admitted = tokio::task::spawn_blocking(move || {
            admit_verified_handoff(&workspace, transaction_scope, &admission_gate_id)
        })
        .await
        .map_err(|error| {
            VibeDevArtifactHandoffError::InvalidClaim(format!("handoff worker terminated: {error}"))
        })??;
        let Some(admitted) = admitted else {
            return Ok(None);
        };
        authenticated
            .ensure_live_at(&now)
            .map_err(AppCandidatePublicationError::from)?;

        let receipt = match admitted.selected_kind {
            AppAuthoringArtifactKind::ProcedureSkill => {
                let bytes = admitted.procedure_bytes.ok_or_else(|| {
                    VibeDevArtifactHandoffError::InvalidClaim(
                        "verified procedure bytes are missing".to_owned(),
                    )
                })?;
                VibeDevArtifactPublicationReceipt::ProcedureSkill {
                    verification_attestation_ref: admitted.verification_attestation_ref,
                    publication: self
                        .procedure_publications
                        .publish(authenticated, &bytes, now)
                        .await?,
                }
            },
            AppAuthoringArtifactKind::DeclarativeApp
            | AppAuthoringArtifactKind::AppWithPrivateProcedures => {
                let companion_capability = match admitted.capability_bytes.as_deref() {
                    Some(bytes) => Some(
                        self.capability_publications
                            .publish(authenticated, bytes, now)
                            .await?,
                    ),
                    None => None,
                };
                VibeDevArtifactPublicationReceipt::App {
                    verification_attestation_ref: admitted.verification_attestation_ref,
                    publication: self
                        .candidate_publications
                        .publish_verified_vibedev_candidate(
                            authenticated,
                            gate_id,
                            admitted.root_task_id,
                            admitted.root_execution_id,
                            admitted.artifact_path,
                            now,
                        )
                        .await?,
                    companion_capability,
                }
            },
            AppAuthoringArtifactKind::ExecutableCapability => {
                let bytes = admitted.capability_bytes.ok_or_else(|| {
                    VibeDevArtifactHandoffError::InvalidClaim(
                        "verified capability bytes are missing".to_owned(),
                    )
                })?;
                let companion_procedure = match admitted.procedure_bytes.as_deref() {
                    Some(procedure_bytes) => Some(
                        self.procedure_publications
                            .publish(authenticated, procedure_bytes, now)
                            .await?,
                    ),
                    None => None,
                };
                VibeDevArtifactPublicationReceipt::ExecutableCapability {
                    verification_attestation_ref: admitted.verification_attestation_ref,
                    publication: self
                        .capability_publications
                        .publish(authenticated, &bytes, now)
                        .await?,
                    companion_procedure,
                }
            },
        };
        Ok(Some(receipt))
    }
}

fn admit_verified_handoff(
    workspace: &ArtifactV2Workspace,
    scope: TransactionScope,
    gate_id: &GateId,
) -> Result<Option<AdmittedVibeDevArtifactHandoff>, VibeDevArtifactHandoffError> {
    let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
    let store = VerificationStore::new(scope_root.clone(), scope.clone());
    let gate = store.load_gate(gate_id).map_err(|error| {
        VibeDevArtifactHandoffError::InvalidClaim(format!("verified gate is unavailable: {error}"))
    })?;
    let Some(attestation_id) = gate.active_attestation_ref.as_ref() else {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "verified gate has no active attestation".to_owned(),
        ));
    };
    let attestation = store.load_attestation(attestation_id).map_err(|error| {
        VibeDevArtifactHandoffError::InvalidClaim(format!(
            "verified attestation is unavailable: {error}"
        ))
    })?;
    validate_verified_vibedev_evidence(
        &scope,
        &gate,
        &attestation,
        &gate.root_task_id,
        &gate.root_execution_id,
    )?;

    let binding = resolve_coding_repo_binding(&scope_root, Some(&gate.project_binding))
        .map_err(VibeDevArtifactHandoffError::InvalidClaim)?;
    let before = SourceSnapshot::capture(&binding.real_path, &[])
        .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
    if before.digest() != attestation.key.snapshot_digest {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "repository no longer matches the accepted green snapshot".to_owned(),
        ));
    }
    let handoff_key = vibedev_artifact_handoff_path(&gate.root_task_id)?;
    let Some(handoff_entry) = before.entries.get(&handoff_key) else {
        return Ok(None);
    };
    if handoff_entry.kind != EntryKind::File {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "handoff path is not a regular file".to_owned(),
        ));
    }
    let handoff_path = binding.real_path.join(&handoff_key);
    let handoff_bytes = read_bounded_file(&handoff_path, VIBEDEV_ARTIFACT_HANDOFF_MAX_BYTES)
        .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
    ensure_snapshot_file_matches(handoff_entry, &handoff_bytes, "handoff")?;
    if !json_bytes_nesting_is_bounded(&handoff_bytes, VIBEDEV_ARTIFACT_HANDOFF_MAX_DEPTH)
        || !json_bytes_nodes_are_bounded(&handoff_bytes, VIBEDEV_ARTIFACT_HANDOFF_MAX_NODES)
    {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "handoff JSON exceeds its depth or node ceiling".to_owned(),
        ));
    }
    let claim: VibeDevArtifactHandoffClaim = serde_json::from_slice(&handoff_bytes)
        .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
    if claim.schema_version != VIBEDEV_ARTIFACT_HANDOFF_VERSION {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "unsupported handoff schema version".to_owned(),
        ));
    }
    if claim.artifact_path.is_empty() || claim.artifact_path.len() > VIBEDEV_ARTIFACT_PATH_MAX_BYTES
    {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "artifact_path is empty or oversized".to_owned(),
        ));
    }
    let artifact_path = normalize_package_relative_path(Path::new(&claim.artifact_path))?;
    if artifact_path.as_os_str().is_empty() {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(
            "artifact_path must name a child file or directory, not the repository root".to_owned(),
        ));
    }
    let decision = select_authoring_artifact(AppArtifactSelectionInput {
        requirements: claim.requirements,
    })?;
    if decision.companions().iter().any(|companion| {
        !matches!(
            companion,
            AppAuthoringArtifactKind::ExecutableCapability
                | AppAuthoringArtifactKind::ProcedureSkill
        )
    }) {
        return Err(VibeDevArtifactHandoffError::UnsupportedSelection(
            decision.primary(),
        ));
    }

    let mut procedure_bytes = None;
    let mut capability_bytes = None;
    let mut recapture_after_file = false;

    if decision.primary() == AppAuthoringArtifactKind::ProcedureSkill {
        procedure_bytes = Some(read_verified_skill_file(
            &binding.real_path,
            &before,
            &artifact_path,
            APP_PROCEDURE_SKILL_MAX_BYTES,
            "procedure artifact",
        )?);
        recapture_after_file = true;
    }
    if decision.primary() == AppAuthoringArtifactKind::ExecutableCapability {
        capability_bytes = Some(read_verified_skill_file(
            &binding.real_path,
            &before,
            &artifact_path,
            APP_CAPABILITY_SKILL_MAX_BYTES,
            "capability artifact",
        )?);
        recapture_after_file = true;
    }
    if decision
        .companions()
        .contains(&AppAuthoringArtifactKind::ExecutableCapability)
    {
        let companion = companion_capability_path(&artifact_path);
        capability_bytes = Some(read_verified_skill_file(
            &binding.real_path,
            &before,
            &companion,
            APP_CAPABILITY_SKILL_MAX_BYTES,
            "companion capability artifact",
        )?);
        recapture_after_file = true;
    }
    if decision
        .companions()
        .contains(&AppAuthoringArtifactKind::ProcedureSkill)
    {
        let companion = companion_procedure_path(&artifact_path);
        procedure_bytes = Some(read_verified_skill_file(
            &binding.real_path,
            &before,
            &companion,
            APP_PROCEDURE_SKILL_MAX_BYTES,
            "companion procedure artifact",
        )?);
        recapture_after_file = true;
    }

    if recapture_after_file {
        let after = SourceSnapshot::capture(&binding.real_path, &[])
            .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
        if after.digest() != attestation.key.snapshot_digest {
            return Err(VibeDevArtifactHandoffError::InvalidClaim(
                "repository changed during verified handoff admission".to_owned(),
            ));
        }
    }

    let verification_attestation_ref = AppReference::parse(format!(
        "attestation:vibedev:{}",
        attestation.attestation_id.as_str()
    ))
    .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
    Ok(Some(AdmittedVibeDevArtifactHandoff {
        root_task_id: gate.root_task_id,
        root_execution_id: gate.root_execution_id,
        verification_attestation_ref,
        artifact_path,
        selected_kind: decision.primary(),
        procedure_bytes,
        capability_bytes,
    }))
}

fn companion_capability_path(app_path: &Path) -> PathBuf {
    let mut name = app_path.file_name().unwrap_or_default().to_os_string();
    name.push(".tool.md");
    match app_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn companion_procedure_path(capability_path: &Path) -> PathBuf {
    let stem = capability_path
        .file_stem()
        .unwrap_or_default()
        .to_os_string();
    let mut name = stem;
    name.push(".procedure.md");
    match capability_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

fn read_verified_skill_file(
    repo_root: &Path,
    snapshot: &SourceSnapshot,
    relative: &Path,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>, VibeDevArtifactHandoffError> {
    let snapshot_path = artifact_path_to_snapshot_key(relative)?;
    let entry = snapshot.entries.get(&snapshot_path).ok_or_else(|| {
        VibeDevArtifactHandoffError::InvalidClaim(format!(
            "{label} is absent from the verified snapshot"
        ))
    })?;
    if entry.kind != EntryKind::File {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(format!(
            "{label} is not a regular file"
        )));
    }
    let bytes = read_bounded_file(&repo_root.join(relative), max_bytes)
        .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
    ensure_snapshot_file_matches(entry, &bytes, label)?;
    Ok(bytes)
}

pub fn vibedev_artifact_handoff_path(
    root_task_id: &str,
) -> Result<String, VibeDevArtifactHandoffError> {
    ArtifactV2Workspace::validate_task_id(root_task_id)
        .map_err(|error| VibeDevArtifactHandoffError::InvalidClaim(error.to_string()))?;
    Ok(format!(
        "{VIBEDEV_ARTIFACT_HANDOFF_DIR}/{root_task_id}.json"
    ))
}

fn artifact_path_to_snapshot_key(path: &Path) -> Result<String, VibeDevArtifactHandoffError> {
    let key = path
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .map(|components| components.join("/"))
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            VibeDevArtifactHandoffError::InvalidClaim(
                "artifact_path is not a non-empty UTF-8 relative path".to_owned(),
            )
        })?;
    Ok(key)
}

fn ensure_snapshot_file_matches(
    entry: &crate::magician_v2::execution::verification::snapshot::SnapshotEntry,
    bytes: &[u8],
    label: &str,
) -> Result<(), VibeDevArtifactHandoffError> {
    let digest = AppDigest::blake3(bytes);
    let digest = digest
        .as_str()
        .strip_prefix("blake3:")
        .unwrap_or(digest.as_str());
    if entry.digest != digest || entry.size != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return Err(VibeDevArtifactHandoffError::InvalidClaim(format!(
            "{label} differs from the accepted green snapshot"
        )));
    }
    Ok(())
}
