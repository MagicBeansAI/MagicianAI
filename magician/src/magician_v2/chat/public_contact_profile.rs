use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::magician_v2::agents::storage::sanitize_segment;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::models::ChatSession;

const PUBLIC_CONTACT_PROFILE_SCHEMA_VERSION: u32 = 1;
const PUBLIC_CONTACT_RESEARCH_SUMMARY_MAX_CHARS: usize = 12_000;
const PUBLIC_CONTACT_RESEARCH_EXCERPT_MAX_CHARS: usize = 1_200;
const PUBLIC_CONTACT_RESEARCH_MAX_SOURCES: usize = 16;
const PUBLIC_CONTACT_RESEARCH_MAX_WARNINGS: usize = 8;
const PUBLIC_CONTACT_RESEARCH_MAX_SUGGESTIONS: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicContactSenderKey {
    pub workspace: String,
    pub channel_type: String,
    pub channel_address: String,
}

impl PublicContactSenderKey {
    pub fn for_session(session: &ChatSession) -> Self {
        let channel_type = session.origin_channel.channel_type.trim();
        let address = session
            .origin_channel
            .address
            .as_deref()
            .map(str::trim)
            .filter(|address| !address.is_empty());
        match (channel_type.is_empty(), address) {
            (false, Some(address)) => Self {
                workspace: session.workspace.clone(),
                channel_type: channel_type.to_string(),
                channel_address: address.to_string(),
            },
            _ => Self {
                workspace: session.workspace.clone(),
                channel_type: "session".to_string(),
                channel_address: session.id.clone(),
            },
        }
    }

