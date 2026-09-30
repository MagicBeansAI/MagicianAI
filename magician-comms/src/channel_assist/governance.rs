//! The storage-governance service: snapshot/vacuum/compact maintenance over
//! the comms stores (channel-assist, attention-learning). Lives in the comms
//! crate because it holds the concrete stores; the governance vocabulary
//! (kinds, safety classes, descriptors) stays lib-side in
//! `magician_v2::storage_governance`.

pub mod automatic;

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
const TRANSPORT_LOG_RETENTION_DAYS: u32 = {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    let window = magician::magician_v2::transport_log::EVENTS_RETENTION_WINDOW_MS;
    let whole_days = window / DAY_MS;
    let partial_day = window % DAY_MS != 0;
    if whole_days < 1 {
        1
    } else if partial_day {
        whole_days as u32 + 1
    } else {
        whole_days as u32
    }
};
const APP_DIRECTORY_INVENTORY_MAX_ENTRIES: usize = 20_000;
const APP_DIRECTORY_INVENTORY_MAX_DEPTH: usize = 64;
const DIRECTORY_INVENTORY_MAX_ENTRIES: usize = 50_000;
const DIRECTORY_INVENTORY_MAX_DEPTH: usize = 64;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use duckdb::{AccessMode, Config as DuckDbConfig, Connection};
use magician::magician_v2::storage_governance::record_compaction_metrics;
const STORAGE_SNAPSHOT_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);
const STORAGE_SNAPSHOT_GATE_COUNT: usize = 16;
const STORAGE_SNAPSHOT_CACHE_MAX_SCOPES: usize = 256;

#[derive(Clone)]
struct CachedStorageSnapshot {
    captured_at: Instant,
    snapshot: StorageSnapshot,
}

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::database_owners::{
    database_file_path, host_database_path, DatabaseOwner,
};
use magician::magician_v2::storage_governance::{
    app_store_maintenance_actions, DuckDbCompactionReport, DuckDbTarget, StorageActionDescriptor,
    StorageEntry, StorageKind, StorageMaintenanceReport, StorageSafetyClass, StorageSnapshot,
    APP_STORE_INVENTORY_POLICY,
};
use magician::magician_v2::ui_threads::UiThreadStore;
use serde::{Deserialize, Serialize};

