//! Hot projection cache for memory candidates.
//!
//! Canonical memory remains in semantic tier/user/episode files. This module
//! stores compact prompt-ready projections under `memory/index`, keyed by the
//! source memory candidate key. Renderers may prefer an active projection over a
//! large/noisy source while feedback continues to update the source candidate.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, OnceLock, RwLock},
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    memory_candidates::SemanticMemoryType,
    memory_temperature::{MemoryTemperatureTier, MemoryTemperatureUtilityLabel},
    storage_trait::{MemoryStorage, MemoryStorageError},
};

pub const MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION: u32 = 3;
pub const MEMORY_HOT_PROJECTION_POLICY_VERSION: u32 = 1;
pub const MEMORY_HOT_PROJECTIONS_FILE: &str = "hot_projections.json";
const DEFAULT_MAX_UNINJECTED_AGE_DAYS: i64 = 30;
const DEFAULT_MAX_UNVERIFIED_AGE_DAYS: i64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHotProjectionIndex {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub projections: BTreeMap<String, MemoryHotProjectionRecord>,
}

impl Default for MemoryHotProjectionIndex {
    fn default() -> Self {
        Self {
            schema_version: MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION,
            updated_at: Utc::now(),
            projections: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHotProjectionRecord {
    pub source_memory_candidate_key: String,
    pub semantic_memory_type: SemanticMemoryType,
    pub temperature_tier: MemoryTemperatureTier,
    pub compact_text: String,
    pub source_text_hash: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub source_tier_name: Option<String>,
    #[serde(default)]
    pub source_item_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub last_verified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_injected_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub injected_count: u32,
    #[serde(default)]
    pub review_run_id: Option<String>,
    #[serde(default)]
    pub review_label: Option<MemoryTemperatureUtilityLabel>,
    #[serde(default)]
    pub reviewer_confidence: Option<f64>,
    #[serde(default)]
    pub promotion_reason: Option<String>,
    #[serde(default = "default_hot_projection_policy_version")]
    pub projection_policy_version: u32,
    #[serde(default)]
    pub last_regenerated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub regeneration_count: u32,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub deactivated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub deactivation_reason: Option<String>,
    #[serde(default)]
    pub last_lifecycle_check_at: Option<DateTime<Utc>>,
}

impl MemoryHotProjectionRecord {
    pub fn is_active_for_source_hash(&self, source_text_hash: &str) -> bool {
        self.active
            && self.source_text_hash == source_text_hash
            && !self.compact_text.trim().is_empty()
    }

    /// Evaluate the prompt-visible result of lifecycle maintenance without
    /// mutating the durable projection index.
    ///
    /// A current candidate refreshes `last_verified_at` before verification
    /// expiry is evaluated, so only policy, source, payload, and unused-age
    /// checks can reject that candidate during prompt rendering.
    pub fn is_prompt_eligible_for_source_hash(
        &self,
        source_text_hash: &str,
        policy: MemoryHotProjectionMaintenancePolicy,
        now: DateTime<Utc>,
    ) -> bool {
        self.is_active_for_source_hash(source_text_hash)
            && self.projection_policy_version == policy.expected_policy_version
            && !(self.injected_count == 0
                && now - self.updated_at > Duration::days(policy.max_uninjected_age_days.max(1)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HotProjectionFileStamp {
    len: u64,
    modified_nanos: Option<u128>,
}

#[derive(Debug, Clone)]
struct HotProjectionSnapshotCacheEntry {
    stamp: Option<HotProjectionFileStamp>,
    index: Arc<MemoryHotProjectionIndex>,
}

fn hot_projection_snapshot_cache(
) -> &'static RwLock<HashMap<PathBuf, HotProjectionSnapshotCacheEntry>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, HotProjectionSnapshotCacheEntry>>> =
        OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryHotProjectionUpsert {
    pub source_memory_candidate_key: String,
    pub semantic_memory_type: SemanticMemoryType,
    pub source_text_hash: String,
    pub compact_text: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub source_tier_name: Option<String>,
    #[serde(default)]
    pub source_item_key: Option<String>,
    #[serde(default)]
    pub review_run_id: Option<String>,
    pub review_label: MemoryTemperatureUtilityLabel,
    #[serde(default)]
    pub reviewer_confidence: Option<f64>,
    #[serde(default)]
    pub promotion_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryHotProjectionUpsertSummary {
    pub upserted: usize,
    pub created: usize,
    pub updated: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryHotProjectionUsageSummary {
    pub touched: usize,
    pub missing: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryHotProjectionMaintenancePolicy {
    pub max_uninjected_age_days: i64,
    pub max_unverified_age_days: i64,
    #[serde(default = "default_hot_projection_policy_version")]
    pub expected_policy_version: u32,
}

impl Default for MemoryHotProjectionMaintenancePolicy {
    fn default() -> Self {
        Self {
            max_uninjected_age_days: DEFAULT_MAX_UNINJECTED_AGE_DAYS,
            max_unverified_age_days: DEFAULT_MAX_UNVERIFIED_AGE_DAYS,
            expected_policy_version: MEMORY_HOT_PROJECTION_POLICY_VERSION,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryHotProjectionMaintenanceSummary {
    pub reviewed: usize,
    pub deactivated: usize,
    pub source_hash_mismatch: usize,
    pub unused_expired: usize,
    pub verification_expired: usize,
    pub policy_version_changed: usize,
}

fn default_hot_projection_policy_version() -> u32 {
    MEMORY_HOT_PROJECTION_POLICY_VERSION
}

pub fn memory_hot_projections_path(storage: &dyn MemoryStorage) -> PathBuf {
    storage
        .root()
        .join("index")
        .join(MEMORY_HOT_PROJECTIONS_FILE)
}

/// Rewrite pre-v6 projection keys to the length-prefixed candidate-key
/// encoding, driven by the same `legacy -> current` table the temperature
/// overlay migration builds.
///
/// Hot projections are keyed by `source_memory_candidate_key`, so they share
/// the temperature overlay's key space and must move with it. A projection left
/// under a legacy key would never match its source again and would be
/// deactivated as orphaned on the next maintenance pass, silently discarding a
/// compact projection that is still correct.
pub fn migrate_memory_hot_projection_keys(
    index: &mut MemoryHotProjectionIndex,
    renames: &BTreeMap<String, String>,
) -> usize {
    if renames.is_empty() {
        return 0;
    }
    let mut migrated = 0usize;
    for (legacy_key, current_key) in renames {
        if index.projections.contains_key(current_key) {
            continue;
        }
        let Some(mut projection) = index.projections.remove(legacy_key) else {
            continue;
        };
        projection.source_memory_candidate_key = current_key.clone();
        index.projections.insert(current_key.clone(), projection);
        migrated += 1;
    }
    migrated
}

pub async fn load_memory_hot_projection_index(
    storage: &dyn MemoryStorage,
) -> Result<MemoryHotProjectionIndex, MemoryStorageError> {
    Ok((*load_memory_hot_projection_index_snapshot(storage).await?).clone())
}

/// Load a process-shared immutable projection snapshot for prompt rendering.
///
/// Mutation entry points still clone the snapshot before their existing
/// read-modify-write cycle. Prompt renders avoid both JSON reparsing and the
/// lifecycle write that previously serialized concurrent memory branches.
pub async fn load_memory_hot_projection_index_snapshot(
    storage: &dyn MemoryStorage,
) -> Result<Arc<MemoryHotProjectionIndex>, MemoryStorageError> {
    let path = memory_hot_projections_path(storage);
    let stamp = hot_projection_file_stamp(&path).await?;
    if let Ok(cache) = hot_projection_snapshot_cache().read() {
        if let Some(entry) = cache.get(&path) {
            if entry.stamp == stamp {
                return Ok(Arc::clone(&entry.index));
            }
        }
    }

    let value = match storage.read_json_value(&path).await {
        Ok(value) => value,
        Err(MemoryStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let index = Arc::new(MemoryHotProjectionIndex::default());
            update_hot_projection_snapshot_cache(path, stamp, Arc::clone(&index));
            return Ok(index);
        },
        Err(error) => return Err(error),
    };
    let index = Arc::new(serde_json::from_value(value).map_err(MemoryStorageError::Json)?);

    // Same rule as the temperature overlay: cache only if the file held still
    // while we read it. Stamping afterwards files this content under a later
    // revision's stamp, and every later load then matches and returns content
    // that is already stale — including the load that begins each write-locked
    // read-modify-write cycle, which would then save over the newer revision.
    let stamp_after = hot_projection_file_stamp(&path).await?;
    if stamp_after == stamp {
        update_hot_projection_snapshot_cache(path, stamp_after, Arc::clone(&index));
    }
    Ok(index)
}

pub async fn save_memory_hot_projection_index(
    storage: &dyn MemoryStorage,
    index: &MemoryHotProjectionIndex,
) -> Result<(), MemoryStorageError> {
    let path = memory_hot_projections_path(storage);
    let value = serde_json::to_value(index).map_err(MemoryStorageError::Json)?;
    storage.write_json_value_atomic(&path, &value).await?;
    let stamp = hot_projection_file_stamp(&path).await?;
    update_hot_projection_snapshot_cache(path, stamp, Arc::new(index.clone()));
    Ok(())
}

async fn hot_projection_file_stamp(
    path: &PathBuf,
) -> Result<Option<HotProjectionFileStamp>, MemoryStorageError> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MemoryStorageError::Io(error)),
    };
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    Ok(Some(HotProjectionFileStamp {
        len: metadata.len(),
        modified_nanos,
    }))
}

fn update_hot_projection_snapshot_cache(
    path: PathBuf,
    stamp: Option<HotProjectionFileStamp>,
    index: Arc<MemoryHotProjectionIndex>,
) {
    if let Ok(mut cache) = hot_projection_snapshot_cache().write() {
        cache.insert(path, HotProjectionSnapshotCacheEntry { stamp, index });
    }
}

/// Serialize projection read-modify-write cycles within the process.
///
/// The atomic file write prevents a torn file, but three separate entry points
/// (upsert, maintenance, usage) each do `load -> modify -> save`, so concurrent
/// cycles would silently lose whichever update landed first. The temperature
/// overlay solved this with the same guard; the projection index never got one.
/// Process-global is fine — these writes are infrequent and short. Only leaf
/// entry points take it, never nested, so it cannot deadlock.
fn projection_write_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Rename projection keys onto the current candidate-key encoding, under the
/// same guard as every other projection write.
pub async fn migrate_memory_hot_projection_keys_for_scope(
    storage: &dyn MemoryStorage,
    renames: &BTreeMap<String, String>,
) -> Result<usize, MemoryStorageError> {
    if renames.is_empty() {
        return Ok(0);
    }
    let _guard = projection_write_lock().lock().await;
    let mut index = load_memory_hot_projection_index(storage).await?;
    let migrated = migrate_memory_hot_projection_keys(&mut index, renames);
    if migrated > 0 {
        save_memory_hot_projection_index(storage, &index).await?;
    }
    Ok(migrated)
}

pub async fn upsert_memory_hot_projections(
    storage: &dyn MemoryStorage,
    upserts: &[MemoryHotProjectionUpsert],
) -> Result<MemoryHotProjectionUpsertSummary, MemoryStorageError> {
    let _guard = projection_write_lock().lock().await;
    let mut index = load_memory_hot_projection_index(storage).await?;
    let now = Utc::now();
    let summary = apply_memory_hot_projection_upserts(&mut index, upserts, now);
    if summary.upserted > 0 {
        save_memory_hot_projection_index(storage, &index).await?;
    }
    Ok(summary)
}

pub fn apply_memory_hot_projection_upserts(
    index: &mut MemoryHotProjectionIndex,
    upserts: &[MemoryHotProjectionUpsert],
    now: DateTime<Utc>,
) -> MemoryHotProjectionUpsertSummary {
    let mut summary = MemoryHotProjectionUpsertSummary::default();
    if index.schema_version != MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION {
        index.schema_version = MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION;
    }

    for upsert in upserts {
        let key = upsert.source_memory_candidate_key.trim();
        let compact_text = upsert.compact_text.trim();
        if key.is_empty() || compact_text.is_empty() || upsert.source_text_hash.trim().is_empty() {
            summary.skipped += 1;
            continue;
        }

        let Some(temperature_tier) = memory_hot_projection_temperature_tier(
            upsert.semantic_memory_type,
            upsert.review_label,
        ) else {
            summary.skipped += 1;
            continue;
        };

        let source_ids = normalized_source_ids(&upsert.source_ids);
        let source_tier_name = normalized_optional_string(upsert.source_tier_name.as_deref());
        let source_item_key = normalized_optional_string(upsert.source_item_key.as_deref());
        match index.projections.get_mut(key) {
            Some(existing) => {
                let regenerated = existing.semantic_memory_type != upsert.semantic_memory_type
                    || existing.temperature_tier != temperature_tier
                    || existing.source_text_hash != upsert.source_text_hash.trim()
                    || existing.compact_text != compact_text
                    || existing.source_ids != source_ids
                    || existing.review_label != Some(upsert.review_label)
                    || existing.source_tier_name != source_tier_name
                    || existing.source_item_key != source_item_key
                    || existing.projection_policy_version != MEMORY_HOT_PROJECTION_POLICY_VERSION;
                existing.semantic_memory_type = upsert.semantic_memory_type;
                existing.temperature_tier = temperature_tier;
                existing.compact_text = compact_text.to_string();
                existing.source_text_hash = upsert.source_text_hash.trim().to_string();
                existing.source_ids = source_ids;
                existing.source_tier_name = source_tier_name;
                existing.source_item_key = source_item_key;
                existing.updated_at = now;
                existing.last_verified_at = Some(now);
                existing.review_run_id = upsert.review_run_id.clone();
                existing.review_label = Some(upsert.review_label);
                existing.reviewer_confidence = upsert
                    .reviewer_confidence
                    .map(|value| value.clamp(0.0, 1.0));
                existing.promotion_reason = upsert
                    .promotion_reason
                    .as_deref()
                    .map(|reason| truncate_projection_text(reason, 1_000));
                existing.projection_policy_version = MEMORY_HOT_PROJECTION_POLICY_VERSION;
                if regenerated {
                    existing.last_regenerated_at = Some(now);
                    existing.regeneration_count = existing.regeneration_count.saturating_add(1);
                }
                existing.active = true;
                existing.deactivated_at = None;
                existing.deactivation_reason = None;
                existing.last_lifecycle_check_at = Some(now);
                summary.updated += 1;
                summary.upserted += 1;
            },
            None => {
                index.projections.insert(
                    key.to_string(),
                    MemoryHotProjectionRecord {
                        source_memory_candidate_key: key.to_string(),
                        semantic_memory_type: upsert.semantic_memory_type,
                        temperature_tier,
                        compact_text: compact_text.to_string(),
                        source_text_hash: upsert.source_text_hash.trim().to_string(),
                        source_ids,
                        source_tier_name,
                        source_item_key,
                        created_at: now,
                        updated_at: now,
                        last_verified_at: Some(now),
                        last_injected_at: None,
                        injected_count: 0,
                        review_run_id: upsert.review_run_id.clone(),
                        review_label: Some(upsert.review_label),
                        reviewer_confidence: upsert
                            .reviewer_confidence
                            .map(|value| value.clamp(0.0, 1.0)),
                        promotion_reason: upsert
                            .promotion_reason
                            .as_deref()
                            .map(|reason| truncate_projection_text(reason, 1_000)),
                        projection_policy_version: MEMORY_HOT_PROJECTION_POLICY_VERSION,
                        last_regenerated_at: None,
                        regeneration_count: 0,
                        active: true,
                        deactivated_at: None,
                        deactivation_reason: None,
                        last_lifecycle_check_at: Some(now),
                    },
                );
                summary.created += 1;
                summary.upserted += 1;
            },
        }
    }

    if summary.upserted > 0 {
        index.updated_at = now;
    }
    summary
}

pub async fn maintain_memory_hot_projections(
    storage: &dyn MemoryStorage,
    source_hashes: &BTreeMap<String, String>,
    policy: MemoryHotProjectionMaintenancePolicy,
) -> Result<
    (
        MemoryHotProjectionIndex,
        MemoryHotProjectionMaintenanceSummary,
    ),
    MemoryStorageError,
> {
    let _guard = projection_write_lock().lock().await;
    let mut index = load_memory_hot_projection_index(storage).await?;
    let now = Utc::now();
    let summary = apply_memory_hot_projection_maintenance(&mut index, source_hashes, policy, now);
    if summary.reviewed > 0 {
        save_memory_hot_projection_index(storage, &index).await?;
    }
    Ok((index, summary))
}

pub fn apply_memory_hot_projection_maintenance(
    index: &mut MemoryHotProjectionIndex,
    source_hashes: &BTreeMap<String, String>,
    policy: MemoryHotProjectionMaintenancePolicy,
    now: DateTime<Utc>,
) -> MemoryHotProjectionMaintenanceSummary {
    let mut summary = MemoryHotProjectionMaintenanceSummary::default();
    if index.schema_version != MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION {
        index.schema_version = MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION;
    }
    let max_uninjected_age = Duration::days(policy.max_uninjected_age_days.max(1));
    let max_unverified_age = Duration::days(policy.max_unverified_age_days.max(1));
    for projection in index.projections.values_mut() {
        if !projection.active {
            continue;
        }
        summary.reviewed += 1;
        projection.last_lifecycle_check_at = Some(now);
        if projection.projection_policy_version != policy.expected_policy_version {
            deactivate_projection(
                projection,
                now,
                "projection_policy_version_changed",
                &mut summary.policy_version_changed,
            );
            continue;
        }
        if let Some(source_hash) = source_hashes.get(&projection.source_memory_candidate_key) {
            if source_hash != &projection.source_text_hash {
                deactivate_projection(
                    projection,
                    now,
                    "source_hash_mismatch",
                    &mut summary.source_hash_mismatch,
                );
                continue;
            }
            projection.last_verified_at = Some(now);
        }
        if projection.injected_count == 0 && now - projection.updated_at > max_uninjected_age {
            deactivate_projection(
                projection,
                now,
                "unused_projection_expired",
                &mut summary.unused_expired,
            );
            continue;
        }
        let last_verified = projection.last_verified_at.unwrap_or(projection.updated_at);
        if now - last_verified > max_unverified_age {
            deactivate_projection(
                projection,
                now,
                "projection_verification_expired",
                &mut summary.verification_expired,
            );
        }
    }
    summary.deactivated = summary.source_hash_mismatch
        + summary.unused_expired
        + summary.verification_expired
        + summary.policy_version_changed;
    if summary.reviewed > 0 {
        index.updated_at = now;
    }
    summary
}

fn deactivate_projection(
    projection: &mut MemoryHotProjectionRecord,
    now: DateTime<Utc>,
    reason: &str,
    counter: &mut usize,
) {
    if !projection.active {
        return;
    }
    projection.active = false;
    projection.deactivated_at = Some(now);
    projection.deactivation_reason = Some(reason.to_string());
    projection.updated_at = now;
    *counter += 1;
}

pub fn memory_hot_projection_temperature_tier(
    semantic_memory_type: SemanticMemoryType,
    review_label: MemoryTemperatureUtilityLabel,
) -> Option<MemoryTemperatureTier> {
    if !memory_hot_projection_lane_allows_projection(semantic_memory_type) {
        return None;
    }
    match review_label {
        MemoryTemperatureUtilityLabel::LoadBearing
            if memory_hot_projection_lane_allows_t0(semantic_memory_type) =>
        {
            Some(MemoryTemperatureTier::T0)
        },
        MemoryTemperatureUtilityLabel::LoadBearing | MemoryTemperatureUtilityLabel::Useful => {
            Some(MemoryTemperatureTier::T1)
        },
        _ => None,
    }
}

pub fn memory_hot_projection_lane_allows_projection(lane: SemanticMemoryType) -> bool {
    matches!(
        lane,
        SemanticMemoryType::UserPreference
            | SemanticMemoryType::Procedure
            | SemanticMemoryType::ProjectContext
            | SemanticMemoryType::CodeKnowledge
            | SemanticMemoryType::Entity
            | SemanticMemoryType::Episode
    )
}

pub fn memory_hot_projection_lane_allows_t0(lane: SemanticMemoryType) -> bool {
    matches!(
        lane,
        SemanticMemoryType::UserPreference
            | SemanticMemoryType::Procedure
            | SemanticMemoryType::ProjectContext
            | SemanticMemoryType::CodeKnowledge
            | SemanticMemoryType::Entity
    )
}

pub async fn record_memory_hot_projection_usage(
    storage: &dyn MemoryStorage,
    source_memory_candidate_keys: &[String],
) -> Result<MemoryHotProjectionUsageSummary, MemoryStorageError> {
    let _guard = projection_write_lock().lock().await;
    let mut index = load_memory_hot_projection_index(storage).await?;
    let now = Utc::now();
    let summary = apply_memory_hot_projection_usage(&mut index, source_memory_candidate_keys, now);
    if summary.touched > 0 {
        save_memory_hot_projection_index(storage, &index).await?;
    }
    Ok(summary)
}

pub fn apply_memory_hot_projection_usage(
    index: &mut MemoryHotProjectionIndex,
    source_memory_candidate_keys: &[String],
    now: DateTime<Utc>,
) -> MemoryHotProjectionUsageSummary {
    let mut summary = MemoryHotProjectionUsageSummary::default();
    let mut seen = std::collections::BTreeSet::<String>::new();
    for key in source_memory_candidate_keys {
        let key = key.trim();
        if key.is_empty() || !seen.insert(key.to_string()) {
            continue;
        }
        let Some(projection) = index.projections.get_mut(key) else {
            summary.missing += 1;
            continue;
        };
        projection.last_injected_at = Some(now);
        projection.injected_count = projection.injected_count.saturating_add(1);
        projection.updated_at = now;
        summary.touched += 1;
    }
    if summary.touched > 0 {
        index.updated_at = now;
    }
    summary
}

pub fn source_text_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

pub fn truncate_projection_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        trimmed.chars().take(max_chars).collect()
    }
}

fn normalized_source_ids(source_ids: &[String]) -> Vec<String> {
    let mut ids = source_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn normalized_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_projection(key: &str) -> MemoryHotProjectionRecord {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap();
        MemoryHotProjectionRecord {
            source_memory_candidate_key: key.to_string(),
            semantic_memory_type: SemanticMemoryType::Procedure,
            temperature_tier: MemoryTemperatureTier::T1,
            compact_text: "compact".to_string(),
            source_text_hash: "hash".to_string(),
            source_ids: Vec::new(),
            source_tier_name: None,
            source_item_key: None,
            created_at: now,
            updated_at: now,
            last_verified_at: None,
            last_injected_at: None,
            injected_count: 3,
            review_run_id: None,
            review_label: None,
            reviewer_confidence: None,
            promotion_reason: None,
            projection_policy_version: MEMORY_HOT_PROJECTION_POLICY_VERSION,
            last_regenerated_at: None,
            regeneration_count: 0,
            active: true,
            deactivated_at: None,
            deactivation_reason: None,
            last_lifecycle_check_at: None,
        }
    }

    /// Projections are keyed by candidate identity, so they must move with the
    /// overlay. A projection left under a legacy key can never match its source
    /// again and the next maintenance pass deactivates it as orphaned — quietly
    /// discarding a compact projection that is still correct.
    #[test]
    fn migration_moves_a_projection_onto_the_current_key_intact() {
        let legacy = "agent:cto::design_decisions.entries:0".to_string();
        let current = "mt1:5:agent:3:cto:0::23:design_decisions.entries:1:0".to_string();
        let mut index = MemoryHotProjectionIndex::default();
        index
            .projections
            .insert(legacy.clone(), test_projection(&legacy));

        let renames = [(legacy.clone(), current.clone())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(migrate_memory_hot_projection_keys(&mut index, &renames), 1);

        assert!(!index.projections.contains_key(&legacy));
        let moved = &index.projections[&current];
        assert_eq!(moved.source_memory_candidate_key, current);
        assert_eq!(moved.injected_count, 3, "usage must survive the rename");
        assert!(moved.active);
    }

    /// A rename must never clobber a projection already sitting under the
    /// destination key — that one is the live record.
    #[test]
    fn migration_never_overwrites_an_existing_current_projection() {
        let legacy = "agent:cto::tier:0".to_string();
        let current = "mt1:5:agent:3:cto:0::4:tier:1:0".to_string();
        let mut index = MemoryHotProjectionIndex::default();
        index
            .projections
            .insert(legacy.clone(), test_projection(&legacy));
        let mut live = test_projection(&current);
        live.injected_count = 99;
        index.projections.insert(current.clone(), live);

        let renames = [(legacy.clone(), current.clone())]
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(migrate_memory_hot_projection_keys(&mut index, &renames), 0);
        assert_eq!(index.projections[&current].injected_count, 99);
        assert!(index.projections.contains_key(&legacy), "left untouched");
    }

    #[test]
    fn migration_with_nothing_to_rename_is_inert() {
        let mut index = MemoryHotProjectionIndex::default();
        let key = "mt1:5:agent:3:cto:0::4:tier:1:0".to_string();
        index.projections.insert(key.clone(), test_projection(&key));
        assert_eq!(
            migrate_memory_hot_projection_keys(&mut index, &BTreeMap::new()),
            0
        );
        assert_eq!(index.projections.len(), 1);
    }

    #[test]
    fn upsert_creates_and_updates_projection() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        let summary = apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "agent:agent-1::episodes.items:ep-1".to_string(),
                semantic_memory_type: SemanticMemoryType::Episode,
                source_text_hash: "hash-1".to_string(),
                compact_text: "Compact successful workflow summary.".to_string(),
                source_ids: vec!["source-b".to_string(), "source-a".to_string()],
                source_tier_name: Some("episodes".to_string()),
                source_item_key: Some("ep-1".to_string()),
                review_run_id: Some("run-1".to_string()),
                review_label: MemoryTemperatureUtilityLabel::LoadBearing,
                reviewer_confidence: Some(0.9),
                promotion_reason: Some("load-bearing".to_string()),
            }],
            now,
        );

        assert_eq!(summary.created, 1);
        assert_eq!(summary.upserted, 1);
        let projection = index
            .projections
            .get("agent:agent-1::episodes.items:ep-1")
            .expect("projection");
        assert_eq!(projection.temperature_tier, MemoryTemperatureTier::T1);
        assert_eq!(
            projection.source_ids,
            vec!["source-a".to_string(), "source-b".to_string()]
        );
        assert!(projection.is_active_for_source_hash("hash-1"));

        let later = now + chrono::Duration::minutes(5);
        let summary = apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "agent:agent-1::episodes.items:ep-1".to_string(),
                semantic_memory_type: SemanticMemoryType::Episode,
                source_text_hash: "hash-2".to_string(),
                compact_text: "Updated projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: Some("episodes".to_string()),
                source_item_key: Some("ep-1".to_string()),
                review_run_id: Some("run-2".to_string()),
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: Some(0.8),
                promotion_reason: Some("useful".to_string()),
            }],
            later,
        );

        assert_eq!(summary.updated, 1);
        let projection = index
            .projections
            .get("agent:agent-1::episodes.items:ep-1")
            .expect("projection");
        assert_eq!(projection.temperature_tier, MemoryTemperatureTier::T1);
        assert_eq!(projection.compact_text, "Updated projection.");
        assert!(!projection.is_active_for_source_hash("hash-1"));
        assert!(projection.is_active_for_source_hash("hash-2"));
    }

    #[test]
    fn hot_projection_tier_policy_reserves_t0_for_stable_lanes() {
        assert_eq!(
            memory_hot_projection_temperature_tier(
                SemanticMemoryType::ProjectContext,
                MemoryTemperatureUtilityLabel::LoadBearing
            ),
            Some(MemoryTemperatureTier::T0)
        );
        assert_eq!(
            memory_hot_projection_temperature_tier(
                SemanticMemoryType::Episode,
                MemoryTemperatureUtilityLabel::LoadBearing
            ),
            Some(MemoryTemperatureTier::T1)
        );
        assert_eq!(
            memory_hot_projection_temperature_tier(
                SemanticMemoryType::SourceEvidence,
                MemoryTemperatureUtilityLabel::Useful
            ),
            None
        );
    }

    #[test]
    fn usage_updates_projection_injection_counts() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::ProjectContext,
                source_text_hash: "hash".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: None,
                source_item_key: None,
                review_run_id: None,
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: None,
                promotion_reason: None,
            }],
            now,
        );

        let later = now + chrono::Duration::minutes(1);
        let summary =
            apply_memory_hot_projection_usage(&mut index, &["source-key".to_string()], later);
        let projection = index.projections.get("source-key").expect("projection");

        assert_eq!(summary.touched, 1);
        assert_eq!(projection.injected_count, 1);
        assert_eq!(projection.last_injected_at, Some(later));
    }

    #[test]
    fn maintenance_deactivates_projection_when_source_hash_changes() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::Entity,
                source_text_hash: "hash-old".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: None,
                source_item_key: None,
                review_run_id: None,
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: None,
                promotion_reason: None,
            }],
            now,
        );

        let mut source_hashes = BTreeMap::new();
        source_hashes.insert("source-key".to_string(), "hash-new".to_string());
        let later = now + chrono::Duration::minutes(1);
        let summary = apply_memory_hot_projection_maintenance(
            &mut index,
            &source_hashes,
            MemoryHotProjectionMaintenancePolicy::default(),
            later,
        );
        let projection = index.projections.get("source-key").expect("projection");

        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.deactivated, 1);
        assert_eq!(summary.source_hash_mismatch, 1);
        assert!(!projection.active);
        assert_eq!(
            projection.deactivation_reason.as_deref(),
            Some("source_hash_mismatch")
        );
        assert_eq!(projection.deactivated_at, Some(later));
    }

    #[test]
    fn maintenance_deactivates_projection_when_never_injected_and_old() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::ProjectContext,
                source_text_hash: "hash".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: None,
                source_item_key: None,
                review_run_id: None,
                review_label: MemoryTemperatureUtilityLabel::LoadBearing,
                reviewer_confidence: None,
                promotion_reason: None,
            }],
            now,
        );

        let later = now + chrono::Duration::days(31);
        let summary = apply_memory_hot_projection_maintenance(
            &mut index,
            &BTreeMap::new(),
            MemoryHotProjectionMaintenancePolicy::default(),
            later,
        );
        let projection = index.projections.get("source-key").expect("projection");

        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.deactivated, 1);
        assert_eq!(summary.unused_expired, 1);
        assert!(!projection.active);
        assert_eq!(
            projection.deactivation_reason.as_deref(),
            Some("unused_projection_expired")
        );
    }

    #[test]
    fn maintenance_deactivates_projection_from_old_policy_version() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::Procedure,
                source_text_hash: "hash".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: vec!["source-ref".to_string()],
                source_tier_name: Some("procedures".to_string()),
                source_item_key: Some("procedure-1".to_string()),
                review_run_id: None,
                review_label: MemoryTemperatureUtilityLabel::LoadBearing,
                reviewer_confidence: None,
                promotion_reason: None,
            }],
            now,
        );
        index
            .projections
            .get_mut("source-key")
            .expect("projection")
            .projection_policy_version = MEMORY_HOT_PROJECTION_POLICY_VERSION + 1;

        let later = now + chrono::Duration::minutes(1);
        let summary = apply_memory_hot_projection_maintenance(
            &mut index,
            &BTreeMap::new(),
            MemoryHotProjectionMaintenancePolicy::default(),
            later,
        );
        let projection = index.projections.get("source-key").expect("projection");

        assert_eq!(summary.deactivated, 1);
        assert_eq!(summary.policy_version_changed, 1);
        assert!(!projection.active);
        assert_eq!(
            projection.deactivation_reason.as_deref(),
            Some("projection_policy_version_changed")
        );
    }

    #[test]
    fn prompt_eligibility_matches_mutating_maintenance_for_current_candidate() {
        let created_at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let now = created_at + chrono::Duration::days(31);
        let policy = MemoryHotProjectionMaintenancePolicy::default();
        let mut seed = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut seed,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::ProjectContext,
                source_text_hash: "hash".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: None,
                source_item_key: None,
                review_run_id: None,
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: None,
                promotion_reason: None,
            }],
            created_at,
        );
        let base = seed
            .projections
            .get("source-key")
            .expect("projection")
            .clone();

        let mut injected_old = base.clone();
        injected_old.injected_count = 1;
        let mut wrong_policy = base.clone();
        wrong_policy.projection_policy_version += 1;
        let mut inactive = base.clone();
        inactive.active = false;
        let mut empty = base.clone();
        empty.compact_text.clear();
        let mut recent_unused = base.clone();
        recent_unused.updated_at = now - chrono::Duration::days(1);

        for projection in [
            base,
            injected_old,
            wrong_policy,
            inactive,
            empty,
            recent_unused,
        ] {
            let prompt_eligible =
                projection.is_prompt_eligible_for_source_hash("hash", policy, now);
            let mut maintained = MemoryHotProjectionIndex::default();
            maintained
                .projections
                .insert("source-key".to_string(), projection);
            apply_memory_hot_projection_maintenance(
                &mut maintained,
                &BTreeMap::from([("source-key".to_string(), "hash".to_string())]),
                policy,
                now,
            );
            let maintained_eligible = maintained
                .projections
                .get("source-key")
                .expect("maintained projection")
                .is_active_for_source_hash("hash");

            assert_eq!(prompt_eligible, maintained_eligible);
        }
    }

    #[test]
    fn upsert_records_regeneration_when_projection_payload_changes() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::ProjectContext,
                source_text_hash: "hash".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: vec!["source-a".to_string()],
                source_tier_name: Some("project_context".to_string()),
                source_item_key: Some("ctx-1".to_string()),
                review_run_id: Some("run-1".to_string()),
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: Some(0.8),
                promotion_reason: Some("useful".to_string()),
            }],
            now,
        );

        let later = now + chrono::Duration::minutes(2);
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::ProjectContext,
                source_text_hash: "hash".to_string(),
                compact_text: "Updated projection.".to_string(),
                source_ids: vec!["source-a".to_string(), "source-b".to_string()],
                source_tier_name: Some("project_context".to_string()),
                source_item_key: Some("ctx-1".to_string()),
                review_run_id: Some("run-2".to_string()),
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: Some(0.9),
                promotion_reason: Some("still useful".to_string()),
            }],
            later,
        );
        let projection = index.projections.get("source-key").expect("projection");

        assert_eq!(projection.regeneration_count, 1);
        assert_eq!(projection.last_regenerated_at, Some(later));
        assert_eq!(
            projection.source_ids,
            vec!["source-a".to_string(), "source-b".to_string()]
        );
        assert_eq!(
            projection.source_tier_name.as_deref(),
            Some("project_context")
        );
        assert_eq!(projection.source_item_key.as_deref(), Some("ctx-1"));
    }

    #[test]
    fn upsert_reactivates_previously_deactivated_projection() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut index = MemoryHotProjectionIndex::default();
        apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::Episode,
                source_text_hash: "hash-old".to_string(),
                compact_text: "Projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: None,
                source_item_key: None,
                review_run_id: None,
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: None,
                promotion_reason: None,
            }],
            now,
        );
        let mut source_hashes = BTreeMap::new();
        source_hashes.insert("source-key".to_string(), "hash-new".to_string());
        apply_memory_hot_projection_maintenance(
            &mut index,
            &source_hashes,
            MemoryHotProjectionMaintenancePolicy::default(),
            now + chrono::Duration::minutes(1),
        );

        let later = now + chrono::Duration::minutes(2);
        let summary = apply_memory_hot_projection_upserts(
            &mut index,
            &[MemoryHotProjectionUpsert {
                source_memory_candidate_key: "source-key".to_string(),
                semantic_memory_type: SemanticMemoryType::Episode,
                source_text_hash: "hash-new".to_string(),
                compact_text: "Fresh projection.".to_string(),
                source_ids: Vec::new(),
                source_tier_name: None,
                source_item_key: None,
                review_run_id: Some("run-2".to_string()),
                review_label: MemoryTemperatureUtilityLabel::Useful,
                reviewer_confidence: Some(0.7),
                promotion_reason: Some("fresh review".to_string()),
            }],
            later,
        );
        let projection = index.projections.get("source-key").expect("projection");

        assert_eq!(summary.updated, 1);
        assert!(projection.active);
        assert_eq!(projection.deactivated_at, None);
        assert_eq!(projection.deactivation_reason, None);
        assert!(projection.is_active_for_source_hash("hash-new"));
    }
}