    pub fn as_storage_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.workspace, self.channel_type, self.channel_address
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PublicContactProfileConfidence {
    #[default]
    Unknown,
    UserClaimed,
    OwnerReviewed,
    ResearchedLow,
    ResearchedMedium,
    ResearchedHigh,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdentityResearchStatus {
    #[default]
    NotEligible,
    Eligible,
    Queued,
    Active,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicContactProfileProvenance {
    pub field: String,
    pub source: String,
    pub observed_at: i64,
    pub chat_session_id: Option<String>,
    pub chat_turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicContactResearchLead {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicContactMemorySuggestion {
    pub field: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicContactResearchResult {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_user_output_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub confidence: PublicContactProfileConfidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub possible_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<PublicContactResearchLead>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_memory_fields: Vec<PublicContactMemorySuggestion>,
    pub stored_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicContactProfile {
    pub schema_version: u32,
    pub sender_key: PublicContactSenderKey,
    pub principal: String,
    pub workspace: String,
    pub display_name: Option<String>,
    pub claimed_name: Option<String>,
    pub claimed_org: Option<String>,
    pub claimed_role: Option<String>,
    pub purpose: Option<String>,
    pub confidence: PublicContactProfileConfidence,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub accepted_turn_count: u64,
    pub first_contact_day_key: String,
    pub first_contact_llm_count_today: u64,
    pub coalesced_message_count: u64,
    pub research_status: IdentityResearchStatus,
    pub last_researched_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_job_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_dispatched_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_queued_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_active_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_completed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_failed_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_skip_reason: Option<String>,
    #[serde(default)]
    pub research_day_key: String,
    #[serde(default)]
    pub research_count_today: u64,
    pub owner_review_priority: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_result: Option<PublicContactResearchResult>,
    #[serde(default)]
    pub provenance: Vec<PublicContactProfileProvenance>,
}

impl PublicContactProfile {
    pub fn new(session: &ChatSession, sender_key: PublicContactSenderKey, now_ms: i64) -> Self {
        Self {
            schema_version: PUBLIC_CONTACT_PROFILE_SCHEMA_VERSION,
            sender_key,
            principal: session.principal.clone(),
            workspace: session.workspace.clone(),
            display_name: None,
            claimed_name: None,
            claimed_org: None,
            claimed_role: None,
            purpose: None,
            confidence: PublicContactProfileConfidence::Unknown,
            first_seen_at: now_ms,
            last_seen_at: now_ms,
            accepted_turn_count: 0,
            first_contact_day_key: String::new(),
            first_contact_llm_count_today: 0,
            coalesced_message_count: 0,
            research_status: IdentityResearchStatus::NotEligible,
            last_researched_at: None,
            research_job_id: None,
            research_task_id: None,
            research_execution_id: None,
            research_dispatched_at: None,
            research_queued_at: None,
            research_active_at: None,
            research_completed_at: None,
            research_failed_at: None,
            research_failure_reason: None,
            research_skip_reason: None,
            research_day_key: String::new(),
            research_count_today: 0,
            owner_review_priority: false,
            research_result: None,
            provenance: Vec::new(),
        }
    }

    pub fn missing_identity(&self) -> bool {
        self.claimed_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
            && self
                .claimed_org
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
            && self
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
    }

    pub fn missing_purpose(&self) -> bool {
        self.purpose
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    }

    pub fn research_count_for_day(&self, day_key: &str) -> u64 {
        if self.research_day_key == day_key {
            self.research_count_today
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PublicContactProfileClaims {
    pub claimed_name: Option<String>,
    pub claimed_org: Option<String>,
    pub claimed_role: Option<String>,
    pub purpose: Option<String>,
}

#[derive(Clone)]
pub struct FilePublicContactProfileStore {
    workspace_layout: ArtifactV2Workspace,
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl FilePublicContactProfileStore {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            locks: Arc::new(DashMap::new()),
        }
    }

    pub async fn load_or_create_for_session(
        &self,
        session: &ChatSession,
        sender_display_name: Option<&str>,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        let sender_key = PublicContactSenderKey::for_session(session);
        let lock = self.lock_for(&sender_key);
        let _guard = lock.lock().await;
        let path = self.profile_path(&session.principal, &session.workspace, &sender_key);
        match self
            .workspace_layout
            .read_json_path::<PublicContactProfile, _>(&path)
            .await
        {
            Ok(mut profile) => {
                profile.last_seen_at = profile.last_seen_at.max(now_ms);
                merge_display_name(&mut profile, sender_display_name, &session.id, None, now_ms);
                self.save_locked(&profile).await?;
                Ok(profile)
            },
            Err(error) if is_not_found_error(&error) => {
                let mut profile = PublicContactProfile::new(session, sender_key, now_ms);
                merge_display_name(&mut profile, sender_display_name, &session.id, None, now_ms);
                self.save_locked(&profile).await?;
                Ok(profile)
            },
            Err(error) => Err(error).context("failed to load public contact profile"),
        }
    }

    pub async fn list_profiles(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<PublicContactProfile>> {
        let dir = self.profile_dir(principal, workspace);
        let entries = self
            .workspace_layout
            .read_dir_path_or_empty(&dir)
            .await
            .context("failed to list public contact profiles")?;
        let mut profiles = Vec::new();
        for entry in entries {
            if !entry.is_file || !entry.file_name.ends_with(".json") {
                continue;
            }
            let path = dir.join(&entry.file_name);
            let profile = self
                .workspace_layout
                .read_json_path::<PublicContactProfile, _>(&path)
                .await
                .with_context(|| {
                    format!(
                        "failed to read public contact profile `{}`",
                        entry.file_name
                    )
                })?;
            profiles.push(profile);
        }
        profiles.sort_by(|a, b| {
            b.last_seen_at.cmp(&a.last_seen_at).then_with(|| {
                a.sender_key
                    .as_storage_key()
                    .cmp(&b.sender_key.as_storage_key())
            })
        });
        Ok(profiles)
    }

    pub async fn record_first_contact_attempt(
        &self,
        session: &ChatSession,
        day_key: &str,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            if profile.first_contact_day_key != day_key {
                profile.first_contact_day_key = day_key.to_string();
                profile.first_contact_llm_count_today = 0;
            }
            profile.first_contact_llm_count_today =
                profile.first_contact_llm_count_today.saturating_add(1);
        })
        .await
    }

    pub async fn record_coalesced_messages(
        &self,
        session: &ChatSession,
        count: u64,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            profile.coalesced_message_count = profile.coalesced_message_count.saturating_add(count);
        })
        .await
    }

    pub async fn record_accepted_turn(
        &self,
        session: &ChatSession,
        user_text: Option<&str>,
        sender_display_name: Option<&str>,
        chat_turn_id: Option<&str>,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        let claims = user_text
            .map(extract_public_contact_claims)
            .unwrap_or_default();
        self.update_profile(session, now_ms, |profile| {
            profile.accepted_turn_count = profile.accepted_turn_count.saturating_add(1);
            merge_display_name(
                profile,
                sender_display_name,
                &session.id,
                chat_turn_id,
                now_ms,
            );
            merge_claims(profile, &claims, &session.id, chat_turn_id, now_ms);
            if !profile.missing_identity() && !profile.missing_purpose() {
                if matches!(
                    profile.research_status,
                    IdentityResearchStatus::NotEligible
                        | IdentityResearchStatus::Eligible
                        | IdentityResearchStatus::Failed
                        | IdentityResearchStatus::Skipped
                ) {
                    profile.research_status = IdentityResearchStatus::Eligible;
                }
                profile.owner_review_priority = true;
            }
        })
        .await
    }

    pub async fn mark_research_queued(
        &self,
        session: &ChatSession,
        job_id: &str,
        day_key: &str,
        owner_review_priority: bool,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            if profile.research_day_key != day_key {
                profile.research_day_key = day_key.to_string();
                profile.research_count_today = 0;
            }
            profile.research_count_today = profile.research_count_today.saturating_add(1);
            profile.research_status = IdentityResearchStatus::Queued;
            profile.research_job_id = Some(job_id.to_string());
            profile.research_task_id = None;
            profile.research_execution_id = None;
            profile.research_dispatched_at = None;
            profile.research_queued_at = Some(now_ms);
            profile.research_active_at = None;
            profile.research_completed_at = None;
            profile.research_failed_at = None;
            profile.research_failure_reason = None;
            profile.research_skip_reason = None;
            if owner_review_priority {
                profile.owner_review_priority = true;
            }
        })
        .await
    }

    pub async fn mark_research_active(
        &self,
        session: &ChatSession,
        job_id: &str,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            profile.research_status = IdentityResearchStatus::Active;
            profile.research_job_id = Some(job_id.to_string());
            profile.research_active_at = Some(now_ms);
            profile.research_failed_at = None;
            profile.research_failure_reason = None;
            profile.research_skip_reason = None;
        })
        .await
    }

    pub async fn mark_research_dispatched(
        &self,
        session: &ChatSession,
        job_id: &str,
        task_id: &str,
        execution_id: Option<&str>,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            profile.research_status = IdentityResearchStatus::Active;
            profile.research_job_id = Some(job_id.to_string());
            profile.research_task_id = Some(task_id.to_string());
            profile.research_execution_id = execution_id.map(str::to_string);
            profile.research_dispatched_at = Some(now_ms);
            profile.research_active_at = Some(now_ms);
            profile.research_failed_at = None;
            profile.research_failure_reason = None;
            profile.research_skip_reason = None;
        })
        .await
    }

