use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::hash::Hash;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use fs2::FileExt;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};

use super::capability_bridge::LearningCapabilityEvolutionBridge;
use super::procedure_index::{mark_procedure_index_dirty, procedure_index_content_changed};
use super::types::{
    CreateLearningCandidateRequest, CreateLearningEventRequest, CreateLearningProcedureRequest,
    CreateLearningSkillInvocationEvidenceRequest, LearningCandidate, LearningCandidateState,
    LearningCandidateType, LearningCapabilityEvolutionApplicationRecord,
    LearningCapabilityEvolutionBacklogItem, LearningCapabilityEvolutionImplementationRecord,
    LearningCapabilityEvolutionPostPromotionMonitorRecord,
    LearningCapabilityEvolutionPromotionRecord, LearningCapabilityEvolutionProposal,
    LearningCapabilityEvolutionRollbackRecommendationRecord,
    LearningCapabilityEvolutionStewardRunReport, LearningCapabilityEvolutionValidationReport,
    LearningDecisionLogEntry, LearningEvaluationBacklogItem, LearningEvaluationRunReport,
    LearningEvent, LearningEvidenceRef, LearningGrowthEvaluationRunReport, LearningProcedure,
    LearningProcedureDecisionLogEntry, LearningProcedureStatus, LearningRiskLevel, LearningScope,
    LearningSkillInvocationEvidence, LearningSkillInvocationFailureClass,
    LearningSkillInvocationFailureCluster, LearningSkillInvocationStatus,
};

const SKILL_INVOCATION_FAILURE_CLUSTER_ROUTE_THRESHOLD: u64 = 3;
const MAX_FAILURE_CLUSTER_EVIDENCE_REFS: usize = 24;

