//! Scheduled owner-mediated DuckDB maintenance. No database work is on the
//! HTTP startup barrier; only one database is compacted at a time.
use super::*;
use magician::magician_v2::{
    feed::FeedStore, storage_governance::database_maintenance::DatabaseMaintenanceConfig,
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseMaintenanceStatus {
    pub database: String,
    pub run_id: String,
    pub state: String,
    pub message: String,
    pub updated_at_ms: i64,
    pub last_checked_at_ms: i64,
    pub last_success_at_ms: Option<i64>,
    pub pending_since_ms: Option<i64>,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub duration_ms: u64,
}

fn path(layout: &ArtifactV2Workspace, principal: &str, workspace: &str, database: &str) -> PathBuf {
    let owner = if database == "channel_assist" {
        DatabaseOwner::ChannelAssistDuckdb
    } else {
        DatabaseOwner::FeedDuckdb
    };
    let db = database_file_path(layout, principal, workspace, owner);
    db.with_extension("automatic-maintenance.json")
}

fn read_status(
    layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    database: &str,
) -> Result<DatabaseMaintenanceStatus> {
    let path = path(layout, principal, workspace, database);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file() && metadata.len() <= 16384,
                "invalid maintenance status file"
            );
            let bytes = std::fs::read(path)?;
            match serde_json::from_slice::<DatabaseMaintenanceStatus>(&bytes) {
                Ok(status)
                    if status.database == database
                        && matches!(
                            status.state.as_str(),
                            "idle" | "running" | "completed" | "deferred" | "failed" | "disabled"
                        ) =>
                {
                    Ok(status)
                },
                _ => Ok(DatabaseMaintenanceStatus {
                    database: database.to_owned(),
                    state: "failed".to_owned(),
                    message:
                        "Maintenance status could not be read and will be rebuilt automatically."
                            .to_owned(),
                    ..Default::default()
                }),
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(DatabaseMaintenanceStatus {
                database: database.to_owned(),
                state: "idle".to_owned(),
                message: "Automatic storage maintenance is ready.".to_owned(),
                ..Default::default()
            })
        },
        Err(error) => Err(error.into()),
    }
}

pub struct AutomaticDatabaseMaintenance {
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}
impl AutomaticDatabaseMaintenance {
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}
impl Drop for AutomaticDatabaseMaintenance {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl StorageGovernanceService {
    pub fn with_feed_store(mut self, store: FeedStore) -> Self {
        self.feed_store = Some(store);
        self
    }

    pub async fn database_maintenance_status(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<DatabaseMaintenanceStatus>> {
        let run_id = self.maintenance_run_id.clone();
        let enabled = self
            .maintenance_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        let layout = self.workspace_layout.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            ["channel_assist", "feed"]
                .into_iter()
                .map(|database| {
                    let mut status = read_status(&layout, &principal, &workspace, database)?;
                    // A stale persisted running state must not masquerade as a live
                    // operation after restart. The next sweep will retry it.
                    if !enabled {
                        status.state = "disabled".to_owned();
                        status.message = "Automatic storage maintenance is disabled.".to_owned();
                    } else if status.state == "running" && status.run_id != run_id {
                        status.state = "deferred".to_owned();
                        status.message = "Interrupted storage maintenance will retry.".to_owned();
                    }
                    Ok(status)
                })
                .collect()
        })
        .await?
    }