use crate::channel_assist::store::MailAssistStore;
use magician::magician_v2::attention::learning::{
    AttentionLearningService, AttentionOptimizeReport, AttentionReclaimReport,
    AttentionRetentionReport,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionLearningMaintenanceOperation {
    Optimize,
    RetentionPreview,
    RetentionApply,
    Reclaim,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionLearningMaintenanceReport {
    pub principal: String,
    pub workspace: String,
    pub operation: AttentionLearningMaintenanceOperation,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub retention_days: Option<u32>,
    pub optimize: Option<AttentionOptimizeReport>,
    pub retention: Option<AttentionRetentionReport>,
    pub reclaim: Option<AttentionReclaimReport>,
}

#[derive(Clone)]
pub struct StorageGovernanceService {
    workspace_layout: ArtifactV2Workspace,
    mail_store: MailAssistStore,
    feed_store: Option<magician::magician_v2::feed::FeedStore>,
    ui_thread_store: UiThreadStore,
    social_store: std::sync::Arc<magician::magician_v2::social::store::SocialStoreRegistry>,
    attention_learning: Option<AttentionLearningService>,
    maintenance_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
    maintenance_run_id: String,
    maintenance_enabled: Arc<std::sync::atomic::AtomicBool>,
    snapshot_gates: std::sync::Arc<Vec<tokio::sync::Mutex<()>>>,
    snapshot_cache:
        std::sync::Arc<std::sync::Mutex<BTreeMap<(String, String), CachedStorageSnapshot>>>,
    llm_content_settings: Option<
        magician::magician_v2::analytics::llm_trace_content::LlmContentCaptureSettingsHandle,
    >,
}

impl StorageGovernanceService {
    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        mail_store: MailAssistStore,
        ui_thread_store: UiThreadStore,
        social_store: std::sync::Arc<magician::magician_v2::social::store::SocialStoreRegistry>,
    ) -> Self {
        Self {
            workspace_layout,
            mail_store,
            feed_store: None,
            ui_thread_store,
            social_store,
            attention_learning: None,
            maintenance_gate: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            maintenance_run_id: uuid::Uuid::new_v4().to_string(),
            maintenance_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            snapshot_gates: std::sync::Arc::new(
                (0..STORAGE_SNAPSHOT_GATE_COUNT)
                    .map(|_| tokio::sync::Mutex::new(()))
                    .collect(),
            ),
            snapshot_cache: std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            llm_content_settings: None,
        }
    }

    pub fn with_attention_learning(mut self, service: AttentionLearningService) -> Self {
        self.attention_learning = Some(service);
        self
    }

    pub fn with_llm_content_settings(
        mut self,
        settings: magician::magician_v2::analytics::llm_trace_content::LlmContentCaptureSettingsHandle,
    ) -> Self {
        self.llm_content_settings = Some(settings);
        self
    }

    pub async fn snapshot(&self, principal: &str, workspace: &str) -> Result<StorageSnapshot> {
        // Scope-keyed followers recheck the short cache after acquiring their
        // bounded coalescing gate, so a burst causes one filesystem walk rather
        // than one blocking walk per request.
        let _snapshot_guard = self.snapshot_gate(principal, workspace).lock().await;
        if let Some(snapshot) = self.cached_snapshot(principal, workspace)? {
            return Ok(snapshot);
        }
        let layout = self.workspace_layout.clone();
        let principal_owned = principal.to_string();
        let workspace_owned = workspace.to_string();
        let restricted_retention = self.llm_content_settings.as_ref().map(|settings| {
            let retention = settings.snapshot().retention;
            (
                retention.sanitized_io_days,
                retention.context_metadata_days,
                retention.facts_days,
            )
        });
        let social_stores = Arc::clone(&self.social_store);
        let social_scope =
            magician::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(
                &principal.to_string(),
                &workspace.to_string(),
            );
        let snapshot = tokio::task::spawn_blocking(move || {
            let social_store = social_stores.existing_store_for_scope(&social_scope)?;
            let mut snapshot = build_snapshot_with_restricted_retention(
                &layout,
                &principal_owned,
                &workspace_owned,
                restricted_retention,
            )?;
            for entry in &mut snapshot.entries {
                if entry.id == "social_sqlite" {
                    entry.row_count = social_store
                        .as_ref()
                        .and_then(|store| store.diagnostic_total_row_count().ok());
                }
            }
            Ok::<StorageSnapshot, anyhow::Error>(snapshot)
        })
        .await
        .context("storage inventory task panicked")??;
        self.store_cached_snapshot(principal, workspace, snapshot.clone())?;
        Ok(snapshot)
    }

    fn cached_snapshot(&self, principal: &str, workspace: &str) -> Result<Option<StorageSnapshot>> {
        let key = (principal.to_owned(), workspace.to_owned());
        let cache = self
            .snapshot_cache
            .lock()
            .map_err(|_| anyhow!("storage snapshot cache lock poisoned"))?;
        Ok(cache.get(&key).and_then(|entry| {
            (entry.captured_at.elapsed() < STORAGE_SNAPSHOT_CACHE_TTL)
                .then(|| entry.snapshot.clone())
        }))
    }

    fn store_cached_snapshot(
        &self,
        principal: &str,
        workspace: &str,
        snapshot: StorageSnapshot,
    ) -> Result<()> {
        let key = (principal.to_owned(), workspace.to_owned());
        let mut cache = self
            .snapshot_cache
            .lock()
            .map_err(|_| anyhow!("storage snapshot cache lock poisoned"))?;
        if !cache.contains_key(&key) && cache.len() >= STORAGE_SNAPSHOT_CACHE_MAX_SCOPES {
            let oldest = cache
                .iter()
                .min_by_key(|(_, entry)| entry.captured_at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                cache.remove(&oldest);
            }
        }
        cache.insert(
            key,
            CachedStorageSnapshot {
                captured_at: Instant::now(),
                snapshot,
            },
        );
        Ok(())
    }

    fn invalidate_cached_snapshot(&self, principal: &str, workspace: &str) {
        if let Ok(mut cache) = self.snapshot_cache.lock() {
            cache.remove(&(principal.to_owned(), workspace.to_owned()));
        }
    }

    fn snapshot_gate(&self, principal: &str, workspace: &str) -> &tokio::sync::Mutex<()> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        principal.hash(&mut hasher);
        workspace.hash(&mut hasher);
        let gate_count = u64::try_from(self.snapshot_gates.len()).unwrap_or(1);
        let index = usize::try_from(hasher.finish() % gate_count).unwrap_or(0);
        &self.snapshot_gates[index]
    }

    pub async fn compact_databases(
        &self,
        principal: &str,
        workspace: &str,
        targets: &[DuckDbTarget],
    ) -> Result<StorageMaintenanceReport> {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let _snapshot_guard = self.snapshot_gate(principal, workspace).lock().await;
        let started_at_ms = Utc::now().timestamp_millis();
        let mut reports = Vec::new();
        for target in targets {
            let report = match target {
                DuckDbTarget::Analytics => {
                    let pool = magician::magician_v2::analytics::scoped_pool(
                        &self.workspace_layout,
                        principal,
                        workspace,
                    )?;
                    let pool = std::sync::Arc::clone(&pool);
                    tokio::task::spawn_blocking(move || pool.compact())
                        .await
                        .context("analytics compaction task panicked")??
                },
                DuckDbTarget::ChannelAssist => {
                    self.mail_store.compact_scope(principal, workspace).await?
                },
                DuckDbTarget::UiThreads => {
                    self.ui_thread_store
                        .compact_scope(principal, workspace)
                        .await?
                },
                DuckDbTarget::Social => {
                    let social_stores = Arc::clone(&self.social_store);
                    let social_scope = magician::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(&principal.to_string(), &workspace.to_string());
                    tokio::task::spawn_blocking(move || {
                        match social_stores.existing_store_for_scope(&social_scope)? {
                            Some(social_store) => social_store.compact(),
                            None => Ok(DuckDbCompactionReport {
                                database: "social.db".to_string(),
                                bytes_before: 0,
                                bytes_after: 0,
                                bytes_reclaimed: 0,
                                table_count: 0,
                                row_count: 0,
                            }),
                        }
                    })
                    .await
                    .context("social compaction task panicked")??
                },
            };
            reports.push(report.clone());
            record_compaction_metrics(
                &self.workspace_layout,
                magician::magician_v2::storage_governance::compaction_metrics::CompactionMetricTrigger::Manual,
                &StorageMaintenanceReport {
                    principal: principal.to_string(),
                    workspace: workspace.to_string(),
                    started_at_ms,
                    completed_at_ms: Utc::now().timestamp_millis(),
                    duckdb: vec![report],
                    parquet: None,
                    canonical_llm: None,
                    retention: None,
                },
            );
        }
        let report = StorageMaintenanceReport {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            duckdb: reports,
            parquet: None,
            canonical_llm: None,
            retention: None,
        };
        self.invalidate_cached_snapshot(principal, workspace);
        Ok(report)
    }

    pub async fn compact_parquet(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<StorageMaintenanceReport> {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let _snapshot_guard = self.snapshot_gate(principal, workspace).lock().await;
        let started_at_ms = Utc::now().timestamp_millis();
        let layout = self.workspace_layout.clone();
        let principal_owned = principal.to_string();
        let workspace_owned = workspace.to_string();
        let (parquet, canonical_llm) = tokio::task::spawn_blocking(move || {
            let mut parquet =
                magician::magician_v2::analytics::parquet_maintenance::compact_scope(&layout, &principal_owned, &workspace_owned, 2)?;
            // The activity spine is not in `PartitionedDataset::ALL` — it folds
            // hours before days, so it has its own driver — and `compact_scope`
            // therefore skips it. Calling it explicitly here is what makes the
            // dataset's "Compact Parquet" button do what it says.
            parquet.absorb(magician::magician_v2::analytics::parquet_maintenance::compact_activity_rows(
                &layout,
                &principal_owned,
                &workspace_owned,
            )?);
            // Canonical LLM facts retain immutable revisions as a corruption
            // fallback; their governed compact object still collapses read
            // fan-out. Raw batch pruning applies to the batch-owned streams.
            let scope = magicllm::LlmScope::new(principal_owned, workspace_owned);
            let canonical = magician::magician_v2::analytics::llm_fact_compactor::compact_scope(
                &layout,
                &scope,
                magician::magician_v2::analytics::llm_fact_compactor::LlmFactCompactionPolicy::default(),
            )?;
            Ok::<_, anyhow::Error>((parquet, canonical))
        })
        .await
        .context("Parquet compaction task panicked")??;
        let report = StorageMaintenanceReport {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            duckdb: Vec::new(),
            parquet: Some(parquet),
            canonical_llm: Some(canonical_llm),
            retention: None,
        };
        record_compaction_metrics(
            &self.workspace_layout,
            magician::magician_v2::storage_governance::compaction_metrics::CompactionMetricTrigger::Manual,
            &report,
        );
        self.invalidate_cached_snapshot(principal, workspace);
        Ok(report)
    }

    pub async fn apply_retention(
        &self,
        principal: &str,
        workspace: &str,
        retention_days: u32,
    ) -> Result<StorageMaintenanceReport> {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let _snapshot_guard = self.snapshot_gate(principal, workspace).lock().await;
        if !(30..=3650).contains(&retention_days) {
            return Err(anyhow!("retention_days must be between 30 and 3650"));
        }
        let started_at_ms = Utc::now().timestamp_millis();
        let layout = self.workspace_layout.clone();
        let principal_owned = principal.to_string();
        let workspace_owned = workspace.to_string();
        let retention = tokio::task::spawn_blocking(move || {
            apply_scope_retention(&layout, &principal_owned, &workspace_owned, retention_days)
        })
        .await
        .context("storage retention task panicked")??;
        let report = StorageMaintenanceReport {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            duckdb: Vec::new(),
            parquet: None,
            canonical_llm: None,
            retention: Some(retention),
        };
        self.invalidate_cached_snapshot(principal, workspace);
        Ok(report)
    }

    pub async fn clear_compaction_metrics(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<
        magician::magician_v2::storage_governance::compaction_metrics::CompactionMetricsSnapshot,
    > {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let _snapshot_guard = self.snapshot_gate(principal, workspace).lock().await;
        let layout = self.workspace_layout.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let principal_for_task = principal.clone();
        let workspace_for_task = workspace.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            magician::magician_v2::storage_governance::compaction_metrics::clear(
                &layout,
                &principal_for_task,
                &workspace_for_task,
            )
        })
        .await
        .context("clearing compaction metrics task panicked")??;
        self.invalidate_cached_snapshot(&principal, &workspace);
        Ok(snapshot)
    }

    fn attention_learning(&self) -> Result<AttentionLearningService> {
        self.attention_learning
            .clone()
            .context("attention learning maintenance is unavailable")
    }

    pub async fn optimize_attention_learning(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AttentionLearningMaintenanceReport> {
        // Inventory observes this SQLite owner through file metadata only.
        // Its read-coalescing gate must remain free during attention work;
        // the maintenance gate and store owner still serialize mutations.
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let started_at_ms = Utc::now().timestamp_millis();
        let optimize = self
            .attention_learning()?
            .store()
            .optimize_database()
            .await?;
        let report = AttentionLearningMaintenanceReport {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            operation: AttentionLearningMaintenanceOperation::Optimize,
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            retention_days: None,
            optimize: Some(optimize),
            retention: None,
            reclaim: None,
        };
        self.invalidate_cached_snapshot(principal, workspace);
        Ok(report)
    }

    pub async fn retain_attention_learning(
        &self,
        principal: &str,
        workspace: &str,
        retention_days: u32,
        apply: bool,
    ) -> Result<AttentionLearningMaintenanceReport> {
        if !(30..=3_650).contains(&retention_days) {
            return Err(anyhow!("retention_days must be between 30 and 3650"));
        }
        // Inventory observes this SQLite owner through file metadata only.
        // Its read-coalescing gate must remain free during attention work;
        // the maintenance gate and store owner still serialize mutations.
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let started_at_ms = Utc::now().timestamp_millis();
        let now = Utc::now().timestamp_millis();
        let cutoff_at = now.saturating_sub(i64::from(retention_days).saturating_mul(86_400_000));
        let learning = self.attention_learning()?;
        let mut retention = learning
            .store()
            .apply_scoped_retention(principal, workspace, cutoff_at.max(1), apply)
            .await?;
        let deliveries = learning
            .compact_attention_deliveries(principal, workspace, now, apply)
            .await?;
        for (name, count) in deliveries.affected_rows {
            retention
                .affected_rows
                .insert(format!("delivery_{name}"), count);
        }
        let report = AttentionLearningMaintenanceReport {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            operation: if apply {
                AttentionLearningMaintenanceOperation::RetentionApply
            } else {
                AttentionLearningMaintenanceOperation::RetentionPreview
            },
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            retention_days: Some(retention_days),
            optimize: None,
            retention: Some(retention),
            reclaim: None,
        };
        if apply {
            self.invalidate_cached_snapshot(principal, workspace);
        }
        Ok(report)
    }

    pub async fn reclaim_attention_learning(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AttentionLearningMaintenanceReport> {
        // Inventory observes this SQLite owner through file metadata only.
        // Its read-coalescing gate must remain free during attention work;
        // the maintenance gate and store owner still serialize mutations.
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let started_at_ms = Utc::now().timestamp_millis();
        let reclaim = self
            .attention_learning()?
            .store()
            .reclaim_database_space()
            .await?;
        let report = AttentionLearningMaintenanceReport {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            operation: AttentionLearningMaintenanceOperation::Reclaim,
            started_at_ms,
            completed_at_ms: Utc::now().timestamp_millis(),
            retention_days: None,
            optimize: None,
            retention: None,
            reclaim: Some(reclaim),
        };
        self.invalidate_cached_snapshot(principal, workspace);
        Ok(report)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
fn build_snapshot(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> Result<StorageSnapshot> {
    let defaults = magician::config::LlmTraceRetentionSettings::default();
    build_snapshot_with_restricted_retention(
        layout,
        principal,
        workspace,
        Some((
            defaults.sanitized_io_days,
            defaults.context_metadata_days,
            defaults.facts_days,
        )),
    )
}

fn cataloged_database_path(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    owner: DatabaseOwner,
) -> PathBuf {
    if owner.is_host() {
        host_database_path(layout.base_root(), owner)
    } else {
        database_file_path(layout, principal, workspace, owner)
    }
}

fn build_snapshot_with_restricted_retention(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    restricted_retention: Option<(u32, u32, u32)>,
) -> Result<StorageSnapshot> {
    let scope_root = layout.scope_root(principal, workspace);
    let mut entries = Vec::new();
    for (id, label, path, safety, policy, actions) in [
        (
            "analytics_duckdb",
            "Analytics catalog",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::AnalyticsDuckdb),
            StorageSafetyClass::Regenerable,
            "Hot query catalog; losslessly rebuilt from its Parquet mirror when necessary.",
            vec![compact_action("analytics")],
        ),
        (
            "channel_assist_duckdb",
            "Comms intelligence",
            cataloged_database_path(
                layout,
                principal,
                workspace,
                DatabaseOwner::ChannelAssistDuckdb,
            ),
            StorageSafetyClass::LifecycleManaged,
            "No age deletion. Provider reconciliation and user lifecycle state remain authoritative.",
            vec![compact_action("channel_assist")],
        ),
        (
            "ui_threads_duckdb",
            "Thread index",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::UiThreadsDuckdb),
            StorageSafetyClass::Authoritative,
            "Soft-delete tombstones preserve thread identity; physical compaction is lossless.",
            vec![compact_action("ui_threads")],
        ),
        (
            "feed_duckdb",
            "Today & feed projection",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::FeedDuckdb),
            StorageSafetyClass::Regenerable,
            "Derived read model. Orphan rows can be purged from authoritative task and attention sources.",
            vec![StorageActionDescriptor {
                id: "purge_feed_orphans".to_string(),
                label: "Purge stale projections".to_string(),
                description: "Remove feed rows whose authoritative task or attention source no longer exists.".to_string(),
                confirmation: None,
                destructive: true,
            }],
        ),
    ] {
        entries.push(database_entry(
            &scope_root,
            id,
            label,
            &path,
            StorageKind::DuckDb,
            safety,
            None,
            policy,
            actions,
        )?);
    }

    entries.push(database_entry(
        &scope_root,
        "browser_engine_usage_sqlite",
        "Browser engine activity",
        &cataloged_database_path(
            layout,
            principal,
            workspace,
            DatabaseOwner::BrowserEngineUsage,
        ),
        StorageKind::Sqlite,
        StorageSafetyClass::Observability,
        Some(90),
        "Content-free command-attempt facts retain sanitized HTTP(S) origin/path only and are bounded to 90 days and 50,000 rows per scope.",
        Vec::new(),
    )?);

    entries.push(database_entry(
        layout.base_root(),
        "attention_learning_sqlite",
        "Attention learning",
        &cataloged_database_path(
            layout,
            principal,
            workspace,
            DatabaseOwner::AttentionLearning,
        ),
        StorageKind::Sqlite,
        StorageSafetyClass::LifecycleManaged,
        None,
        "Shared physical learning ledger with scope-owned evidence. Online optimization preserves every row; history cleanup is previewed and applied only to the selected scope; disk reclamation drains the store and verifies an exact rebuild before replacement.",
        vec![
            StorageActionDescriptor {
                id: "optimize_attention_learning".to_string(),
                label: "Optimize now".to_string(),
                description: "Refresh SQLite planner statistics online without deleting history or shrinking the file.".to_string(),
                confirmation: Some("OPTIMIZE ATTENTION".to_string()),
                destructive: false,
            },
            StorageActionDescriptor {
                id: "retain_attention_learning".to_string(),
                label: "Clean old history".to_string(),
                description: "Preview and then remove old analytical learning rows for only this principal/workspace while preserving active evidence and current models.".to_string(),
                confirmation: Some("CLEAN ATTENTION HISTORY".to_string()),
                destructive: true,
            },
            StorageActionDescriptor {
                id: "reclaim_attention_learning".to_string(),
                label: "Reclaim disk space".to_string(),
                description: "Hold attention writes while rebuilding and verifying every row; briefly drain reads only for the final atomic replacement with rollback protection.".to_string(),
                confirmation: Some("RECLAIM ATTENTION DATABASE".to_string()),
                destructive: false,
            },
        ],
    )?);

    let app_store_path =
        cataloged_database_path(layout, principal, workspace, DatabaseOwner::AppStoreSqlite);
    validate_inventory_path_components(layout.base_root(), &app_store_path)?;
    entries.push(database_entry(
        &scope_root,
        "app_store_sqlite",
        "App store",
        &app_store_path,
        StorageKind::Sqlite,
        StorageSafetyClass::Authoritative,
        None,
        APP_STORE_INVENTORY_POLICY,
        if app_store_path.try_exists()? {
            app_store_maintenance_actions()
        } else {
            Vec::new()
        },
    )?);

    for (id, label, path, safety, policy) in [
        (
            "app_packages",
            "App packages",
            layout.app_packages_root(principal, workspace),
            StorageSafetyClass::LifecycleManaged,
            "Immutable package and cache bytes are lifecycle-owned by the future app registry; generic storage maintenance cannot delete or compact them.",
        ),
        (
            "app_attachments",
            "App attachments",
            layout.app_attachments_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "Retained app attachments may contain personal data and are deleted only through the future app retention and purge settlement.",
        ),
        (
            "app_exports",
            "App exports",
            layout.app_exports_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "App export archives remain inspect-only until the archive, encryption and purge owners implement their load-bearing lifecycle.",
        ),
        (
            "app_captures",
            "App captures",
            layout.app_captures_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "Prompt, debug and provider captures remain inspect-only and may be removed only by their future data-policy and retention owner.",
        ),
        (
            "app_evaluations",
            "App evaluations",
            layout.app_evaluations_root(principal, workspace),
            StorageSafetyClass::Restricted,
            "Evaluation artifacts retain their source labels and follow the future app data-policy and purge settlement; generic cleanup is forbidden.",
        ),
    ] {
        validate_inventory_path_components(layout.base_root(), &path)?;
        entries.push(app_directory_entry(
            &scope_root,
            id,
            label,
            &path,
            StorageKind::Directory,
            safety,
            None,
            policy,
            Vec::new(),
        )?);
    }

    // The retention column carries the window the policy prose already states.
    // Leaving it `None` while the prose says "bounded to 30 days" makes the
    // typed field and the human field disagree, and only one of them is what a
    // caller filters on.
    for (id, label, path, retention_days, policy, actions) in [
        (
            "attention_funnel_sqlite",
            "Attention funnel",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::AttentionFunnel),
            Some(30),
            "Automatically bounded to 30 days and 50,000 events per scope; clearing it would weaken durable dedupe.",
            Vec::new(),
        ),
        (
            "resurfacing_sqlite",
            "Resurfacing state",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::Resurfacing),
            Some(90),
            "Automatically bounded to 90 days with per-scope candidate and feedback caps.",
            Vec::new(),
        ),
        (
            "social_sqlite",
            "Social Feed",
            cataloged_database_path(layout, principal, workspace, DatabaseOwner::SocialSqlite),
            None,
            "Scope-isolated agent social network data.",
            vec![compact_action("social")],
        ),
    ] {
        entries.push(database_entry(
            layout.base_root(),
            id,
            label,
            &path,
            StorageKind::Sqlite,
            StorageSafetyClass::LifecycleManaged,
            retention_days,
            policy,
            actions,
        )?);
    }

    let analytics_root = layout.analytics_root(principal, workspace);
    for (id, label, relative, safety, retention, policy) in [
        ("events", "Runtime events", "events", StorageSafetyClass::Observability, Some(90), "Completed partitions compact automatically; 90-day telemetry retention."),
        ("memory_events", "Memory diagnostics", "memory_events", StorageSafetyClass::Observability, Some(90), "Recall/utility diagnostics; compacted and retained for 90 days."),
        ("activity_rows", "Activity spine", "activity_rows", StorageSafetyClass::Observability, Some(magician::magician_v2::analytics::parquet_maintenance::ACTIVITY_DETAIL_RETENTION_DAYS as u32), "One row per completed span. Hours fold after 2h and days after 48h; full rows are summarised into Activity rollups after 7 days rather than deleted."),
        ("activity_rollups", "Activity rollups", "activity_rollups", StorageSafetyClass::Observability, Some(magician::magician_v2::analytics::parquet_maintenance::ACTIVITY_ROLLUP_RETENTION_DAYS as u32), "Per-hour counts and latency percentiles that outlive the spans behind them. Written when spine detail expires; kept 13 months so last year's shape is still comparable."),
        ("llm_calls", "LLM calls", "llm_calls", StorageSafetyClass::Observability, Some(90), "Canonical facts retain verified fallback revisions; legacy batches compact; 90-day retention."),
        ("llm_embeddings", "Embedding batches", "llm_embeddings", StorageSafetyClass::Observability, Some(90), "Content-free local embedding telemetry; completed partitions compact and expire after 90 days."),
        ("llm_provider_attempts", "Provider attempts", "llm_provider_attempts", StorageSafetyClass::Observability, Some(90), "Canonical provider-attempt lineage with 90-day retention."),
        ("llm_tool_calls", "Tool-call lineage", "llm_tool_calls", StorageSafetyClass::Observability, Some(90), "Tool causality facts are now covered by governed 90-day retention."),
        ("llm_capture_gaps", "Capture gaps", "llm_capture_gaps", StorageSafetyClass::Observability, Some(90), "Capture-quality evidence with 90-day retention."),
        ("llm_dispatch", "LLM dispatch", "llm_dispatch", StorageSafetyClass::Observability, Some(90), "Queue timing telemetry; completed partitions compact and expire after 90 days."),
        ("llm_call_io", "Restricted LLM content", "llm_call_io", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.0), "Restricted payload content uses the live configured retention lifecycle."),
        ("llm_context_blocks", "LLM context lineage", "llm_context_blocks", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.1), "Restricted context provenance follows the live configured retention and tombstone lifecycle."),
        ("llm_content_tombstones", "Restricted-content tombstones", "llm_content_tombstones", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.2.max(value.0)), "Deletion evidence follows the stricter of fact and sanitized-content retention and is never handled by generic telemetry cleanup."),
        ("llm_content_access_audit", "Restricted-content access audit", "llm_content_access_audit", StorageSafetyClass::Restricted, restricted_retention.map(|value| value.2), "Every restricted reveal is durably audited and retained by the live restricted-fact policy."),
    ] {
        entries.push(directory_entry(
            &scope_root,
            id,
            label,
            &analytics_root.join(relative),
            StorageKind::Parquet,
            safety,
            retention,
            policy,
            // `activity_rollups` is deliberately absent: it is produced *by*
            // maintenance, one object per day, and has nothing to fold.
            if matches!(id, "events" | "memory_events" | "llm_calls" | "llm_embeddings" | "llm_dispatch" | "activity_rows") {
                vec![compact_parquet_action()]
            } else {
                Vec::new()
            },
        )?);
    }
    // The per-scope transport log — the NDJSON tail `/runtime`, `/events` and
    // `/attention` all stream from. It has been unregistered since it was
    // written, despite having already forced its own compactor to be rewritten
    // once, which is exactly the position a store should not be in: growth
    // nobody can see until it causes an incident.
    //
    // Not to be confused with the `events` row above. That one is
    // `<scope>/analytics/events`, a Parquet dataset. The similar name is a
    // trap, and this comment is here so the next reader does not conclude one
    // of them is a duplicate of the other and delete a row.
    //
    // The path is resolved by the writer's own rule rather than assembled from
    // `scope_root`. The two sanitizers disagree: `ArtifactV2Workspace` accepts
    // any segment that is not a traversal, while `transport_log` accepts only
    // `[A-Za-z0-9_-]` up to 128 characters and routes everything else to
    // `_quarantine/_quarantine`. A principal like `user.name` therefore has a
    // `scope_root` the transport log never writes to, and this row used to stat
    // that empty path and report a confident 0 bytes for a log that was in fact
    // growing somewhere else.
    entries.push(database_entry(
        // Displayed relative to the base root, not the scope root: for a scope
        // the transport log quarantines, the two are different directories and
        // only the base-relative form says which one the bytes are in.
        layout.base_root(),
        "transport_log_jsonl",
        "Runtime transport log",
        &magician::magician_v2::transport_log::scope_log_path(
            layout.base_root(),
            principal,
            workspace,
        ),
        StorageKind::Journal,
        StorageSafetyClass::Observability,
        // The compactor's floor is a rolling 24 hours, which is one day — the
        // finest granularity `retention_days` can express. Reporting `None`
        // here said "no retention policy" about the one store in this list with
        // the tightest one.
        Some(TRANSPORT_LOG_RETENTION_DAYS),
        "Append-only live-tail transport log, bounded by its own compactor to a 24-hour / 2,000-event window per scope. Generic Parquet retention does not touch it.",
        Vec::new(),
    )?);

    entries.push(directory_entry(
        &scope_root,
        "llm_trace_journal",
        "LLM recovery journal",
        &layout.analytics_llm_trace_journal_root(principal, workspace),
        StorageKind::Journal,
        StorageSafetyClass::Authoritative,
        None,
        "Append-before-materialize recovery state. It is never removed by generic retention.",
        Vec::new(),
    )?);
    entries.push(directory_entry(
        &scope_root,
        "llm_restricted_journal",
        "Restricted-content recovery journal",
        &layout.analytics_llm_restricted_journal_root(principal, workspace),
        StorageKind::Journal,
        StorageSafetyClass::Restricted,
        None,
        "Append-before-materialize restricted recovery state; committed payload segments are pruned only by their journal owner.",
        Vec::new(),
    )?);

    let compaction_metrics =
        magician::magician_v2::storage_governance::compaction_metrics::snapshot(
            layout, principal, workspace,
        )?;
    let metrics_path = magician::magician_v2::storage_governance::compaction_metrics::metrics_path(
        layout, principal, workspace,
    );
    entries.push(StorageEntry {
        id: "compaction_metrics".to_string(),
        label: "Compaction metrics".to_string(),
        kind: StorageKind::Journal,
        safety_class: StorageSafetyClass::Observability,
        relative_path: relative_display(&scope_root, &metrics_path),
        size_bytes: compaction_metrics.storage_bytes,
        allocated_bytes: compaction_metrics.allocated_bytes,
        wal_bytes: 0,
        shm_bytes: 0,
        file_count: usize::from(compaction_metrics.storage_bytes > 0),
        inventory_complete: true,
        row_count: Some(compaction_metrics.event_count as u64),
        oldest_partition: None,
        newest_partition: None,
        retention_days: None,
        policy: format!(
            "Content-free compaction history is capped at {} events and can be cleared independently.",
            compaction_metrics.retained_event_limit
        ),
        actions: vec![StorageActionDescriptor {
            id: "clear_compaction_metrics".to_string(),
            label: "Clear metrics".to_string(),
            description: "Clear only the bounded compaction-history ledger; no database, Parquet object, or recovery state is touched."
                .to_string(),
            confirmation: Some("CLEAR COMPACTION METRICS".to_string()),
            destructive: true,
        }],
    });

    let total_size_bytes = entries
        .iter()
        .map(|entry| {
            entry
                .size_bytes
                .saturating_add(entry.wal_bytes)
                .saturating_add(entry.shm_bytes)
        })
        .sum();
    let total_allocated_bytes = entries.iter().map(|entry| entry.allocated_bytes).sum();
    Ok(StorageSnapshot {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        generated_at_ms: Utc::now().timestamp_millis(),
        total_size_bytes,
        total_allocated_bytes,
        entries,
        compaction_metrics,
        safeguards: vec![
            "DuckDB files are copied, schema/row verified, then atomically swapped with rollback backup.".to_string(),
            "Parquet raw batches are deleted only after compacted row-count, checksum, manifest, and fsync verification.".to_string(),
            "Mail and comms lifecycle data is never age-truncated by storage maintenance.".to_string(),
            "Symlinked stores, partitions, and source objects are rejected rather than followed.".to_string(),
            "Compaction metrics are content-free, bounded to 256 events per scope, and clear independently of governed data.".to_string(),
        ],
    })
}

