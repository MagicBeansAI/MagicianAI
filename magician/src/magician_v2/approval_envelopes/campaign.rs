//! Owner-reviewed campaign scope over existing task and counterparty records.
//! Capture consumes finite capacity but supplies no delivery or live-send trust.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::EnvelopeStoreScope;
use crate::magician_v2::{agents::AgentStorage, artifact_v2::workspace::ArtifactV2Workspace};

#[cfg(test)]
#[path = "campaign_tests.rs"]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignTarget {
    pub task_id: String,
    pub task_context_hash: String,
    pub identity_id: String,
    /// A register mutation cannot carry simulated coverage to a new address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
}

/// Owner-selected handoff routing, separate from the quality contract to avoid
/// a policy-version/allowed-contract hash cycle. The admission snapshot pins it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureHandoffTarget {
    pub campaign_id: Uuid,
    pub version_hash: String,
    pub identity_id: String,
}

impl CaptureHandoffTarget {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.campaign_id.is_nil()
                && valid_hash(&self.version_hash)
                && !self.identity_id.trim().is_empty(),
            "capture handoff needs an exact campaign version and identity"
        );
        Ok(())
    }
}

/// Structured owner rules. Prose or an actor may propose this record, but only
/// authenticated activation supplies authority. All matches use exact IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureApprovalPolicy {
    pub source_document: String,
    pub always_review: bool,
    pub important_identity_ids: BTreeSet<String>,
    pub allow_automatic_capture: bool,
    pub clean_approvals_before_automatic_capture: Option<u32>,
    pub audit_every: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampaignSpec {
    pub name: String,
    pub goal_id: Option<String>,
    pub sender_identity: String,
    pub channel: String,
    pub message_kind: String,
    pub targets: Vec<CampaignTarget>,
    pub allowed_contract_hashes: BTreeSet<String>,
    pub content_contract_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_template: Option<super::content::TemplateContract>,
    pub maximum_acts: u32,
    pub maximum_acts_per_day: u32,
    pub maximum_acts_per_recipient: u32,
    pub expires_at: DateTime<Utc>,
    pub approval: CaptureApprovalPolicy,
}