    pub async fn mark_research_completed(
        &self,
        session: &ChatSession,
        job_id: &str,
        task_id: Option<&str>,
        execution_id: Option<&str>,
        primary_user_output_id: Option<&str>,
        summary: Option<&str>,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            profile.research_status = IdentityResearchStatus::Completed;
            profile.research_job_id = Some(job_id.to_string());
            if let Some(task_id) = task_id.and_then(non_empty_trimmed) {
                profile.research_task_id = Some(task_id.to_string());
            }
            if let Some(execution_id) = execution_id.and_then(non_empty_trimmed) {
                profile.research_execution_id = Some(execution_id.to_string());
            }
            profile.last_researched_at = Some(now_ms);
            profile.research_completed_at = Some(now_ms);
            profile.research_active_at = None;
            profile.research_failed_at = None;
            profile.research_failure_reason = None;
            profile.research_skip_reason = None;
            if let Some(task_id) = profile.research_task_id.clone() {
                let normalized_summary = summary.and_then(normalize_research_summary);
                let parsed = normalized_summary
                    .as_deref()
                    .map(parse_public_contact_research_summary)
                    .unwrap_or_default();
                profile.research_result = Some(PublicContactResearchResult {
                    task_id,
                    execution_id: execution_id
                        .and_then(non_empty_trimmed)
                        .map(str::to_string)
                        .or_else(|| profile.research_execution_id.clone()),
                    primary_user_output_id: primary_user_output_id
                        .and_then(non_empty_trimmed)
                        .map(str::to_string),
                    excerpt: normalized_summary
                        .as_deref()
                        .map(|summary| research_summary_excerpt(summary)),
                    summary: normalized_summary,
                    confidence: parsed.confidence,
                    possible_identity: parsed.possible_identity,
                    org: parsed.org,
                    role: parsed.role,
                    sources: parsed.sources,
                    warnings: parsed.warnings,
                    suggested_memory_fields: parsed.suggested_memory_fields,
                    stored_at: now_ms,
                });
                profile.provenance.push(PublicContactProfileProvenance {
                    field: "research_result".to_string(),
                    source: "web_researcher_task".to_string(),
                    observed_at: now_ms,
                    chat_session_id: Some(session.id.clone()),
                    chat_turn_id: None,
                });
            }
            profile.owner_review_priority = true;
        })
        .await
    }

    pub async fn mark_research_failed(
        &self,
        session: &ChatSession,
        job_id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            profile.research_status = IdentityResearchStatus::Failed;
            profile.research_job_id = Some(job_id.to_string());
            profile.research_active_at = None;
            profile.research_failed_at = Some(now_ms);
            profile.research_failure_reason = Some(reason.to_string());
            profile.research_skip_reason = None;
        })
        .await
    }

    pub async fn mark_research_failed_by_sender_key(
        &self,
        principal: &str,
        workspace: &str,
        sender_key: &PublicContactSenderKey,
        job_id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_existing_profile_by_sender_key(
            principal,
            workspace,
            sender_key,
            now_ms,
            |profile| {
                profile.research_status = IdentityResearchStatus::Failed;
                profile.research_job_id = Some(job_id.to_string());
                profile.research_active_at = None;
                profile.research_failed_at = Some(now_ms);
                profile.research_failure_reason = Some(reason.to_string());
                profile.research_skip_reason = None;
            },
        )
        .await
    }

    pub async fn mark_research_skipped(
        &self,
        session: &ChatSession,
        reason: &str,
        now_ms: i64,
    ) -> Result<PublicContactProfile> {
        self.update_profile(session, now_ms, |profile| {
            profile.research_status = IdentityResearchStatus::Skipped;
            profile.research_skip_reason = Some(reason.to_string());
            profile.research_job_id = None;
            profile.research_task_id = None;
            profile.research_execution_id = None;
            profile.research_dispatched_at = None;
            profile.research_queued_at = None;
            profile.research_active_at = None;
            profile.research_failure_reason = None;
        })
        .await
    }

    async fn update_profile<F>(
        &self,
        session: &ChatSession,
        now_ms: i64,
        update: F,
    ) -> Result<PublicContactProfile>
    where
        F: FnOnce(&mut PublicContactProfile),
    {
        let sender_key = PublicContactSenderKey::for_session(session);
        let lock = self.lock_for(&sender_key);
        let _guard = lock.lock().await;
        let path = self.profile_path(&session.principal, &session.workspace, &sender_key);
        let mut profile = match self
            .workspace_layout
            .read_json_path::<PublicContactProfile, _>(&path)
            .await
        {
            Ok(profile) => profile,
            Err(error) if is_not_found_error(&error) => {
                PublicContactProfile::new(session, sender_key, now_ms)
            },
            Err(error) => return Err(error).context("failed to load public contact profile"),
        };
        profile.last_seen_at = now_ms;
        update(&mut profile);
        self.save_locked(&profile).await?;
        Ok(profile)
    }

    async fn update_existing_profile_by_sender_key<F>(
        &self,
        principal: &str,
        workspace: &str,
        sender_key: &PublicContactSenderKey,
        now_ms: i64,
        update: F,
    ) -> Result<PublicContactProfile>
    where
        F: FnOnce(&mut PublicContactProfile),
    {
        let lock = self.lock_for(sender_key);
        let _guard = lock.lock().await;
        let path = self.profile_path(principal, workspace, sender_key);
        let mut profile = self
            .workspace_layout
            .read_json_path::<PublicContactProfile, _>(&path)
            .await
            .context("failed to load existing public contact profile")?;
        profile.last_seen_at = now_ms;
        update(&mut profile);
        self.save_locked(&profile).await?;
        Ok(profile)
    }

    async fn save_locked(&self, profile: &PublicContactProfile) -> Result<()> {
        let dir = self.profile_dir(&profile.principal, &profile.workspace);
        self.workspace_layout
            .create_dir_all_path(&dir)
            .await
            .context("failed to create public contact profile directory")?;
        let path = self.profile_path(&profile.principal, &profile.workspace, &profile.sender_key);
        self.workspace_layout
            .write_json_atomic_path(&path, profile)
            .await
            .context("failed to write public contact profile")?;
        Ok(())
    }

    fn lock_for(&self, sender_key: &PublicContactSenderKey) -> Arc<Mutex<()>> {
        self.locks
            .entry(sender_key.as_storage_key())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn profile_dir(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace_layout
            .scope_root(principal, workspace)
            .join("ui")
            .join("public_contacts")
    }

    fn profile_path(
        &self,
        principal: &str,
        workspace: &str,
        sender_key: &PublicContactSenderKey,
    ) -> PathBuf {
        self.profile_dir(principal, workspace).join(format!(
            "{}.json",
            sanitize_segment(&sender_key.as_storage_key())
        ))
    }
}