/// Everything `/storage`'s "Apply retention" action expires, in one place.
///
/// Two sweeps, because the datasets have two different lifecycles and only one
/// of them can be expressed as a day count.
///
/// `magician::magician_v2::analytics::parquet_maintenance::apply_retention` is a flat `remove_dir_all` past a
/// single window, and it deliberately excludes the activity spine: deleting
/// seven-day-old spans outright is exactly what the tiering exists to prevent.
/// So the button used to do nothing at all for `activity_rows` and
/// `activity_rollups`, while the registry advertised `retention_days` of 7 and
/// 396 on those rows — an operator pressing it would reasonably conclude the
/// spine had been tiered, and it had not.
///
/// It cannot be expressed as a parameter either: the handler clamps
/// `retention_days` to `30..=3650`, so the 7-day detail tier is unreachable
/// through it. The spine needs its own call, which is what this is — the same
/// shape `compact_parquet` already uses to drive `compact_activity_rows`.
fn apply_scope_retention(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    retention_days: u32,
) -> Result<magician::magician_v2::analytics::parquet_maintenance::RetentionStats> {
    let mut retention = magician::magician_v2::analytics::parquet_maintenance::apply_retention(
        layout,
        principal,
        workspace,
        retention_days,
    )?;
    let activity = magician::magician_v2::analytics::parquet_maintenance::apply_activity_retention(
        layout, principal, workspace,
    )?;
    retention.partitions_scanned += activity.partitions_scanned;
    retention.partitions_removed += activity.partitions_removed;
    retention.bytes_removed = retention
        .bytes_removed
        .saturating_add(activity.bytes_removed);
    Ok(retention)
}