fn hash(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl CampaignSpec {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [
                &self.name,
                &self.sender_identity,
                &self.channel,
                &self.message_kind,
                &self.approval.source_document
            ]
            .iter()
            .all(|value| !value.trim().is_empty())
                && self
                    .goal_id
                    .as_ref()
                    .is_none_or(|id| !id.trim().is_empty() && id.trim() == id)
                && self.maximum_acts > 0
                && self.maximum_acts_per_day > 0
                && self.maximum_acts_per_recipient > 0
                && self.approval.audit_every > 0
                && self
                    .approval
                    .clean_approvals_before_automatic_capture
                    .is_none_or(|n| n > 0)
                && valid_hash(&self.content_contract_hash)
                && !self.allowed_contract_hashes.is_empty()
                && self
                    .allowed_contract_hashes
                    .iter()
                    .all(|value| valid_hash(value))
                && !self.targets.is_empty(),
            "campaign needs exact identities, reviewed content and finite positive bounds"
        );
        let mut unique = BTreeSet::new();
        let mut addresses = BTreeMap::new();
        for target in &self.targets {
            ArtifactV2Workspace::validate_task_id(&target.task_id)?;
            ensure!(
                !target.identity_id.trim().is_empty()
                    && valid_hash(&target.task_context_hash)
                    && target
                        .recipient
                        .as_ref()
                        .is_none_or(|recipient| !recipient.trim().is_empty()
                            && recipient.trim() == recipient)
                    && unique.insert((&target.task_id, &target.identity_id)),
                "duplicate or unpinned campaign target"
            );
            if let Some(recipient) = &target.recipient {
                ensure!(
                    addresses
                        .insert(&target.identity_id, recipient)
                        .is_none_or(|old| old == recipient),
                    "one campaign identity cannot name multiple recipients"
                );
            }
        }
        ensure!(
            self.approval
                .important_identity_ids
                .iter()
                .all(|id| self.targets.iter().any(|target| &target.identity_id == id)),
            "important identity is outside the reviewed campaign"
        );
        if let Some(template) = &self.content_template {
            ensure!(
                template.hash()? == self.content_contract_hash,
                "campaign content hash differs from its approved template"
            );
        }
        ensure!(
            !self.approval.allow_automatic_capture || self.content_template.is_some(),
            "automatic capture requires an owner-approved bounded template"
        );
        ensure!(
            !self.approval.allow_automatic_capture
                || self.targets.iter().all(|target| target.recipient.is_some()),
            "automatic capture requires pinned normalized recipient addresses"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignVersion {
    pub id: Uuid,
    pub version_hash: String,
    pub source_hash: String,
    pub spec: CampaignSpec,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    AwaitingReview,
    Captured,
    Rejected,
}

/// Populated from controller-sealed artifacts by the service, never from model
/// declarations or a client-submitted receipt. The binding includes all files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureBinding {
    pub scope: magicllm::LlmScope,
    pub campaign_id: Uuid,
    pub version_hash: String,
    pub run_id: Uuid,
    pub task_id: String,
    pub task_context_hash: String,
    pub identity_id: String,
    pub recipient: String,
    pub sender_identity: String,
    pub channel: String,
    pub message_kind: String,
    pub content_contract_hash: String,
    pub contract_hash: String,
    pub artifact_hash: String,
    pub artifact_files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_validation: Option<super::content::ValidatedTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_review_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedAct {
    pub id: String,
    pub binding_hash: String,
    pub binding: CaptureBinding,
    pub status: CaptureStatus,
    pub reason: String,
    pub presented_at: DateTime<Utc>,
    pub audit_required: bool,
    pub owner_reviewed_by: Option<String>,
    pub owner_reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum Record {
    Activated(CampaignVersion),
    Revoked {
        at: DateTime<Utc>,
        version_hash: String,
        by: String,
    },
    Capture(CapturedAct),
}

#[derive(Debug, Clone, Serialize)]
pub struct CampaignState {
    pub campaign: CampaignVersion,
    pub captures: BTreeMap<String, CapturedAct>,
    pub simulated_clean_approvals: u32,
    pub simulated_clean_approvals_by_identity: BTreeMap<String, u32>,
    pub production_clean_approvals: u32,
    pub live_dispatch_enabled: bool,
    pub automatic_capture_suspended: bool,
    pub activated_versions: BTreeSet<String>,
}

impl CampaignState {
    /// Receipt-only lookup survives policy expiry/successors. It cannot create
    /// or approve an act; multiple historical bindings are an explicit gap.
    pub(crate) fn capture_receipt(
        &self,
        target: &CaptureHandoffTarget,
        run_id: Uuid,
        artifact_hash: &str,
    ) -> Result<Option<&CapturedAct>> {
        let mut matches = self.captures.values().filter(|act| {
            act.binding.campaign_id == target.campaign_id
                && act.binding.version_hash == target.version_hash
                && act.binding.identity_id == target.identity_id
                && act.binding.run_id == run_id
                && act.binding.artifact_hash == artifact_hash
        });
        let receipt = matches.next();
        ensure!(
            matches.next().is_none(),
            "capture handoff has ambiguous historical recipient bindings"
        );
        Ok(receipt)
    }
}

#[derive(Clone)]
pub struct CampaignStore {
    workspace: ArtifactV2Workspace,
}

impl CampaignStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    fn path(&self, scope: &EnvelopeStoreScope, id: Uuid) -> Result<std::path::PathBuf> {
        ensure!(
            magicllm::LlmScope::new(&scope.principal, &scope.workspace).is_valid(),
            "invalid campaign scope"
        );
        Ok(self
            .workspace
            .scope_root(&scope.principal, &scope.workspace)
            .join("approval_envelopes")
            .join("campaigns")
            .join(format!("{id}.jsonl")))
    }

    fn append(&self, path: &std::path::Path, record: &Record) -> Result<()> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        crate::magician_v2::jsonl::append_log_line(&self.workspace, path, &line)
    }

    pub fn load(&self, scope: &EnvelopeStoreScope, id: Uuid) -> Result<Option<CampaignState>> {
        let path = self.path(scope, id)?;
        let Some(raw) = crate::magician_v2::jsonl::read_log_if_present(&self.workspace, &path)?
        else {
            return Ok(None);
        };
        let mut campaign: Option<CampaignVersion> = None;
        let mut captures: BTreeMap<String, CapturedAct> = BTreeMap::new();
        let mut versions: BTreeMap<String, CampaignSpec> = BTreeMap::new();
        let mut clean: BTreeMap<String, u32> = BTreeMap::new();
        let mut suspended = false;
        for record in crate::magician_v2::jsonl::parse_log_lines::<Record>(&raw, &path)? {
            match record {
                Record::Activated(version) => {
                    ensure!(
                        version.id == id
                            && version.version_hash == hash(&version.spec)?
                            && version.source_hash
                                == blake3::hash(version.spec.approval.source_document.as_bytes())
                                    .to_hex()
                                    .to_string()
                            && version.approved_by == scope.principal,
                        "campaign activation lost its scope/content binding"
                    );
                    version.spec.validate()?;
                    ensure!(
                        !versions.contains_key(&version.version_hash),
                        "campaign version was reactivated"
                    );
                    versions.insert(version.version_hash.clone(), version.spec.clone());
                    clean.clear();
                    suspended = false;
                    campaign = Some(version);
                },
                Record::Revoked {
                    at,
                    version_hash,
                    by,
                } => {
                    let current = campaign.as_mut().context("revocation precedes campaign")?;
                    ensure!(
                        current.version_hash == version_hash && by == scope.principal,
                        "campaign revocation version mismatch"
                    );
                    current.revoked_at = Some(at);
                },
                Record::Capture(capture) => {
                    ensure!(
                        capture.binding.scope
                            == magicllm::LlmScope::new(&scope.principal, &scope.workspace)
                            && capture.binding.campaign_id == id
                            && capture.binding_hash == hash(&capture.binding)?
                            && capture.id == format!("capture-{}", capture.binding_hash),
                        "capture binding is corrupt"
                    );
                    let spec = versions
                        .get(&capture.binding.version_hash)
                        .context("capture precedes approved campaign version")?;
                    ensure!(
                        capture.binding.sender_identity == spec.sender_identity
                            && capture.binding.channel == spec.channel
                            && capture.binding.message_kind == spec.message_kind
                            && capture.binding.content_contract_hash == spec.content_contract_hash
                            && spec
                                .allowed_contract_hashes
                                .contains(&capture.binding.contract_hash)
                            && spec.targets.iter().any(|target| target.task_id
                                == capture.binding.task_id
                                && target.task_context_hash == capture.binding.task_context_hash
                                && target.identity_id == capture.binding.identity_id
                                && target.recipient.as_ref().is_none_or(
                                    |recipient| recipient == &capture.binding.recipient
                                ))
                            && capture
                                .owner_reviewed_by
                                .as_deref()
                                .is_none_or(|by| by == scope.principal)
                            && capture.owner_reviewed_by.is_some()
                                == capture.owner_reviewed_at.is_some(),
                        "capture lost its reviewed campaign provenance"
                    );
                    if let Some(proof) = &capture.binding.content_validation {
                        let template = spec
                            .content_template
                            .as_ref()
                            .context("validated capture has no approved template")?;
                        template.validate_proof(proof, &capture.binding.artifact_files)?;
                        ensure!(
                            capture.binding.content_review_reason.is_none(),
                            "validated capture cannot also require content review"
                        );
                    }
                    if let Some(old) = captures.get(&capture.id) {
                        ensure!(
                            old.binding == capture.binding
                                && old.presented_at == capture.presented_at
                                && old.audit_required == capture.audit_required
                                && old.status == CaptureStatus::AwaitingReview
                                && capture.status != CaptureStatus::AwaitingReview
                                && capture.owner_reviewed_by.is_some(),
                            "completed capture cannot be rewritten"
                        );
                    }
                    if campaign
                        .as_ref()
                        .is_some_and(|current| current.version_hash == capture.binding.version_hash)
                    {
                        if capture.status == CaptureStatus::Rejected {
                            clean.insert(capture.binding.identity_id.clone(), 0);
                            suspended |= capture.audit_required;
                        } else if capture.status == CaptureStatus::Captured
                            && capture.owner_reviewed_by.is_some()
                        {
                            let count = clean
                                .entry(capture.binding.identity_id.clone())
                                .or_default();
                            if capture.binding.content_validation.is_some() {
                                *count = count
                                    .checked_add(1)
                                    .context("simulated trust count overflow")?;
                            } else {
                                // Exact review can approve novel content locally,
                                // but a correction cannot retain template trust.
                                *count = 0;
                            }
                        }
                    }
                    captures.insert(capture.id.clone(), capture);
                },
            }
        }
        let Some(campaign) = campaign else {
            anyhow::bail!("campaign log has no activation")
        };
        Ok(Some(CampaignState {
            campaign,
            captures,
            simulated_clean_approvals: clean.values().try_fold(0_u32, |total, count| {
                total
                    .checked_add(*count)
                    .context("simulated approval total overflow")
            })?,
            simulated_clean_approvals_by_identity: clean,
            production_clean_approvals: 0,
            live_dispatch_enabled: false,
            automatic_capture_suspended: suspended,
            activated_versions: versions.into_keys().collect(),
        }))
    }

    pub fn list(&self, scope: &EnvelopeStoreScope) -> Result<Vec<CampaignState>> {
        self.list_ids(scope)?
            .into_iter()
            .map(|id| self.load(scope, id)?.context("listed campaign vanished"))
            .collect()
    }

    /// Discovery only; every policy must still pass journal validation.
    pub fn list_ids(&self, scope: &EnvelopeStoreScope) -> Result<Vec<Uuid>> {
        let directory = self
            .path(scope, Uuid::nil())?
            .parent()
            .context("campaign parent missing")?
            .to_owned();
        let mut ids = Vec::new();
        for path in crate::magician_v2::jsonl::list_log_paths(&self.workspace, &directory)? {
            let id = Uuid::parse_str(
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .context("invalid campaign file")?,
            )?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub(crate) fn validate_handoff_target(
        &self,
        scope: &EnvelopeStoreScope,
        target: &CaptureHandoffTarget,
        task_id: &str,
        context_hash: &str,
        contract_hash: &str,
    ) -> Result<()> {
        target.validate()?;
        let state = self
            .load(scope, target.campaign_id)?
            .context("capture handoff campaign is missing")?;
        ensure!(
            state.campaign.version_hash == target.version_hash
                && state.campaign.revoked_at.is_none()
                && state.campaign.spec.expires_at > Utc::now()
                && state
                    .campaign
                    .spec
                    .allowed_contract_hashes
                    .contains(contract_hash)
                && state
                    .campaign
                    .spec
                    .targets
                    .iter()
                    .any(|entry| entry.task_id == task_id
                        && entry.task_context_hash == context_hash
                        && entry.identity_id == target.identity_id),
            "capture handoff exceeds the reviewed campaign/task contract"
        );
        Ok(())
    }

    pub fn activate(
        &self,
        scope: &EnvelopeStoreScope,
        id: Uuid,
        expected_version: Option<&str>,
        spec: CampaignSpec,
    ) -> Result<CampaignState> {
        spec.validate()?;
        ensure!(
            spec.expires_at > Utc::now(),
            "campaign approval already expired"
        );
        let path = self.path(scope, id)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        let version_hash = hash(&spec)?;
        let current = self.load(scope, id)?;
        if let Some(current) = &current {
            if current.campaign.spec == spec && current.campaign.revoked_at.is_none() {
                return Ok(current.clone());
            }
            ensure!(
                Some(current.campaign.version_hash.as_str()) == expected_version,
                "campaign changed before owner approval"
            );
            ensure!(
                current.campaign.version_hash != version_hash,
                "revoked version needs a new owner policy"
            );
            ensure!(
                !current.activated_versions.contains(&version_hash),
                "prior campaign policy cannot restore its old trust count"
            );
        } else {
            ensure!(
                expected_version.is_none(),
                "expected campaign version is absent"
            )
        }
        let version = CampaignVersion {
            id,
            version_hash,
            source_hash: blake3::hash(spec.approval.source_document.as_bytes())
                .to_hex()
                .to_string(),
            spec,
            approved_by: scope.principal.clone(),
            approved_at: Utc::now(),
            revoked_at: None,
        };
        self.append(&path, &Record::Activated(version))?;
        self.load(scope, id)?
            .context("campaign vanished after activation")
    }

    pub fn revoke(
        &self,
        scope: &EnvelopeStoreScope,
        id: Uuid,
        expected_version: &str,
    ) -> Result<CampaignState> {
        let path = self.path(scope, id)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        let current = self.load(scope, id)?.context("campaign missing")?;
        ensure!(
            current.campaign.version_hash == expected_version,
            "campaign changed before revocation"
        );
        if current.campaign.revoked_at.is_none() {
            self.append(
                &path,
                &Record::Revoked {
                    at: Utc::now(),
                    version_hash: expected_version.into(),
                    by: scope.principal.clone(),
                },
            )?;
        }
        self.load(scope, id)?
            .context("campaign vanished after revocation")
    }

    pub(crate) fn capture(
        &self,
        scope: &EnvelopeStoreScope,
        binding: CaptureBinding,
        owner_approved_exact: bool,
    ) -> Result<CapturedAct> {
        let path = self.path(scope, binding.campaign_id)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        let current = self
            .load(scope, binding.campaign_id)?
            .context("campaign missing")?;
        let spec = &current.campaign.spec;
        let now = Utc::now();
        ensure!(
            binding.scope == magicllm::LlmScope::new(&scope.principal, &scope.workspace)
                && current.campaign.revoked_at.is_none()
                && spec.expires_at > now
                && binding.version_hash == current.campaign.version_hash
                && binding.sender_identity == spec.sender_identity
                && binding.channel == spec.channel
                && binding.message_kind == spec.message_kind
                && binding.content_contract_hash == spec.content_contract_hash
                && spec
                    .allowed_contract_hashes
                    .contains(&binding.contract_hash)
                && spec
                    .targets
                    .iter()
                    .any(|target| target.task_id == binding.task_id
                        && target.task_context_hash == binding.task_context_hash
                        && target.identity_id == binding.identity_id
                        && target
                            .recipient
                            .as_ref()
                            .is_none_or(|recipient| recipient == &binding.recipient))
                && valid_hash(&binding.artifact_hash)
                && !binding.artifact_files.is_empty()
                && binding
                    .artifact_files
                    .values()
                    .all(|value| valid_hash(value)),
            "capture exceeds its owner-reviewed campaign binding"
        );
        let binding_hash = hash(&binding)?;
        if let Some(proof) = &binding.content_validation {
            spec.content_template
                .as_ref()
                .context("capture has no approved template")?
                .validate_proof(proof, &binding.artifact_files)?;
            ensure!(
                binding.content_review_reason.is_none(),
                "validated capture also requires content review"
            );
        }
        let id = format!("capture-{binding_hash}");
        if let Some(existing) = current.captures.get(&id) {
            return Ok(existing.clone());
        }
        ensure!(
            current.captures.len() < spec.maximum_acts as usize
                && current
                    .captures
                    .values()
                    .filter(|act| act.presented_at.date_naive() == now.date_naive())
                    .count()
                    < spec.maximum_acts_per_day as usize
                && current
                    .captures
                    .values()
                    .filter(|act| act.binding.recipient == binding.recipient)
                    .count()
                    < spec.maximum_acts_per_recipient as usize,
            "campaign capture capacity is exhausted"
        );
        let ordinal = u32::try_from(current.captures.len())?
            .checked_add(1)
            .context("campaign ordinal overflow")?;
        let audit_required = ordinal % spec.approval.audit_every == 0;
        let identity_clean = current
            .simulated_clean_approvals_by_identity
            .get(&binding.identity_id)
            .copied()
            .unwrap_or(0);
        let automatically_eligible = !current.automatic_capture_suspended
            && binding.content_validation.is_some()
            && binding.content_review_reason.is_none()
            && identity_clean > 0
            && !spec.approval.always_review
            && !spec
                .approval
                .important_identity_ids
                .contains(&binding.identity_id)
            && spec.approval.allow_automatic_capture
            && !audit_required
            && spec
                .approval
                .clean_approvals_before_automatic_capture
                .is_none_or(|required| identity_clean >= required);
        let reviewed = owner_approved_exact;
        let status = if reviewed || automatically_eligible {
            CaptureStatus::Captured
        } else {
            CaptureStatus::AwaitingReview
        };
        let reason = if reviewed {
            "owner approved exact accepted artifact"
        } else if automatically_eligible {
            "validated template and independent artifact review within owner capture policy"
        } else {
            binding
                .content_review_reason
                .as_deref()
                .unwrap_or("campaign policy requires exact owner review")
        }
        .to_owned();
        let act = CapturedAct {
            id,
            binding_hash,
            binding,
            status,
            reason,
            presented_at: now,
            audit_required,
            owner_reviewed_by: reviewed.then(|| scope.principal.clone()),
            owner_reviewed_at: reviewed.then_some(now),
        };
        self.append(&path, &Record::Capture(act.clone()))?;
        Ok(act)
    }

    pub fn review(
        &self,
        scope: &EnvelopeStoreScope,
        campaign_id: Uuid,
        id: &str,
        binding_hash: &str,
        approved: bool,
    ) -> Result<CapturedAct> {
        let path = self.path(scope, campaign_id)?;
        let _lock = AgentStorage::acquire_file_lock_exclusive_sync(&path)?;
        let current = self.load(scope, campaign_id)?.context("campaign missing")?;
        let mut act = current.captures.get(id).context("capture missing")?.clone();
        ensure!(
            act.binding_hash == binding_hash,
            "owner review does not bind the exact capture"
        );
        let target = if approved {
            CaptureStatus::Captured
        } else {
            CaptureStatus::Rejected
        };
        if act.status != CaptureStatus::AwaitingReview {
            ensure!(
                act.status == target,
                "capture already has a different owner decision"
            );
            return Ok(act);
        }
        if approved {
            ensure!(
                current.campaign.revoked_at.is_none()
                    && current.campaign.spec.expires_at > Utc::now()
                    && act.binding.version_hash == current.campaign.version_hash,
                "campaign coverage changed before review"
            )
        }
        act.status = target;
        act.owner_reviewed_by = Some(scope.principal.clone());
        act.owner_reviewed_at = Some(Utc::now());
        act.reason = if approved {
            "owner approved the exact capture"
        } else {
            "owner rejected the exact capture; simulated clean count resets"
        }
        .into();
        self.append(&path, &Record::Capture(act.clone()))?;
        Ok(act)
    }
}