#[derive(Debug, Clone, Default)]
pub struct LearningCandidateFilters {
    pub state: Option<String>,
    pub candidate_type: Option<String>,
    pub source_agent_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningEvaluationBacklogFilters {
    pub status: Option<String>,
    pub target_agent_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningEvaluationRunFilters {
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningGrowthEvaluationRunFilters {
    pub status: Option<String>,
    pub suite_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningProcedureFilters {
    pub status: Option<String>,
    pub owner_agent: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningSkillInvocationEvidenceFilters {
    pub source: Option<String>,
    pub status: Option<String>,
    pub failure_class: Option<String>,
    pub skill_name: Option<String>,
    pub tool_action_name: Option<String>,
    pub agent_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionBacklogFilters {
    pub status: Option<String>,
    pub candidate_type: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionProposalFilters {
    pub status: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionValidationFilters {
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionImplementationFilters {
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionApplicationFilters {
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionRollbackRecommendationFilters {
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub application_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionPostPromotionMonitorFilters {
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub promotion_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionPromotionFilters {
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct LearningCapabilityEvolutionStewardRunFilters {
    pub status: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct LearningStore {
    workspace_layout: ArtifactV2Workspace,
}

#[derive(Debug, Clone)]
struct LearningDirEntry {
    path: PathBuf,
    is_dir: bool,
    is_file: bool,
}

#[derive(Debug, Clone, Copy)]
struct LearningFileType {
    is_dir: bool,
    is_file: bool,
}

impl LearningDirEntry {
    fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn file_type(&self) -> Result<LearningFileType> {
        Ok(LearningFileType {
            is_dir: self.is_dir,
            is_file: self.is_file,
        })
    }
}

impl LearningFileType {
    fn is_dir(&self) -> bool {
        self.is_dir
    }

    fn is_file(&self) -> bool {
        self.is_file
    }
}

impl LearningStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn workspace_layout(&self) -> &ArtifactV2Workspace {
        &self.workspace_layout
    }

    fn path_exists<P: AsRef<Path>>(&self, path: P) -> bool {
        self.workspace_layout
            .metadata_path_sync(path.as_ref())
            .ok()
            .flatten()
            .is_some()
    }

    fn read_dir<P: AsRef<Path>>(&self, root: P) -> Result<Vec<Result<LearningDirEntry>>> {
        let root = root.as_ref();
        let entries = match self.workspace_layout.read_dir_path_sync(root) {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(error) => return Err(error).with_context(|| format!("listing {}", root.display())),
        };
        Ok(entries
            .into_iter()
            .map(|entry| {
                Ok(LearningDirEntry {
                    path: self.workspace_layout.base_root().join(entry.relative_path),
                    is_dir: entry.is_dir,
                    is_file: entry.is_file,
                })
            })
            .collect())
    }

    fn write_yaml_pretty<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let content = serde_yaml::to_string(value)?;
        self.write_classified_bytes(path, content.as_bytes())
    }

    fn write_json_pretty<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let content = serde_json::to_vec_pretty(value)?;
        self.write_classified_bytes(path, &content)
    }

    fn write_classified_bytes(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        if crate::magician_v2::agent_owners::store_for_any_owner(&self.workspace_layout, path)
            .is_some()
        {
            crate::magician_v2::agent_owners::persist_agent_file_sync(
                &self.workspace_layout,
                path,
                bytes,
            )
            .with_context(|| format!("writing {}", path.display()))?;
            return Ok(());
        }
        match crate::magician_v2::typed_io::persist_sync(&self.workspace_layout, path, bytes) {
            Ok(true) => return Ok(()),
            Ok(false) => {},
            Err(error) => {
                return Err(anyhow::anyhow!("writing {}: {error}", path.display()));
            },
        }
        self.workspace_layout
            .write_atomic_path_sync(path, bytes)
            .with_context(|| format!("writing {}", path.display()))
    }

    fn append_jsonl<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.workspace_layout
            .append_path_sync(path, &line)
            .with_context(|| format!("appending {}", path.display()))
    }

    fn read_json<T: for<'de> serde::Deserialize<'de>>(&self, path: &Path) -> Result<T> {
        let content = self
            .workspace_layout
            .read_to_string_path_sync(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&content).with_context(|| format!("parsing {}", path.display()))
    }

    fn read_first_existing_json<T: for<'de> serde::Deserialize<'de>>(
        &self,
        paths: &[PathBuf],
        missing_message: impl Into<String>,
    ) -> Result<T> {
        for path in paths {
            if self.path_exists(path) {
                return self.read_json(path);
            }
        }
        Err(anyhow!(missing_message.into()))
    }

    fn read_yaml<T: for<'de> serde::Deserialize<'de>>(&self, path: &Path) -> Result<T> {
        let content = self
            .workspace_layout
            .read_to_string_path_sync(path)
            .with_context(|| format!("reading {}", path.display()))?;
        serde_yaml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
    }

    fn read_jsonl<T: for<'de> serde::Deserialize<'de>>(&self, path: &Path) -> Result<Vec<T>> {
        let content = match self.workspace_layout.read_to_string_path_sync(path) {
            Ok(content) => content,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        let mut values = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            values.push(
                serde_json::from_str(line)
                    .with_context(|| format!("parsing {} line {}", path.display(), idx + 1))?,
            );
        }
        Ok(values)
    }

    fn count_files_with_extension(&self, root: &Path, extension: &str) -> usize {
        let Ok(entries) = self.read_dir(root) else {
            return 0;
        };
        entries
            .into_iter()
            .flatten()
            .filter(|entry| entry.is_file)
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str()) == Some(extension)
            })
            .count()
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        match self.workspace_layout.remove_file_path_sync(path) {
            Ok(()) => Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(())
            },
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    pub fn learning_root(&self, scope: &LearningScope) -> std::path::PathBuf {
        self.workspace_layout
            .learning_root(&scope.principal, &scope.workspace)
    }

    pub fn append_event(
        &self,
        scope: LearningScope,
        request: CreateLearningEventRequest,
    ) -> Result<LearningEvent> {
        let event = request.into_event(scope);
        let path = self.event_log_path(&event.scope, event.created_at.date_naive().to_string());
        self.append_jsonl(&path, &event)?;
        Ok(event)
    }

    pub fn record_skill_invocation_evidence(
        &self,
        scope: LearningScope,
        request: CreateLearningSkillInvocationEvidenceRequest,
    ) -> Result<LearningSkillInvocationEvidence> {
        let evidence = request.into_skill_invocation_evidence(scope);
        self.write_skill_invocation_evidence(&evidence)?;
        let evidence_path = self.skill_invocation_evidence_path(&evidence);
        let failure_class = evidence
            .failure_class
            .as_ref()
            .map(|failure_class| failure_class.as_str().to_string());
        let event_type = match evidence.status.as_str() {
            "succeeded" => "skill_invocation_succeeded",
            "cancelled" => "skill_invocation_cancelled",
            "blocked" => "skill_invocation_blocked",
            _ => "skill_invocation_failed",
        };
        let mut evidence_refs = evidence.evidence_refs.clone();
        let evidence_ref = LearningEvidenceRef {
            kind: "skill_invocation_evidence".to_string(),
            id: Some(evidence.id.clone()),
            path: Some(evidence_path.to_string_lossy().to_string()),
            uri: None,
            summary: Some(format!(
                "{} invocation of `{}`",
                evidence.status.as_str(),
                evidence.skill_name
            )),
        };
        evidence_refs.push(evidence_ref.clone());
        let action_label = evidence
            .tool_action_name
            .as_deref()
            .map(|action| format!(" action `{action}`"))
            .unwrap_or_default();
        self.append_event(
            evidence.scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: event_type.to_string(),
                agent_id: evidence.agent_id.clone(),
                task_id: evidence.task_id.clone(),
                execution_id: evidence.execution_id.clone(),
                chat_session_id: evidence.chat_session_id.clone(),
                summary: format!(
                    "Skill `{}`{} {} via {}.",
                    evidence.skill_name,
                    action_label,
                    evidence.status.as_str(),
                    evidence.source.as_str()
                ),
                evidence_refs,
                payload: serde_json::json!({
                    "skill_invocation_id": evidence.id.clone(),
                    "skill_name": evidence.skill_name.clone(),
                    "tool_action_name": evidence.tool_action_name.clone(),
                    "source": evidence.source.as_str(),
                    "status": evidence.status.as_str(),
                    "failure_class": failure_class,
                    "duration_ms": evidence.duration_ms,
                    "retry_count": evidence.retry_count,
                    "input_fingerprint": evidence.input_fingerprint.clone(),
                }),
            },
        )?;
        if matches!(evidence.status, LearningSkillInvocationStatus::Failed) {
            if let Err(error) =
                self.record_skill_invocation_failure_cluster(&evidence, evidence_ref)
            {
                warn!(
                    skill_name = %evidence.skill_name,
                    tool_action_name = ?evidence.tool_action_name,
                    failure_class = ?evidence.failure_class,
                    error = %error,
                    "failed to aggregate skill invocation failure evidence"
                );
            }
        }
        Ok(evidence)
    }

    pub fn write_skill_invocation_evidence(
        &self,
        evidence: &LearningSkillInvocationEvidence,
    ) -> Result<()> {
        let path = self.skill_invocation_evidence_path(evidence);
        self.write_json_pretty(&path, evidence)
    }

    pub fn read_skill_invocation_evidence(
        &self,
        scope: &LearningScope,
        date: &str,
        invocation_id: &str,
    ) -> Result<LearningSkillInvocationEvidence> {
        let path = self.workspace_layout.learning_skill_invocation_path(
            &scope.principal,
            &scope.workspace,
            date,
            invocation_id,
        );
        if !self.path_exists(&path) {
            return Err(anyhow!(
                "learning skill invocation evidence `{invocation_id}` was not found for date `{date}`"
            ));
        }
        self.read_json(&path)
    }

    pub fn list_skill_invocation_evidence(
        &self,
        scope: &LearningScope,
        filters: LearningSkillInvocationEvidenceFilters,
    ) -> Result<Vec<LearningSkillInvocationEvidence>> {
        let root = self
            .workspace_layout
            .learning_skill_invocations_dir(&scope.principal, &scope.workspace);
        let mut records = Vec::new();
        if !self.path_exists(&root) {
            return Ok(records);
        }
        let source_filter = filters.source.as_deref().map(normalize_filter_value);
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let failure_filter = filters.failure_class.as_deref().map(normalize_filter_value);
        for day_entry in self.read_dir(&root).with_context(|| {
            format!(
                "listing learning skill invocation evidence at {}",
                root.display()
            )
        })? {
            let day_entry = day_entry?;
            if !day_entry.file_type()?.is_dir() {
                continue;
            }
            for entry in self.read_dir(day_entry.path()).with_context(|| {
                format!(
                    "listing learning skill invocation evidence at {}",
                    day_entry.path().display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let record: LearningSkillInvocationEvidence = self.read_json(&entry.path())?;
                if let Some(expected) = source_filter.as_deref() {
                    if record.source.as_str() != expected {
                        continue;
                    }
                }
                if let Some(expected) = status_filter.as_deref() {
                    if record.status.as_str() != expected {
                        continue;
                    }
                }
                if let Some(expected) = failure_filter.as_deref() {
                    if record
                        .failure_class
                        .as_ref()
                        .map(|failure_class| failure_class.as_str())
                        != Some(expected)
                    {
                        continue;
                    }
                }
                if let Some(expected) = filters.skill_name.as_deref() {
                    if record.skill_name != expected {
                        continue;
                    }
                }
                if let Some(expected) = filters.tool_action_name.as_deref() {
                    if record.tool_action_name.as_deref() != Some(expected) {
                        continue;
                    }
                }
                if let Some(expected) = filters.agent_id.as_deref() {
                    if record.agent_id.as_deref() != Some(expected) {
                        continue;
                    }
                }
                records.push(record);
            }
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    pub fn read_skill_invocation_failure_cluster(
        &self,
        scope: &LearningScope,
        cluster_id: &str,
    ) -> Result<LearningSkillInvocationFailureCluster> {
        let path = self
            .workspace_layout
            .learning_skill_invocation_failure_cluster_path(
                &scope.principal,
                &scope.workspace,
                cluster_id,
            );
        if !self.path_exists(&path) {
            return Err(anyhow!(
                "learning skill invocation failure cluster `{cluster_id}` was not found"
            ));
        }
        self.read_json(&path)
    }

    pub fn write_skill_invocation_failure_cluster(
        &self,
        cluster: &LearningSkillInvocationFailureCluster,
    ) -> Result<()> {
        let path = self
            .workspace_layout
            .learning_skill_invocation_failure_cluster_path(
                &cluster.scope.principal,
                &cluster.scope.workspace,
                &cluster.id,
            );
        self.write_json_pretty(&path, cluster)
    }

    pub fn create_candidate(
        &self,
        scope: LearningScope,
        request: CreateLearningCandidateRequest,
    ) -> Result<LearningCandidate> {
        let candidate = request.into_candidate(scope)?;
        self.write_candidate(&candidate)?;
        let decision = candidate_created_decision(&candidate);
        self.append_decision(&decision)?;
        Ok(candidate)
    }

    /// Idempotently create a review candidate with a deterministic caller ID.
    /// Replays return the existing byte-equivalent proposal and repair a
    /// missing creation decision after an interrupted first write.
    pub fn ensure_candidate_with_id(
        &self,
        scope: LearningScope,
        request: CreateLearningCandidateRequest,
        candidate_id: &str,
    ) -> Result<LearningCandidate> {
        if !valid_learning_candidate_id(candidate_id) {
            return Err(anyhow!("invalid caller learning candidate id"));
        }
        let mut candidate = request.into_candidate(scope)?;
        candidate.id = candidate_id.to_string();
        let path = self.workspace_layout.learning_candidate_path(
            &candidate.scope.principal,
            &candidate.scope.workspace,
            &candidate.id,
        );
        let lock_path = path.with_extension("json.lock");
        ensure_parent(&lock_path)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening learning candidate lock {}", lock_path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("locking learning candidate {}", candidate.id))?;

        if self.path_exists(&path) {
            let existing: LearningCandidate = self.read_json(&path)?;
            if !learning_candidate_matches_proposal(&existing, &candidate) {
                return Err(anyhow!(
                    "learning candidate id `{}` already exists with different content",
                    candidate.id
                ));
            }
            let decisions = self.workspace_layout.learning_candidate_decisions_path(
                &existing.scope.principal,
                &existing.scope.workspace,
                &existing.id,
            );
            if !self.path_exists(decisions) {
                self.append_decision(&candidate_created_decision(&existing))?;
            }
            return Ok(existing);
        }

        self.write_candidate(&candidate)?;
        if let Err(error) = self.append_decision(&candidate_created_decision(&candidate)) {
            if let Err(cleanup_error) = self.remove_file(&path) {
                return Err(anyhow!(
                    "failed to append creation decision for learning candidate `{}`; rollback \
                     cleanup also failed: {}; original error: {:#}",
                    candidate.id,
                    cleanup_error,
                    error
                ));
            }
            return Err(error).with_context(|| {
                format!(
                    "appending creation decision for learning candidate `{}`; candidate was rolled back",
                    candidate.id
                )
            });
        }
        Ok(candidate)
    }

    pub fn create_procedure(
        &self,
        scope: LearningScope,
        request: CreateLearningProcedureRequest,
    ) -> Result<LearningProcedure> {
        if request.status != LearningProcedureStatus::Draft {
            return Err(anyhow!(
                "new learning procedures must be created as draft; use the status transition endpoint to promote or archive them"
            ));
        }
        let actor = request.actor.clone();
        let reason = request
            .reason
            .clone()
            .unwrap_or_else(|| "Procedure recorded for review and later retrieval.".to_string());
        let procedure = request.into_procedure(scope);
        validate_procedure_id(&procedure.id)?;
        let _procedure_lock = self.acquire_procedure_lock(&procedure.scope, &procedure.id)?;
        if self
            .find_procedure_path(&procedure.scope, &procedure.id)?
            .is_some()
        {
            return Err(anyhow!(
                "learning procedure `{}` already exists in this scope",
                procedure.id
            ));
        }
        self.write_procedure(&procedure)?;
        let procedure_path = self.workspace_layout.learning_procedure_path(
            &procedure.scope.principal,
            &procedure.scope.workspace,
            procedure.status.as_str(),
            &procedure.id,
        );
        let decision = LearningProcedureDecisionLogEntry {
            id: format!("lpd_{}", Uuid::new_v4().simple()),
            procedure_id: procedure.id.clone(),
            scope: procedure.scope.clone(),
            actor,
            from_status: None,
            to_status: procedure.status.clone(),
            decision: "created".to_string(),
            reason,
            evidence_refs: procedure.evidence_refs.clone(),
            created_at: Utc::now(),
        };
        if let Err(error) = self.append_procedure_decision(&decision) {
            if self.path_exists(&procedure_path) {
                if let Err(cleanup_error) = self.remove_file(&procedure_path) {
                    return Err(anyhow!(
                        "failed to append creation decision for learning procedure `{}`; rollback cleanup also failed at {}: {}; original error: {:#}",
                        procedure.id,
                        procedure_path.display(),
                        cleanup_error,
                        error
                    ));
                }
            }
            return Err(error).with_context(|| {
                format!(
                    "appending creation decision for learning procedure `{}`; procedure file was rolled back",
                    procedure.id
                )
            });
        }
        Ok(procedure)
    }

    pub fn list_candidates(
        &self,
        scope: &LearningScope,
        filters: LearningCandidateFilters,
    ) -> Result<Vec<LearningCandidate>> {
        let root = self
            .workspace_layout
            .learning_candidates_dir(&scope.principal, &scope.workspace);
        let mut candidates = Vec::new();
        if !self.path_exists(&root) {
            return Ok(candidates);
        }
        let state_filter = filters.state.as_deref().map(normalize_filter_value);
        let type_filter = filters
            .candidate_type
            .as_deref()
            .map(normalize_filter_value);
        for entry in self
            .read_dir(&root)
            .with_context(|| format!("listing learning candidates at {}", root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let candidate: LearningCandidate = self.read_json(&entry.path())?;
            if let Some(expected) = state_filter.as_deref() {
                if candidate.state.as_str() != expected {
                    continue;
                }
            }
            if let Some(expected) = type_filter.as_deref() {
                if candidate.candidate_type.as_str() != expected {
                    continue;
                }
            }
            if let Some(expected) = filters.source_agent_id.as_deref() {
                if candidate.source_agent_id.as_deref() != Some(expected) {
                    continue;
                }
            }
            candidates.push(candidate);
        }
        candidates.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        if let Some(limit) = filters.limit {
            candidates.truncate(limit);
        }
        Ok(candidates)
    }

    pub fn read_candidate(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<LearningCandidate> {
        let path = self.workspace_layout.learning_candidate_path(
            &scope.principal,
            &scope.workspace,
            candidate_id,
        );
        if !self.path_exists(&path) {
            return Err(anyhow!("learning candidate `{candidate_id}` was not found"));
        }
        self.read_json(&path)
    }

    pub fn list_procedures(
        &self,
        scope: &LearningScope,
        filters: LearningProcedureFilters,
    ) -> Result<Vec<LearningProcedure>> {
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let statuses: Vec<LearningProcedureStatus> =
            if let Some(expected) = status_filter.as_deref() {
                match procedure_status_from_str(expected) {
                    Some(status) => vec![status],
                    None => return Ok(Vec::new()),
                }
            } else {
                LearningProcedureStatus::all().to_vec()
            };
        let mut procedures = Vec::new();
        for status in statuses {
            let root = self.workspace_layout.learning_procedure_status_dir(
                &scope.principal,
                &scope.workspace,
                status.as_str(),
            );
            if !self.path_exists(&root) {
                continue;
            }
            for entry in self
                .read_dir(&root)
                .with_context(|| format!("listing learning procedures at {}", root.display()))?
            {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("yaml") {
                    continue;
                }
                let procedure: LearningProcedure = self.read_yaml(&entry.path())?;
                if let Some(expected) = filters.owner_agent.as_deref() {
                    if procedure.owner_agent.as_deref() != Some(expected) {
                        continue;
                    }
                }
                self.ensure_single_procedure_status(scope, &procedure.id)?;
                procedures.push(procedure);
            }
        }
        procedures.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        if let Some(limit) = filters.limit {
            procedures.truncate(limit);
        }
        Ok(procedures)
    }

    pub fn read_procedure(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<LearningProcedure> {
        let Some(path) = self.find_procedure_path(scope, procedure_id)? else {
            return Err(anyhow!("learning procedure `{procedure_id}` was not found"));
        };
        self.read_yaml(&path)
    }

    pub fn read_procedure_with_decisions(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<Value> {
        let procedure = self.read_procedure(scope, procedure_id)?;
        let decisions = self.read_procedure_decisions(scope, procedure_id)?;
        Ok(serde_json::json!({
            "procedure": procedure,
            "decisions": decisions
        }))
    }

    pub fn transition_procedure_status(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
        to_status: LearningProcedureStatus,
        actor: impl Into<String>,
        decision: impl Into<String>,
        reason: impl Into<String>,
        evidence_refs: Vec<LearningEvidenceRef>,
    ) -> Result<LearningProcedure> {
        let _procedure_lock = self.acquire_procedure_lock(scope, procedure_id)?;
        let Some(from_path) = self.find_procedure_path(scope, procedure_id)? else {
            return Err(anyhow!("learning procedure `{procedure_id}` was not found"));
        };
        let mut procedure: LearningProcedure = self.read_yaml(&from_path)?;
        let original_procedure = procedure.clone();
        let from_status = procedure.status.clone();
        let changes_active_index = from_status != to_status
            && (from_status == LearningProcedureStatus::Active
                || to_status == LearningProcedureStatus::Active);
        procedure.status = to_status.clone();
        procedure.updated_at = Utc::now();
        procedure.version = procedure.version.saturating_add(1);
        let to_path = self.workspace_layout.learning_procedure_path(
            &scope.principal,
            &scope.workspace,
            to_status.as_str(),
            &procedure.id,
        );
        self.write_procedure(&procedure)?;
        if from_path != to_path && self.path_exists(&from_path) {
            if let Err(error) = self.remove_file(&from_path).with_context(|| {
                format!(
                    "removing stale procedure file {} after writing {}",
                    from_path.display(),
                    to_path.display()
                )
            }) {
                if self.path_exists(&to_path) {
                    if let Err(cleanup_error) = self.remove_file(&to_path) {
                        return Err(anyhow!(
                            "failed to remove stale procedure file {} after writing {}; rollback cleanup also failed: {}; original error: {:#}",
                            from_path.display(),
                            to_path.display(),
                            cleanup_error,
                            error
                        ));
                    }
                }
                return Err(error);
            }
        }
        let decision_entry = LearningProcedureDecisionLogEntry {
            id: format!("lpd_{}", Uuid::new_v4().simple()),
            procedure_id: procedure.id.clone(),
            scope: procedure.scope.clone(),
            actor: actor.into(),
            from_status: Some(from_status),
            to_status,
            decision: decision.into(),
            reason: reason.into(),
            evidence_refs,
            created_at: Utc::now(),
        };
        if let Err(error) = self.append_procedure_decision(&decision_entry) {
            if let Err(rollback_error) =
                self.rollback_procedure_transition(&original_procedure, &to_path)
            {
                return Err(anyhow!(
                    "failed to append status decision for learning procedure `{}` and rollback failed: {:#}; original error: {:#}",
                    procedure.id,
                    rollback_error,
                    error
                ));
            }
            return Err(error).with_context(|| {
                format!(
                    "appending status decision for learning procedure `{}`; status transition was rolled back",
                    procedure.id
                )
            });
        }
        if changes_active_index {
            mark_procedure_index_dirty(self, scope);
        }
        Ok(procedure)
    }

    pub fn update_procedure(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
        actor: impl Into<String>,
        decision: impl Into<String>,
        reason: impl Into<String>,
        evidence_refs: Vec<LearningEvidenceRef>,
        update: impl FnOnce(&mut LearningProcedure),
    ) -> Result<LearningProcedure> {
        let _procedure_lock = self.acquire_procedure_lock(scope, procedure_id)?;
        let Some(path) = self.find_procedure_path(scope, procedure_id)? else {
            return Err(anyhow!("learning procedure `{procedure_id}` was not found"));
        };
        let mut procedure: LearningProcedure = self.read_yaml(&path)?;
        let original_procedure = procedure.clone();
        let status = procedure.status.clone();
        update(&mut procedure);
        procedure.status = status.clone();
        procedure.updated_at = Utc::now();
        procedure.version = procedure.version.saturating_add(1);
        let changes_active_index = procedure.status == LearningProcedureStatus::Active
            && procedure_index_content_changed(&original_procedure, &procedure);
        self.write_procedure(&procedure)?;
        let decision_entry = LearningProcedureDecisionLogEntry {
            id: format!("lpd_{}", Uuid::new_v4().simple()),
            procedure_id: procedure.id.clone(),
            scope: procedure.scope.clone(),
            actor: actor.into(),
            from_status: Some(status.clone()),
            to_status: status,
            decision: decision.into(),
            reason: reason.into(),
            evidence_refs,
            created_at: Utc::now(),
        };
        if let Err(error) = self.append_procedure_decision(&decision_entry) {
            if let Err(rollback_error) = self.write_procedure(&original_procedure) {
                return Err(anyhow!(
                    "failed to append update decision for learning procedure `{}` and rollback failed: {:#}; original error: {:#}",
                    procedure.id,
                    rollback_error,
                    error
                ));
            }
            return Err(error).with_context(|| {
                format!(
                    "appending update decision for learning procedure `{}`; content update was rolled back",
                    procedure.id
                )
            });
        }
        if changes_active_index {
            mark_procedure_index_dirty(self, scope);
        }
        Ok(procedure)
    }

    pub fn read_procedure_decisions(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<Vec<LearningProcedureDecisionLogEntry>> {
        let path = self.workspace_layout.learning_procedure_decisions_path(
            &scope.principal,
            &scope.workspace,
            procedure_id,
        );
        self.read_jsonl(&path)
    }

    /// Heuristic check: did this error come from a missing record rather than a
    /// real IO/parse failure? Callers like the bridges want to treat "not yet
    /// persisted" as a soft signal but propagate genuine corruption.
    pub fn error_is_not_found(&self, error: &anyhow::Error) -> bool {
        let message = error.to_string();
        message.contains("was not found") || message.contains("not found")
    }

    pub fn read_candidate_with_decisions(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<Value> {
        let candidate = self.read_candidate(scope, candidate_id)?;
        let decisions = self.read_decisions(scope, candidate_id)?;
        Ok(serde_json::json!({
            "candidate": candidate,
            "decisions": decisions
        }))
    }

    pub fn write_evaluation_backlog_item(
        &self,
        item: &LearningEvaluationBacklogItem,
    ) -> Result<()> {
        let path = self.workspace_layout.learning_evaluation_backlog_path(
            &item.scope.principal,
            &item.scope.workspace,
            &item.candidate_id,
        );
        self.write_json_pretty(&path, item)
    }

    pub fn read_evaluation_backlog_item(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<LearningEvaluationBacklogItem> {
        let path = self.workspace_layout.learning_evaluation_backlog_path(
            &scope.principal,
            &scope.workspace,
            candidate_id,
        );
        if !self.path_exists(&path) {
            return Err(anyhow!(
                "learning evaluation backlog item for candidate `{candidate_id}` was not found"
            ));
        }
        self.read_json(&path)
    }

    pub fn list_evaluation_backlog_items(
        &self,
        scope: &LearningScope,
        filters: LearningEvaluationBacklogFilters,
    ) -> Result<Vec<LearningEvaluationBacklogItem>> {
        let root = self
            .workspace_layout
            .learning_evaluation_backlog_dir(&scope.principal, &scope.workspace);
        let mut items = Vec::new();
        if !self.path_exists(&root) {
            return Ok(items);
        }
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        for entry in self
            .read_dir(&root)
            .with_context(|| format!("listing learning evaluation backlog at {}", root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let item: LearningEvaluationBacklogItem = self.read_json(&entry.path())?;
            if let Some(expected) = status_filter.as_deref() {
                if item.status.as_str() != expected {
                    continue;
                }
            }
            if let Some(expected) = filters.target_agent_id.as_deref() {
                if item.target_agent_id.as_deref() != Some(expected) {
                    continue;
                }
            }
            items.push(item);
        }
        items.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        if let Some(limit) = filters.limit {
            items.truncate(limit);
        }
        Ok(items)
    }

    pub fn write_evaluation_run_report(&self, report: &LearningEvaluationRunReport) -> Result<()> {
        let path = self.workspace_layout.learning_evaluation_run_path(
            &report.scope.principal,
            &report.scope.workspace,
            &report.candidate_id,
            &report.id,
        );
        self.write_json_pretty(&path, report)
    }

    pub fn read_evaluation_run_report(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        run_id: &str,
    ) -> Result<LearningEvaluationRunReport> {
        let path = self.workspace_layout.learning_evaluation_run_path(
            &scope.principal,
            &scope.workspace,
            candidate_id,
            run_id,
        );
        if !self.path_exists(&path) {
            return Err(anyhow!(
                "learning evaluation run report `{run_id}` for candidate `{candidate_id}` was not found"
            ));
        }
        self.read_json(&path)
    }

    pub fn list_evaluation_run_reports(
        &self,
        scope: &LearningScope,
        filters: LearningEvaluationRunFilters,
    ) -> Result<Vec<LearningEvaluationRunReport>> {
        let root = self
            .workspace_layout
            .learning_evaluation_runs_dir(&scope.principal, &scope.workspace);
        let mut reports = Vec::new();
        if !self.path_exists(&root) {
            return Ok(reports);
        }
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let candidate_filter = filters.candidate_id.as_deref();
        let candidate_dirs: Vec<std::path::PathBuf> = if let Some(candidate_id) = candidate_filter {
            vec![self
                .workspace_layout
                .learning_evaluation_candidate_runs_dir(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                )]
        } else {
            let mut dirs = Vec::new();
            for entry in self.read_dir(&root).with_context(|| {
                format!("listing learning evaluation runs at {}", root.display())
            })? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    dirs.push(entry.path());
                }
            }
            dirs
        };
        for dir in candidate_dirs {
            if !self.path_exists(&dir) {
                continue;
            }
            for entry in self.read_dir(&dir).with_context(|| {
                format!(
                    "listing learning evaluation run reports at {}",
                    dir.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let report: LearningEvaluationRunReport = self.read_json(&entry.path())?;
                if let Some(expected) = status_filter.as_deref() {
                    if report.status.as_str() != expected {
                        continue;
                    }
                }
                reports.push(report);
            }
        }
        reports.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            reports.truncate(limit);
        }
        Ok(reports)
    }

    pub fn write_growth_evaluation_run_report(
        &self,
        report: &LearningGrowthEvaluationRunReport,
    ) -> Result<()> {
        let path = self.workspace_layout.learning_growth_evaluation_run_path(
            &report.scope.principal,
            &report.scope.workspace,
            &report.id,
        );
        self.write_json_pretty(&path, report)
    }

    pub fn read_growth_evaluation_run_report(
        &self,
        scope: &LearningScope,
        run_id: &str,
    ) -> Result<LearningGrowthEvaluationRunReport> {
        let path = self.workspace_layout.learning_growth_evaluation_run_path(
            &scope.principal,
            &scope.workspace,
            run_id,
        );
        if !self.path_exists(&path) {
            return Err(anyhow!(
                "learning growth evaluation run report `{run_id}` was not found"
            ));
        }
        self.read_json(&path)
    }

    pub fn list_growth_evaluation_run_reports(
        &self,
        scope: &LearningScope,
        filters: LearningGrowthEvaluationRunFilters,
    ) -> Result<Vec<LearningGrowthEvaluationRunReport>> {
        let root = self
            .workspace_layout
            .learning_growth_evaluation_runs_dir(&scope.principal, &scope.workspace);
        let mut reports = Vec::new();
        if !self.path_exists(&root) {
            return Ok(reports);
        }
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let suite_filter = filters.suite_id.as_deref();
        for entry in self.read_dir(&root).with_context(|| {
            format!(
                "listing learning growth evaluation run reports at {}",
                root.display()
            )
        })? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let report: LearningGrowthEvaluationRunReport = self.read_json(&entry.path())?;
            if let Some(expected) = status_filter.as_deref() {
                if report.status.as_str() != expected {
                    continue;
                }
            }
            if let Some(expected) = suite_filter {
                if report.suite_id != expected {
                    continue;
                }
            }
            reports.push(report);
        }
        reports.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            reports.truncate(limit);
        }
        Ok(reports)
    }

    pub fn write_capability_evolution_backlog_item(
        &self,
        item: &LearningCapabilityEvolutionBacklogItem,
    ) -> Result<()> {
        let path = self.workspace_layout.skill_evolution_backlog_path(
            &item.scope.principal,
            &item.scope.workspace,
            &item.candidate_id,
        );
        self.write_json_pretty(&path, item)
    }

    pub fn read_capability_evolution_backlog_item(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<LearningCapabilityEvolutionBacklogItem> {
        self.read_first_existing_json(
            &[
                self.workspace_layout.skill_evolution_backlog_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                ),
                self.workspace_layout.capability_evolution_backlog_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                ),
            ],
            format!(
                "learning capability-evolution backlog item for candidate `{candidate_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_backlog_items(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionBacklogFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionBacklogItem>> {
        let mut items = Vec::new();
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let type_filter = filters
            .candidate_type
            .as_deref()
            .map(normalize_filter_value);
        for root in [
            self.workspace_layout
                .skill_evolution_backlog_dir(&scope.principal, &scope.workspace),
            self.workspace_layout
                .capability_evolution_backlog_dir(&scope.principal, &scope.workspace),
        ] {
            if !self.path_exists(&root) {
                continue;
            }
            for entry in self.read_dir(&root).with_context(|| {
                format!(
                    "listing learning capability-evolution backlog at {}",
                    root.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let item: LearningCapabilityEvolutionBacklogItem = self.read_json(&entry.path())?;
                items.push(item);
            }
        }
        let mut items = dedupe_by_key(items, |item| item.candidate_id.clone());
        items.retain(|item| {
            status_filter
                .as_deref()
                .map_or(true, |expected| item.status.as_str() == expected)
                && type_filter
                    .as_deref()
                    .map_or(true, |expected| item.candidate_type.as_str() == expected)
                && filters.capability_id.as_deref().map_or(true, |expected| {
                    item.capability_id.as_deref() == Some(expected)
                })
        });
        items.sort_by(|left, right| {
            right
                .rank_score
                .partial_cmp(&left.rank_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
        });
        if let Some(limit) = filters.limit {
            items.truncate(limit);
        }
        Ok(items)
    }

    pub fn write_capability_evolution_proposal(
        &self,
        proposal: &LearningCapabilityEvolutionProposal,
    ) -> Result<()> {
        let path = self.workspace_layout.skill_evolution_proposal_path(
            &proposal.scope.principal,
            &proposal.scope.workspace,
            &proposal.candidate_id,
        );
        self.write_json_pretty(&path, proposal)
    }

    pub fn read_capability_evolution_proposal(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<LearningCapabilityEvolutionProposal> {
        self.read_first_existing_json(
            &[
                self.workspace_layout.skill_evolution_proposal_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                ),
                self.workspace_layout.capability_evolution_proposal_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                ),
            ],
            format!(
                "learning capability-evolution proposal for candidate `{candidate_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_proposals(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionProposalFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionProposal>> {
        let mut proposals = Vec::new();
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        for root in [
            self.workspace_layout
                .skill_evolution_proposals_dir(&scope.principal, &scope.workspace),
            self.workspace_layout
                .capability_evolution_proposals_dir(&scope.principal, &scope.workspace),
        ] {
            if !self.path_exists(&root) {
                continue;
            }
            for entry in self.read_dir(&root).with_context(|| {
                format!(
                    "listing learning capability-evolution proposals at {}",
                    root.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let proposal: LearningCapabilityEvolutionProposal =
                    self.read_json(&entry.path())?;
                proposals.push(proposal);
            }
        }
        let mut proposals = dedupe_by_key(proposals, |proposal| proposal.candidate_id.clone());
        proposals.retain(|proposal| {
            status_filter
                .as_deref()
                .map_or(true, |expected| proposal.status.as_str() == expected)
                && filters.capability_id.as_deref().map_or(true, |expected| {
                    proposal.capability_id.as_deref() == Some(expected)
                })
        });
        proposals.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        if let Some(limit) = filters.limit {
            proposals.truncate(limit);
        }
        Ok(proposals)
    }

    pub fn write_capability_evolution_validation_report(
        &self,
        report: &LearningCapabilityEvolutionValidationReport,
    ) -> Result<()> {
        let path = self.workspace_layout.skill_evolution_validation_path(
            &report.scope.principal,
            &report.scope.workspace,
            &report.candidate_id,
            &report.id,
        );
        self.write_json_pretty(&path, report)
    }

    pub fn read_capability_evolution_validation_report(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        validation_id: &str,
    ) -> Result<LearningCapabilityEvolutionValidationReport> {
        self.read_first_existing_json(
            &[
                self.workspace_layout.skill_evolution_validation_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                    validation_id,
                ),
                self.workspace_layout.capability_evolution_validation_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                    validation_id,
                ),
            ],
            format!(
                "learning capability-evolution validation report `{validation_id}` for candidate `{candidate_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_validation_reports(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionValidationFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionValidationReport>> {
        let mut reports = Vec::new();
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let candidate_filter = filters.candidate_id.as_deref();
        let capability_filter = filters.capability_id.as_deref();
        let candidate_dirs: Vec<std::path::PathBuf> = if let Some(candidate_id) = candidate_filter {
            vec![
                self.workspace_layout
                    .skill_evolution_candidate_validations_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
                self.workspace_layout
                    .capability_evolution_candidate_validations_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
            ]
        } else {
            let mut dirs = Vec::new();
            for root in [
                self.workspace_layout
                    .skill_evolution_validations_dir(&scope.principal, &scope.workspace),
                self.workspace_layout
                    .capability_evolution_validations_dir(&scope.principal, &scope.workspace),
            ] {
                if !self.path_exists(&root) {
                    continue;
                }
                for entry in self.read_dir(&root).with_context(|| {
                    format!(
                        "listing learning capability-evolution validations at {}",
                        root.display()
                    )
                })? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        dirs.push(entry.path());
                    }
                }
            }
            dirs
        };
        for dir in candidate_dirs {
            if !self.path_exists(&dir) {
                continue;
            }
            for entry in self.read_dir(&dir).with_context(|| {
                format!(
                    "listing learning capability-evolution validation reports at {}",
                    dir.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let report: LearningCapabilityEvolutionValidationReport =
                    self.read_json(&entry.path())?;
                reports.push(report);
            }
        }
        let mut reports = dedupe_by_key(reports, |report| {
            (report.candidate_id.clone(), report.id.clone())
        });
        reports.retain(|report| {
            candidate_filter.map_or(true, |expected| report.candidate_id == expected)
                && status_filter
                    .as_deref()
                    .map_or(true, |expected| report.status.as_str() == expected)
                && capability_filter.map_or(true, |expected| {
                    report.capability_id.as_deref() == Some(expected)
                })
        });
        reports.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            reports.truncate(limit);
        }
        Ok(reports)
    }

    pub fn write_capability_evolution_implementation_record(
        &self,
        record: &LearningCapabilityEvolutionImplementationRecord,
    ) -> Result<()> {
        let path = self.workspace_layout.skill_evolution_implementation_path(
            &record.scope.principal,
            &record.scope.workspace,
            &record.candidate_id,
            &record.id,
        );
        self.write_json_pretty(&path, record)
    }

    pub fn read_capability_evolution_implementation_record(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        implementation_id: &str,
    ) -> Result<LearningCapabilityEvolutionImplementationRecord> {
        self.read_first_existing_json(
            &[
                self.workspace_layout.skill_evolution_implementation_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                    implementation_id,
                ),
                self.workspace_layout.capability_evolution_implementation_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                    implementation_id,
                ),
            ],
            format!(
                "learning capability-evolution implementation record `{implementation_id}` for candidate `{candidate_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_implementation_records(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionImplementationFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionImplementationRecord>> {
        let mut records = Vec::new();
        let candidate_filter = filters.candidate_id.as_deref();
        let capability_filter = filters.capability_id.as_deref();
        let candidate_dirs: Vec<std::path::PathBuf> = if let Some(candidate_id) = candidate_filter {
            vec![
                self.workspace_layout
                    .skill_evolution_candidate_implementations_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
                self.workspace_layout
                    .capability_evolution_candidate_implementations_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
            ]
        } else {
            let mut dirs = Vec::new();
            for root in [
                self.workspace_layout
                    .skill_evolution_implementations_dir(&scope.principal, &scope.workspace),
                self.workspace_layout
                    .capability_evolution_implementations_dir(&scope.principal, &scope.workspace),
            ] {
                if !self.path_exists(&root) {
                    continue;
                }
                for entry in self.read_dir(&root).with_context(|| {
                    format!(
                        "listing learning capability-evolution implementations at {}",
                        root.display()
                    )
                })? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        dirs.push(entry.path());
                    }
                }
            }
            dirs
        };
        for dir in candidate_dirs {
            if !self.path_exists(&dir) {
                continue;
            }
            for entry in self.read_dir(&dir).with_context(|| {
                format!(
                    "listing learning capability-evolution implementation records at {}",
                    dir.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let record: LearningCapabilityEvolutionImplementationRecord =
                    self.read_json(&entry.path())?;
                records.push(record);
            }
        }
        let mut records = dedupe_by_key(records, |record| {
            (record.candidate_id.clone(), record.id.clone())
        });
        records.retain(|record| {
            candidate_filter.map_or(true, |expected| record.candidate_id == expected)
                && capability_filter.map_or(true, |expected| {
                    record.capability_id.as_deref() == Some(expected)
                })
        });
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    pub fn write_capability_evolution_application_record(
        &self,
        record: &LearningCapabilityEvolutionApplicationRecord,
    ) -> Result<()> {
        let path = self.workspace_layout.skill_evolution_application_path(
            &record.scope.principal,
            &record.scope.workspace,
            &record.candidate_id,
            &record.id,
        );
        self.write_json_pretty(&path, record)
    }

    pub fn read_capability_evolution_application_record(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        application_id: &str,
    ) -> Result<LearningCapabilityEvolutionApplicationRecord> {
        self.read_first_existing_json(
            &[
                self.workspace_layout.skill_evolution_application_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                    application_id,
                ),
                self.workspace_layout.capability_evolution_application_path(
                    &scope.principal,
                    &scope.workspace,
                    candidate_id,
                    application_id,
                ),
            ],
            format!(
                "learning capability-evolution application record `{application_id}` for candidate `{candidate_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_application_records(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionApplicationFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionApplicationRecord>> {
        let mut records = Vec::new();
        let candidate_filter = filters.candidate_id.as_deref();
        let capability_filter = filters.capability_id.as_deref();
        let candidate_dirs: Vec<std::path::PathBuf> = if let Some(candidate_id) = candidate_filter {
            vec![
                self.workspace_layout
                    .skill_evolution_candidate_applications_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
                self.workspace_layout
                    .capability_evolution_candidate_applications_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
            ]
        } else {
            let mut dirs = Vec::new();
            for root in [
                self.workspace_layout
                    .skill_evolution_applications_dir(&scope.principal, &scope.workspace),
                self.workspace_layout
                    .capability_evolution_applications_dir(&scope.principal, &scope.workspace),
            ] {
                if !self.path_exists(&root) {
                    continue;
                }
                for entry in self.read_dir(&root).with_context(|| {
                    format!(
                        "listing learning capability-evolution applications at {}",
                        root.display()
                    )
                })? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        dirs.push(entry.path());
                    }
                }
            }
            dirs
        };
        for dir in candidate_dirs {
            if !self.path_exists(&dir) {
                continue;
            }
            for entry in self.read_dir(&dir).with_context(|| {
                format!(
                    "listing learning capability-evolution application records at {}",
                    dir.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let record: LearningCapabilityEvolutionApplicationRecord =
                    self.read_json(&entry.path())?;
                records.push(record);
            }
        }
        let mut records = dedupe_by_key(records, |record| {
            (record.candidate_id.clone(), record.id.clone())
        });
        records.retain(|record| {
            candidate_filter.map_or(true, |expected| record.candidate_id == expected)
                && capability_filter.map_or(true, |expected| {
                    record.capability_id.as_deref() == Some(expected)
                })
        });
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    pub fn write_capability_evolution_rollback_recommendation_record(
        &self,
        record: &LearningCapabilityEvolutionRollbackRecommendationRecord,
    ) -> Result<()> {
        let path = self
            .workspace_layout
            .skill_evolution_rollback_recommendation_path(
                &record.scope.principal,
                &record.scope.workspace,
                &record.candidate_id,
                &record.id,
            );
        self.write_json_pretty(&path, record)
    }

    pub fn read_capability_evolution_rollback_recommendation_record(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        recommendation_id: &str,
    ) -> Result<LearningCapabilityEvolutionRollbackRecommendationRecord> {
        self.read_first_existing_json(
            &[
                self.workspace_layout
                    .skill_evolution_rollback_recommendation_path(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                        recommendation_id,
                    ),
                self.workspace_layout
                    .capability_evolution_rollback_recommendation_path(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                        recommendation_id,
                    ),
            ],
            format!(
                "learning capability-evolution rollback recommendation `{recommendation_id}` for candidate `{candidate_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_rollback_recommendation_records(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionRollbackRecommendationFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionRollbackRecommendationRecord>> {
        let mut records = Vec::new();
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        let candidate_filter = filters.candidate_id.as_deref();
        let capability_filter = filters.capability_id.as_deref();
        let application_filter = filters.application_id.as_deref();
        let candidate_dirs: Vec<std::path::PathBuf> = if let Some(candidate_id) = candidate_filter {
            vec![
                self.workspace_layout
                    .skill_evolution_candidate_rollback_recommendations_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
                self.workspace_layout
                    .capability_evolution_candidate_rollback_recommendations_dir(
                        &scope.principal,
                        &scope.workspace,
                        candidate_id,
                    ),
            ]
        } else {
            let mut dirs = Vec::new();
            for root in [
                self.workspace_layout
                    .skill_evolution_rollback_recommendations_dir(
                        &scope.principal,
                        &scope.workspace,
                    ),
                self.workspace_layout
                    .capability_evolution_rollback_recommendations_dir(
                        &scope.principal,
                        &scope.workspace,
                    ),
            ] {
                if !self.path_exists(&root) {
                    continue;
                }
                for entry in self.read_dir(&root).with_context(|| {
                    format!(
                        "listing learning capability-evolution rollback recommendations at {}",
                        root.display()
                    )
                })? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() {
                        dirs.push(entry.path());
                    }
                }
            }
            dirs
        };
        for dir in candidate_dirs {
            if !self.path_exists(&dir) {
                continue;
            }
            for entry in self.read_dir(&dir).with_context(|| {
                format!(
                    "listing learning capability-evolution rollback recommendation records at {}",
                    dir.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let record: LearningCapabilityEvolutionRollbackRecommendationRecord =
                    self.read_json(&entry.path())?;
                records.push(record);
            }
        }
        let mut records = dedupe_by_key(records, |record| {
            (record.candidate_id.clone(), record.id.clone())
        });
        records.retain(|record| {
            candidate_filter.map_or(true, |expected| record.candidate_id == expected)
                && status_filter
                    .as_deref()
                    .map_or(true, |expected| record.status.as_str() == expected)
                && capability_filter.map_or(true, |expected| {
                    record.capability_id.as_deref() == Some(expected)
                })
                && application_filter.map_or(true, |expected| record.application_id == expected)
        });
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    pub fn write_capability_evolution_post_promotion_monitor_record(
        &self,
        record: &LearningCapabilityEvolutionPostPromotionMonitorRecord,
    ) -> Result<()> {
        let path = self
            .workspace_layout
            .skill_evolution_post_promotion_monitor_path(
                &record.scope.principal,
                &record.scope.workspace,
                &record.promotion_id,
            );
        self.write_json_pretty(&path, record)
    }

    pub fn read_capability_evolution_post_promotion_monitor_record(
        &self,
        scope: &LearningScope,
        promotion_id: &str,
    ) -> Result<LearningCapabilityEvolutionPostPromotionMonitorRecord> {
        self.read_first_existing_json(
            &[
                self.workspace_layout
                    .skill_evolution_post_promotion_monitor_path(
                        &scope.principal,
                        &scope.workspace,
                        promotion_id,
                    ),
                self.workspace_layout
                    .capability_evolution_post_promotion_monitor_path(
                        &scope.principal,
                        &scope.workspace,
                        promotion_id,
                    ),
            ],
            format!(
                "learning capability-evolution post-promotion monitor `{promotion_id}` was not found"
            ),
        )
    }

    pub fn list_capability_evolution_post_promotion_monitor_records(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionPostPromotionMonitorFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionPostPromotionMonitorRecord>> {
        let mut records = Vec::new();
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        for root in [
            self.workspace_layout
                .skill_evolution_post_promotion_monitors_dir(&scope.principal, &scope.workspace),
            self.workspace_layout
                .capability_evolution_post_promotion_monitors_dir(
                    &scope.principal,
                    &scope.workspace,
                ),
        ] {
            if !self.path_exists(&root) {
                continue;
            }
            for entry in self.read_dir(&root).with_context(|| {
                format!(
                    "listing learning capability-evolution post-promotion monitors at {}",
                    root.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let record: LearningCapabilityEvolutionPostPromotionMonitorRecord =
                    self.read_json(&entry.path())?;
                records.push(record);
            }
        }
        let mut records = dedupe_by_key(records, |record| record.promotion_id.clone());
        records.retain(|record| {
            status_filter
                .as_deref()
                .map_or(true, |expected| record.status.as_str() == expected)
                && filters
                    .candidate_id
                    .as_deref()
                    .map_or(true, |expected| record.candidate_id == expected)
                && filters.capability_id.as_deref().map_or(true, |expected| {
                    record.capability_id.as_deref() == Some(expected)
                })
                && filters
                    .promotion_id
                    .as_deref()
                    .map_or(true, |expected| record.promotion_id == expected)
        });
        records.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        if let Some(limit) = filters.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    pub fn append_capability_evolution_promotion_record(
        &self,
        record: &LearningCapabilityEvolutionPromotionRecord,
    ) -> Result<()> {
        let path = self
            .workspace_layout
            .skill_evolution_promotion_audit_path(&record.scope.principal, &record.scope.workspace);
        self.append_jsonl(&path, record)
    }

    pub fn read_capability_evolution_promotion_record(
        &self,
        scope: &LearningScope,
        promotion_id: &str,
    ) -> Result<LearningCapabilityEvolutionPromotionRecord> {
        for record in self.read_capability_evolution_promotion_audit_records(scope)? {
            if record.id == promotion_id {
                return Ok(record);
            }
        }
        Err(anyhow!(
            "learning capability-evolution promotion record `{promotion_id}` was not found"
        ))
    }

    pub fn list_capability_evolution_promotion_records(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionPromotionFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionPromotionRecord>> {
        let mut records = self.read_capability_evolution_promotion_audit_records(scope)?;
        if let Some(expected) = filters.candidate_id.as_deref() {
            records.retain(|record| record.candidate_id == expected);
        }
        if let Some(expected) = filters.capability_id.as_deref() {
            records.retain(|record| record.capability_id.as_deref() == Some(expected));
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        if let Some(limit) = filters.limit {
            records.truncate(limit);
        }
        Ok(records)
    }

    fn read_capability_evolution_promotion_audit_records(
        &self,
        scope: &LearningScope,
    ) -> Result<Vec<LearningCapabilityEvolutionPromotionRecord>> {
        let mut records = Vec::new();
        let mut malformed_count = 0usize;
        let mut first_malformed: Option<(PathBuf, usize, String)> = None;
        let mut foreign_count = 0usize;
        let mut first_foreign: Option<(PathBuf, usize, String)> = None;

        for path in [
            self.workspace_layout
                .skill_evolution_promotion_audit_path(&scope.principal, &scope.workspace),
            self.workspace_layout
                .capability_evolution_promotion_audit_path(&scope.principal, &scope.workspace),
        ] {
            if !self.path_exists(&path) {
                continue;
            }
            let content = self
                .workspace_layout
                .read_to_string_path_sync(&path)
                .with_context(|| format!("opening promotion audit at {}", path.display()))?;
            for (idx, line) in content.lines().enumerate() {
                let line_number = idx + 1;
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let value = match serde_json::from_str::<Value>(line) {
                    Ok(value) => value,
                    Err(error) => {
                        malformed_count += 1;
                        if first_malformed.is_none() {
                            first_malformed = Some((path.clone(), line_number, error.to_string()));
                        }
                        continue;
                    },
                };
                match serde_json::from_value::<LearningCapabilityEvolutionPromotionRecord>(value) {
                    Ok(record) => records.push(record),
                    Err(error) => {
                        foreign_count += 1;
                        if first_foreign.is_none() {
                            first_foreign = Some((path.clone(), line_number, error.to_string()));
                        }
                    },
                }
            }
        }
        let records = dedupe_by_key(records, |record| record.id.clone());

        if let Some((path, line, error)) = first_malformed {
            tracing::warn!(
                path = %path.display(),
                skipped_lines = malformed_count,
                first_line = line,
                first_error = %error,
                "skipped malformed promotion audit lines"
            );
        }
        if let Some((path, line, error)) = first_foreign {
            tracing::debug!(
                path = %path.display(),
                skipped_lines = foreign_count,
                first_line = line,
                first_error = %error,
                "skipped non-Skill-Evolution promotion audit lines"
            );
        }

        Ok(records)
    }

    pub fn write_capability_evolution_steward_run_report(
        &self,
        report: &LearningCapabilityEvolutionStewardRunReport,
    ) -> Result<()> {
        let path = self.workspace_layout.skill_evolution_steward_run_path(
            &report.scope.principal,
            &report.scope.workspace,
            &report.id,
        );
        self.write_json_pretty(&path, report)
    }

    pub fn read_capability_evolution_steward_run_report(
        &self,
        scope: &LearningScope,
        run_id: &str,
    ) -> Result<LearningCapabilityEvolutionStewardRunReport> {
        self.read_first_existing_json(
            &[
                self.workspace_layout.skill_evolution_steward_run_path(
                    &scope.principal,
                    &scope.workspace,
                    run_id,
                ),
                self.workspace_layout.capability_evolution_steward_run_path(
                    &scope.principal,
                    &scope.workspace,
                    run_id,
                ),
            ],
            format!("learning capability-evolution steward run `{run_id}` was not found"),
        )
    }

    pub fn list_capability_evolution_steward_run_reports(
        &self,
        scope: &LearningScope,
        filters: LearningCapabilityEvolutionStewardRunFilters,
    ) -> Result<Vec<LearningCapabilityEvolutionStewardRunReport>> {
        let mut reports = Vec::new();
        let status_filter = filters.status.as_deref().map(normalize_filter_value);
        for root in [
            self.workspace_layout
                .skill_evolution_steward_runs_dir(&scope.principal, &scope.workspace),
            self.workspace_layout
                .capability_evolution_steward_runs_dir(&scope.principal, &scope.workspace),
        ] {
            if !self.path_exists(&root) {
                continue;
            }
            for entry in self.read_dir(&root).with_context(|| {
                format!(
                    "listing learning capability-evolution steward runs at {}",
                    root.display()
                )
            })? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let report: LearningCapabilityEvolutionStewardRunReport =
                    self.read_json(&entry.path())?;
                reports.push(report);
            }
        }
        let mut reports = dedupe_by_key(reports, |report| report.id.clone());
        reports.retain(|report| {
            status_filter
                .as_deref()
                .map_or(true, |expected| report.status.as_str() == expected)
        });
        reports.sort_by(|left, right| right.completed_at.cmp(&left.completed_at));
        if let Some(limit) = filters.limit {
            reports.truncate(limit);
        }
        Ok(reports)
    }

    pub fn transition_candidate(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        to_state: LearningCandidateState,
        actor: impl Into<String>,
        decision: impl Into<String>,
        reason: impl Into<String>,
        evidence_refs: Vec<LearningEvidenceRef>,
    ) -> Result<LearningCandidate> {
        let mut candidate = self.read_candidate(scope, candidate_id)?;
        validate_transition(&candidate.state, &to_state)?;
        let original_candidate = candidate.clone();
        let from_state = candidate.state.clone();
        candidate.state = to_state.clone();
        candidate.updated_at = Utc::now();
        self.write_candidate(&candidate)?;
        let decision = LearningDecisionLogEntry {
            id: format!("ld_{}", Uuid::new_v4().simple()),
            candidate_id: candidate.id.clone(),
            scope: candidate.scope.clone(),
            actor: actor.into(),
            from_state: Some(from_state),
            to_state,
            decision: decision.into(),
            reason: reason.into(),
            evidence_refs,
            created_at: Utc::now(),
        };
        if let Err(error) = self.append_decision(&decision) {
            if let Err(rollback_error) = self.write_candidate(&original_candidate) {
                return Err(anyhow!(
                    "failed to append transition decision for learning candidate `{}` and rollback failed: {:#}; original error: {:#}",
                    candidate.id,
                    rollback_error,
                    error
                ));
            }
            return Err(error).with_context(|| {
                format!(
                    "appending transition decision for learning candidate `{}`; state transition was rolled back",
                    candidate.id
                )
            });
        }
        Ok(candidate)
    }

    pub fn revise_memory_candidate_value(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        revised_value: impl Into<String>,
        actor: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<LearningCandidate> {
        let revised_value = revised_value.into().trim().to_string();
        if revised_value.is_empty() {
            return Err(anyhow!("revised memory value cannot be empty"));
        }
        self.revise_memory_candidate_json_value(
            scope,
            candidate_id,
            Value::String(revised_value.clone()),
            revised_value,
            actor,
            reason,
        )
    }

    pub fn revise_memory_candidate_json_value(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
        revised_value: Value,
        revised_summary: impl Into<String>,
        actor: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<LearningCandidate> {
        let mut candidate = self.read_candidate(scope, candidate_id)?;
        if !candidate.candidate_type.is_memory_candidate() {
            return Err(anyhow!(
                "candidate `{}` is `{}`; only memory candidates can be edited from the learning feed",
                candidate.id,
                candidate.candidate_type.as_str()
            ));
        }
        if candidate.state.is_terminal() {
            return Err(anyhow!(
                "candidate `{}` is terminal in state `{}` and cannot be edited from the learning feed",
                candidate.id,
                candidate.state.as_str()
            ));
        }

        let revised_summary = revised_summary.into().trim().to_string();
        if revised_summary.is_empty() {
            return Err(anyhow!("revised memory summary cannot be empty"));
        }
        let actor = actor.into();
        let reason = reason.into();
        let original_candidate = candidate.clone();
        let state = candidate.state.clone();
        let now = Utc::now();

        let payload = memory_payload_object_mut(&mut candidate.proposed_change)?;
        let previous_value = payload.get("value").cloned();
        payload.insert("value".to_string(), revised_value);
        payload.insert(
            "feed_revision".to_string(),
            serde_json::json!({
                "actor": actor.clone(),
                "reason": reason.clone(),
                "previous_value": previous_value,
                "revised_at": now,
            }),
        );

        candidate.summary = revised_summary;
        candidate.updated_at = now;
        self.write_candidate(&candidate)?;
        let decision_entry = LearningDecisionLogEntry {
            id: format!("ld_{}", Uuid::new_v4().simple()),
            candidate_id: candidate.id.clone(),
            scope: candidate.scope.clone(),
            actor,
            from_state: Some(state.clone()),
            to_state: state,
            decision: "feed_memory_value_revised".to_string(),
            reason,
            evidence_refs: candidate.evidence_refs.clone(),
            created_at: Utc::now(),
        };
        if let Err(error) = self.append_decision(&decision_entry) {
            if let Err(rollback_error) = self.write_candidate(&original_candidate) {
                return Err(anyhow!(
                    "failed to append edit decision for learning candidate `{}` and rollback failed: {:#}; original error: {:#}",
                    candidate.id,
                    rollback_error,
                    error
                ));
            }
            return Err(error).with_context(|| {
                format!(
                    "appending edit decision for learning candidate `{}`; content edit was rolled back",
                    candidate.id
                )
            });
        }
        Ok(candidate)
    }

    pub fn read_decisions(
        &self,
        scope: &LearningScope,
        candidate_id: &str,
    ) -> Result<Vec<LearningDecisionLogEntry>> {
        let path = self.workspace_layout.learning_candidate_decisions_path(
            &scope.principal,
            &scope.workspace,
            candidate_id,
        );
        self.read_jsonl(&path)
    }

    pub fn list_events(
        &self,
        scope: &LearningScope,
        max_lines: usize,
    ) -> Result<Vec<LearningEvent>> {
        let root = self
            .workspace_layout
            .learning_events_dir(&scope.principal, &scope.workspace);
        if !self.path_exists(&root) {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in self
            .read_dir(&root)
            .with_context(|| format!("listing learning events at {}", root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            {
                files.push(entry.path());
            }
        }
        files.sort();
        files.reverse();
        let mut events = Vec::new();
        for path in files {
            let mut file_events: Vec<LearningEvent> = self.read_jsonl(&path)?;
            file_events.reverse();
            for event in file_events {
                if events.len() >= max_lines {
                    return Ok(events);
                }
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn has_completed_reflection_for_episode(
        &self,
        scope: &LearningScope,
        episode_id: &str,
        boundary: &str,
    ) -> Result<bool> {
        let root = self
            .workspace_layout
            .learning_events_dir(&scope.principal, &scope.workspace);
        if !self.path_exists(&root) {
            return Ok(false);
        }
        for entry in self
            .read_dir(&root)
            .with_context(|| format!("listing learning events at {}", root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            {
                for event in self.read_jsonl::<LearningEvent>(&entry.path())? {
                    if event.event_type != "learning_reflection_completed" {
                        continue;
                    }
                    let same_episode =
                        event.payload.get("episode_id").and_then(Value::as_str) == Some(episode_id);
                    let same_boundary =
                        event.payload.get("boundary").and_then(Value::as_str) == Some(boundary);
                    if same_episode && same_boundary {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }

    pub fn count_candidates(&self, scope: &LearningScope) -> usize {
        self.count_files_with_extension(
            &self
                .workspace_layout
                .learning_candidates_dir(&scope.principal, &scope.workspace),
            "json",
        )
    }

    pub fn count_event_records(&self, scope: &LearningScope) -> usize {
        let root = self
            .workspace_layout
            .learning_events_dir(&scope.principal, &scope.workspace);
        if !self.path_exists(&root) {
            return 0;
        }
        let mut count = 0usize;
        let Ok(entries) = self.read_dir(root) else {
            return 0;
        };
        for entry in entries.into_iter().flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            count += self
                .workspace_layout
                .read_to_string_path_sync(entry.path())
                .map(|content| content.lines().count())
                .unwrap_or(0);
        }
        count
    }

    pub fn count_skill_invocation_evidence(&self, scope: &LearningScope) -> usize {
        let root = self
            .workspace_layout
            .learning_skill_invocations_dir(&scope.principal, &scope.workspace);
        if !self.path_exists(&root) {
            return 0;
        }
        let Ok(day_dirs) = self.read_dir(root) else {
            return 0;
        };
        day_dirs
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                if entry
                    .file_type()
                    .ok()
                    .is_some_and(|file_type| file_type.is_dir())
                {
                    Some(self.count_files_with_extension(&entry.path(), "json"))
                } else {
                    None
                }
            })
            .sum()
    }

    pub fn count_evaluation_backlog_items(&self, scope: &LearningScope) -> usize {
        self.count_files_with_extension(
            &self
                .workspace_layout
                .learning_evaluation_backlog_dir(&scope.principal, &scope.workspace),
            "json",
        )
    }

    pub fn count_evaluation_run_reports(&self, scope: &LearningScope) -> usize {
        let root = self
            .workspace_layout
            .learning_evaluation_runs_dir(&scope.principal, &scope.workspace);
        if !self.path_exists(&root) {
            return 0;
        }
        let mut count = 0usize;
        let Ok(candidate_dirs) = self.read_dir(root) else {
            return 0;
        };
        for candidate_dir in candidate_dirs.into_iter().flatten() {
            let Ok(file_type) = candidate_dir.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            count += self.count_files_with_extension(&candidate_dir.path(), "json");
        }
        count
    }

    pub fn count_growth_evaluation_run_reports(&self, scope: &LearningScope) -> usize {
        self.count_files_with_extension(
            &self
                .workspace_layout
                .learning_growth_evaluation_runs_dir(&scope.principal, &scope.workspace),
            "json",
        )
    }

    pub fn count_procedures(&self, scope: &LearningScope) -> usize {
        LearningProcedureStatus::all()
            .iter()
            .map(|status| {
                self.count_files_with_extension(
                    &self.workspace_layout.learning_procedure_status_dir(
                        &scope.principal,
                        &scope.workspace,
                        status.as_str(),
                    ),
                    "yaml",
                )
            })
            .sum()
    }

    pub fn count_capability_evolution_backlog_items(&self, scope: &LearningScope) -> usize {
        self.list_capability_evolution_backlog_items(
            scope,
            LearningCapabilityEvolutionBacklogFilters::default(),
        )
        .map(|items| items.len())
        .unwrap_or(0)
    }

    pub fn count_capability_evolution_proposals(&self, scope: &LearningScope) -> usize {
        self.list_capability_evolution_proposals(
            scope,
            LearningCapabilityEvolutionProposalFilters::default(),
        )
        .map(|proposals| proposals.len())
        .unwrap_or(0)
    }

    pub fn count_capability_evolution_validation_reports(&self, scope: &LearningScope) -> usize {
        self.list_capability_evolution_validation_reports(
            scope,
            LearningCapabilityEvolutionValidationFilters::default(),
        )
        .map(|reports| reports.len())
        .unwrap_or(0)
    }

    pub fn count_capability_evolution_implementation_records(
        &self,
        scope: &LearningScope,
    ) -> usize {
        self.list_capability_evolution_implementation_records(
            scope,
            LearningCapabilityEvolutionImplementationFilters::default(),
        )
        .map(|records| records.len())
        .unwrap_or(0)
    }

    pub fn count_capability_evolution_application_records(&self, scope: &LearningScope) -> usize {
        self.list_capability_evolution_application_records(
            scope,
            LearningCapabilityEvolutionApplicationFilters::default(),
        )
        .map(|records| records.len())
        .unwrap_or(0)
    }

    pub fn count_capability_evolution_promotion_records(&self, scope: &LearningScope) -> usize {
        self.read_capability_evolution_promotion_audit_records(scope)
            .map(|records| records.len())
            .unwrap_or(0)
    }

    fn event_log_path(&self, scope: &LearningScope, date: String) -> std::path::PathBuf {
        self.workspace_layout
            .learning_events_dir(&scope.principal, &scope.workspace)
            .join(format!("{date}.jsonl"))
    }

    fn skill_invocation_evidence_path(
        &self,
        evidence: &LearningSkillInvocationEvidence,
    ) -> PathBuf {
        self.workspace_layout.learning_skill_invocation_path(
            &evidence.scope.principal,
            &evidence.scope.workspace,
            &evidence.created_at.date_naive().to_string(),
            &evidence.id,
        )
    }

    fn record_skill_invocation_failure_cluster(
        &self,
        evidence: &LearningSkillInvocationEvidence,
        evidence_ref: LearningEvidenceRef,
    ) -> Result<()> {
        let Some(failure_class) = evidence.failure_class.clone() else {
            return Ok(());
        };
        let Some(candidate_type) = candidate_type_for_skill_invocation_failure(&failure_class)
        else {
            return Ok(());
        };
        let cluster_id = skill_invocation_failure_cluster_id(evidence);
        let _lock =
            self.acquire_skill_invocation_failure_cluster_lock(&evidence.scope, &cluster_id)?;
        let now = Utc::now();
        let mut cluster =
            match self.read_skill_invocation_failure_cluster(&evidence.scope, &cluster_id) {
                Ok(cluster) => cluster,
                Err(error) if self.error_is_not_found(&error) => {
                    LearningSkillInvocationFailureCluster {
                        id: cluster_id.clone(),
                        scope: evidence.scope.clone(),
                        cluster_key: skill_invocation_failure_cluster_key(evidence),
                        source: evidence.source.clone(),
                        skill_name: evidence.skill_name.clone(),
                        tool_action_name: evidence.tool_action_name.clone(),
                        failure_class,
                        input_fingerprint: evidence.input_fingerprint.clone(),
                        occurrence_count: 0,
                        first_seen_at: evidence.created_at,
                        last_seen_at: evidence.created_at,
                        last_error_summary: None,
                        last_result_summary: None,
                        candidate_id: None,
                        routed_at: None,
                        route_reason: None,
                        evidence_refs: Vec::new(),
                        payload: serde_json::json!({
                            "route_threshold": SKILL_INVOCATION_FAILURE_CLUSTER_ROUTE_THRESHOLD,
                        }),
                    }
                },
                Err(error) => return Err(error),
            };

        cluster.occurrence_count = cluster.occurrence_count.saturating_add(1);
        cluster.last_seen_at = now;
        cluster.last_error_summary = evidence.error_summary.clone();
        cluster.last_result_summary = evidence.result_summary.clone();
        if !cluster
            .evidence_refs
            .iter()
            .any(|existing| existing.id == evidence_ref.id)
        {
            cluster.evidence_refs.push(evidence_ref);
            if cluster.evidence_refs.len() > MAX_FAILURE_CLUSTER_EVIDENCE_REFS {
                let excess = cluster
                    .evidence_refs
                    .len()
                    .saturating_sub(MAX_FAILURE_CLUSTER_EVIDENCE_REFS);
                cluster.evidence_refs.drain(0..excess);
            }
        }
        cluster.payload = serde_json::json!({
            "route_threshold": SKILL_INVOCATION_FAILURE_CLUSTER_ROUTE_THRESHOLD,
            "last_invocation_id": evidence.id,
            "last_task_id": evidence.task_id,
            "last_execution_id": evidence.execution_id,
            "last_chat_session_id": evidence.chat_session_id,
            "last_agent_id": evidence.agent_id,
        });

        if cluster.occurrence_count >= SKILL_INVOCATION_FAILURE_CLUSTER_ROUTE_THRESHOLD {
            self.ensure_failure_cluster_routed(&mut cluster, evidence, candidate_type)?;
        }
        self.write_skill_invocation_failure_cluster(&cluster)
    }

    fn ensure_failure_cluster_routed(
        &self,
        cluster: &mut LearningSkillInvocationFailureCluster,
        evidence: &LearningSkillInvocationEvidence,
        candidate_type: LearningCandidateType,
    ) -> Result<()> {
        if let Some(candidate_id) = cluster.candidate_id.clone() {
            if cluster.routed_at.is_some()
                || self
                    .read_capability_evolution_backlog_item(&cluster.scope, &candidate_id)
                    .is_ok()
            {
                return Ok(());
            }
            let candidate = self.read_candidate(&cluster.scope, &candidate_id)?;
            return self.route_failure_cluster_candidate(cluster, &candidate);
        }

        let cluster_path = self
            .workspace_layout
            .learning_skill_invocation_failure_cluster_path(
                &cluster.scope.principal,
                &cluster.scope.workspace,
                &cluster.id,
            );
        let mut evidence_refs = cluster.evidence_refs.clone();
        evidence_refs.push(LearningEvidenceRef {
            kind: "skill_invocation_failure_cluster".to_string(),
            id: Some(cluster.id.clone()),
            path: Some(cluster_path.to_string_lossy().to_string()),
            uri: None,
            summary: Some(format!(
                "{} repeated `{}` failure(s) for `{}`",
                cluster.occurrence_count,
                cluster.failure_class.as_str(),
                skill_action_label(&cluster.skill_name, cluster.tool_action_name.as_deref())
            )),
        });

        let proposed_fix_type =
            proposed_fix_type_for_skill_invocation_failure(&cluster.failure_class);
        let candidate = self.create_candidate(
            cluster.scope.clone(),
            CreateLearningCandidateRequest {
                principal: None,
                workspace: None,
                candidate_type,
                state: LearningCandidateState::Proposed,
                title: format!(
                    "Fix repeated {} failure for `{}`",
                    cluster.failure_class.as_str(),
                    skill_action_label(&cluster.skill_name, cluster.tool_action_name.as_deref())
                ),
                summary: format!(
                    "The `{}` invocation has failed {} times with `{}`. Use the linked invocation evidence to update the skill/tool surface.",
                    skill_action_label(&cluster.skill_name, cluster.tool_action_name.as_deref()),
                    cluster.occurrence_count,
                    cluster.failure_class.as_str()
                ),
                rationale: "Repeated live invocation evidence is a stronger signal than one-off reflection text and should be reviewed as a Skill Evolution item.".to_string(),
                proposed_change: serde_json::json!({
                    "capability_evolution": {
                        "source": "skill_invocation_failure_cluster",
                        "skill_invocation_cluster_id": cluster.id,
                        "skill_invocation_cluster_key": cluster.cluster_key,
                        "capability_id": cluster.skill_name,
                        "skill_name": cluster.skill_name,
                        "tool_action_name": cluster.tool_action_name,
                        "source_surface": cluster.source.as_str(),
                        "failure_class": cluster.failure_class.as_str(),
                        "failure_pattern": format!(
                            "Repeated `{}` failure for `{}`",
                            cluster.failure_class.as_str(),
                            skill_action_label(&cluster.skill_name, cluster.tool_action_name.as_deref())
                        ),
                        "proposed_fix_type": proposed_fix_type,
                        "input_fingerprint": cluster.input_fingerprint,
                        "occurrence_count": cluster.occurrence_count,
                        "last_error_summary": cluster.last_error_summary,
                        "last_result_summary": cluster.last_result_summary,
                        "required_eval": {
                            "goal": "Replay or smoke-test the failing skill/tool shape using redacted fixture inputs and confirm the failure class is resolved.",
                            "source": "skill_invocation_failure_cluster"
                        },
                        "promotion_gate": {
                            "requires_operator_review": true,
                            "requires_validation": true,
                            "source": "skill_invocation_failure_cluster"
                        },
                        "expected_behavior": "The same skill/tool invocation shape should complete without the observed repeated failure class."
                    }
                }),
                proposed_target: Some(cluster.skill_name.clone()),
                confidence: Some((0.66 + (cluster.occurrence_count as f64 * 0.06)).min(0.92)),
                source_agent_id: evidence.agent_id.clone(),
                source_task_id: evidence.task_id.clone(),
                source_execution_id: evidence.execution_id.clone(),
                source_chat_session_id: evidence.chat_session_id.clone(),
                event_refs: Vec::new(),
                evidence_refs,
                risk_level: risk_level_for_skill_invocation_failure(&cluster.failure_class),
                review_required: true,
                review_reason: Some(
                    "Skill Evolution candidates from repeated live failures require review before any skill/schema/wrapper change is applied.".to_string(),
                ),
                review_policy: serde_json::json!({
                    "requires_review": true,
                    "source": "skill_invocation_failure_cluster"
                }),
                promotion_target: None,
                promotion_policy: serde_json::json!({
                    "eligible_for_auto_promotion": false,
                    "requires_validation": true,
                    "source": "skill_invocation_failure_cluster"
                }),
            },
        )?;
        cluster.candidate_id = Some(candidate.id.clone());
        self.write_skill_invocation_failure_cluster(cluster)?;
        self.route_failure_cluster_candidate(cluster, &candidate)
    }

    fn route_failure_cluster_candidate(
        &self,
        cluster: &mut LearningSkillInvocationFailureCluster,
        candidate: &LearningCandidate,
    ) -> Result<()> {
        let outcome = LearningCapabilityEvolutionBridge::new(self.workspace_layout.clone())
            .route_candidate(self, &cluster.scope, candidate)?;
        cluster.route_reason = Some(outcome.reason);
        if outcome.routed {
            cluster.routed_at = Some(Utc::now());
        }
        Ok(())
    }

    fn acquire_skill_invocation_failure_cluster_lock(
        &self,
        scope: &LearningScope,
        cluster_id: &str,
    ) -> Result<fs::File> {
        let path = self
            .workspace_layout
            .learning_skill_invocation_failure_cluster_lock_path(
                &scope.principal,
                &scope.workspace,
                cluster_id,
            );
        ensure_parent(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "opening skill invocation failure cluster lock {}",
                    path.display()
                )
            })?;
        file.lock_exclusive().with_context(|| {
            format!(
                "locking skill invocation failure cluster lock {}",
                path.display()
            )
        })?;
        Ok(file)
    }

    fn write_candidate(&self, candidate: &LearningCandidate) -> Result<()> {
        let path = self.workspace_layout.learning_candidate_path(
            &candidate.scope.principal,
            &candidate.scope.workspace,
            &candidate.id,
        );
        self.write_json_pretty(&path, candidate)
    }

    fn find_procedure_path(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<Option<PathBuf>> {
        let matches = self.find_procedure_paths(scope, procedure_id)?;
        match matches.as_slice() {
            [] => Ok(None),
            [(_, path)] => Ok(Some(path.clone())),
            _ => Err(duplicate_procedure_status_error(procedure_id, &matches)),
        }
    }

    fn find_procedure_paths(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<Vec<(LearningProcedureStatus, PathBuf)>> {
        validate_procedure_id(procedure_id)?;
        let mut matches = Vec::new();
        for status in LearningProcedureStatus::all() {
            let path = self.workspace_layout.learning_procedure_path(
                &scope.principal,
                &scope.workspace,
                status.as_str(),
                procedure_id,
            );
            if self.path_exists(&path) {
                matches.push((status, path));
            }
        }
        Ok(matches)
    }

    fn ensure_single_procedure_status(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<()> {
        let matches = self.find_procedure_paths(scope, procedure_id)?;
        if matches.len() <= 1 {
            return Ok(());
        }
        Err(duplicate_procedure_status_error(procedure_id, &matches))
    }

    fn acquire_procedure_lock(
        &self,
        scope: &LearningScope,
        procedure_id: &str,
    ) -> Result<fs::File> {
        validate_procedure_id(procedure_id)?;
        let path = self
            .workspace_layout
            .learning_procedure_decisions_dir(&scope.principal, &scope.workspace)
            .join(format!(".{procedure_id}.lock"));
        ensure_parent(&path)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening procedure lock {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("locking procedure lock {}", path.display()))?;
        Ok(file)
    }

    fn write_procedure(&self, procedure: &LearningProcedure) -> Result<()> {
        validate_procedure_id(&procedure.id)?;
        let path = self.workspace_layout.learning_procedure_path(
            &procedure.scope.principal,
            &procedure.scope.workspace,
            procedure.status.as_str(),
            &procedure.id,
        );
        self.write_yaml_pretty(&path, procedure)
    }

    fn rollback_procedure_transition(
        &self,
        original_procedure: &LearningProcedure,
        new_path: &Path,
    ) -> Result<()> {
        self.write_procedure(original_procedure)?;
        let original_path = self.workspace_layout.learning_procedure_path(
            &original_procedure.scope.principal,
            &original_procedure.scope.workspace,
            original_procedure.status.as_str(),
            &original_procedure.id,
        );
        if new_path != original_path.as_path() && self.path_exists(&new_path) {
            self.remove_file(new_path).with_context(|| {
                format!(
                    "removing rolled-back procedure file {} after restoring {}",
                    new_path.display(),
                    original_path.display()
                )
            })?;
        }
        Ok(())
    }

    fn append_decision(&self, decision: &LearningDecisionLogEntry) -> Result<()> {
        let path = self.workspace_layout.learning_candidate_decisions_path(
            &decision.scope.principal,
            &decision.scope.workspace,
            &decision.candidate_id,
        );
        self.append_jsonl(&path, decision)
    }

    fn append_procedure_decision(
        &self,
        decision: &LearningProcedureDecisionLogEntry,
    ) -> Result<()> {
        let path = self.workspace_layout.learning_procedure_decisions_path(
            &decision.scope.principal,
            &decision.scope.workspace,
            &decision.procedure_id,
        );
        self.append_jsonl(&path, decision)
    }
}

fn validate_transition(from: &LearningCandidateState, to: &LearningCandidateState) -> Result<()> {
    if from == to {
        return Ok(());
    }
    if from.is_terminal() {
        return Err(anyhow!(
            "learning candidate is terminal in state `{}` and cannot transition to `{}`",
            from.as_str(),
            to.as_str()
        ));
    }
    Ok(())
}

fn memory_payload_object_mut(
    proposed_change: &mut Value,
) -> Result<&mut serde_json::Map<String, Value>> {
    if proposed_change
        .as_object()
        .is_some_and(|root| root.get("memory").is_some())
    {
        let memory = proposed_change
            .get_mut("memory")
            .ok_or_else(|| anyhow!("memory proposed_change payload is missing"))?;
        if !memory.is_object() {
            let previous = std::mem::replace(memory, Value::Object(serde_json::Map::new()));
            if let Some(object) = memory.as_object_mut() {
                object.insert("previous_memory_payload".to_string(), previous);
            }
        }
        return proposed_change
            .get_mut("memory")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow!("memory proposed_change payload is not an object"));
    }
    if !proposed_change.is_object() {
        let previous = std::mem::replace(proposed_change, Value::Object(serde_json::Map::new()));
        if let Some(object) = proposed_change.as_object_mut() {
            object.insert("previous_proposed_change".to_string(), previous);
        }
    }
    proposed_change
        .as_object_mut()
        .ok_or_else(|| anyhow!("memory proposed_change payload is not an object"))
}

fn normalize_filter_value(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn procedure_status_from_str(value: &str) -> Option<LearningProcedureStatus> {
    match normalize_filter_value(value).as_str() {
        "draft" => Some(LearningProcedureStatus::Draft),
        "active" => Some(LearningProcedureStatus::Active),
        "deprecated" => Some(LearningProcedureStatus::Deprecated),
        "archived" => Some(LearningProcedureStatus::Archived),
        _ => None,
    }
}

fn duplicate_procedure_status_error(
    procedure_id: &str,
    matches: &[(LearningProcedureStatus, PathBuf)],
) -> anyhow::Error {
    let locations = matches
        .iter()
        .map(|(status, path)| format!("{}:{}", status.as_str(), path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    anyhow!(
        "learning procedure `{procedure_id}` exists in multiple status directories: {locations}"
    )
}

fn validate_procedure_id(procedure_id: &str) -> Result<()> {
    let trimmed = procedure_id.trim();
    if trimmed.is_empty() || trimmed.len() > 160 {
        return Err(anyhow!(
            "learning procedure id must be non-empty and at most 160 characters"
        ));
    }
    let valid = trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if !valid {
        return Err(anyhow!(
            "learning procedure id `{procedure_id}` may contain only ASCII letters, numbers, `_`, and `-`"
        ));
    }
    Ok(())
}

fn valid_learning_candidate_id(candidate_id: &str) -> bool {
    candidate_id.starts_with("lc_")
        && candidate_id.len() <= 128
        && candidate_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn learning_candidate_matches_proposal(
    existing: &LearningCandidate,
    proposed: &LearningCandidate,
) -> bool {
    existing.scope == proposed.scope
        && existing.candidate_type == proposed.candidate_type
        && existing.state == proposed.state
        && existing.title == proposed.title
        && existing.summary == proposed.summary
        && existing.rationale == proposed.rationale
        && existing.proposed_change == proposed.proposed_change
        && existing.proposed_target == proposed.proposed_target
        && existing.confidence == proposed.confidence
        && existing.source_agent_id == proposed.source_agent_id
        && existing.source_task_id == proposed.source_task_id
        && existing.source_execution_id == proposed.source_execution_id
        && existing.source_chat_session_id == proposed.source_chat_session_id
        && existing.event_refs == proposed.event_refs
        && existing.evidence_refs == proposed.evidence_refs
        && existing.risk_level == proposed.risk_level
        && existing.review_required == proposed.review_required
        && existing.review_reason == proposed.review_reason
        && existing.review_policy == proposed.review_policy
        && existing.promotion_target == proposed.promotion_target
        && existing.promotion_policy == proposed.promotion_policy
}

fn candidate_created_decision(candidate: &LearningCandidate) -> LearningDecisionLogEntry {
    LearningDecisionLogEntry {
        id: format!("ld_{}", Uuid::new_v4().simple()),
        candidate_id: candidate.id.clone(),
        scope: candidate.scope.clone(),
        actor: "system".to_string(),
        from_state: None,
        to_state: candidate.state.clone(),
        decision: "created".to_string(),
        reason: "Candidate recorded for review; no promotion action was applied.".to_string(),
        evidence_refs: candidate.evidence_refs.clone(),
        created_at: Utc::now(),
    }
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    Ok(())
}

fn dedupe_by_key<T, K, F>(records: Vec<T>, mut key_fn: F) -> Vec<T>
where
    K: Eq + Hash,
    F: FnMut(&T) -> K,
{
    let mut seen = HashSet::new();
    records
        .into_iter()
        .filter(|record| seen.insert(key_fn(record)))
        .collect()
}

fn skill_invocation_failure_cluster_key(evidence: &LearningSkillInvocationEvidence) -> String {
    let failure_class = evidence
        .failure_class
        .as_ref()
        .map(LearningSkillInvocationFailureClass::as_str)
        .unwrap_or("unknown");
    format!(
        "{}|{}|{}|{}|{}",
        evidence.source.as_str(),
        evidence.skill_name.trim(),
        evidence
            .tool_action_name
            .as_deref()
            .map(str::trim)
            .unwrap_or(""),
        failure_class,
        evidence.input_fingerprint.trim()
    )
}

fn skill_invocation_failure_cluster_id(evidence: &LearningSkillInvocationEvidence) -> String {
    let key = skill_invocation_failure_cluster_key(evidence);
    let mut hasher = Sha256::new();
    hasher.update(b"magician-learning-skill-invocation-failure-cluster-v1\0");
    hasher.update(key.as_bytes());
    format!("lsic_{:x}", hasher.finalize())
}

fn skill_action_label(skill_name: &str, action_name: Option<&str>) -> String {
    match action_name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(action) => format!("{skill_name}::{action}"),
        None => skill_name.to_string(),
    }
}

fn candidate_type_for_skill_invocation_failure(
    failure_class: &LearningSkillInvocationFailureClass,
) -> Option<LearningCandidateType> {
    match failure_class {
        LearningSkillInvocationFailureClass::BadSchema => {
            Some(LearningCandidateType::ToolSchemaUpdate)
        },
        LearningSkillInvocationFailureClass::WrapperCrash
        | LearningSkillInvocationFailureClass::ParseFailure
        | LearningSkillInvocationFailureClass::AuthFailure
        | LearningSkillInvocationFailureClass::MissingEnvConfig
        | LearningSkillInvocationFailureClass::NetworkServiceFailure
        | LearningSkillInvocationFailureClass::Timeout => {
            Some(LearningCandidateType::ToolWrapperFix)
        },
        LearningSkillInvocationFailureClass::ToolMisuse
        | LearningSkillInvocationFailureClass::CapabilityUnavailable => {
            Some(LearningCandidateType::SkillUpdate)
        },
        LearningSkillInvocationFailureClass::ResourceAuthorityDenied
        | LearningSkillInvocationFailureClass::UserDeniedOrHitlBlocked
        | LearningSkillInvocationFailureClass::Cancelled
        | LearningSkillInvocationFailureClass::Unknown => None,
    }
}

fn proposed_fix_type_for_skill_invocation_failure(
    failure_class: &LearningSkillInvocationFailureClass,
) -> &'static str {
    match failure_class {
        LearningSkillInvocationFailureClass::BadSchema => "tool_schema",
        LearningSkillInvocationFailureClass::MissingEnvConfig
        | LearningSkillInvocationFailureClass::AuthFailure => "env_or_auth_setup",
        LearningSkillInvocationFailureClass::ToolMisuse
        | LearningSkillInvocationFailureClass::CapabilityUnavailable => "skill_guidance",
        _ => "wrapper_script",
    }
}

fn risk_level_for_skill_invocation_failure(
    failure_class: &LearningSkillInvocationFailureClass,
) -> LearningRiskLevel {
    match failure_class {
        LearningSkillInvocationFailureClass::WrapperCrash
        | LearningSkillInvocationFailureClass::AuthFailure
        | LearningSkillInvocationFailureClass::MissingEnvConfig => LearningRiskLevel::High,
        _ => LearningRiskLevel::Medium,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::learning::{
        LearningCapabilityEvolutionBacklogStatus, LearningSkillInvocationFailureClass,
        LearningSkillInvocationSource, LearningSkillInvocationStatus,
    };

    #[test]
    fn skill_invocation_evidence_round_trips_and_appends_event() {
        let tmp = tempfile::tempdir().unwrap();
        let store = LearningStore::new(ArtifactV2Workspace::new(tmp.path()));
        let scope = LearningScope::new("anonymous", "default");

        let evidence = store
            .record_skill_invocation_evidence(
                scope.clone(),
                CreateLearningSkillInvocationEvidenceRequest {
                    source: LearningSkillInvocationSource::CompiledPack,
                    skill_name: "search_memory".to_string(),
                    tool_action_name: None,
                    agent_id: Some("agent-1".to_string()),
                    task_id: Some("task-1".to_string()),
                    execution_id: Some("exec-1".to_string()),
                    chat_session_id: Some("chat-1".to_string()),
                    input_fingerprint: "sha256:test".to_string(),
                    input_shape: serde_json::json!({"query": {"kind": "string", "len": 5}}),
                    status: LearningSkillInvocationStatus::Failed,
                    failure_class: Some(LearningSkillInvocationFailureClass::BadSchema),
                    error_summary: Some("missing field".to_string()),
                    result_summary: None,
                    duration_ms: 42,
                    retry_count: 0,
                    evidence_refs: Vec::new(),
                    payload: serde_json::json!({"dispatch": "test"}),
                },
            )
            .unwrap();

        assert_eq!(store.count_skill_invocation_evidence(&scope), 1);
        assert_eq!(store.count_event_records(&scope), 1);

        let date = evidence.created_at.date_naive().to_string();
        let loaded = store
            .read_skill_invocation_evidence(&scope, &date, &evidence.id)
            .unwrap();
        assert_eq!(loaded.skill_name, "search_memory");
        assert_eq!(
            loaded.failure_class,
            Some(LearningSkillInvocationFailureClass::BadSchema)
        );

        let filtered = store
            .list_skill_invocation_evidence(
                &scope,
                LearningSkillInvocationEvidenceFilters {
                    status: Some("failed".to_string()),
                    failure_class: Some("bad_schema".to_string()),
                    skill_name: Some("search_memory".to_string()),
                    ..LearningSkillInvocationEvidenceFilters::default()
                },
            )
            .unwrap();
        assert_eq!(filtered.len(), 1);

        let events = store.list_events(&scope, 10).unwrap();
        assert_eq!(events[0].event_type, "skill_invocation_failed");
        assert_eq!(
            events[0]
                .payload
                .get("skill_invocation_id")
                .and_then(Value::as_str),
            Some(evidence.id.as_str())
        );
    }

    #[test]
    fn capability_promotion_listing_skips_legacy_audit_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = LearningStore::new(ArtifactV2Workspace::new(tmp.path()));
        let scope = LearningScope::new("anonymous", "default");
        let path = store
            .workspace_layout
            .capability_evolution_promotion_audit_path(&scope.principal, &scope.workspace);
        std::fs::create_dir_all(path.parent().expect("promotion audit parent")).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n",
                serde_json::json!({
                    "timestamp": 1778945939_i64,
                    "pack_name": "api_replay_legacy",
                    "source": "api_mined",
                    "source_ref": "legacy-source",
                    "next_status": "trial",
                    "reason": "legacy capability-pack audit row"
                })
            ),
        )
        .unwrap();

        let record = LearningCapabilityEvolutionPromotionRecord {
            id: "lcepromo_valid".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate_valid".to_string(),
            proposal_id: "proposal_valid".to_string(),
            validation_id: "validation_valid".to_string(),
            implementation_id: Some("implementation_valid".to_string()),
            application_id: Some("application_valid".to_string()),
            capability_id: Some("skill:valid".to_string()),
            actor: "tester".to_string(),
            summary: "valid promotion".to_string(),
            applied_files: vec!["skills/valid/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: Utc::now(),
        };
        store
            .append_capability_evolution_promotion_record(&record)
            .unwrap();

        let records = store
            .list_capability_evolution_promotion_records(
                &scope,
                LearningCapabilityEvolutionPromotionFilters::default(),
            )
            .unwrap();
        assert_eq!(records, vec![record.clone()]);
        assert_eq!(
            store.count_capability_evolution_promotion_records(&scope),
            1
        );
        assert!(store
            .workspace_layout
            .skill_evolution_promotion_audit_path(&scope.principal, &scope.workspace)
            .exists());
        assert_eq!(
            store
                .read_capability_evolution_promotion_record(&scope, "lcepromo_valid")
                .unwrap(),
            record
        );
    }

    #[test]
    fn skill_evolution_backlog_writes_new_root_and_merges_legacy_records() {
        let tmp = tempfile::tempdir().unwrap();
        let store = LearningStore::new(ArtifactV2Workspace::new(tmp.path()));
        let scope = LearningScope::new("anonymous", "default");
        let now = Utc::now();
        let mut legacy = LearningCapabilityEvolutionBacklogItem {
            id: "lce_candidate_split".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate_split".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Queued,
            candidate_type: LearningCandidateType::SkillUpdate,
            title: "legacy backlog".to_string(),
            summary: "legacy".to_string(),
            rationale: "legacy".to_string(),
            capability_id: Some("skill:split".to_string()),
            failure_pattern: Some("legacy".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/split/SKILL.md".to_string()],
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skills/split/SKILL.md".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: None,
            recurrence_count: 0,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: false,
            rank_score: 1.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: Value::Null,
            created_at: now,
            updated_at: now,
        };
        let legacy_path = store.workspace_layout.capability_evolution_backlog_path(
            &scope.principal,
            &scope.workspace,
            &legacy.candidate_id,
        );
        store.write_json_pretty(&legacy_path, &legacy).unwrap();

        let mut current = legacy.clone();
        current.title = "current backlog".to_string();
        current.summary = "current".to_string();
        current.status = LearningCapabilityEvolutionBacklogStatus::InReview;
        current.rank_score = 10.0;
        current.updated_at = now + chrono::Duration::seconds(1);
        store
            .write_capability_evolution_backlog_item(&current)
            .unwrap();
        let current_path = store.workspace_layout.skill_evolution_backlog_path(
            &scope.principal,
            &scope.workspace,
            &current.candidate_id,
        );
        assert!(store.path_exists(&current_path));

        legacy.title = "legacy backlog after current write".to_string();
        store.write_json_pretty(&legacy_path, &legacy).unwrap();

        let loaded = store
            .read_capability_evolution_backlog_item(&scope, &current.candidate_id)
            .unwrap();
        assert_eq!(loaded.title, "current backlog");
        let listed = store
            .list_capability_evolution_backlog_items(
                &scope,
                LearningCapabilityEvolutionBacklogFilters::default(),
            )
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "current backlog");
        assert_eq!(
            listed[0].status,
            LearningCapabilityEvolutionBacklogStatus::InReview
        );
        let queued = store
            .list_capability_evolution_backlog_items(
                &scope,
                LearningCapabilityEvolutionBacklogFilters {
                    status: Some("queued".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(queued.is_empty());
    }

    #[test]
    fn repeated_skill_invocation_failures_route_one_capability_backlog_item() {
        let tmp = tempfile::tempdir().unwrap();
        let store = LearningStore::new(ArtifactV2Workspace::new(tmp.path()));
        let scope = LearningScope::new("anonymous", "default");
        let mut last_evidence = None;

        for idx in 0..3 {
            let evidence = store
                .record_skill_invocation_evidence(
                    scope.clone(),
                    CreateLearningSkillInvocationEvidenceRequest {
                        source: LearningSkillInvocationSource::PrimitiveCliTemplate,
                        skill_name: "zepto".to_string(),
                        tool_action_name: Some("search".to_string()),
                        agent_id: Some("executive-assistant".to_string()),
                        task_id: Some(format!("task-{idx}")),
                        execution_id: Some(format!("exec-{idx}")),
                        chat_session_id: None,
                        input_fingerprint: "sha256:shape".to_string(),
                        input_shape: serde_json::json!({"query": {"kind": "string", "len": 12}}),
                        status: LearningSkillInvocationStatus::Failed,
                        failure_class: Some(LearningSkillInvocationFailureClass::BadSchema),
                        error_summary: Some("missing field `query`".to_string()),
                        result_summary: None,
                        duration_ms: 50,
                        retry_count: 0,
                        evidence_refs: Vec::new(),
                        payload: serde_json::json!({"dispatch": "test"}),
                    },
                )
                .unwrap();
            last_evidence = Some(evidence);
        }

        let last_evidence = last_evidence.unwrap();
        let cluster_id = skill_invocation_failure_cluster_id(&last_evidence);
        let cluster = store
            .read_skill_invocation_failure_cluster(&scope, &cluster_id)
            .unwrap();
        assert_eq!(cluster.occurrence_count, 3);
        assert!(cluster.candidate_id.is_some());
        assert!(cluster.routed_at.is_some());

        let items = store
            .list_capability_evolution_backlog_items(
                &scope,
                LearningCapabilityEvolutionBacklogFilters {
                    capability_id: Some("zepto".to_string()),
                    ..LearningCapabilityEvolutionBacklogFilters::default()
                },
            )
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].candidate_type,
            LearningCandidateType::ToolSchemaUpdate
        );
        assert_eq!(items[0].proposed_fix_type.as_deref(), Some("tool_schema"));

        let candidate_id = cluster.candidate_id.clone();
        let _ = store
            .record_skill_invocation_evidence(
                scope.clone(),
                CreateLearningSkillInvocationEvidenceRequest {
                    source: LearningSkillInvocationSource::PrimitiveCliTemplate,
                    skill_name: "zepto".to_string(),
                    tool_action_name: Some("search".to_string()),
                    agent_id: Some("executive-assistant".to_string()),
                    task_id: Some("task-4".to_string()),
                    execution_id: Some("exec-4".to_string()),
                    chat_session_id: None,
                    input_fingerprint: "sha256:shape".to_string(),
                    input_shape: serde_json::json!({"query": {"kind": "string", "len": 12}}),
                    status: LearningSkillInvocationStatus::Failed,
                    failure_class: Some(LearningSkillInvocationFailureClass::BadSchema),
                    error_summary: Some("missing field `query`".to_string()),
                    result_summary: None,
                    duration_ms: 55,
                    retry_count: 0,
                    evidence_refs: Vec::new(),
                    payload: serde_json::json!({"dispatch": "test"}),
                },
            )
            .unwrap();
        let updated_cluster = store
            .read_skill_invocation_failure_cluster(&scope, &cluster_id)
            .unwrap();
        assert_eq!(updated_cluster.occurrence_count, 4);
        assert_eq!(updated_cluster.candidate_id, candidate_id);
        let items = store
            .list_capability_evolution_backlog_items(
                &scope,
                LearningCapabilityEvolutionBacklogFilters {
                    capability_id: Some("zepto".to_string()),
                    ..LearningCapabilityEvolutionBacklogFilters::default()
                },
            )
            .unwrap();
        assert_eq!(items.len(), 1);
    }
}