fn compact_parquet_action() -> StorageActionDescriptor {
    StorageActionDescriptor {
        id: "compact_parquet".to_string(),
        label: "Compact analytics".to_string(),
        description: "Merge completed batch-owned partitions and roll the active canonical LLM generation forward without deleting immutable facts."
            .to_string(),
        confirmation: Some("COMPACT PARQUET".to_string()),
        destructive: false,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_storage_inventory_progresses_during_attention_maintenance() {
        use std::future::Future;
        use std::task::Poll;
        let temporary = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(temporary.path());
        let learning = AttentionLearningService::open(
            temporary.path(),
            magician::config::AttentionLearningConfig::default(),
        )
        .unwrap();
        let service = StorageGovernanceService::new(
            layout.clone(),
            MailAssistStore::open_workspace(layout.clone()).unwrap(),
            UiThreadStore::open_workspace(layout.clone()).unwrap(),
            Arc::new(
                magician::magician_v2::social::store::SocialStoreRegistry::new(layout.clone()),
            ),
        )
        .with_attention_learning(learning);
        // Inventory inspects opaque bytes; it must neither open nor initialize
        // an App database while another store's real write is waiting.
        let app_path =
            database_file_path(&layout, "owner", "default", DatabaseOwner::AppStoreSqlite);
        std::fs::create_dir_all(app_path.parent().unwrap()).unwrap();
        std::fs::write(&app_path, b"opaque app inventory fixture").unwrap();
        let blocker = rusqlite::Connection::open(host_database_path(
            temporary.path(),
            DatabaseOwner::AttentionLearning,
        ))
        .unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut maintenance =
            Box::pin(service.retain_attention_learning("owner", "default", 90, true));
        // Poll the real owner through its first suspension, while the external
        // fixture writer keeps attention retention from completing.
        let pending = std::future::poll_fn(|cx| {
            Poll::Ready(matches!(maintenance.as_mut().poll(cx), Poll::Pending))
        })
        .await;
        assert!(pending);
        assert!(service.maintenance_gate.try_lock().is_err());
        let observed = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            service.snapshot("owner", "default"),
        )
        .await;
        blocker.execute_batch("ROLLBACK").unwrap();
        let maintained = maintenance.await.unwrap();
        assert_eq!(
            maintained.operation,
            AttentionLearningMaintenanceOperation::RetentionApply
        );
        let snapshot = observed
            .expect("App inventory waited on unrelated attention maintenance")
            .unwrap();
        let app = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "app_store_sqlite")
            .unwrap();
        assert_eq!(app.size_bytes, b"opaque app inventory fixture".len() as u64);
        assert_eq!(app.actions.len(), 3);
        assert!(
            service
                .cached_snapshot("owner", "default")
                .unwrap()
                .is_none(),
            "completed maintenance still invalidates the snapshot cache"
        );
    }

    #[test]
    fn inventory_is_scope_bound_and_does_not_follow_symlinks() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let events = layout.analytics_root("owner", "default").join("events");
        let partition = events.join("dt=2026-07-01");
        std::fs::create_dir_all(&partition).expect("partition");
        std::fs::write(partition.join("batch_1.parquet"), vec![0_u8; 128]).expect("batch");
        std::fs::write(external.path().join("secret"), vec![0_u8; 4096]).expect("external");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), events.join("redirected")).expect("symlink");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let events = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "events")
            .unwrap();
        assert_eq!(events.size_bytes, 128);
        assert_eq!(events.file_count, 1);
        assert_eq!(events.oldest_partition.as_deref(), Some("2026-07-01"));
    }

    /// The transport-log row must stat the file the writer actually writes.
    ///
    /// `ArtifactV2Workspace`'s segment sanitizer accepts a `.` in a principal;
    /// `transport_log`'s does not, and routes that scope to
    /// `_quarantine/_quarantine`. The row was assembled from `scope_root`, so
    /// for any such scope it stat'd a path that does not exist and reported a
    /// confident zero for a log that was growing elsewhere.
    #[test]
    fn the_transport_log_row_follows_the_writers_own_scope_sanitizer() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        // A `.` is legal for the workspace layout and illegal for the log.
        let (principal, workspace) = ("user.name", "default");
        let written = magician::magician_v2::transport_log::scope_log_path(
            layout.base_root(),
            principal,
            workspace,
        );
        assert_ne!(
            written,
            layout.scope_root(principal, workspace).join("events.jsonl"),
            "this scope must be one the two sanitizers disagree about, or the test proves nothing"
        );
        std::fs::create_dir_all(written.parent().expect("parent")).expect("log dir");
        std::fs::write(&written, vec![b'x'; 512]).expect("transport log");

        let snapshot = build_snapshot(&layout, principal, workspace).expect("snapshot");
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "transport_log_jsonl")
            .expect("transport log row");

        assert_eq!(
            entry.size_bytes, 512,
            "the row must report the bytes at the path the writer uses"
        );
        assert_eq!(
            entry.wal_bytes, 0,
            "a journal has no write-ahead sidecar; nothing should be looked up"
        );
        assert_eq!(
            entry.retention_days,
            Some(TRANSPORT_LOG_RETENTION_DAYS),
            "the tightest retention window in the registry must not report as absent"
        );
    }

    /// `/storage`'s "Apply retention" must tier the spine, not skip it.
    ///
    /// The registry advertises `retention_days` of 7 and 396 on the two spine
    /// rows, but the action ran only `magician::magician_v2::analytics::parquet_maintenance::apply_retention`,
    /// which excludes the spine by design. Pressing the button therefore did
    /// nothing for the only two datasets whose retention an operator cannot
    /// express through it — `retention_days` is clamped to `30..=3650`, so the
    /// 7-day detail window is not reachable as a parameter.
    #[test]
    fn applying_retention_tiers_the_activity_spine() {
        use magician::magician_v2::analytics::activity_rows_sink::{write_rows, ActivityRow};

        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let detail_root = layout.analytics_activity_rows_root("owner", "default");
        let rollup_root = layout.analytics_activity_rollups_root("owner", "default");
        let today = Utc::now().date_naive();
        let expired =
            today - chrono::Duration::days(magician::magician_v2::analytics::parquet_maintenance::ACTIVITY_DETAIL_RETENTION_DAYS);

        let started_at_ms = expired
            .and_hms_opt(3, 0, 0)
            .expect("valid hour")
            .and_utc()
            .timestamp_millis();
        write_rows(
            &detail_root,
            &[ActivityRow {
                activity_id: "1".to_string(),
                parent_activity_id: None,
                root_activity_id: "1".to_string(),
                name: "unit_of_work".to_string(),
                target: "magician::storage_governance_test".to_string(),
                kind: "background".to_string(),
                workload_class: Some("ambient".to_string()),
                priority: None,
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                agent_id: None,
                thread_id: None,
                task_id: None,
                model: None,
                started_at_ms,
                duration_ms: 10,
                outcome: "success".to_string(),
                dt: expired.format("%Y-%m-%d").to_string(),
                hour: 3,
            }],
        );
        assert!(detail_root.join(format!("dt={expired}")).is_dir());

        // 90 days, the value the scheduled sweep uses and well inside the
        // handler's clamp. It says nothing about the spine's 7-day tier, which
        // is the point.
        let stats = apply_scope_retention(&layout, "owner", "default", 90).expect("retention");

        assert!(
            !detail_root.join(format!("dt={expired}")).exists(),
            "expired spine detail must be tiered away by the same action that expires everything else"
        );
        assert!(
            rollup_root.join(format!("dt={expired}")).is_dir(),
            "and it must be summarised into the rollup tier, never deleted outright"
        );
        assert!(
            stats.partitions_removed >= 1,
            "the action's report must account for the partition it removed"
        );
    }

    #[test]
    fn bounded_directory_inventory_reports_partial_results_at_its_entry_ceiling() {
        let temporary = tempfile::tempdir().expect("tempdir");
        std::fs::write(temporary.path().join("one"), b"one").expect("first file");
        std::fs::write(temporary.path().join("two"), b"two").expect("second file");

        let inventory = inventory_directory_with_limits(
            temporary.path(),
            Some(DirectoryInventoryLimits {
                max_entries: 1,
                max_depth: 8,
            }),
        )
        .expect("bounded inventory");

        assert!(!inventory.complete);
        assert_eq!(inventory.file_count, 1);
    }

    #[test]
    fn inventory_declares_mail_as_lifecycle_managed_without_retention_action() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let mail = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "channel_assist_duckdb")
            .expect("mail entry");
        assert_eq!(mail.safety_class, StorageSafetyClass::LifecycleManaged);
        assert_eq!(mail.actions.len(), 1);
        assert_eq!(mail.actions[0].id, "compact_channel_assist");
        assert!(mail.retention_days.is_none());
    }

    #[test]
    fn inventory_counts_sqlite_wal_and_live_restricted_retention() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        std::fs::write(
            layout.base_root().join("attention_funnel.db"),
            vec![0_u8; 10],
        )
        .expect("sqlite");
        std::fs::write(
            layout.base_root().join("attention_funnel.db-wal"),
            vec![0_u8; 7],
        )
        .expect("sqlite wal");

        let snapshot = build_snapshot_with_restricted_retention(
            &layout,
            "owner",
            "default",
            Some((14, 45, 120)),
        )
        .expect("snapshot");
        let sqlite = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "attention_funnel_sqlite")
            .expect("sqlite entry");
        assert_eq!(sqlite.size_bytes, 10);
        assert_eq!(sqlite.wal_bytes, 7);
        assert_eq!(sqlite.file_count, 2);
        assert!(snapshot.total_size_bytes >= 17);
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_call_io")
                .and_then(|entry| entry.retention_days),
            Some(14)
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_context_blocks")
                .and_then(|entry| entry.retention_days),
            Some(45)
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_content_access_audit")
                .and_then(|entry| entry.retention_days),
            Some(120)
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .find(|entry| entry.id == "llm_content_tombstones")
                .and_then(|entry| entry.retention_days),
            Some(120)
        );
    }

    #[test]
    fn inventory_includes_scope_owned_browser_engine_observability() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let database = layout
            .scope_root("owner", "default")
            .join("analytics/browser_engine_usage.sqlite3");
        std::fs::create_dir_all(database.parent().unwrap()).expect("analytics dir");
        std::fs::write(&database, vec![0_u8; 11]).expect("browser analytics");
        std::fs::write(
            database.with_file_name("browser_engine_usage.sqlite3-wal"),
            vec![0_u8; 7],
        )
        .expect("browser analytics wal");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "browser_engine_usage_sqlite")
            .expect("browser-engine inventory entry");
        assert_eq!(entry.safety_class, StorageSafetyClass::Observability);
        assert_eq!(
            entry.relative_path,
            "analytics/browser_engine_usage.sqlite3"
        );
        assert_eq!(entry.size_bytes, 11);
        assert_eq!(entry.wal_bytes, 7);
        assert!(entry.actions.is_empty());
    }

    #[test]
    fn dormant_app_inventory_is_scope_bound_inspect_only_and_does_not_materialize_storage() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let apps_root = layout.apps_root("owner", "default");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let expected = [
            (
                "app_store_sqlite",
                StorageKind::Sqlite,
                StorageSafetyClass::Authoritative,
                "apps/app_store.sqlite3",
            ),
            (
                "app_packages",
                StorageKind::Directory,
                StorageSafetyClass::LifecycleManaged,
                "apps/packages",
            ),
            (
                "app_attachments",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/attachments",
            ),
            (
                "app_exports",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/exports",
            ),
            (
                "app_captures",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/captures",
            ),
            (
                "app_evaluations",
                StorageKind::Directory,
                StorageSafetyClass::Restricted,
                "apps/evaluations",
            ),
        ];

        for (id, kind, safety, relative_path) in expected {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(entry.kind, kind, "kind for {id}");
            assert_eq!(entry.safety_class, safety, "safety class for {id}");
            assert_eq!(entry.relative_path, relative_path, "path for {id}");
            assert_eq!(entry.size_bytes, 0, "apparent size for {id}");
            assert_eq!(entry.allocated_bytes, 0, "allocated size for {id}");
            assert_eq!(entry.wal_bytes, 0, "WAL size for {id}");
            assert_eq!(entry.file_count, 0, "file count for {id}");
            assert!(entry.actions.is_empty(), "generic action exposed for {id}");
        }
        assert!(
            !apps_root.exists(),
            "read-only inventory must not create dormant app storage"
        );
    }

    #[test]
    fn dormant_app_inventory_counts_sqlite_wal_and_real_files_without_following_links() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let database = layout.app_store_db_path("owner", "default");
        let packages = layout.app_packages_root("owner", "default");
        std::fs::create_dir_all(&packages).expect("packages");
        std::fs::write(&database, vec![0_u8; 13]).expect("app database");
        std::fs::write(
            database.with_file_name("app_store.sqlite3-wal"),
            vec![0_u8; 5],
        )
        .expect("app database wal");
        std::fs::write(
            database.with_file_name("app_store.sqlite3-shm"),
            vec![0_u8; 7],
        )
        .expect("app database shared memory");
        std::fs::write(packages.join("package.bin"), vec![0_u8; 17]).expect("package");
        std::fs::write(external.path().join("secret"), vec![0_u8; 4096]).expect("external");
        #[cfg(unix)]
        std::os::unix::fs::symlink(external.path(), packages.join("redirected")).expect("symlink");

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        let database = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "app_store_sqlite")
            .expect("app store entry");
        assert_eq!(database.size_bytes, 13);
        assert_eq!(database.wal_bytes, 5);
        assert_eq!(database.shm_bytes, 7);
        assert_eq!(database.file_count, 3);
        assert_eq!(database.actions, app_store_maintenance_actions());
        assert!(database.policy.contains("SQLCipher"));
        let packages = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "app_packages")
            .expect("app packages entry");
        assert_eq!(packages.size_bytes, 17);
        assert_eq!(packages.file_count, 1);
        assert!(packages.inventory_complete);
        assert!(packages.actions.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn dormant_app_inventory_rejects_a_symlinked_storage_root() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let apps_root = layout.apps_root("owner", "default");
        std::fs::create_dir_all(&apps_root).expect("apps root");
        std::fs::write(external.path().join("secret"), vec![0_u8; 4096]).expect("external");
        std::os::unix::fs::symlink(
            external.path(),
            layout.app_attachments_root("owner", "default"),
        )
        .expect("symlink");

        let error = build_snapshot(&layout, "owner", "default").expect_err("symlink root");
        assert!(
            error
                .to_string()
                .contains("storage inventory path contains a symlink"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dormant_app_inventory_rejects_a_symlinked_apps_ancestor() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let external = tempfile::tempdir().expect("external");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let scope_root = layout.scope_root("owner", "default");
        std::fs::create_dir_all(&scope_root).expect("scope root");
        std::fs::create_dir_all(external.path().join("packages")).expect("external packages");
        std::fs::write(external.path().join("packages/secret"), vec![0_u8; 4096])
            .expect("external");
        std::os::unix::fs::symlink(external.path(), layout.apps_root("owner", "default"))
            .expect("apps symlink");

        let error = build_snapshot(&layout, "owner", "default").expect_err("ancestor symlink");
        assert!(
            error
                .to_string()
                .contains("storage inventory path contains a symlink"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn compaction_reports_persist_per_area_metrics_and_inventory_footprint() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temporary.path());
        let report = StorageMaintenanceReport {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            started_at_ms: 100,
            completed_at_ms: 125,
            duckdb: Vec::new(),
            parquet: Some(magician::magician_v2::analytics::parquet_maintenance::ParquetCompactionStats {
                partitions_compacted: 1,
                raw_files_compacted: 8,
                raw_files_pruned: 8,
                rows_compacted: 80,
                files_before: 9,
                files_after: 2,
                bytes_before: 4_096,
                bytes_after: 1_024,
                bytes_reclaimed: 3_072,
                areas: vec![magician::magician_v2::analytics::parquet_maintenance::ParquetCompactionAreaStats {
                    dataset: "events".to_string(),
                    partitions_compacted: 1,
                    files_before: 9,
                    files_after: 2,
                    raw_files_compacted: 8,
                    raw_files_pruned: 8,
                    rows_compacted: 80,
                    bytes_before: 4_096,
                    bytes_after: 1_024,
                    bytes_reclaimed: 3_072,
                    query_files_avoided: 7,
                }],
                ..Default::default()
            }),
            canonical_llm: None,
            retention: None,
        };
        record_compaction_metrics(
            &layout,
            magician::magician_v2::storage_governance::compaction_metrics::CompactionMetricTrigger::ScheduledFull,
            &report,
        );

        let snapshot = build_snapshot(&layout, "owner", "default").expect("snapshot");
        assert_eq!(snapshot.compaction_metrics.event_count, 1);
        assert_eq!(snapshot.compaction_metrics.total_files_compacted, 8);
        assert_eq!(snapshot.compaction_metrics.total_query_files_avoided, 7);
        assert_eq!(snapshot.compaction_metrics.total_bytes_reclaimed, 3_072);
        assert!(snapshot.compaction_metrics.storage_bytes > 0);
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == "compaction_metrics")
            .expect("metrics inventory entry");
        assert_eq!(entry.size_bytes, snapshot.compaction_metrics.storage_bytes);
        assert_eq!(entry.row_count, Some(1));
        assert_eq!(entry.actions[0].id, "clear_compaction_metrics");
    }
}

fn database_entry(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
) -> Result<StorageEntry> {
    let size_bytes = regular_file_size(path)?;
    let allocated_bytes = regular_file_allocated_size(path)?;
    // Only stores that actually have a write-ahead sidecar get one looked up.
    // A journal has none: it is a plain append-only file whose durable writer
    // stages through a uniquely-named temp and renames. Statting a `.wal` for
    // it was a guaranteed miss dressed up as a measurement.
    let wal = match kind {
        StorageKind::Sqlite => Some(sqlite_wal_path(path)),
        StorageKind::DuckDb => Some(companion_wal_path(path)),
        StorageKind::Journal | StorageKind::Parquet | StorageKind::Directory => None,
    };
    let wal_bytes = match wal.as_deref() {
        Some(wal) => regular_file_size(wal)?,
        None => 0,
    };
    let allocated_bytes = match wal.as_deref() {
        Some(wal) => allocated_bytes.saturating_add(regular_file_allocated_size(wal)?),
        None => allocated_bytes,
    };
    let shm = (kind == StorageKind::Sqlite).then(|| {
        let mut name = path.as_os_str().to_os_string();
        name.push("-shm");
        PathBuf::from(name)
    });
    let shm_bytes = match shm.as_deref() {
        Some(shm) => regular_file_size(shm)?,
        None => 0,
    };
    let allocated_bytes = match shm.as_deref() {
        Some(shm) => allocated_bytes.saturating_add(regular_file_allocated_size(shm)?),
        None => allocated_bytes,
    };
    // Live Channel/Feed connections are owned by their admission gates. An
    // inventory must not hold an untracked DuckDB handle across a file swap.
    let row_count = if kind == StorageKind::DuckDb
        && size_bytes > 0
        && !matches!(id, "channel_assist_duckdb" | "feed_duckdb")
    {
        duckdb_estimated_rows(path).unwrap_or(None)
    } else {
        None
    };
    Ok(StorageEntry {
        id: id.to_string(),
        label: label.to_string(),
        kind,
        safety_class,
        relative_path: relative_display(display_root, path),
        size_bytes,
        allocated_bytes,
        wal_bytes,
        shm_bytes,
        file_count: usize::from(size_bytes > 0)
            + usize::from(wal_bytes > 0)
            + usize::from(shm_bytes > 0),
        inventory_complete: true,
        row_count,
        oldest_partition: None,
        newest_partition: None,
        retention_days,
        policy: policy.to_string(),
        actions,
    })
}

fn directory_entry(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
) -> Result<StorageEntry> {
    let inventory = inventory_directory(path)?;
    directory_entry_from_inventory(
        display_root,
        id,
        label,
        path,
        kind,
        safety_class,
        retention_days,
        policy,
        actions,
        inventory,
    )
}

fn app_directory_entry(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
) -> Result<StorageEntry> {
    let inventory = inventory_directory_with_limits(
        path,
        Some(DirectoryInventoryLimits {
            max_entries: APP_DIRECTORY_INVENTORY_MAX_ENTRIES,
            max_depth: APP_DIRECTORY_INVENTORY_MAX_DEPTH,
        }),
    )?;
    directory_entry_from_inventory(
        display_root,
        id,
        label,
        path,
        kind,
        safety_class,
        retention_days,
        policy,
        actions,
        inventory,
    )
}

#[allow(clippy::too_many_arguments)]
fn directory_entry_from_inventory(
    display_root: &Path,
    id: &str,
    label: &str,
    path: &Path,
    kind: StorageKind,
    safety_class: StorageSafetyClass,
    retention_days: Option<u32>,
    policy: &str,
    actions: Vec<StorageActionDescriptor>,
    inventory: DirectoryInventory,
) -> Result<StorageEntry> {
    Ok(StorageEntry {
        id: id.to_string(),
        label: label.to_string(),
        kind,
        safety_class,
        relative_path: relative_display(display_root, path),
        size_bytes: inventory.size_bytes,
        allocated_bytes: inventory.allocated_bytes,
        wal_bytes: 0,
        shm_bytes: 0,
        file_count: inventory.file_count,
        inventory_complete: inventory.complete,
        row_count: None,
        oldest_partition: inventory.partitions.first().cloned(),
        newest_partition: inventory.partitions.last().cloned(),
        retention_days,
        policy: policy.to_string(),
        actions,
    })
}

struct DirectoryInventory {
    size_bytes: u64,
    allocated_bytes: u64,
    file_count: usize,
    partitions: Vec<String>,
    complete: bool,
}

impl Default for DirectoryInventory {
    fn default() -> Self {
        Self {
            size_bytes: 0,
            allocated_bytes: 0,
            file_count: 0,
            partitions: Vec::new(),
            complete: true,
        }
    }
}

#[derive(Clone, Copy)]
struct DirectoryInventoryLimits {
    max_entries: usize,
    max_depth: usize,
}

fn inventory_directory(root: &Path) -> Result<DirectoryInventory> {
    inventory_directory_with_limits(
        root,
        Some(DirectoryInventoryLimits {
            max_entries: DIRECTORY_INVENTORY_MAX_ENTRIES,
            max_depth: DIRECTORY_INVENTORY_MAX_DEPTH,
        }),
    )
}

fn inventory_directory_with_limits(
    root: &Path,
    limits: Option<DirectoryInventoryLimits>,
) -> Result<DirectoryInventory> {
    let mut inventory = DirectoryInventory::default();
    let metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(inventory),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_dir() {
        return Err(anyhow!(
            "storage inventory root is not a real directory: {}",
            root.display()
        ));
    }
    let mut visited_entries = 0usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    'inventory: while let Some((directory, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&directory)? {
            visited_entries = visited_entries.saturating_add(1);
            if limits.is_some_and(|limits| visited_entries > limits.max_entries) {
                inventory.complete = false;
                break 'inventory;
            }
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("dt=") && name.len() == 13)
                {
                    inventory.partitions.push(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or_default()
                            .trim_start_matches("dt=")
                            .to_string(),
                    );
                }
                if limits.is_some_and(|limits| depth >= limits.max_depth) {
                    inventory.complete = false;
                } else {
                    stack.push((path, depth.saturating_add(1)));
                }
            } else if file_type.is_file() {
                let metadata = entry.metadata()?;
                inventory.size_bytes = inventory.size_bytes.saturating_add(metadata.len());
                inventory.allocated_bytes = inventory
                    .allocated_bytes
                    .saturating_add(allocated_bytes(&metadata));
                inventory.file_count += 1;
            }
        }
    }
    inventory.partitions.sort();
    inventory.partitions.dedup();
    Ok(inventory)
}

