//! Global capability registry with cross-origin index
//!
//! Maintains a fast-lookup index at
//! `magician_data_v3/scopes/{principal}/{workspace}/api_mining/registry_index.json`
//! that summarizes all capabilities across all origins. The index can be fully
//! rebuilt from capability files on disk via `rebuild()`.

use super::action_binding::{
    merge_action_bindings, params_from_action_binding, ActionBinding, ActionContext,
};
use super::capability::{ApiCapability, CapabilitySummary, ConfidenceLevel, GraphqlOperationKind};
use super::capability_store::CapabilityStore;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::process_storage;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::warn;

/// Schema version for the registry index
// 1.1 persists `parent_origin` in capability summaries so site-grouped
// dashboard reads can stay index-only after integrity repair.
const INDEX_VERSION: &str = "1.1.0";

fn default_api_mining_base() -> PathBuf {
    process_storage::workspace().api_mining_root("__unbound__", "__unbound__")
}

/// The global registry index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    /// Schema version
    pub version: String,

    /// Per-origin entries, keyed by origin_key
    pub origins: HashMap<String, OriginEntry>,

    /// Epoch seconds of last full index rebuild
    pub last_rebuilt: i64,
}

/// Summary of an origin's capabilities and traces
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginEntry {
    /// Filesystem-safe origin key
    pub origin_key: String,

    /// Human-readable origin URL
    pub origin_url: String,

    /// Number of capabilities for this origin
    pub capability_count: usize,

    /// Number of traces stored for this origin
    pub trace_count: usize,

    /// Lightweight capability summaries
    pub capabilities: Vec<CapabilitySummary>,

    /// Last update timestamp
    pub updated_at: i64,
}

/// The capability registry — manages the global index and provides lookup
pub struct CapabilityRegistry {
    /// Path to the registry index file
    index_path: PathBuf,
    workspace_layout: ArtifactV2Workspace,

    /// In-memory index (loaded on init, updated on mutations)
    index: RegistryIndex,

    /// Capability store for full capability read/write
    store: CapabilityStore,
}

#[derive(Debug, Clone)]
pub struct ActionBindingMatch {
    pub origin: String,
    pub summary: CapabilitySummary,
    pub binding: ActionBinding,
}

/// Point-in-time integrity and takeover-readiness snapshot for the registry
/// index. Counts are derived from actual capability files on disk, then compared
/// with `registry_index.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryHealthSnapshot {
    pub index_version: String,
    pub expected_index_version: String,
    pub version_mismatch: bool,
    pub last_rebuilt: i64,
    pub checked_at: i64,
    pub origin_count: usize,
    pub indexed_capability_count: usize,
    pub loadable_capability_count: usize,
    pub stale_index_count: usize,
    pub unindexed_capability_count: usize,
    pub candidates_by_confidence: HashMap<String, usize>,
    pub action_bindings_count: usize,
    pub takeover_ready_bindings_count: usize,
    pub replayable_capability_count: usize,
    pub stale_origins: Vec<String>,
    pub unindexed_origins: Vec<String>,
    pub origin_readiness: Vec<OriginTakeoverReadiness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<RegistryHealthWarning>,
}

