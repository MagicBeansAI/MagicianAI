//! `memory.user_knowledge_bounds` — the size bounds for the owner's user
//! memory file (`memory/users/knowledge.json`).
//!
//! Defaults live here, so a live config without the block behaves exactly as
//! documented below. The `memory` block rejects unknown keys, so a config that
//! names this block cannot be read by a binary older than the one that added
//! it: ship the binary first, then add the key.
//!
//! See `docs/archive/plans/2026-10-18-user-memory-growth-bounds.md`.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// What the bounds do when a write would push data out of the main file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserKnowledgeBoundsMode {
    /// Writes behave as they did before the bounds existed.
    Off,
    /// Work out what would move, log it, change nothing. The default.
    #[default]
    Observe,
    /// Activity logs go to their own files; repeats are stored once; caps
    /// apply. Everything that leaves the main file is archived first.
    Enforce,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MagicianMemoryBoundsSettings {
    #[serde(default)]
    pub mode: UserKnowledgeBoundsMode,
    /// Size goal for `knowledge.json`. A goal for the size report, not a stop.
    #[serde(default = "default_hot_file_target_bytes")]
    pub hot_file_target_bytes: u64,
    #[serde(default)]
    pub lanes: UserKnowledgeLaneSettings,
    /// Sections where one claim is stored once. An agent-made restatement
    /// (the same key however it is spelled, or near-identical wording under
    /// another key) replaces the older wording; an exact duplicate of an
    /// owner record merges into the owner's strongest copy. Either way the
    /// replaced wording is archived and its evidence carried. Protection is
    /// per record, so an owner-stated preference is never replaced by an
    /// agent's rewording; any registered section may be listed.
    #[serde(default = "default_fold_inferred_same_key")]
    pub fold_inferred_same_key: Vec<String>,
    /// Records per section. Over the cap, unprotected records are archived,
    /// least used first. Owner-stated, owner-confirmed, corrected, reviewed,
    /// restored and conflicted records are never archived.
    #[serde(default = "default_max_live")]
    pub max_live: BTreeMap<String, usize>,
    /// Under cap pressure, retired records older than this go first.
    #[serde(default = "default_retired_archive_after_days")]
    pub retired_archive_after_days: u32,
    #[serde(default)]
    pub archive: UserKnowledgeArchiveSettings,
    #[serde(default)]
    pub review: UserKnowledgeReviewSettings,
}