fn duckdb_estimated_rows(path: &Path) -> Result<Option<u64>> {
    let config = DuckDbConfig::default()
        .access_mode(AccessMode::ReadOnly)
        .context("configuring read-only storage inventory")?;
    let connection = match Connection::open_with_flags(path, config) {
        Ok(connection) => connection,
        Err(_) => return Ok(None),
    };
    let rows: i64 = connection
        .query_row(
            "SELECT CAST(coalesce(sum(estimated_size), 0) AS BIGINT) FROM duckdb_tables() WHERE NOT internal",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(Some(u64::try_from(rows).unwrap_or(0)))
}

fn regular_file_size(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata.len()),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn regular_file_allocated_size(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(allocated_bytes(&metadata)),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated_bytes(metadata: &std::fs::Metadata) -> u64 {
    metadata.len()
}

fn companion_wal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    path.with_file_name(format!("{name}.wal"))
}

fn sqlite_wal_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("store.db");
    path.with_file_name(format!("{name}-wal"))
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn validate_inventory_path_components(base_root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(base_root).map_err(|_| {
        anyhow!(
            "storage inventory path escapes its trusted root: {}",
            path.display()
        )
    })?;
    let mut current = base_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(segment) = component else {
            return Err(anyhow!(
                "storage inventory path has a non-normal component: {}",
                path.display()
            ));
        };
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(anyhow!(
                    "storage inventory path contains a symlink: {}",
                    current.display()
                ));
            },
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn compact_action(target: &str) -> StorageActionDescriptor {
    StorageActionDescriptor {
        id: format!("compact_{target}"),
        label: "Compact safely".to_string(),
        description: "Copy into a fresh database, verify every table/schema/index/view, then atomically swap.".to_string(),
        confirmation: Some("COMPACT DATABASE".to_string()),
        destructive: false,
    }
}