fn is_not_found_error(error: &crate::magician_v2::artifact_v2::ArtifactV2Error) -> bool {
    matches!(
        error,
        crate::magician_v2::artifact_v2::ArtifactV2Error::Io(io_error)
            if io_error.kind() == std::io::ErrorKind::NotFound
    )
}

fn merge_claims(
    profile: &mut PublicContactProfile,
    claims: &PublicContactProfileClaims,
    chat_session_id: &str,
    chat_turn_id: Option<&str>,
    now_ms: i64,
) {
    merge_claim_field(
        &mut profile.claimed_name,
        claims.claimed_name.as_deref(),
        "claimed_name",
        &mut profile.provenance,
        chat_session_id,
        chat_turn_id,
        now_ms,
    );
    merge_claim_field(
        &mut profile.claimed_org,
        claims.claimed_org.as_deref(),
        "claimed_org",
        &mut profile.provenance,
        chat_session_id,
        chat_turn_id,
        now_ms,
    );
    merge_claim_field(
        &mut profile.claimed_role,
        claims.claimed_role.as_deref(),
        "claimed_role",
        &mut profile.provenance,
        chat_session_id,
        chat_turn_id,
        now_ms,
    );
    merge_claim_field(
        &mut profile.purpose,
        claims.purpose.as_deref(),
        "purpose",
        &mut profile.provenance,
        chat_session_id,
        chat_turn_id,
        now_ms,
    );
    if claims.claimed_name.is_some()
        || claims.claimed_org.is_some()
        || claims.claimed_role.is_some()
        || claims.purpose.is_some()
    {
        profile.confidence = PublicContactProfileConfidence::UserClaimed;
    }
}

fn merge_display_name(
    profile: &mut PublicContactProfile,
    display_name: Option<&str>,
    chat_session_id: &str,
    chat_turn_id: Option<&str>,
    now_ms: i64,
) {
    let Some(value) = display_name.and_then(normalize_public_contact_display_name) else {
        return;
    };
    if profile
        .display_name
        .as_deref()
        .map(str::trim)
        .is_some_and(|existing| existing.eq_ignore_ascii_case(&value))
    {
        return;
    }

    profile.display_name = Some(value);
    profile.provenance.push(PublicContactProfileProvenance {
        field: "display_name".to_string(),
        source: "channel_display_name".to_string(),
        observed_at: now_ms,
        chat_session_id: Some(chat_session_id.to_string()),
        chat_turn_id: chat_turn_id.map(str::to_string),
    });
}

fn normalize_public_contact_display_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.contains('@') || !trimmed.chars().any(char::is_alphabetic) {
        return None;
    }
    let lower = trimmed.to_lowercase();
    let rejected = [
        "unknown",
        "unknown user",
        "whatsapp user",
        "kapso user",
        "contact",
    ];
    if rejected.iter().any(|value| lower == *value) {
        return None;
    }
    Some(trimmed.chars().take(120).collect())
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn normalize_research_summary(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .chars()
            .take(PUBLIC_CONTACT_RESEARCH_SUMMARY_MAX_CHARS)
            .collect(),
    )
}