impl Default for MagicianMemoryBoundsSettings {
    fn default() -> Self {
        Self {
            mode: UserKnowledgeBoundsMode::default(),
            hot_file_target_bytes: default_hot_file_target_bytes(),
            lanes: UserKnowledgeLaneSettings::default(),
            fold_inferred_same_key: default_fold_inferred_same_key(),
            max_live: default_max_live(),
            retired_archive_after_days: default_retired_archive_after_days(),
            archive: UserKnowledgeArchiveSettings::default(),
            review: UserKnowledgeReviewSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UserKnowledgeLaneSettings {
    /// Under `mode: enforce`, the activity logs (email/chat evidence, channel
    /// patterns, channel feedback) are written to `memory/users/lanes/`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Lane rows older than this move to the archive.
    #[serde(default = "default_lane_hot_days")]
    pub hot_days: u32,
    /// Rows per lane; the oldest beyond it move to the archive.
    #[serde(default = "default_lane_max_rows")]
    pub max_rows: BTreeMap<String, usize>,
    /// Let the evidence distiller read the email and chat activity logs.
    /// Off until the owner says yes: it means background model calls and new
    /// memory suggestions drawn from email and chat.
    #[serde(default)]
    pub distill_enabled: bool,
}

impl Default for UserKnowledgeLaneSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hot_days: default_lane_hot_days(),
            max_rows: default_lane_max_rows(),
            distill_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UserKnowledgeArchiveSettings {
    /// Monthly archive files older than this many months are compressed.
    /// Nothing in the archive is ever deleted.
    #[serde(default = "default_compress_after_months")]
    pub compress_after_months: u32,
}

impl Default for UserKnowledgeArchiveSettings {
    fn default() -> Self {
        Self {
            compress_after_months: default_compress_after_months(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UserKnowledgeReviewSettings {
    /// Lifecycle review calls per hour (reserved before each call).
    #[serde(default = "default_review_max_per_hour")]
    pub max_per_hour: usize,
    /// Tries per record revision before it is set aside so it stops blocking
    /// the queue. The record stays in service as it was.
    #[serde(default = "default_review_max_attempts_per_record")]
    pub max_attempts_per_record: u32,
    /// Waiting records per section under `mode: enforce`; over it, the least
    /// used inferred waiting records are archived.
    #[serde(default = "default_review_max_pending_per_section")]
    pub max_pending_per_section: usize,
    /// Days a set-aside record waits before it is queued again with fresh
    /// tries. Only a verdict the reviewer could not use for that record, or a
    /// plan rejected for it, counts as a try; timeouts and outages never do.
    #[serde(default = "default_review_retry_after_days")]
    pub retry_after_days: u32,
    /// Existing records shown to the reviewer with each incoming one, chosen
    /// by meaning (local embeddings; word overlap when the model is down).
    /// The decision engine answers them one at a time, so a long list never
    /// finishes inside the deadline.
    #[serde(default = "default_review_max_offered")]
    pub max_offered: usize,
    /// Seconds one review may take: the decision engine's answers plus the
    /// written verdict. Background work, held without the memory lock.
    #[serde(default = "default_review_deadline_secs")]
    pub deadline_secs: u64,
}

impl Default for UserKnowledgeReviewSettings {
    fn default() -> Self {
        Self {
            max_per_hour: default_review_max_per_hour(),
            max_attempts_per_record: default_review_max_attempts_per_record(),
            max_pending_per_section: default_review_max_pending_per_section(),
            retry_after_days: default_review_retry_after_days(),
            max_offered: default_review_max_offered(),
            deadline_secs: default_review_deadline_secs(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_hot_file_target_bytes() -> u64 {
    1_500_000
}

fn default_fold_inferred_same_key() -> Vec<String> {
    vec![
        "workflows".to_string(),
        "organization".to_string(),
        "preferences".to_string(),
    ]
}

fn default_max_live() -> BTreeMap<String, usize> {
    BTreeMap::from([
        ("workflows".to_string(), 600),
        ("organization".to_string(), 250),
        ("preferences".to_string(), 600),
        ("knowledge".to_string(), 200),
        ("skills".to_string(), 100),
        ("accounts".to_string(), 100),
    ])
}

fn default_retired_archive_after_days() -> u32 {
    30
}

fn default_lane_hot_days() -> u32 {
    45
}

fn default_lane_max_rows() -> BTreeMap<String, usize> {
    BTreeMap::from([
        ("email_evidence".to_string(), 400),
        ("chat_evidence".to_string(), 200),
        ("channel_patterns".to_string(), 150),
        ("channel_feedback".to_string(), 120),
    ])
}

fn default_compress_after_months() -> u32 {
    3
}

fn default_review_max_per_hour() -> usize {
    6
}

fn default_review_max_attempts_per_record() -> u32 {
    3
}

fn default_review_max_pending_per_section() -> usize {
    300
}

fn default_review_retry_after_days() -> u32 {
    14
}

fn default_review_max_offered() -> usize {
    8
}

fn default_review_deadline_secs() -> u64 {
    120
}

/// The smallest accepted `hot_file_target_bytes`.
pub const MIN_HOT_FILE_TARGET_BYTES: u64 = 256 * 1024;

impl MagicianMemoryBoundsSettings {
    /// Config-load validation. Messages name the offending key.
    pub fn validate(&self) -> Result<(), String> {
        const PREFIX: &str = "memory.user_knowledge_bounds";
        if self.hot_file_target_bytes < MIN_HOT_FILE_TARGET_BYTES {
            return Err(format!(
                "{PREFIX}.hot_file_target_bytes must be at least {MIN_HOT_FILE_TARGET_BYTES}"
            ));
        }
        if self.lanes.hot_days == 0 {
            return Err(format!("{PREFIX}.lanes.hot_days must be at least 1"));
        }
        for (lane, cap) in &self.lanes.max_rows {
            if crate::magician_v2::agents::user_memory_lanes::Lane::from_name(lane).is_none() {
                return Err(format!(
                    "{PREFIX}.lanes.max_rows.{lane} is not an activity-log lane"
                ));
            }
            if *cap == 0 {
                return Err(format!("{PREFIX}.lanes.max_rows.{lane} must be at least 1"));
            }
        }
        for section in &self.fold_inferred_same_key {
            if !super::is_registered_section(section) {
                return Err(format!(
                    "{PREFIX}.fold_inferred_same_key may list only user-memory sections; \
                     `{section}` is not one"
                ));
            }
        }
        for (section, cap) in &self.max_live {
            if *cap == 0 {
                return Err(format!("{PREFIX}.max_live.{section} must be at least 1"));
            }
        }
        if self.retired_archive_after_days == 0 {
            return Err(format!(
                "{PREFIX}.retired_archive_after_days must be at least 1"
            ));
        }
        if self.archive.compress_after_months == 0 {
            return Err(format!(
                "{PREFIX}.archive.compress_after_months must be at least 1"
            ));
        }
        if self.review.max_per_hour == 0 {
            return Err(format!("{PREFIX}.review.max_per_hour must be at least 1"));
        }
        if self.review.max_attempts_per_record == 0 {
            return Err(format!(
                "{PREFIX}.review.max_attempts_per_record must be at least 1"
            ));
        }
        if !(1..=48).contains(&self.review.max_offered) {
            return Err(format!("{PREFIX}.review.max_offered must be 1 to 48"));
        }
        if !(30..=600).contains(&self.review.deadline_secs) {
            return Err(format!("{PREFIX}.review.deadline_secs must be 30 to 600"));
        }
        if self.review.retry_after_days == 0 {
            return Err(format!(
                "{PREFIX}.review.retry_after_days must be at least 1"
            ));
        }
        if self.review.max_pending_per_section == 0 {
            return Err(format!(
                "{PREFIX}.review.max_pending_per_section must be at least 1"
            ));
        }
        Ok(())
    }

    pub fn enforcing(&self) -> bool {
        self.mode == UserKnowledgeBoundsMode::Enforce
    }

    /// Whether the activity logs are written to their own files.
    pub fn lanes_active(&self) -> bool {
        self.enforcing() && self.lanes.enabled
    }

    pub fn lane_max_rows(&self, lane: &str) -> Option<usize> {
        self.lanes.max_rows.get(lane).copied()
    }
}

static INSTALLED: RwLock<Option<Arc<MagicianMemoryBoundsSettings>>> = RwLock::new(None);

tokio::task_local! {
    static OVERRIDE: Arc<MagicianMemoryBoundsSettings>;
}

/// Publish the accepted settings. Called on startup after config validation.
pub fn install_settings(settings: MagicianMemoryBoundsSettings) {
    let mut slot = INSTALLED
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = Some(Arc::new(settings));
}

/// The settings in force: a task-scoped override (tests and the one-time
/// migration), else the installed settings, else the defaults.
pub fn current_settings() -> Arc<MagicianMemoryBoundsSettings> {
    if let Ok(settings) = OVERRIDE.try_with(Arc::clone) {
        return settings;
    }
    INSTALLED
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(MagicianMemoryBoundsSettings::default()))
}

/// Run `future` with `settings` in force for this task only.
pub async fn with_settings<F: std::future::Future>(
    settings: MagicianMemoryBoundsSettings,
    future: F,
) -> F::Output {
    OVERRIDE.scope(Arc::new(settings), future).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_from_an_empty_block_and_observe() {
        let parsed: MagicianMemoryBoundsSettings = serde_yaml::from_str("{}").unwrap();
        assert_eq!(parsed, MagicianMemoryBoundsSettings::default());
        assert_eq!(parsed.mode, UserKnowledgeBoundsMode::Observe);
        assert!(!parsed.lanes_active());
        assert!(!parsed.lanes.distill_enabled);
        assert_eq!(parsed.max_live["workflows"], 600);
        assert_eq!(parsed.review.max_per_hour, 6);
        parsed.validate().unwrap();
    }

    #[test]
    fn bad_values_are_rejected_with_the_key_named() {
        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.hot_file_target_bytes = 1000;
        assert!(settings
            .validate()
            .unwrap_err()
            .contains("hot_file_target_bytes"));

        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.lanes.hot_days = 0;
        assert!(settings.validate().unwrap_err().contains("lanes.hot_days"));

        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.max_live.insert("workflows".into(), 0);
        assert!(settings
            .validate()
            .unwrap_err()
            .contains("max_live.workflows"));

        // Only user-memory sections fold; preferences may (protection is per
        // record).
        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.fold_inferred_same_key.push("user".into());
        assert!(settings
            .validate()
            .unwrap_err()
            .contains("fold_inferred_same_key"));
        assert!(MagicianMemoryBoundsSettings::default()
            .fold_inferred_same_key
            .contains(&"preferences".to_string()));

        let mut settings = MagicianMemoryBoundsSettings::default();
        settings.lanes.max_rows.insert("preferences".into(), 10);
        assert!(settings.validate().unwrap_err().contains("lanes.max_rows"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(serde_yaml::from_str::<MagicianMemoryBoundsSettings>("surprise: 1").is_err());
        assert!(
            serde_yaml::from_str::<MagicianMemoryBoundsSettings>("lanes: {surprise: 1}").is_err()
        );
    }

    #[tokio::test]
    async fn a_task_override_wins_only_inside_its_scope() {
        let mut enforce = MagicianMemoryBoundsSettings::default();
        enforce.mode = UserKnowledgeBoundsMode::Enforce;
        let inside = with_settings(enforce, async { current_settings().mode }).await;
        assert_eq!(inside, UserKnowledgeBoundsMode::Enforce);
    }
}