    pub fn spawn_database_maintenance(
        &self,
        config: DatabaseMaintenanceConfig,
    ) -> Result<AutomaticDatabaseMaintenance> {
        config.validate()?;
        self.maintenance_enabled
            .store(config.enabled, std::sync::atomic::Ordering::Relaxed);
        let cancel = CancellationToken::new();
        let cancelled = cancel.clone();
        let service = self.clone();
        let handle = tokio::spawn(async move {
            if !config.enabled {
                return;
            }
            tokio::select! { _ = cancelled.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(config.startup_delay_seconds)) => {} }
            loop {
                let layout = service.workspace_layout.clone();
                if let Ok(Ok(scopes)) =
                    tokio::task::spawn_blocking(move || layout.list_scope_segments_sync()).await
                {
                    for (principal, workspace) in scopes {
                        for database in ["channel_assist", "feed"] {
                            if cancelled.is_cancelled() {
                                return;
                            }
                            if let Err(error) = service
                                .maintain_database(&principal, &workspace, database, &config)
                                .await
                            {
                                tracing::warn!(%principal, %workspace, database, %error, "automatic database maintenance check failed");
                            }
                        }
                    }
                }
                tokio::select! { _ = cancelled.cancelled() => return, _ = tokio::time::sleep(Duration::from_secs(60)) => {} }
            }
        });
        Ok(AutomaticDatabaseMaintenance {
            cancel,
            handle: Some(handle),
        })
    }

    async fn maintain_database(
        &self,
        principal: &str,
        workspace: &str,
        database: &str,
        config: &DatabaseMaintenanceConfig,
    ) -> Result<()> {
        let layout = self.workspace_layout.clone();
        let p = principal.to_owned();
        let w = workspace.to_owned();
        let d = database.to_owned();
        let (mut status, size) = tokio::task::spawn_blocking(move || {
            let owner = if d == "channel_assist" {
                DatabaseOwner::ChannelAssistDuckdb
            } else {
                DatabaseOwner::FeedDuckdb
            };
            let db = database_file_path(&layout, &p, &w, owner);
            let size = match std::fs::metadata(db) {
                Ok(m) => m.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            };
            Ok::<_, anyhow::Error>((read_status(&layout, &p, &w, &d)?, size))
        })
        .await??;
        if size < config.min_database_mib * 1024 * 1024 {
            if matches!(status.state.as_str(), "running" | "failed" | "deferred") {
                status.state = "idle".to_owned();
                status.message = "Storage is below the maintenance size threshold.".to_owned();
                status.pending_since_ms = None;
                return self
                    .persist_maintenance_status(principal, workspace, status)
                    .await;
            }
            return Ok(());
        }
        let now = Utc::now().timestamp_millis();
        let interval = if status.pending_since_ms.is_some() && status.state != "failed" {
            60
        } else {
            config.check_interval_seconds
        };
        if !(status.state == "running" && status.run_id != self.maintenance_run_id)
            && now.saturating_sub(status.last_checked_at_ms) < (interval * 1000) as i64
        {
            return Ok(());
        }
        // Share the manual maintenance gate. Taking it here also ensures that
        // no second database consumes a repair budget while this one is active.
        let Ok(_serial) = self.maintenance_gate.try_lock() else {
            return Ok(());
        };
        let inspection = if database == "channel_assist" {
            self.mail_store
                .inspect_maintenance(principal, workspace)
                .await
        } else if let Some(feed) = self.feed_store.as_ref() {
            feed.inspect_maintenance(principal, workspace).await
        } else {
            return Ok(());
        };
        status.last_checked_at_ms = now;
        let info = match inspection {
            Ok(info) => info,
            Err(error) => {
                status.state = "failed".to_owned();
                status.message = if requires_restart(&error) {
                    "Storage needs recovery. Restart the service to retry."
                } else {
                    "Storage inspection could not finish and will retry automatically."
                }
                .to_owned();
                tracing::warn!(%principal, %workspace, database, %error, "Storage inspection failed");
                return self
                    .persist_maintenance_status(principal, workspace, status)
                    .await;
            },
        };
        if !should_compact(&info, &status, now, config) {
            status.pending_since_ms = None;
            if status.state != "completed" {
                status.state = "idle".to_owned();
                status.message = "Storage layout is healthy.".to_owned();
            }
            return self
                .persist_maintenance_status(principal, workspace, status)
                .await;
        }
        let pending = *status.pending_since_ms.get_or_insert(now);
        let wait = drain_wait(pending, now, config);
        let span = tracing::info_span!(target: "magician::storage_maintenance", "Database maintenance",
            activity_kind = "background", workload_class = "system", principal, workspace,
            operation = "database_compaction", database, activity_outcome = tracing::field::Empty);
        async {
            status.state = "running".to_owned();
            status.message = "Optimizing storage. Related requests may briefly wait.".to_owned();
            self.persist_maintenance_status(principal, workspace, status.clone()).await?;
            tracing::info!(target: "magician::storage_maintenance", "Optimizing storage; related requests may briefly wait");
            let started = Utc::now().timestamp_millis();
            let elapsed = Instant::now();
            let result = if database == "channel_assist" { self.mail_store.compact_scope_with_wait(principal, workspace, wait).await }
                else { self.feed_store.as_ref().expect("checked feed owner").compact_scope_with_wait(principal, workspace, wait).await };
            status.duration_ms = elapsed.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
            match result {
                Ok(report) => {
                    status.state = "completed".to_owned(); status.message = "Storage optimization completed.".to_owned();
                    status.last_success_at_ms = Some(Utc::now().timestamp_millis()); status.pending_since_ms = None;
                    status.bytes_after = report.bytes_after; status.bytes_reclaimed = report.bytes_reclaimed;
                    record_compaction_metrics(&self.workspace_layout,
                        magician::magician_v2::storage_governance::compaction_metrics::CompactionMetricTrigger::ScheduledFull,
                        &StorageMaintenanceReport { principal: principal.to_owned(), workspace: workspace.to_owned(), started_at_ms: started,
                            completed_at_ms: Utc::now().timestamp_millis(), duckdb: vec![report], parquet: None, canonical_llm: None, retention: None });
                    tracing::Span::current().record("activity_outcome", "success");
                    tracing::info!(target: "magician::storage_maintenance", bytes_reclaimed = status.bytes_reclaimed, duration_ms = status.duration_ms, "Storage optimization completed");
                },
                Err(error) => {
                    let deferred = error.to_string().contains("deferred") || error.to_string().contains("already running");
                    status.state = if deferred { "deferred" } else { "failed" }.to_owned();
                    status.message = if requires_restart(&error) { "Storage needs recovery. Restart the service to retry." } else if deferred { "Storage is busy. Optimization will retry automatically." } else { "Storage optimization could not finish and will retry automatically." }.to_owned();
                    tracing::Span::current().record("activity_outcome", if deferred { "cancelled" } else { "error" });
                    tracing::warn!(target: "magician::storage_maintenance", %error, "Storage optimization deferred or failed; retry scheduled");
                }
            }
            self.invalidate_cached_snapshot(principal, workspace);
            self.persist_maintenance_status(principal, workspace, status).await
        }.instrument(span).await
    }

    async fn persist_maintenance_status(
        &self,
        principal: &str,
        workspace: &str,
        mut status: DatabaseMaintenanceStatus,
    ) -> Result<()> {
        let layout = self.workspace_layout.clone();
        let p = principal.to_owned();
        let w = workspace.to_owned();
        status.updated_at_ms = Utc::now().timestamp_millis();
        status.run_id = self.maintenance_run_id.clone();
        tokio::task::spawn_blocking(move || {
            layout.write_json_atomic_path_sync(path(&layout, &p, &w, &status.database), &status)
        })
        .await??;
        Ok(())
    }
}