fn research_summary_excerpt(value: &str) -> String {
    let collapsed = value
        .chars()
        .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= PUBLIC_CONTACT_RESEARCH_EXCERPT_MAX_CHARS {
        return collapsed;
    }
    let mut excerpt = collapsed
        .chars()
        .take(PUBLIC_CONTACT_RESEARCH_EXCERPT_MAX_CHARS)
        .collect::<String>();
    excerpt.push_str("...");
    excerpt
}

#[derive(Debug, Clone, Default)]
struct ParsedPublicContactResearch {
    confidence: PublicContactProfileConfidence,
    possible_identity: Option<String>,
    org: Option<String>,
    role: Option<String>,
    sources: Vec<PublicContactResearchLead>,
    warnings: Vec<String>,
    suggested_memory_fields: Vec<PublicContactMemorySuggestion>,
}

fn parse_public_contact_research_summary(summary: &str) -> ParsedPublicContactResearch {
    let sources = extract_research_sources(summary);
    let warnings = extract_research_warnings(summary);
    let suggested_memory_fields = extract_research_memory_suggestions(summary);
    let possible_identity =
        first_suggestion_value(&suggested_memory_fields, &["name", "identity", "person"])
            .or_else(|| extract_likely_research_identity(summary));
    let org = first_suggestion_value(
        &suggested_memory_fields,
        &["org", "organization", "company"],
    )
    .or_else(|| extract_labeled_research_value(summary, &["organization", "company", "org"]));
    let role = first_suggestion_value(&suggested_memory_fields, &["role", "title"])
        .or_else(|| extract_labeled_research_value(summary, &["role", "title"]));
    let confidence =
        infer_research_confidence(summary, possible_identity.as_deref(), &sources, &warnings);

    ParsedPublicContactResearch {
        confidence,
        possible_identity,
        org,
        role,
        sources,
        warnings,
        suggested_memory_fields,
    }
}

fn extract_likely_research_identity(summary: &str) -> Option<String> {
    if summary.to_lowercase().contains("insufficient evidence") {
        return None;
    }

    let mut after_identity_heading = false;
    for raw_line in summary.lines() {
        let line = clean_research_line(raw_line);
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if after_identity_heading {
            if is_research_section_heading(&lower) {
                after_identity_heading = false;
            } else if let Some(candidate) = clean_identity_candidate(&line) {
                return Some(candidate);
            }
        }

        if !is_identity_heading(&lower) {
            continue;
        }
        if let Some(value) = value_after_research_label(
            &line,
            &[
                "likely identity summary",
                "likely identity",
                "identity summary",
                "possible identity",
                "identity",
            ],
        )
        .and_then(|value| clean_identity_candidate(&value))
        {
            return Some(value);
        }
        after_identity_heading = true;
    }
    None
}

fn extract_labeled_research_value(summary: &str, labels: &[&str]) -> Option<String> {
    for raw_line in summary.lines() {
        let line = clean_research_line(raw_line);
        if line.is_empty() {
            continue;
        }
        if let Some(value) = value_after_research_label(&line, labels)
            .and_then(|value| clean_research_field_value(&value, 180))
        {
            return Some(value);
        }
    }
    None
}

fn extract_research_sources(summary: &str) -> Vec<PublicContactResearchLead> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for raw_line in summary.lines() {
        for url in extract_urls_from_line(raw_line) {
            if !seen.insert(url.clone()) {
                continue;
            }
            sources.push(PublicContactResearchLead {
                label: research_source_label(raw_line, &url),
                confidence: confidence_label_from_text(raw_line),
                url,
            });
            if sources.len() >= PUBLIC_CONTACT_RESEARCH_MAX_SOURCES {
                return sources;
            }
        }
    }
    sources
}

fn extract_urls_from_line(line: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for token in line.split_whitespace() {
        let Some(idx) = token.find("https://").or_else(|| token.find("http://")) else {
            continue;
        };
        let url = token[idx..]
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '<' | '>' | '[' | ']' | '(' | ')' | '{' | '}' | '"' | '\''
                )
            })
            .trim_end_matches(|ch: char| {
                matches!(ch, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
            });
        if url.starts_with("http://") || url.starts_with("https://") {
            urls.push(url.chars().take(300).collect());
        }
    }
    urls
}