impl RegistryHealthSnapshot {
    pub fn has_index_drift(&self) -> bool {
        self.version_mismatch || self.stale_index_count > 0 || self.unindexed_capability_count > 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OriginTakeoverReadiness {
    pub origin_key: String,
    pub origin_url: String,
    pub indexed_capability_count: usize,
    pub loadable_capability_count: usize,
    pub replayable_capability_count: usize,
    pub action_bindings_count: usize,
    pub takeover_ready_bindings_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_policy_decision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_replay_allowed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_mode: Option<String>,
    pub inactive_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryHealthWarning {
    pub code: String,
    pub message: String,
}

/// Result of checking and optionally repairing the registry index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryRepairReport {
    pub repaired: bool,
    pub before: RegistryHealthSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<RegistryHealthSnapshot>,
}

impl CapabilityRegistry {
    /// Create a new registry with the default base path
    pub fn new() -> Result<Self, String> {
        Self::with_base_path(default_api_mining_base())
    }

    /// Create a registry with a custom base path
    pub fn with_base_path<P: AsRef<Path>>(base: P) -> Result<Self, String> {
        let mut registry = Self::load_from_base_path(base.as_ref())?;
        if let Err(e) = registry.repair_index_if_needed() {
            warn!("Failed to audit/repair registry index from disk: {}", e);
        }
        Ok(registry)
    }

    /// Open and integrity-repair a registry, surfacing repair failures to the
    /// caller. Operator API reads use this stricter boundary so they never
    /// render an index known to be stale after an I/O failure.
    pub fn with_repaired_base_path<P: AsRef<Path>>(base: P) -> Result<Self, String> {
        let mut registry = Self::load_from_base_path(base.as_ref())?;
        registry.repair_index_if_needed()?;
        Ok(registry)
    }

    fn load_from_base_path(base: &Path) -> Result<Self, String> {
        let base = base.to_path_buf();
        let index_path = base.join("registry_index.json");
        let workspace_layout = ArtifactV2Workspace::with_local_file_provider(&base);
        let store = CapabilityStore::with_base_path(&base);

        let index = if workspace_layout
            .metadata_path_sync(&index_path)
            .map_err(|e| format!("Failed to inspect registry index: {}", e))?
            .is_some()
        {
            let content = workspace_layout
                .read_to_string_path_sync(&index_path)
                .map_err(|e| format!("Failed to read registry index: {}", e))?;
            serde_json::from_str(&content).unwrap_or_else(|e| {
                warn!("Corrupt registry index, rebuilding: {}", e);
                Self::empty_index()
            })
        } else {
            Self::empty_index()
        };

        Ok(Self {
            index_path,
            workspace_layout,
            index,
            store,
        })
    }

    /// Set the maximum capabilities per origin (from config).
    /// Delegates to the underlying CapabilityStore.
    pub fn with_max_capabilities(mut self, max: usize) -> Self {
        self.store = self.store.with_max_capabilities(max);
        self
    }

    /// Create an empty index
    fn empty_index() -> RegistryIndex {
        RegistryIndex {
            version: INDEX_VERSION.to_string(),
            origins: HashMap::new(),
            last_rebuilt: chrono::Utc::now().timestamp(),
        }
    }

    /// Persist the current index to disk
    fn save_index(&self) -> Result<(), String> {
        let content = serde_json::to_string_pretty(&self.index)
            .map_err(|e| format!("Failed to serialize index: {}", e))?;

        self.workspace_layout
            .write_atomic_path_sync(&self.index_path, content.as_bytes())
            .map_err(|e| format!("Failed to write index: {}", e))?;

        Ok(())
    }

    /// Rebuild the entire index from capability files on disk
    pub fn rebuild(&mut self) -> Result<(), String> {
        let all_origins = self.store.load_all_origins()?;
        let mut new_origins = HashMap::new();

        for (origin_key, capabilities) in all_origins {
            let origin_url = capabilities
                .first()
                .map(|c| c.origin.clone())
                .unwrap_or_else(|| origin_key.clone());

            let summaries: Vec<CapabilitySummary> =
                capabilities.iter().map(|c| c.to_summary()).collect();

            let updated_at = capabilities.iter().map(|c| c.updated_at).max().unwrap_or(0);

            new_origins.insert(
                origin_key.clone(),
                OriginEntry {
                    origin_key,
                    origin_url,
                    capability_count: capabilities.len(),
                    trace_count: capabilities.iter().map(|c| c.trace_ids.len()).sum(),
                    capabilities: summaries,
                    updated_at,
                },
            );
        }

        self.index.version = INDEX_VERSION.to_string();
        self.index.origins = new_origins;
        self.index.last_rebuilt = chrono::Utc::now().timestamp();
        self.save_index()
    }

    /// Compare the in-memory registry index against capability files on disk.
    pub fn health_snapshot(&self) -> Result<RegistryHealthSnapshot, String> {
        let all_origins = self.store.load_all_origins()?;
        let mut disk_ids_by_origin: HashMap<String, HashSet<String>> = HashMap::new();
        let mut disk_caps_by_origin: HashMap<String, Vec<&ApiCapability>> = HashMap::new();
        let mut origin_keys: HashSet<String> = HashSet::new();

        for (origin_key, capabilities) in &all_origins {
            origin_keys.insert(origin_key.clone());
            disk_ids_by_origin.insert(
                origin_key.clone(),
                capabilities.iter().map(|cap| cap.id.clone()).collect(),
            );
            disk_caps_by_origin.insert(origin_key.clone(), capabilities.iter().collect());
        }

        for origin_key in self.index.origins.keys() {
            origin_keys.insert(origin_key.clone());
        }

        let mut indexed_capability_count = 0usize;
        let mut loadable_capability_count = 0usize;
        let mut stale_index_count = 0usize;
        let mut unindexed_capability_count = 0usize;
        let mut candidates_by_confidence: HashMap<String, usize> = HashMap::new();
        let mut action_bindings_count = 0usize;
        let mut takeover_ready_bindings_count = 0usize;
        let mut replayable_capability_count = 0usize;
        let mut stale_origins: Vec<String> = Vec::new();
        let mut unindexed_origins: Vec<String> = Vec::new();
        let mut origin_readiness: Vec<OriginTakeoverReadiness> = Vec::new();

        let mut sorted_origin_keys: Vec<String> = origin_keys.into_iter().collect();
        sorted_origin_keys.sort();

        for origin_key in sorted_origin_keys {
            let indexed_entry = self.index.origins.get(&origin_key);
            let indexed_summaries = indexed_entry
                .map(|entry| entry.capabilities.as_slice())
                .unwrap_or(&[]);
            let disk_capabilities = disk_caps_by_origin
                .get(&origin_key)
                .map(|caps| caps.as_slice())
                .unwrap_or(&[]);

            indexed_capability_count += indexed_summaries.len();
            loadable_capability_count += disk_capabilities.len();

            let disk_ids = disk_ids_by_origin.get(&origin_key);
            let indexed_ids: HashSet<&str> = indexed_summaries
                .iter()
                .map(|summary| summary.id.as_str())
                .collect();

            let origin_stale_count = indexed_summaries
                .iter()
                .filter(|summary| {
                    disk_ids
                        .map(|ids| !ids.contains(&summary.id))
                        .unwrap_or(true)
                })
                .count();
            if origin_stale_count > 0 {
                stale_origins.push(origin_key.clone());
                stale_index_count += origin_stale_count;
            }

            let origin_unindexed_count = disk_capabilities
                .iter()
                .filter(|capability| !indexed_ids.contains(capability.id.as_str()))
                .count();
            if origin_unindexed_count > 0 {
                unindexed_origins.push(origin_key.clone());
                unindexed_capability_count += origin_unindexed_count;
            }

            let mut origin_replayable_count = 0usize;
            let mut origin_action_bindings_count = 0usize;
            let mut origin_takeover_ready_bindings_count = 0usize;

            for capability in disk_capabilities {
                *candidates_by_confidence
                    .entry(confidence_key(&capability.confidence).to_string())
                    .or_insert(0) += 1;

                if capability.is_replayable() {
                    replayable_capability_count += 1;
                    origin_replayable_count += 1;
                }

                let binding_count = capability.action_bindings.len();
                action_bindings_count += binding_count;
                origin_action_bindings_count += binding_count;

                let ready_count = capability
                    .action_bindings
                    .iter()
                    .filter(|binding| binding.is_takeover_ready_for(capability))
                    .count();
                takeover_ready_bindings_count += ready_count;
                origin_takeover_ready_bindings_count += ready_count;
            }

            let origin_url = disk_capabilities
                .first()
                .map(|capability| capability.origin.clone())
                .or_else(|| indexed_entry.map(|entry| entry.origin_url.clone()))
                .unwrap_or_else(|| origin_key.clone());
            let mut inactive_reasons = Vec::new();
            if disk_capabilities.is_empty() {
                inactive_reasons.push("no_loadable_capabilities".to_string());
            } else {
                if origin_replayable_count == 0 {
                    inactive_reasons.push("no_replayable_capabilities".to_string());
                }
                if origin_action_bindings_count == 0 {
                    inactive_reasons.push("no_action_bindings".to_string());
                } else if origin_takeover_ready_bindings_count == 0 {
                    inactive_reasons.push("no_takeover_ready_bindings".to_string());
                }
            }
            if origin_stale_count > 0 {
                inactive_reasons.push("stale_registry_entries".to_string());
            }
            if origin_unindexed_count > 0 {
                inactive_reasons.push("unindexed_capabilities".to_string());
            }

            origin_readiness.push(OriginTakeoverReadiness {
                origin_key,
                origin_url,
                indexed_capability_count: indexed_summaries.len(),
                loadable_capability_count: disk_capabilities.len(),
                replayable_capability_count: origin_replayable_count,
                action_bindings_count: origin_action_bindings_count,
                takeover_ready_bindings_count: origin_takeover_ready_bindings_count,
                origin_policy_decision: None,
                auto_replay_allowed: None,
                replay_mode: None,
                inactive_reasons,
            });
        }

        Ok(RegistryHealthSnapshot {
            index_version: self.index.version.clone(),
            expected_index_version: INDEX_VERSION.to_string(),
            version_mismatch: self.index.version != INDEX_VERSION,
            last_rebuilt: self.index.last_rebuilt,
            checked_at: chrono::Utc::now().timestamp(),
            origin_count: self.index.origins.len(),
            indexed_capability_count,
            loadable_capability_count,
            stale_index_count,
            unindexed_capability_count,
            candidates_by_confidence,
            action_bindings_count,
            takeover_ready_bindings_count,
            replayable_capability_count,
            stale_origins,
            unindexed_origins,
            origin_readiness,
            warnings: Vec::new(),
        })
    }

    /// Rebuild the index only when audit detects drift. This runs during
    /// registry construction so routers do not see stale summaries that later
    /// fail when loading the full capability from disk.
    pub fn repair_index_if_needed(&mut self) -> Result<RegistryRepairReport, String> {
        let before = self.health_snapshot()?;
        if !before.has_index_drift() {
            return Ok(RegistryRepairReport {
                repaired: false,
                before,
                after: None,
            });
        }

        warn!(
            "[API_MINING] Registry index drift detected; rebuilding index \
             (stale_entries={}, unindexed_capabilities={}, version_mismatch={})",
            before.stale_index_count, before.unindexed_capability_count, before.version_mismatch
        );
        self.rebuild()?;
        let after = self.health_snapshot()?;
        Ok(RegistryRepairReport {
            repaired: true,
            before,
            after: Some(after),
        })
    }

    /// Register a new or updated capability
    pub fn register(&mut self, capability: &ApiCapability) -> Result<(), String> {
        // Update index
        let origin_key = CapabilityStore::origin_to_key(&capability.origin);
        let summary = capability.to_summary();

        let entry = self
            .index
            .origins
            .entry(origin_key.clone())
            .or_insert_with(|| OriginEntry {
                origin_key,
                origin_url: capability.origin.clone(),
                capability_count: 0,
                trace_count: 0,
                capabilities: Vec::new(),
                updated_at: 0,
            });

        // First: check for same ID (update-in-place, e.g. after replay outcome recording)
        if let Some(existing_by_id) = entry
            .capabilities
            .iter_mut()
            .find(|c| c.id == capability.id)
        {
            *existing_by_id = summary;
            self.store.save(capability)?;
        }
        // Dedup: check for existing capability with same
        // (method, url_template, graphql_operation, graphql_operation_kind, body_fingerprint)
        // but DIFFERENT ID. This catches duplicate capabilities created by separate mining runs
        // with fresh UUIDs. Merge by keeping the existing entry and updating stats from the new one.
        // graphql_operation/graphql_operation_kind and body_fingerprint are included so different
        // POST variants on the same endpoint stay separate (GraphQL operations, body-aware splits, etc.).
        else if let Some(existing) = entry.capabilities.iter_mut().find(|c| {
            c.method == capability.method
                && c.url_template == capability.url_template
                && c.graphql_operation == summary.graphql_operation
                && graphql_operation_kind_compatible(
                    c.graphql_operation_kind,
                    summary.graphql_operation_kind,
                )
                && c.body_fingerprint == summary.body_fingerprint
        }) {
            let keep_id = existing.id.clone();
            existing.sample_count = existing.sample_count.max(capability.sample_count);
            if capability.confidence > existing.confidence {
                existing.confidence = capability.confidence.clone();
            }
            existing.updated_at = existing.updated_at.max(capability.updated_at);
            existing.side_effects = summary.side_effects.clone();
            existing.graphql_operation = existing
                .graphql_operation
                .clone()
                .or(summary.graphql_operation.clone());
            existing.graphql_operation_kind = merge_graphql_operation_kind(
                existing.graphql_operation_kind,
                summary.graphql_operation_kind,
            );
            existing.graphql_persisted_query_sha256 = existing
                .graphql_persisted_query_sha256
                .clone()
                .or(summary.graphql_persisted_query_sha256.clone());
            existing.action_bindings =
                merge_action_bindings(&existing.action_bindings, &summary.action_bindings);
            // Save the merged capability to disk under the existing ID
            let existing_full = self.store.load(&capability.origin, &keep_id).ok();
            let mut merged = capability.clone();
            if let Some(existing_capability) = existing_full {
                merged.replay_success_count = existing_capability
                    .replay_success_count
                    .max(merged.replay_success_count);
                merged.replay_failure_count = existing_capability
                    .replay_failure_count
                    .max(merged.replay_failure_count);
                merged.consecutive_failures = existing_capability
                    .consecutive_failures
                    .max(merged.consecutive_failures);
                merged.last_validated =
                    merged.last_validated.or(existing_capability.last_validated);
                merged.created_at = merged.created_at.min(existing_capability.created_at);
                merged.trace_ids =
                    merge_trace_ids(&existing_capability.trace_ids, &merged.trace_ids);
                merged.graphql_operation = merged
                    .graphql_operation
                    .or(existing_capability.graphql_operation.clone());
                merged.graphql_operation_kind = merge_graphql_operation_kind(
                    merged.graphql_operation_kind,
                    existing_capability.graphql_operation_kind,
                );
                merged.graphql_persisted_query_sha256 = merged
                    .graphql_persisted_query_sha256
                    .or(existing_capability.graphql_persisted_query_sha256.clone());
                merged.body_template = merged
                    .body_template
                    .or(existing_capability.body_template.clone());
                merged.action_bindings = merge_action_bindings(
                    &existing_capability.action_bindings,
                    &merged.action_bindings,
                );
                // Merge auth metadata — union fields from both sides so
                // partial auth (one has headers, other has cookies) is preserved.
                {
                    let existing_auth = &existing_capability.auth_requirements;
                    for cookie in &existing_auth.cookies {
                        if !merged.auth_requirements.cookies.contains(cookie) {
                            merged.auth_requirements.cookies.push(cookie.clone());
                        }
                    }
                    for header in &existing_auth.headers {
                        if !merged.auth_requirements.headers.contains(header) {
                            merged.auth_requirements.headers.push(header.clone());
                        }
                    }
                    for key in &existing_auth.local_storage_keys {
                        if !merged.auth_requirements.local_storage_keys.contains(key) {
                            merged
                                .auth_requirements
                                .local_storage_keys
                                .push(key.clone());
                        }
                    }
                    for key in &existing_auth.session_storage_keys {
                        if !merged.auth_requirements.session_storage_keys.contains(key) {
                            merged
                                .auth_requirements
                                .session_storage_keys
                                .push(key.clone());
                        }
                    }
                }
                merged.parent_origin = merged
                    .parent_origin
                    .or(existing_capability.parent_origin.clone());
                merged.auth_failure_count = merged
                    .auth_failure_count
                    .max(existing_capability.auth_failure_count);
            }
            merged.id = keep_id;
            merged.sample_count = existing.sample_count;
            merged.confidence = existing.confidence.clone();
            merged.updated_at = existing.updated_at;
            merged.refresh_side_effects();
            self.store.save(&merged)?;
        } else {
            // Brand new capability
            entry.capabilities.push(summary);
            self.store.save(capability)?;
        }

        // Always sync count — defensive against any path that modifies the vec
        entry.capability_count = entry.capabilities.len();
        entry.updated_at = capability.updated_at;

        self.save_index()
    }

    /// Find a capability by URL matching (delegates to fingerprint-aware version with None).
    ///
    /// Searches across all origins for the *best* capability whose url_template
    /// matches the given URL. When multiple capabilities match, selection prefers:
    ///   1. Specificity — fewer `{param}` wildcards wins (literal match is ideal)
    ///   2. Confidence — Trusted > Validated > Candidate > Observed
    ///   3. Deterministic tiebreak — lexicographic by (name, url_template, id)
    pub fn find_by_url(&self, method: &str, url: &str) -> Option<CapabilitySummary> {
        self.find_by_url_with_request_context(method, url, None, None, None)
    }

    /// Find a capability by URL matching with optional body fingerprint disambiguation.
    ///
    /// When `body_fingerprint` is `Some`, only capabilities with a matching fingerprint
    /// are considered. This disambiguates multiple POST capabilities on the same URL
    /// (e.g., Gmail sync mark-as-read vs archive).
    pub fn find_by_url_with_fingerprint(
        &self,
        method: &str,
        url: &str,
        body_fingerprint: Option<&str>,
    ) -> Option<CapabilitySummary> {
        self.find_by_url_with_request_context(method, url, body_fingerprint, None, None)
    }

    /// Find a capability by URL matching with optional request-body disambiguation.
    ///
    /// `graphql_operation` and `graphql_operation_kind` are used when multiple GraphQL POST
    /// capabilities share the same method and `/graphql` URL.
    pub fn find_by_url_with_request_context(
        &self,
        method: &str,
        url: &str,
        body_fingerprint: Option<&str>,
        graphql_operation: Option<&str>,
        graphql_operation_kind: Option<GraphqlOperationKind>,
    ) -> Option<CapabilitySummary> {
        let method_upper = method.to_uppercase();
        let mut best: Option<(CapabilitySummary, usize, usize)> = None; // (candidate, context_wildcards, url_wildcards)

        for entry in self.index.origins.values() {
            for cap in &entry.capabilities {
                if cap.method != method_upper || !Self::url_matches(&cap.url_template, url) {
                    continue;
                }

                // Fingerprint filter: when caller specifies a fingerprint, only match
                // capabilities with the same fingerprint.
                if let Some(fp) = body_fingerprint {
                    match &cap.body_fingerprint {
                        Some(cap_fp) if cap_fp == fp => {}, // match
                        Some(_) => continue,                // different fingerprint, skip
                        None => continue,                   // no fingerprint on cap, skip
                    }
                }

                let context_wildcards =
                    match graphql_context_wildcards(cap, graphql_operation, graphql_operation_kind)
                    {
                        Some(value) => value,
                        None => continue,
                    };
                let url_wildcards = Self::count_template_wildcards(&cap.url_template);

                let is_better = match &best {
                    None => true,
                    Some((current_best, current_context_wildcards, current_url_wildcards)) => {
                        if context_wildcards != *current_context_wildcards {
                            context_wildcards < *current_context_wildcards
                        } else if url_wildcards != *current_url_wildcards {
                            // Fewer wildcards = more specific = preferred
                            url_wildcards < *current_url_wildcards
                        } else if cap.confidence != current_best.confidence {
                            // Higher confidence wins (Trusted > Validated > Candidate > Observed)
                            cap.confidence > current_best.confidence
                        } else {
                            // Total deterministic tiebreak: name → url_template → id.
                            // Names can collide (derived from coarse path hints in miner),
                            // so we fall through to url_template and finally id.
                            (&cap.name, &cap.url_template, &cap.id)
                                < (
                                    &current_best.name,
                                    &current_best.url_template,
                                    &current_best.id,
                                )
                        }
                    },
                };

                if is_better {
                    best = Some((cap.clone(), context_wildcards, url_wildcards));
                }
            }
        }

        best.map(|(cap, _, _)| cap)
    }

    /// Count how many `{param}` wildcard segments a template URL has.
    /// Lower count = more specific (literal match = 0).
    fn count_template_wildcards(template: &str) -> usize {
        let path = template.split('?').next().unwrap_or(template);
        let path_wildcards = path
            .split('/')
            .filter(|seg| seg.starts_with('{') && seg.ends_with('}'))
            .count();

        let query_wildcards = template
            .split('?')
            .nth(1)
            .map(|q| {
                q.split('&')
                    .filter_map(|pair| pair.split_once('=').map(|x| x.1))
                    .filter(|v| v.starts_with('{') && v.ends_with('}'))
                    .count()
            })
            .unwrap_or(0);

        path_wildcards + query_wildcards
    }

    /// Find all replayable capabilities for a given origin
    pub fn find_replayable_for_origin(&self, origin: &str) -> Vec<CapabilitySummary> {
        let origin_key = CapabilityStore::origin_to_key(origin);

        self.index
            .origins
            .get(&origin_key)
            .map(|entry| {
                entry
                    .capabilities
                    .iter()
                    .filter(|c| {
                        c.min_replay_confidence()
                            .map(|min| c.confidence >= min)
                            .unwrap_or(false)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get the full capability by ID (loads from disk)
    pub fn get_capability(
        &self,
        origin: &str,
        capability_id: &str,
    ) -> Result<ApiCapability, String> {
        self.store.load(origin, capability_id)
    }

    /// Get all capabilities for an origin (loads from disk)
    pub fn get_all_for_origin(&self, origin: &str) -> Result<Vec<ApiCapability>, String> {
        self.store.load_all(origin)
    }

    /// Find learned action bindings that can satisfy the current browser action.
    ///
    /// Results are returned best-first so callers can try multiple candidates when
    /// the top binding cannot fully resolve the capability request.
    pub fn find_action_context_candidates(
        &self,
        context: &ActionContext,
    ) -> Vec<ActionBindingMatch> {
        // Phase 0 Gap 2 (v0.6.515): rank by match quality first.
        // Exact CSS-selector matches always beat semantic-only
        // fallback matches; within each tier the existing specificity
        // / sample-count / confidence tiebreakers apply.
        let mut matches: Vec<(
            ActionBindingMatch,
            super::action_binding::ActionMatchQuality,
            usize,
        )> = Vec::new();

        for entry in self.index.origins.values() {
            for cap in &entry.capabilities {
                for binding in &cap.action_bindings {
                    let Some(quality) = binding.match_quality(context) else {
                        continue;
                    };
                    if !binding.is_takeover_ready() {
                        continue;
                    }
                    if !binding.can_resolve_url_template(&cap.url_template) {
                        continue;
                    }
                    if params_from_action_binding(binding, context).is_err() {
                        continue;
                    }

                    let specificity = usize::from(binding.page_origin.is_some())
                        + usize::from(binding.page_path_template.is_some());
                    let candidate = ActionBindingMatch {
                        origin: entry.origin_url.clone(),
                        summary: cap.clone(),
                        binding: binding.clone(),
                    };
                    matches.push((candidate, quality, specificity));
                }
            }
        }

        matches.sort_by(
            |(left, left_quality, left_specificity), (right, right_quality, right_specificity)| {
                right_quality
                    .cmp(left_quality)
                    .then_with(|| right_specificity.cmp(left_specificity))
                    .then_with(|| right.binding.sample_count.cmp(&left.binding.sample_count))
                    .then_with(|| right.summary.confidence.cmp(&left.summary.confidence))
                    .then_with(|| {
                        (
                            &left.summary.name,
                            &left.summary.url_template,
                            &left.summary.id,
                        )
                            .cmp(&(
                                &right.summary.name,
                                &right.summary.url_template,
                                &right.summary.id,
                            ))
                    })
            },
        );

        matches
            .into_iter()
            .map(|(candidate, _, _)| candidate)
            .collect()
    }

    /// Find the single best learned action binding that can satisfy the current
    /// browser action.
    pub fn find_by_action_context(&self, context: &ActionContext) -> Option<ActionBindingMatch> {
        self.find_action_context_candidates(context)
            .into_iter()
            .next()
    }

    /// Remove a capability
    pub fn remove(&mut self, origin: &str, capability_id: &str) -> Result<(), String> {
        // Remove from disk
        self.store.delete(origin, capability_id)?;

        // Remove from index
        let origin_key = CapabilityStore::origin_to_key(origin);
        if let Some(entry) = self.index.origins.get_mut(&origin_key) {
            entry.capabilities.retain(|c| c.id != capability_id);
            entry.capability_count = entry.capabilities.len();

            if entry.capabilities.is_empty() {
                self.index.origins.remove(&origin_key);
            }
        }

        self.save_index()
    }

    /// Remove all capabilities for an origin.
    pub fn remove_origin(&mut self, origin: &str) -> Result<usize, String> {
        let deleted = self.store.delete_origin(origin)?;
        let origin_key = CapabilityStore::origin_to_key(origin);
        self.index.origins.remove(&origin_key);
        self.save_index()?;
        Ok(deleted)
    }

    /// Get summary statistics for the registry
    pub fn stats(&self) -> RegistryStats {
        let mut total_capabilities = 0;
        let mut by_confidence: HashMap<String, usize> = HashMap::new();
        let mut origins = Vec::new();

        for entry in self.index.origins.values() {
            total_capabilities += entry.capability_count;
            origins.push(entry.origin_url.clone());

            for cap in &entry.capabilities {
                let key = format!("{:?}", cap.confidence);
                *by_confidence.entry(key).or_insert(0) += 1;
            }
        }

        RegistryStats {
            total_origins: self.index.origins.len(),
            total_capabilities,
            by_confidence,
            origins,
        }
    }

    /// Get the in-memory index (for inspection)
    pub fn index(&self) -> &RegistryIndex {
        &self.index
    }

    /// Simple URL matching: checks if a template could match a concrete URL
    ///
    /// Handles `{param}` placeholders in templates by treating them as wildcards
    /// for the corresponding path segment or query parameter value.
    fn url_matches(template: &str, url: &str) -> bool {
        // Split path and query components
        let template_path = template.split('?').next().unwrap_or(template);
        let url_path = url.split('?').next().unwrap_or(url);

        let template_parts: Vec<&str> = template_path.split('/').collect();
        let url_parts: Vec<&str> = url_path.split('/').collect();

        if template_parts.len() != url_parts.len() {
            return false;
        }

        for (t, u) in template_parts.iter().zip(url_parts.iter()) {
            if t.starts_with('{') && t.ends_with('}') {
                // Template parameter — matches anything
                continue;
            }
            if t != u {
                return false;
            }
        }

        // If the URL has query params but the template does NOT, reject the match.
        // Replaying without the query would return wrong data while still looking "successful".
        let template_has_query = template.contains('?');
        let url_has_query = url.split('?').nth(1).is_some_and(|q| !q.is_empty());
        if url_has_query && !template_has_query {
            return false;
        }

        // If the template has query params, verify bidirectional key match:
        // 1. Every template key must exist in the URL (forward check)
        // 2. Every URL key must exist in the template (reverse check)
        // Without the reverse check, extra URL params silently get dropped during replay.
        if let Some(template_query) = template.split('?').nth(1) {
            let url_query = url.split('?').nth(1).unwrap_or("");
            let url_params = parse_query_params_lossy(url_query);
            let template_params = parse_query_params_lossy(template_query);
            let template_keys = template_params
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<std::collections::HashSet<_>>();

            // Forward check: every template key must exist in URL
            for (key, template_val) in &template_params {
                if template_val.starts_with('{') && template_val.ends_with('}') {
                    // Parameterized query value — just need the key present
                    if !url_params.iter().any(|(url_key, _)| url_key == key) {
                        return false;
                    }
                } else {
                    // Literal query value — compare decoded query values so a
                    // stored template can match a browser-encoded live URL.
                    if !url_params
                        .iter()
                        .any(|(url_key, url_value)| url_key == key && url_value == template_val)
                    {
                        return false;
                    }
                }
            }

            // Reverse check: reject if URL has extra keys not declared in template.
            // Without this, extra params are silently dropped during replay.
            for (url_key, _) in &url_params {
                if !template_keys.contains(url_key.as_str()) {
                    return false;
                }
            }
        }

        true
    }
}

fn parse_query_params_lossy(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn confidence_key(confidence: &ConfidenceLevel) -> &'static str {
    match confidence {
        ConfidenceLevel::Observed => "observed",
        ConfidenceLevel::Candidate => "candidate",
        ConfidenceLevel::Validated => "validated",
        ConfidenceLevel::Trusted => "trusted",
    }
}

fn graphql_operation_kind_compatible(
    left: Option<GraphqlOperationKind>,
    right: Option<GraphqlOperationKind>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn merge_graphql_operation_kind(
    left: Option<GraphqlOperationKind>,
    right: Option<GraphqlOperationKind>,
) -> Option<GraphqlOperationKind> {
    match (left, right) {
        (Some(left), Some(right)) if left == right => Some(left),
        (Some(left), Some(_)) => Some(left),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn graphql_context_wildcards(
    cap: &CapabilitySummary,
    graphql_operation: Option<&str>,
    graphql_operation_kind: Option<GraphqlOperationKind>,
) -> Option<usize> {
    let mut wildcards = 0usize;

    if let Some(operation) = graphql_operation {
        match &cap.graphql_operation {
            Some(cap_operation) if cap_operation == operation => {},
            _ => return None,
        }
    }

    if let Some(kind) = graphql_operation_kind {
        match cap.graphql_operation_kind {
            Some(cap_kind) if cap_kind == kind => {},
            Some(_) if kind == GraphqlOperationKind::Unknown => wildcards += 1,
            Some(_) => return None,
            None => wildcards += 1,
        }
    }

    Some(wildcards)
}

fn merge_trace_ids(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for id in existing.iter().chain(incoming.iter()) {
        if seen.insert(id) {
            merged.push(id.clone());
        }
    }
    merged
}

impl Default for CapabilityRegistry {
    fn default() -> Self {
        Self::new().expect("Failed to create default CapabilityRegistry")
    }
}

/// Summary statistics for the registry
#[derive(Debug, Clone)]
pub struct RegistryStats {
    pub total_origins: usize,
    pub total_capabilities: usize,
    pub by_confidence: HashMap<String, usize>,
    pub origins: Vec<String>,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::action_binding::ActionParamBinding;
    use super::super::capability::ConfidenceLevel;
    use super::*;
    use tempfile::TempDir;

    fn make_capability(name: &str, origin: &str, method: &str, url: &str) -> ApiCapability {
        ApiCapability::new(
            name.to_string(),
            origin.to_string(),
            method.to_string(),
            url.to_string(),
        )
    }

    #[test]
    fn test_registry_creation() {
        let temp = TempDir::new().unwrap();
        let reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
        assert!(reg.index().origins.is_empty());
    }

    #[test]
    fn test_register_and_find() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let cap = make_capability(
            "gmail_sync",
            "https://mail.google.com",
            "GET",
            "https://mail.google.com/sync/u/0/i/s",
        );

        reg.register(&cap).unwrap();

        let found = reg.find_by_url("GET", "https://mail.google.com/sync/u/0/i/s");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "gmail_sync");
    }

    #[test]
    fn test_url_template_matching() {
        assert!(CapabilityRegistry::url_matches(
            "https://example.com/api/users/{id}",
            "https://example.com/api/users/12345"
        ));

        assert!(!CapabilityRegistry::url_matches(
            "https://example.com/api/users/{id}",
            "https://example.com/api/posts/12345"
        ));

        assert!(CapabilityRegistry::url_matches(
            "https://example.com/api/v1/data",
            "https://example.com/api/v1/data"
        ));

        // Query-bearing URL must NOT match a no-query template (would silently drop query params)
        assert!(!CapabilityRegistry::url_matches(
            "https://example.com/api/data",
            "https://example.com/api/data?page=2&limit=50"
        ));

        // Template with query SHOULD match URL with matching query
        assert!(CapabilityRegistry::url_matches(
            "https://example.com/api/data?page={page}&limit={limit}",
            "https://example.com/api/data?page=2&limit=50"
        ));

        // No-query URL should match no-query template
        assert!(CapabilityRegistry::url_matches(
            "https://example.com/api/data",
            "https://example.com/api/data"
        ));

        // URL with extra query params beyond template must NOT match (would be silently dropped)
        assert!(!CapabilityRegistry::url_matches(
            "https://example.com/api/data?q={q}",
            "https://example.com/api/data?q=search&page=2"
        ));

        // URL with exactly matching query keys should match
        assert!(CapabilityRegistry::url_matches(
            "https://example.com/api/data?q={q}&page={page}",
            "https://example.com/api/data?q=search&page=2"
        ));

        // Browser traces store encoded URLs while mined templates can contain
        // decoded literal query values; matching should compare decoded values.
        assert!(CapabilityRegistry::url_matches(
            "https://example.com/api/data?agent=Algolia for JavaScript (4.13.1); Browser (lite)&q=hello world",
            "https://example.com/api/data?agent=Algolia%20for%20JavaScript%20(4.13.1)%3B%20Browser%20(lite)&q=hello%20world"
        ));
    }

    #[test]
    fn test_replayable_filter() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // GET capability (read-only) — starts as Observed, not replayable
        let mut cap_get = make_capability(
            "get_data",
            "https://example.com",
            "GET",
            "https://example.com/api/data",
        );
        // Promote to Candidate
        cap_get.add_sample("req-2".to_string());
        cap_get.add_sample("req-3".to_string());
        reg.register(&cap_get).unwrap();

        // POST capability (write) at Observed — not replayable (needs Validated)
        let cap_post = make_capability(
            "submit_form",
            "https://example.com",
            "POST",
            "https://example.com/api/submit",
        );
        reg.register(&cap_post).unwrap();

        let replayable = reg.find_replayable_for_origin("https://example.com");
        assert_eq!(replayable.len(), 1);
        assert_eq!(replayable[0].name, "get_data");
    }

    #[test]
    fn test_remove_capability() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let cap = make_capability(
            "test_api",
            "https://example.com",
            "GET",
            "https://example.com/api/test",
        );
        let cap_id = cap.id.clone();

        reg.register(&cap).unwrap();
        assert_eq!(reg.stats().total_capabilities, 1);

        reg.remove("https://example.com", &cap_id).unwrap();
        assert_eq!(reg.stats().total_capabilities, 0);
    }

    #[test]
    fn test_rebuild_from_disk() {
        let temp = TempDir::new().unwrap();

        // First, create and save a capability
        {
            let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
            let cap = make_capability(
                "test_api",
                "https://example.com",
                "GET",
                "https://example.com/api/test",
            );
            reg.register(&cap).unwrap();
        }

        // Create a new registry pointing to the same path — should rebuild from disk
        let reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
        assert_eq!(reg.stats().total_capabilities, 1);
    }

    #[test]
    fn test_registry_open_repairs_stale_index_entries() {
        let temp = TempDir::new().unwrap();
        let origin = "https://example.com";
        let cap = make_capability("test_api", origin, "GET", "https://example.com/api/test");
        let cap_id = cap.id.clone();

        {
            let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
            reg.register(&cap).unwrap();
        }

        let origin_key = CapabilityStore::origin_to_key(origin);
        let cap_path = temp
            .path()
            .join(&origin_key)
            .join("capabilities")
            .join(format!("{}.json", cap_id));
        std::fs::remove_file(cap_path).unwrap();

        let reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
        let health = reg.health_snapshot().unwrap();
        assert_eq!(health.indexed_capability_count, 0);
        assert_eq!(health.loadable_capability_count, 0);
        assert_eq!(health.stale_index_count, 0);
        assert!(reg
            .find_by_url("GET", "https://example.com/api/test")
            .is_none());
    }

    #[test]
    fn registry_version_upgrade_restores_parent_origin_in_summaries() {
        let temp = TempDir::new().unwrap();
        let mut capability = make_capability(
            "search_api",
            "https://api.example.com",
            "GET",
            "https://api.example.com/search",
        );
        capability.parent_origin = Some("https://www.example.com".into());
        {
            let mut registry = CapabilityRegistry::with_base_path(temp.path()).unwrap();
            registry.register(&capability).unwrap();
        }

        let index_path = temp.path().join("registry_index.json");
        let mut stale: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
        stale["version"] = serde_json::Value::String("1.0.0".into());
        for origin in stale["origins"].as_object_mut().unwrap().values_mut() {
            for summary in origin["capabilities"].as_array_mut().unwrap() {
                summary.as_object_mut().unwrap().remove("parent_origin");
            }
        }
        std::fs::write(&index_path, serde_json::to_vec_pretty(&stale).unwrap()).unwrap();

        let registry = CapabilityRegistry::with_base_path(temp.path()).unwrap();
        assert_eq!(registry.index().version, INDEX_VERSION);
        let summary = &registry
            .index()
            .origins
            .values()
            .next()
            .unwrap()
            .capabilities[0];
        assert_eq!(
            summary.parent_origin.as_deref(),
            Some("https://www.example.com")
        );
    }

    #[test]
    fn test_registry_health_counts_takeover_ready_bindings() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
        let mut cap = make_capability(
            "search_api",
            "https://example.com",
            "GET",
            "https://example.com/api/search?q={query}",
        );
        cap.add_sample("trace-2".to_string());
        cap.add_sample("trace-3".to_string());
        cap.action_bindings.push(ActionBinding {
            action_type: "fill".to_string(),
            action_signature: "input.search".to_string(),
            semantic_signature: Some("input:name=search".to_string()),
            page_origin: Some("https://example.com".to_string()),
            page_path_template: Some("/".to_string()),
            param_bindings: vec![ActionParamBinding {
                action_param: "text".to_string(),
                capability_param: "query".to_string(),
            }],
            default_params: HashMap::new(),
            sample_count: 2,
            last_seen_at: Some(chrono::Utc::now().timestamp()),
        });

        reg.register(&cap).unwrap();
        let health = reg.health_snapshot().unwrap();

        assert_eq!(health.indexed_capability_count, 1);
        assert_eq!(health.loadable_capability_count, 1);
        assert_eq!(health.replayable_capability_count, 1);
        assert_eq!(health.action_bindings_count, 1);
        assert_eq!(health.takeover_ready_bindings_count, 1);
        assert_eq!(
            health.candidates_by_confidence.get("candidate").copied(),
            Some(1)
        );
        assert_eq!(health.origin_readiness.len(), 1);
        assert!(health.origin_readiness[0].inactive_reasons.is_empty());
    }

    #[test]
    fn test_stats() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        reg.register(&make_capability(
            "api_1",
            "https://example.com",
            "GET",
            "https://example.com/api/1",
        ))
        .unwrap();

        reg.register(&make_capability(
            "api_2",
            "https://other.com",
            "GET",
            "https://other.com/api/2",
        ))
        .unwrap();

        let stats = reg.stats();
        assert_eq!(stats.total_origins, 2);
        assert_eq!(stats.total_capabilities, 2);
    }

    #[test]
    fn test_recover_from_corrupt_index() {
        let temp = TempDir::new().unwrap();

        // Register a capability normally
        {
            let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
            reg.register(&make_capability(
                "test_api",
                "https://example.com",
                "GET",
                "https://example.com/api/test",
            ))
            .unwrap();
        }

        // Corrupt the index file
        let index_path = temp.path().join("registry_index.json");
        assert!(index_path.exists());
        std::fs::write(&index_path, "{ corrupted json: [").unwrap();

        // Reload registry — should detect corruption and rebuild from disk
        let reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();
        assert_eq!(reg.stats().total_capabilities, 1);
        assert_eq!(reg.stats().total_origins, 1);
    }

    #[test]
    fn test_empty_origin_key() {
        // Verify origin_to_key handles empty strings safely
        let key = CapabilityStore::origin_to_key("");
        assert_eq!(key, "unknown_origin");
        assert!(!key.is_empty());
    }

    #[test]
    fn test_find_by_url_prefers_literal_over_template() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // Register a templated capability: /users/{id}
        let cap_template = make_capability(
            "users_by_id",
            "https://example.com",
            "GET",
            "https://example.com/api/users/{id}",
        );
        reg.register(&cap_template).unwrap();

        // Register a literal capability: /users/me (no wildcards)
        let cap_literal = make_capability(
            "users_me",
            "https://example.com",
            "GET",
            "https://example.com/api/users/me",
        );
        reg.register(&cap_literal).unwrap();

        // Both templates match /users/me, but the literal should win
        let found = reg.find_by_url("GET", "https://example.com/api/users/me");
        assert!(found.is_some(), "Expected a match for /users/me");
        assert_eq!(
            found.unwrap().name,
            "users_me",
            "Literal match must win over template match"
        );

        // Only the template should match /users/12345
        let found2 = reg.find_by_url("GET", "https://example.com/api/users/12345");
        assert!(found2.is_some(), "Expected a match for /users/12345");
        assert_eq!(found2.unwrap().name, "users_by_id");
    }

    #[test]
    fn test_find_by_url_prefers_more_specific_template() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // Two capabilities with different specificity (wildcard counts) that both match
        // the same URL. The more specific one (fewer wildcards) should win.
        let cap_broad = make_capability(
            "broad_api",
            "https://example.com",
            "GET",
            "https://example.com/api/{resource}/{id}",
        );
        let cap_narrow = make_capability(
            "narrow_api",
            "https://example.com",
            "GET",
            "https://example.com/api/users/{id}",
        );

        // Register broad first to ensure order doesn't matter
        reg.register(&cap_broad).unwrap();
        reg.register(&cap_narrow).unwrap();

        // Both match /api/users/42, but narrow has 1 wildcard vs broad's 2
        let found = reg.find_by_url("GET", "https://example.com/api/users/42");
        assert!(found.is_some(), "Expected a match");
        assert_eq!(
            found.unwrap().name,
            "narrow_api",
            "More specific template (fewer wildcards) must win"
        );
    }

    #[test]
    fn test_register_dedup_respects_body_fingerprint() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // Two capabilities for the same URL but with different body fingerprints
        let mut cap_read = make_capability(
            "sync_mark_read",
            "https://mail.google.com",
            "POST",
            "https://mail.google.com/sync/u/0/i/s",
        );
        cap_read.body_fingerprint = Some("json_keys:action,ids".to_string());

        let mut cap_archive = make_capability(
            "sync_archive",
            "https://mail.google.com",
            "POST",
            "https://mail.google.com/sync/u/0/i/s",
        );
        cap_archive.body_fingerprint = Some("json_keys:action,ids,labels".to_string());

        reg.register(&cap_read).unwrap();
        reg.register(&cap_archive).unwrap();

        // Both should exist as separate capabilities
        assert_eq!(
            reg.stats().total_capabilities,
            2,
            "Capabilities with different body fingerprints should not be deduped"
        );
    }

    #[test]
    fn test_register_dedup_respects_graphql_operation() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let mut cap_get_user = make_capability(
            "graphql_get_user",
            "https://api.example.com",
            "POST",
            "https://api.example.com/graphql",
        );
        cap_get_user.graphql_operation = Some("GetUser".to_string());

        let mut cap_list_posts = make_capability(
            "graphql_list_posts",
            "https://api.example.com",
            "POST",
            "https://api.example.com/graphql",
        );
        cap_list_posts.graphql_operation = Some("ListPosts".to_string());

        reg.register(&cap_get_user).unwrap();
        reg.register(&cap_list_posts).unwrap();

        assert_eq!(
            reg.stats().total_capabilities,
            2,
            "Capabilities with different GraphQL operations should not be deduped"
        );

        let found = reg.find_by_url_with_request_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("ListPosts"),
            None,
        );
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "graphql_list_posts");

        let found2 = reg.find_by_url_with_request_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("GetUser"),
            None,
        );
        assert!(found2.is_some());
        assert_eq!(found2.unwrap().name, "graphql_get_user");

        let missing = reg.find_by_url_with_request_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("DeleteUser"),
            None,
        );
        assert!(missing.is_none());
    }

    #[test]
    fn test_register_and_lookup_respect_graphql_operation_kind() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let mut query_cap = make_capability(
            "graphql_get_user_query",
            "https://api.example.com",
            "POST",
            "https://api.example.com/graphql",
        );
        query_cap.graphql_operation = Some("GetUser".to_string());
        query_cap.graphql_operation_kind = Some(GraphqlOperationKind::Query);
        query_cap.refresh_side_effects();

        let mut mutation_cap = make_capability(
            "graphql_get_user_mutation",
            "https://api.example.com",
            "POST",
            "https://api.example.com/graphql",
        );
        mutation_cap.graphql_operation = Some("GetUser".to_string());
        mutation_cap.graphql_operation_kind = Some(GraphqlOperationKind::Mutation);
        mutation_cap.refresh_side_effects();

        reg.register(&query_cap).unwrap();
        reg.register(&mutation_cap).unwrap();

        assert_eq!(
            reg.stats().total_capabilities,
            2,
            "Capabilities with the same operation name but different kinds should stay separate"
        );

        let found_query = reg.find_by_url_with_request_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("GetUser"),
            Some(GraphqlOperationKind::Query),
        );
        assert_eq!(found_query.unwrap().name, "graphql_get_user_query");

        let found_mutation = reg.find_by_url_with_request_context(
            "POST",
            "https://api.example.com/graphql",
            None,
            Some("GetUser"),
            Some(GraphqlOperationKind::Mutation),
        );
        assert_eq!(found_mutation.unwrap().name, "graphql_get_user_mutation");
    }

    #[test]
    fn test_register_dedup_preserves_replay_maturity() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let mut mature = make_capability(
            "gmail_sync",
            "https://mail.google.com",
            "GET",
            "https://mail.google.com/sync/u/0/i/s?cursor={cursor}",
        );
        mature.sample_count = 12;
        mature.replay_success_count = 9;
        mature.replay_failure_count = 1;
        mature.confidence = ConfidenceLevel::Trusted;
        reg.register(&mature).unwrap();

        // Simulate a fresh mining pass creating a duplicate capability with a new ID.
        let mut duplicate = make_capability(
            "gmail_sync_new",
            "https://mail.google.com",
            "GET",
            "https://mail.google.com/sync/u/0/i/s?cursor={cursor}",
        );
        duplicate.sample_count = 3;
        duplicate.replay_success_count = 0;
        duplicate.replay_failure_count = 0;
        duplicate.confidence = ConfidenceLevel::Observed;
        reg.register(&duplicate).unwrap();

        let all = reg
            .get_all_for_origin("https://mail.google.com")
            .expect("origin load should succeed");
        assert_eq!(all.len(), 1, "dedup should keep a single capability");
        let kept = &all[0];
        assert_eq!(kept.replay_success_count, 9);
        assert_eq!(kept.replay_failure_count, 1);
        assert_eq!(kept.confidence, ConfidenceLevel::Trusted);
        assert_eq!(kept.sample_count, 12);
    }

    #[test]
    fn test_find_by_url_with_fingerprint_disambiguation() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // Two POST capabilities on the same URL with different body fingerprints
        let mut cap_read = make_capability(
            "sync_mark_read",
            "https://mail.google.com",
            "POST",
            "https://mail.google.com/sync/u/0/i/s",
        );
        cap_read.body_fingerprint = Some("json_keys:action,ids".to_string());

        let mut cap_archive = make_capability(
            "sync_archive",
            "https://mail.google.com",
            "POST",
            "https://mail.google.com/sync/u/0/i/s",
        );
        cap_archive.body_fingerprint = Some("json_keys:action,ids,labels".to_string());

        reg.register(&cap_read).unwrap();
        reg.register(&cap_archive).unwrap();

        // With fingerprint → should select the right one
        let found = reg.find_by_url_with_fingerprint(
            "POST",
            "https://mail.google.com/sync/u/0/i/s",
            Some("json_keys:action,ids,labels"),
        );
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "sync_archive");

        let found2 = reg.find_by_url_with_fingerprint(
            "POST",
            "https://mail.google.com/sync/u/0/i/s",
            Some("json_keys:action,ids"),
        );
        assert!(found2.is_some());
        assert_eq!(found2.unwrap().name, "sync_mark_read");
    }

    #[test]
    fn test_find_by_url_delegates_to_fingerprint() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let cap = make_capability(
            "test_api",
            "https://example.com",
            "GET",
            "https://example.com/api/data",
        );
        reg.register(&cap).unwrap();

        // find_by_url(m, u) must equal find_by_url_with_fingerprint(m, u, None)
        let a = reg.find_by_url("GET", "https://example.com/api/data");
        let b = reg.find_by_url_with_fingerprint("GET", "https://example.com/api/data", None);
        assert_eq!(a.as_ref().map(|c| &c.id), b.as_ref().map(|c| &c.id));
    }

    #[test]
    fn test_find_by_url_deterministic_tiebreak() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // Two capabilities with same wildcard count (1 each) that can both match
        // the same URL. Different url_templates avoids register dedup.
        // /api/{resource}/list and /api/users/{action} both match /api/users/list.
        let cap_z = make_capability(
            "zzz_api",
            "https://example.com",
            "GET",
            "https://example.com/api/{resource}/list",
        );
        let cap_a = make_capability(
            "aaa_api",
            "https://example.com",
            "GET",
            "https://example.com/api/users/{action}",
        );

        // Register in reverse alphabetical order
        reg.register(&cap_z).unwrap();
        reg.register(&cap_a).unwrap();

        let found = reg.find_by_url("GET", "https://example.com/api/users/list");
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().name,
            "aaa_api",
            "Alphabetically earlier ID must win as tiebreak"
        );
    }

    #[test]
    fn test_replayable_filter_includes_validated_post() {
        let temp = TempDir::new().unwrap();
        let mut reg = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        // POST capability at Validated — should be replayable
        let mut cap_post = make_capability(
            "submit_form",
            "https://example.com",
            "POST",
            "https://example.com/api/submit",
        );
        // Promote to Candidate
        cap_post.add_sample("req-2".to_string());
        cap_post.add_sample("req-3".to_string());
        // Promote to Validated (3 replays)
        cap_post.record_replay_success();
        cap_post.record_replay_success();
        cap_post.record_replay_success();
        assert_eq!(cap_post.confidence, ConfidenceLevel::Validated);
        reg.register(&cap_post).unwrap();

        // GET capability at Candidate — also replayable
        let mut cap_get = make_capability(
            "get_data",
            "https://example.com",
            "GET",
            "https://example.com/api/data",
        );
        cap_get.add_sample("req-4".to_string());
        cap_get.add_sample("req-5".to_string());
        reg.register(&cap_get).unwrap();

        let replayable = reg.find_replayable_for_origin("https://example.com");
        assert_eq!(
            replayable.len(),
            2,
            "Both GET at Candidate and POST at Validated should be replayable"
        );

        let names: Vec<&str> = replayable.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"submit_form"));
        assert!(names.contains(&"get_data"));
    }
}