fn should_compact(
    info: &magician::magician_v2::storage_governance::database_maintenance::Fragmentation,
    status: &DatabaseMaintenanceStatus,
    now: i64,
    config: &DatabaseMaintenanceConfig,
) -> bool {
    let growth = status.bytes_after > 0 && info.bytes > status.bytes_after.saturating_mul(3);
    let urgent = info.max_row_groups >= config.fragmented_row_groups.saturating_mul(4);
    let cooling = status
        .last_success_at_ms
        .is_some_and(|last| now.saturating_sub(last) < (config.cooldown_seconds * 1000) as i64);
    (info.fragmented || growth) && (!cooling || urgent)
}

fn drain_wait(pending: i64, now: i64, config: &DatabaseMaintenanceConfig) -> Duration {
    if now.saturating_sub(pending) >= (config.max_deferral_seconds * 1000) as i64 {
        Duration::from_secs(30)
    } else {
        Duration::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::storage_governance::database_maintenance::Fragmentation;

    #[test]
    fn maintenance_policy_cools_down_but_repairs_severe_fragmentation_and_eventually_drains() {
        let config = DatabaseMaintenanceConfig::default();
        let mut info = Fragmentation {
            bytes: 100_000_000,
            max_row_groups: 128,
            fragmented: true,
        };
        let mut status = DatabaseMaintenanceStatus::default();
        assert!(should_compact(&info, &status, 1000, &config));
        status.last_success_at_ms = Some(999);
        assert!(!should_compact(&info, &status, 1000, &config));
        info.max_row_groups = 512;
        assert!(should_compact(&info, &status, 1000, &config));
        info.fragmented = false;
        info.max_row_groups = 1;
        status.last_success_at_ms = None;
        status.bytes_after = 10_000_000;
        assert!(should_compact(&info, &status, 1000, &config));
        assert_eq!(drain_wait(1000, 1001, &config), Duration::ZERO);
        assert_eq!(
            drain_wait(
                1000,
                1000 + config.max_deferral_seconds as i64 * 1000,
                &config
            ),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn status_is_scope_bound_persistent_and_reconciles_restart_without_opening_databases() {
        let temp = tempfile::tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(temp.path());
        let service = StorageGovernanceService::new(
            layout.clone(),
            MailAssistStore::open_workspace(layout.clone()).unwrap(),
            UiThreadStore::open_workspace(layout.clone()).unwrap(),
            Arc::new(
                magician::magician_v2::social::store::SocialStoreRegistry::new(layout.clone()),
            ),
        );
        let status = DatabaseMaintenanceStatus {
            database: "channel_assist".to_owned(),
            state: "running".to_owned(),
            ..Default::default()
        };
        service
            .persist_maintenance_status("alpha", "prod", status)
            .await
            .unwrap();
        assert_eq!(
            service
                .database_maintenance_status("alpha", "prod")
                .await
                .unwrap()[0]
                .state,
            "running"
        );
        assert_eq!(
            service
                .database_maintenance_status("beta", "prod")
                .await
                .unwrap()[0]
                .state,
            "idle"
        );
        let mut restarted = service.clone();
        restarted.maintenance_run_id = "new-process".to_owned();
        assert_eq!(
            restarted
                .database_maintenance_status("alpha", "prod")
                .await
                .unwrap()[0]
                .state,
            "deferred"
        );
        let handle = service
            .spawn_database_maintenance(DatabaseMaintenanceConfig {
                enabled: false,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            service
                .database_maintenance_status("alpha", "prod")
                .await
                .unwrap()[0]
                .state,
            "disabled"
        );
        handle.shutdown().await;
        assert!(
            !database_file_path(&layout, "alpha", "prod", DatabaseOwner::ChannelAssistDuckdb)
                .exists()
        );
    }
}

fn requires_restart(error: &anyhow::Error) -> bool {
    error.is::<magician::magician_v2::storage_governance::duckdb_compaction::RecoveryRequired>()
        || error
            .chain()
            .any(|cause| cause.to_string().contains("requires recovery"))
}