fn research_source_label(line: &str, url: &str) -> Option<String> {
    let label = clean_research_line(line)
        .replace(url, "")
        .replace("()", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    clean_research_field_value(&label, 220).filter(|value| {
        let lower = value.to_lowercase();
        !lower.starts_with("http://")
            && !lower.starts_with("https://")
            && !lower.starts_with("public profile leads")
            && !lower.starts_with("sources")
    })
}

fn extract_research_warnings(summary: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();
    let mut in_warning_section = false;
    for raw_line in summary.lines() {
        let line = clean_research_line(raw_line);
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if is_public_source_heading(&lower) {
            in_warning_section = false;
            continue;
        }
        if is_warning_heading(&lower) {
            in_warning_section = true;
            continue;
        }
        if in_warning_section && is_research_section_heading(&lower) {
            in_warning_section = false;
        }
        if !(in_warning_section || line_contains_warning(&lower)) {
            continue;
        }
        if is_benign_warning_line(&lower) {
            continue;
        }
        if let Some(value) = clean_research_field_value(&line, 240) {
            let key = value.to_lowercase();
            if seen.insert(key) {
                warnings.push(value);
                if warnings.len() >= PUBLIC_CONTACT_RESEARCH_MAX_WARNINGS {
                    break;
                }
            }
        }
    }
    warnings
}

fn extract_research_memory_suggestions(summary: &str) -> Vec<PublicContactMemorySuggestion> {
    let mut suggestions = Vec::new();
    let mut seen = HashSet::new();
    let mut in_suggestion_section = false;
    for raw_line in summary.lines() {
        let line = clean_research_line(raw_line);
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if is_suggestion_heading(&lower) {
            in_suggestion_section = true;
            continue;
        }
        if !in_suggestion_section {
            continue;
        }
        if let Some(suggestion) = parse_memory_suggestion_line(&line) {
            let key = format!("{}={}", suggestion.field, suggestion.value.to_lowercase());
            if seen.insert(key) {
                suggestions.push(suggestion);
                if suggestions.len() >= PUBLIC_CONTACT_RESEARCH_MAX_SUGGESTIONS {
                    break;
                }
            }
        }
    }
    suggestions
}

fn parse_memory_suggestion_line(line: &str) -> Option<PublicContactMemorySuggestion> {
    let mut body = line.trim();
    let mut source_kind = research_source_kind_from_text(body);
    let lower = body.to_lowercase();
    for prefix in [
        "user-claimed",
        "user claimed",
        "researched lead",
        "research lead",
        "researched",
        "inferred",
    ] {
        if lower.starts_with(prefix) {
            body = body
                .split_once(':')
                .map(|(_, rest)| rest.trim())
                .unwrap_or(body);
            break;
        }
    }
    let (field, value) = body.split_once(':')?;
    let field = normalize_research_field_name(field)?;
    let value = clean_research_field_value(value, 300)?;
    if source_kind.is_none() {
        source_kind = research_source_kind_from_text(&value);
    }
    Some(PublicContactMemorySuggestion {
        field,
        value,
        source_kind,
    })
}

fn first_suggestion_value(
    suggestions: &[PublicContactMemorySuggestion],
    field_needles: &[&str],
) -> Option<String> {
    suggestions.iter().find_map(|suggestion| {
        let field = suggestion.field.to_lowercase();
        field_needles
            .iter()
            .any(|needle| field.contains(needle))
            .then(|| suggestion.value.clone())
    })
}

fn infer_research_confidence(
    summary: &str,
    possible_identity: Option<&str>,
    sources: &[PublicContactResearchLead],
    warnings: &[String],
) -> PublicContactProfileConfidence {
    let lower = summary.to_lowercase();
    if lower.contains("insufficient evidence")
        || lower.contains("not enough evidence")
        || lower.contains("unable to identify")
        || lower.contains("unable to verify")
    {
        return PublicContactProfileConfidence::ResearchedLow;
    }
    let has_evidence = possible_identity.is_some() || !sources.is_empty();
    if !has_evidence {
        return PublicContactProfileConfidence::Unknown;
    }
    if (lower.contains("high confidence") || lower.contains("confidence: high"))
        && possible_identity.is_some()
        && sources.len() >= 2
    {
        return PublicContactProfileConfidence::ResearchedHigh;
    }
    if lower.contains("medium confidence")
        || lower.contains("confidence: medium")
        || (possible_identity.is_some() && sources.len() >= 2 && warnings.is_empty())
    {
        return PublicContactProfileConfidence::ResearchedMedium;
    }
    PublicContactProfileConfidence::ResearchedLow
}

fn value_after_research_label(line: &str, labels: &[&str]) -> Option<String> {
    let lower = line.to_lowercase();
    for label in labels {
        if !lower.starts_with(label) {
            continue;
        }
        let value = line[label.len()..]
            .trim_start_matches(|ch: char| matches!(ch, ':' | '-' | ' '))
            .trim();
        return (!value.is_empty()).then(|| value.to_string());
    }
    None
}

fn clean_research_line(line: &str) -> String {
    let mut value = line.trim().trim_matches('*').trim();
    value = value.trim_start_matches('#').trim();
    value = value
        .trim_start_matches(|ch: char| matches!(ch, '-' | '*' | '+'))
        .trim();
    value = strip_numbered_prefix(value).trim();
    value.trim_matches('*').trim().chars().take(700).collect()
}

fn strip_numbered_prefix(value: &str) -> &str {
    let mut chars = value.char_indices().peekable();
    let mut end_digits = None;
    while let Some((idx, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            end_digits = Some(idx + ch.len_utf8());
            chars.next();
        } else {
            break;
        }
    }
    let Some(end_digits) = end_digits else {
        return value;
    };
    let rest = value[end_digits..].trim_start();
    if rest.starts_with('.') || rest.starts_with(')') {
        rest[1..].trim_start()
    } else {
        value
    }
}

fn clean_identity_candidate(value: &str) -> Option<String> {
    let value = clean_research_field_value(value, 220)?;
    let lower = value.to_lowercase();
    let rejected = [
        "unknown",
        "n/a",
        "none",
        "insufficient evidence",
        "not enough evidence",
        "unable to identify",
    ];
    if rejected
        .iter()
        .any(|needle| lower == *needle || lower.starts_with(needle))
        || lower.starts_with("confidence:")
        || lower.starts_with("source:")
        || lower.starts_with("sources:")
        || lower.starts_with("public profile")
        || !value.chars().any(char::is_alphabetic)
    {
        return None;
    }
    Some(value)
}

fn clean_research_field_value(value: &str, max_chars: usize) -> Option<String> {
    let cleaned = value
        .trim()
        .trim_matches('*')
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .chars()
        .take(max_chars)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let lower = cleaned.to_lowercase();
    if cleaned.is_empty()
        || lower == "unknown"
        || lower == "n/a"
        || lower == "none"
        || lower == "not specified"
    {
        None
    } else {
        Some(cleaned)
    }
}

fn normalize_research_field_name(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .trim_matches('*')
        .to_lowercase()
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch)
            } else if ch.is_whitespace() || ch == '-' {
                Some('_')
            } else {
                None
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(64)
        .collect::<String>();
    (!normalized.is_empty()
        && normalized != "suggested_memory"
        && normalized != "suggested_contact_fields")
        .then_some(normalized)
}

fn confidence_label_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if lower.contains("high confidence") || lower.contains("confidence: high") {
        Some("high".to_string())
    } else if lower.contains("medium confidence") || lower.contains("confidence: medium") {
        Some("medium".to_string())
    } else if lower.contains("low confidence") || lower.contains("confidence: low") {
        Some("low".to_string())
    } else {
        None
    }
}

fn research_source_kind_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if lower.contains("user-claimed") || lower.contains("user claimed") {
        Some("user_claimed".to_string())
    } else if lower.contains("research") || lower.contains("public") {
        Some("researched_lead".to_string())
    } else if lower.contains("inferred") {
        Some("inferred".to_string())
    } else {
        None
    }
}

fn is_identity_heading(lower_line: &str) -> bool {
    lower_line.contains("likely identity") || lower_line.contains("identity summary")
}

fn is_warning_heading(lower_line: &str) -> bool {
    lower_line.contains("warning")
        || lower_line.contains("uncertainty")
        || lower_line.contains("contradiction")
        || lower_line.contains("contradicting")
}

fn is_suggestion_heading(lower_line: &str) -> bool {
    lower_line.contains("suggested memory")
        || lower_line.contains("memory/contact fields")
        || lower_line.contains("contact fields")
}

fn is_public_source_heading(lower_line: &str) -> bool {
    lower_line.starts_with("public profile")
        || lower_line.starts_with("sources")
        || lower_line.contains("public profile leads")
}

fn is_research_section_heading(lower_line: &str) -> bool {
    is_identity_heading(lower_line)
        || is_warning_heading(lower_line)
        || is_suggestion_heading(lower_line)
        || is_public_source_heading(lower_line)
        || lower_line.contains("corroborating details")
}

fn line_contains_warning(lower_line: &str) -> bool {
    lower_line.contains("insufficient evidence")
        || lower_line.contains("not enough evidence")
        || lower_line.contains("unable to verify")
        || lower_line.contains("unable to identify")
        || lower_line.contains("unverified")
        || lower_line.contains("low confidence")
        || lower_line.contains("uncertain")
        || lower_line.contains("contradict")
        || lower_line.contains("warning")
}

fn is_benign_warning_line(lower_line: &str) -> bool {
    lower_line.contains("no contradictions")
        || lower_line.contains("no contradiction")
        || lower_line.contains("no warnings")
        || lower_line.contains("no known contradictions")
}

fn merge_claim_field(
    slot: &mut Option<String>,
    value: Option<&str>,
    field: &str,
    provenance: &mut Vec<PublicContactProfileProvenance>,
    chat_session_id: &str,
    chat_turn_id: Option<&str>,
    now_ms: i64,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if slot
        .as_deref()
        .map(str::trim)
        .is_some_and(|existing| existing.eq_ignore_ascii_case(value))
    {
        return;
    }
    *slot = Some(value.to_string());
    provenance.push(PublicContactProfileProvenance {
        field: field.to_string(),
        source: "user_message".to_string(),
        observed_at: now_ms,
        chat_session_id: Some(chat_session_id.to_string()),
        chat_turn_id: chat_turn_id.map(str::to_string),
    });
}

fn extract_public_contact_claims(text: &str) -> PublicContactProfileClaims {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return PublicContactProfileClaims::default();
    }

    let mut claims = PublicContactProfileClaims::default();
    claims.claimed_name =
        extract_after_prefix(trimmed, &["my name is ", "i am ", "i'm ", "this is "], 80)
            .filter(|value| looks_like_identity_claim(value));
    claims.claimed_org = extract_after_marker(trimmed, &[" from ", " at "], 100)
        .filter(|value| value.split_whitespace().count() <= 8);
    claims.purpose = extract_purpose(trimmed);
    claims
}

fn extract_after_prefix(text: &str, prefixes: &[&str], max_len: usize) -> Option<String> {
    let lower = text.to_lowercase();
    prefixes.iter().find_map(|prefix| {
        lower.strip_prefix(prefix).map(|_| {
            let raw = &text[prefix.len()..];
            clean_claim_value(raw, max_len)
        })?
    })
}

fn extract_after_marker(text: &str, markers: &[&str], max_len: usize) -> Option<String> {
    let lower = text.to_lowercase();
    markers.iter().find_map(|marker| {
        lower.find(marker).and_then(|idx| {
            let raw = &text[idx + marker.len()..];
            clean_claim_value(raw, max_len)
        })
    })
}

fn extract_purpose(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let purpose_markers = [
        "need help",
        "looking for",
        "reaching out",
        "want to",
        "trying to",
        "can you",
        "could you",
        "help me",
    ];
    if purpose_markers.iter().any(|marker| lower.contains(marker)) {
        return Some(text.chars().take(500).collect::<String>());
    }
    None
}

fn clean_claim_value(raw: &str, max_len: usize) -> Option<String> {
    let value = raw
        .split(['\n', '.', ',', ';', '!', '?'])
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(max_len)
        .collect::<String>();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn looks_like_identity_claim(value: &str) -> bool {
    let lower = value.to_lowercase();
    let rejected_starts = [
        "looking ",
        "trying ",
        "here ",
        "just ",
        "calling ",
        "messaging ",
        "reaching ",
        "checking ",
    ];
    !rejected_starts
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        && value.split_whitespace().count() <= 8
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::chat::models::{ChatChannel, ChatSession, ChatSessionStatus};

    fn test_session() -> ChatSession {
        ChatSession {
            internal_voice: None,
            id: "session-1".to_string(),
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            agent_id: "envoy".to_string(),
            ui_thread_id: "kapso:9199".to_string(),
            title: None,
            origin_channel: ChatChannel::new("kapso", "9199@s.whatsapp.net"),
            status: ChatSessionStatus::Active,
            history_lane: crate::magician_v2::history::HistoryLane::Automated,
            is_default_session: false,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn parses_structured_identity_research_summary() {
        let parsed = parse_public_contact_research_summary(
            r#"
1. likely identity summary:
- Jane Rao, product lead at Acme AI (medium confidence)

2. public profile leads with URLs and confidence/uncertainty:
- LinkedIn: https://www.linkedin.com/in/janerao (medium confidence)
- Company page: https://acme.example/team

3. corroborating details and contradictions:
- No contradictions found.

4. suggested memory/contact fields:
- researched lead: name: Jane Rao
- researched lead: org: Acme AI
- researched lead: role: Product lead
"#,
        );

        assert_eq!(
            parsed.confidence,
            PublicContactProfileConfidence::ResearchedMedium
        );
        assert_eq!(parsed.possible_identity.as_deref(), Some("Jane Rao"));
        assert_eq!(parsed.org.as_deref(), Some("Acme AI"));
        assert_eq!(parsed.role.as_deref(), Some("Product lead"));
        assert_eq!(parsed.sources.len(), 2);
        assert!(parsed.warnings.is_empty());
        assert_eq!(parsed.suggested_memory_fields.len(), 3);
    }

    #[test]
    fn parses_insufficient_evidence_as_low_confidence_warning() {
        let parsed = parse_public_contact_research_summary(
            r#"
1. likely identity summary:
Insufficient evidence to identify this contact.

2. public profile leads with URLs and confidence/uncertainty:
No reliable public profile leads.

3. corroborating details and contradictions:
- Unable to verify the claimed organization from public sources.
"#,
        );

        assert_eq!(
            parsed.confidence,
            PublicContactProfileConfidence::ResearchedLow
        );
        assert!(parsed.possible_identity.is_none());
        assert!(parsed.sources.is_empty());
        assert_eq!(parsed.warnings.len(), 2);
    }

    #[tokio::test]
    async fn completed_research_keeps_unreviewed_confidence_on_result_only() {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let store = FilePublicContactProfileStore::new(ArtifactV2Workspace::new(
            temp.path().join("magician_data_v3"),
        ));
        let session = test_session();
        store
            .load_or_create_for_session(&session, Some("Jane"), 1)
            .await
            .expect("profile created");

        let profile = store
            .mark_research_completed(
                &session,
                "job-1",
                Some("task-1"),
                Some("exec-1"),
                Some("out-1"),
                Some(
                    r#"
1. likely identity summary:
- Jane Rao, product lead at Acme AI (high confidence)

2. public profile leads with URLs and confidence/uncertainty:
- LinkedIn: https://www.linkedin.com/in/janerao (high confidence)
- Company page: https://acme.example/team (high confidence)

4. suggested memory/contact fields:
- researched lead: name: Jane Rao
"#,
                ),
                2,
            )
            .await
            .expect("research completed");

        assert_eq!(profile.confidence, PublicContactProfileConfidence::Unknown);
        assert_eq!(
            profile
                .research_result
                .as_ref()
                .map(|result| &result.confidence),
            Some(&PublicContactProfileConfidence::ResearchedHigh)
        );
    }
}
