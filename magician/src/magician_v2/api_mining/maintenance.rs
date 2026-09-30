//! Deployment hardening: storage cleanup and performance logging
//!
//! Provides maintenance utilities for the API mining subsystem:
//! - **Storage cleanup**: Auto-prune traces older than 30 days and capabilities
//!   with 0 replays in 30 days.
//! - **Performance logging**: Track and compare API replay vs browser automation
//!   latency for observability dashboards.

use super::capability::ApiCapability;
use super::capability_store::CapabilityStore;
use super::router::extract_origin;
use super::trace_storage::TraceStorage;
use crate::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};
use crate::magician_v2::secrets::SecretStore;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

// ─────────────────────────── Configuration ────────────────────────────────

/// Default trace retention period in days
const TRACE_RETENTION_DAYS: u64 = 30;

/// Default capability staleness threshold in days
/// Capabilities with 0 replays in this period are pruned.
const CAPABILITY_STALE_DAYS: u64 = 30;

/// Maximum number of trace files per task/origin
const MAX_TRACE_FILES_PER_TASK: usize = 500;

// ─────────────────────────── Storage Cleanup ──────────────────────────────

/// Result of a storage cleanup run
#[derive(Debug, Default)]
pub struct CleanupReport {
    /// Number of trace files deleted
    pub traces_deleted: usize,
    /// Number of capabilities pruned
    pub capabilities_pruned: usize,
    /// Number of stale, unused capabilities newly hidden from default views.
    /// `capabilities_pruned` mirrors this during the compatibility window.
    pub capabilities_hidden: usize,
    /// Total disk space reclaimed in bytes (approximate)
    pub bytes_reclaimed: u64,
    /// Errors encountered during cleanup (non-fatal)
    pub errors: Vec<String>,
}

/// Result of purging one origin's mined artifacts and captured auth.
#[derive(Debug, Default, Serialize)]
pub struct OriginPurgeReport {
    pub capabilities_deleted: usize,
    pub recipes_deleted: usize,
    pub recipe_packs_deleted: usize,
    pub replay_grants_revoked: usize,
    pub projections_deleted: usize,
    pub projection_rows_deleted: usize,
    pub traces_deleted: usize,
    pub trace_files_rewritten: usize,
    pub trace_files_deleted: usize,
    pub captured_auth_cleared: bool,
}

/// Configuration for cleanup operations (mirrors relevant fields from ApiMiningConfig).
pub struct CleanupConfig {
    /// Days to retain raw trace data before cleanup.
    pub trace_retention_days: u64,
    /// Maximum trace files per task/origin.
    pub max_trace_files_per_task: usize,
    /// Days after which capabilities with 0 replays are pruned.
    pub capability_stale_days: u64,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            trace_retention_days: TRACE_RETENTION_DAYS,
            max_trace_files_per_task: MAX_TRACE_FILES_PER_TASK,
            capability_stale_days: CAPABILITY_STALE_DAYS,
        }
    }
}

/// Run storage cleanup: prune old traces and stale capabilities.
///
/// This is safe to call on every startup or periodically. It only deletes
/// files that match retention criteria, never active data.
pub fn run_cleanup(base_path: &Path) -> CleanupReport {
    run_cleanup_with_config(base_path, &CleanupConfig::default())
}

/// Run storage cleanup with explicit configuration values.
///
/// Prefer this over `run_cleanup` when `ApiMiningConfig` is available so that
/// user-configured retention values are respected instead of hardcoded defaults.
pub fn run_cleanup_with_config(base_path: &Path, config: &CleanupConfig) -> CleanupReport {
    let mut report = CleanupReport::default();

    // 1. Prune old trace files
    prune_old_traces(base_path, config.trace_retention_days, &mut report);

    // 2. Enforce per-task trace file limit
    enforce_trace_limits(base_path, config.max_trace_files_per_task, &mut report);

    // 3. Prune stale capabilities (0 replays in 30 days)
    prune_stale_capabilities(base_path, config.capability_stale_days, &mut report);

    if report.traces_deleted > 0 || report.capabilities_pruned > 0 {
        tracing::info!(
            "[API_MINING] Cleanup complete: {} traces deleted, {} capabilities pruned, ~{} bytes reclaimed",
            report.traces_deleted,
            report.capabilities_pruned,
            report.bytes_reclaimed,
        );
    }

    report
}

/// Remove all stored API-mining artifacts for a single origin.
///
/// This deletes learned capabilities, rewrites trace files to remove matching
/// events, and clears captured auth/session data from the shared `SecretStore`.
pub fn purge_origin_artifacts(
    base_path: &Path,
    secret_store: &SecretStore,
    origin: &str,
) -> Result<OriginPurgeReport, String> {
    let mut report = OriginPurgeReport::default();
    let store = CapabilityStore::with_base_path(base_path);
    report.capabilities_deleted = store.delete_origin(origin)?;

    let trace_storage = TraceStorage::with_base_path(base_path);
    let workspace_layout = api_mining_workspace(base_path);
    let trace_files = list_all_trace_files(&workspace_layout, base_path);
    for trace_file in trace_files {
        let traces = trace_storage.read_traces(&trace_file)?;
        let original = traces.len();
        let retained: Vec<_> = traces
            .into_iter()
            .filter(|trace| extract_origin(&trace.url) != origin)
            .collect();
        let deleted = original.saturating_sub(retained.len());
        if deleted == 0 {
            continue;
        }

        if retained.is_empty() {
            remove_provider_file(&workspace_layout, &trace_file)
                .map_err(|e| format!("Failed to delete trace file {:?}: {}", trace_file, e))?;
            report.trace_files_deleted += 1;
            report.traces_deleted += deleted;
            continue;
        }

        rewrite_trace_file(&workspace_layout, &trace_file, &retained)?;
        report.traces_deleted += deleted;
        report.trace_files_rewritten += 1;
    }

    report.captured_auth_cleared = secret_store.clear_captured(origin).unwrap_or(false);
    Ok(report)
}

/// Delete trace files older than `retention_days`.
fn prune_old_traces(base_path: &Path, retention_days: u64, report: &mut CleanupReport) {
    let cutoff = SystemTime::now() - Duration::from_secs(retention_days * 24 * 3600);
    let workspace_layout = api_mining_workspace(base_path);

    // Walk all subdirectories looking for trace_*.jsonl files
    let Ok(entries) = workspace_layout.read_dir_path_sync(base_path) else {
        return;
    };

    for entry in entries {
        let path = entry_path(base_path, &entry);
        if !entry.is_dir {
            continue;
        }

        // Check for trace files directly in this directory (task directories)
        let Ok(files) = workspace_layout.read_dir_path_sync(&path) else {
            continue;
        };

        for file in files {
            if !file.is_file {
                continue;
            }
            let fpath = entry_path(base_path, &file);
            if !is_trace_file(&fpath) {
                continue;
            }

            if let Ok(Some(meta)) = workspace_layout.metadata_path_sync(&fpath) {
                let modified = match meta.modified() {
                    Ok(t) => t,
                    Err(_) => continue, // Skip files whose mtime can't be read
                };
                if modified < cutoff {
                    let size = meta.len();
                    if let Err(e) = remove_provider_file(&workspace_layout, &fpath) {
                        report
                            .errors
                            .push(format!("Failed to delete {:?}: {}", fpath, e));
                    } else {
                        report.traces_deleted += 1;
                        report.bytes_reclaimed += size;
                    }
                }
            }
        }

        // Also check traces/ subdirectory
        let traces_dir = path.join("traces");
        if path_exists(&workspace_layout, &traces_dir) {
            let Ok(trace_files) = workspace_layout.read_dir_path_sync(&traces_dir) else {
                continue;
            };
            for file in trace_files {
                if !file.is_file {
                    continue;
                }
                let fpath = entry_path(base_path, &file);
                if !is_trace_file(&fpath) {
                    continue;
                }

                if let Ok(Some(meta)) = workspace_layout.metadata_path_sync(&fpath) {
                    let modified = match meta.modified() {
                        Ok(t) => t,
                        Err(_) => continue, // Skip files whose mtime can't be read
                    };
                    if modified < cutoff {
                        let size = meta.len();
                        if let Err(e) = remove_provider_file(&workspace_layout, &fpath) {
                            report
                                .errors
                                .push(format!("Failed to delete {:?}: {}", fpath, e));
                        } else {
                            report.traces_deleted += 1;
                            report.bytes_reclaimed += size;
                        }
                    }
                }
            }
        }
    }
}

/// Enforce the per-task trace file limit by deleting oldest files.
fn enforce_trace_limits(base_path: &Path, max_files: usize, report: &mut CleanupReport) {
    let workspace_layout = api_mining_workspace(base_path);
    let Ok(entries) = workspace_layout.read_dir_path_sync(base_path) else {
        return;
    };

    for entry in entries {
        let path = entry_path(base_path, &entry);
        if !entry.is_dir {
            continue;
        }

        // Collect trace files from this directory
        let mut trace_files = collect_trace_files(&workspace_layout, base_path, &path);

        if trace_files.len() <= max_files {
            continue;
        }

        // Sort by modified time (oldest first)
        trace_files.sort_by_key(|f| {
            workspace_layout
                .metadata_path_sync(f)
                .map_err(|e| std::io::Error::other(e.to_string()))
                .and_then(|metadata| {
                    metadata.ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::NotFound, "missing trace metadata")
                    })
                })
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });

        // Delete oldest files until we're under the limit
        let to_delete = trace_files.len() - max_files;
        for fpath in trace_files.iter().take(to_delete) {
            let size = workspace_layout
                .metadata_path_sync(fpath)
                .ok()
                .flatten()
                .map(|m| m.len())
                .unwrap_or(0);
            if let Err(e) = remove_provider_file(&workspace_layout, fpath) {
                report
                    .errors
                    .push(format!("Failed to delete {:?}: {}", fpath, e));
            } else {
                report.traces_deleted += 1;
                report.bytes_reclaimed += size;
            }
        }
    }
}

/// Hide capabilities no recipe has used and that have not changed recently.
/// Records are retained: a later recipe link makes them visible again.
fn prune_stale_capabilities(base_path: &Path, stale_days: u64, report: &mut CleanupReport) {
    let store = CapabilityStore::with_base_path(base_path);
    let stale_secs = i64::try_from(stale_days.saturating_mul(24 * 60 * 60)).unwrap_or(i64::MAX);

    let Ok(origins) = store.list_origins() else {
        return;
    };

    for origin_key in &origins {
        // Reconstruct origin URL from key (best effort)
        let origin_url = origin_key.replace("___", "://").replace('_', "/");

        let Ok(capabilities) = store.load_all(&origin_url) else {
            // Try with the key directly
            let Ok(capabilities) = store.load_all(origin_key) else {
                continue;
            };
            prune_capabilities_for_origin(&store, origin_key, &capabilities, stale_secs, report);
            continue;
        };

        prune_capabilities_for_origin(&store, &origin_url, &capabilities, stale_secs, report);
    }
    if let Ok(mut registry) = super::registry::CapabilityRegistry::with_base_path(base_path) {
        if let Err(error) = registry.rebuild() {
            report.errors.push(format!(
                "Failed to rebuild capability index after hiding: {error}"
            ));
        }
    }
}

/// Hide stale capabilities for a single origin without deleting evidence.
fn prune_capabilities_for_origin(
    store: &CapabilityStore,
    _origin: &str,
    capabilities: &[ApiCapability],
    stale_secs: i64,
    report: &mut CleanupReport,
) {
    for cap in capabilities {
        let age_secs = chrono::Utc::now()
            .timestamp()
            .saturating_sub(cap.updated_at);
        let should_hide = super::relevance::should_hide_unused_after(
            cap.relevance,
            &cap.used_by_recipe_ids,
            age_secs,
            stale_secs,
        );

        if should_hide && !cap.hidden {
            let mut updated = cap.clone();
            updated.hidden = true;
            if let Err(e) = store.save(&updated) {
                report
                    .errors
                    .push(format!("Failed to hide capability {}: {}", cap.id, e));
            } else {
                report.capabilities_hidden += 1;
                // Retain the established field as a default-view-pruned count
                // so existing operators do not silently lose the cleanup signal.
                report.capabilities_pruned += 1;
                tracing::debug!(
                    "[API_MINING] Hid stale unused capability: {} ({}) relevance={:?}",
                    cap.name,
                    cap.id,
                    cap.relevance,
                );
            }
        }
    }
}

// ─────────────────────────── Performance Logging ──────────────────────────

/// Performance comparison entry for API replay vs browser automation.
#[derive(Debug, Clone)]
pub struct PerformanceEntry {
    /// Target URL
    pub url: String,
    /// Capability ID (if API replay was used)
    pub capability_id: Option<String>,
    /// API replay time in milliseconds (None if not attempted)
    pub api_replay_ms: Option<u64>,
    /// Browser automation time in milliseconds (None if not used)
    pub browser_ms: Option<u64>,
    /// Which path was actually used
    pub path_used: ExecutionPath,
    /// Timestamp
    pub timestamp: i64,
}

/// Which execution path was used
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionPath {
    /// API replay succeeded
    ApiReplay,
    /// API replay failed, fell back to browser
    BrowserFallback,
    /// No API capability matched, direct browser
    BrowserDirect,
}

/// Log a performance comparison entry.
///
/// In the future, these entries can be aggregated into a dashboard.
/// For now, we log them as structured tracing events.
pub fn log_performance_comparison(entry: &PerformanceEntry) {
    let speedup = match (entry.api_replay_ms, entry.browser_ms) {
        (Some(api), Some(browser)) if api > 0 => {
            format!("{:.1}x", browser as f64 / api as f64)
        },
        _ => "N/A".to_string(),
    };

    match entry.path_used {
        ExecutionPath::ApiReplay => {
            tracing::info!(
                "[API_MINING_PERF] API replay for '{}' (cap={}): {}ms (speedup: {} vs estimated browser)",
                truncate_url(&entry.url),
                entry.capability_id.as_deref().unwrap_or("?"),
                entry.api_replay_ms.unwrap_or(0),
                speedup,
            );
        },
        ExecutionPath::BrowserFallback => {
            tracing::info!(
                "[API_MINING_PERF] Browser fallback for '{}': replay={}ms (failed), browser={}ms",
                truncate_url(&entry.url),
                entry.api_replay_ms.unwrap_or(0),
                entry.browser_ms.unwrap_or(0),
            );
        },
        ExecutionPath::BrowserDirect => {
            tracing::debug!(
                "[API_MINING_PERF] Browser direct for '{}': {}ms (no API match)",
                truncate_url(&entry.url),
                entry.browser_ms.unwrap_or(0),
            );
        },
    }
}

// ─────────────────────────── Helpers ──────────────────────────────────────

fn is_trace_file(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("jsonl")
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("trace_"))
}

fn api_mining_workspace(base_path: &Path) -> ArtifactV2Workspace {
    ArtifactV2Workspace::with_local_file_provider(base_path)
}

fn entry_path(base_path: &Path, entry: &WorkspaceFileEntry) -> PathBuf {
    base_path.join(&entry.relative_path)
}

fn path_exists(workspace_layout: &ArtifactV2Workspace, path: &Path) -> bool {
    workspace_layout
        .metadata_path_sync(path)
        .ok()
        .flatten()
        .is_some()
}

fn remove_provider_file(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
) -> Result<(), ArtifactV2Error> {
    match workspace_layout.remove_file_path_sync(path) {
        Ok(()) => Ok(()),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn collect_trace_files(
    workspace_layout: &ArtifactV2Workspace,
    base_path: &Path,
    dir: &Path,
) -> Vec<PathBuf> {
    let mut files = Vec::new();

    if let Ok(entries) = workspace_layout.read_dir_path_sync(dir) {
        for entry in entries {
            if !entry.is_file {
                continue;
            }
            let path = entry_path(base_path, &entry);
            if is_trace_file(&path) {
                files.push(path);
            }
        }
    }

    // Also check traces/ subdirectory
    let traces_dir = dir.join("traces");
    if path_exists(workspace_layout, &traces_dir) {
        if let Ok(entries) = workspace_layout.read_dir_path_sync(&traces_dir) {
            for entry in entries {
                if !entry.is_file {
                    continue;
                }
                let path = entry_path(base_path, &entry);
                if is_trace_file(&path) {
                    files.push(path);
                }
            }
        }
    }

    files
}

fn list_all_trace_files(workspace_layout: &ArtifactV2Workspace, base_path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = workspace_layout.read_dir_path_sync(base_path) else {
        return files;
    };

    for entry in entries {
        let path = entry_path(base_path, &entry);
        if entry.is_dir {
            files.extend(collect_trace_files(workspace_layout, base_path, &path));
        }
    }

    files.sort();
    files
}

fn rewrite_trace_file(
    workspace_layout: &ArtifactV2Workspace,
    path: &Path,
    traces: &[crate::magician_v2::api_mining::types::NetworkTraceEvent],
) -> Result<(), String> {
    let mut bytes = Vec::new();
    for trace in traces {
        let line = serde_json::to_string(trace)
            .map_err(|e| format!("Failed to serialize retained trace {:?}: {}", path, e))?;
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    workspace_layout
        .write_atomic_path_sync(path, &bytes)
        .map_err(|e| format!("Failed to rewrite trace file {:?}: {}", path, e))
}

fn truncate_url(url: &str) -> String {
    if url.len() <= 60 {
        url.to_string()
    } else {
        // Find a safe UTF-8 char boundary at or before byte 57
        let pos = (0..=57)
            .rev()
            .find(|&i| url.is_char_boundary(i))
            .unwrap_or(0);
        format!("{}...", &url[..pos])
    }
}

// ─────────────────────────── Tests ────────────────────────────────────────

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::ApiCapability;
    use crate::magician_v2::api_mining::capability_store::CapabilityStore;
    use crate::magician_v2::api_mining::types::{
        NetworkTraceEvent, RequestInitiator, RequestTiming,
    };
    use crate::magician_v2::secrets::{InMemoryKeyProvider, SecretStore};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_capability(name: &str, origin: &str) -> ApiCapability {
        ApiCapability::new(
            name.to_string(),
            origin.to_string(),
            "GET".to_string(),
            format!("{}/api/{}", origin, name),
        )
    }

    fn create_trace(url: &str) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: format!("req-{}", url),
            method: "GET".to_string(),
            url: url.to_string(),
            resource_type: Some("XHR".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::new(),
            request_body: None,
            response_headers: HashMap::new(),
            response_body: None,
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 1.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: None,
            },
            timestamp: 0,
            request_size: 0,
            response_size: 0,
            capture_source: None,
        }
    }

    fn test_secret_store(path: &std::path::Path) -> SecretStore {
        SecretStore::new_empty(Box::new(InMemoryKeyProvider::new()), path.join("secrets"))
    }

    #[test]
    fn test_cleanup_empty_dir() {
        let temp = TempDir::new().unwrap();
        let report = run_cleanup(temp.path());
        assert_eq!(report.traces_deleted, 0);
        assert_eq!(report.capabilities_pruned, 0);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn test_prune_stale_capabilities() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        // Create a capability with old timestamp and 0 replays
        let mut cap = create_test_capability("old_api", "https://example.com");
        cap.created_at = chrono::Utc::now().timestamp() - (60 * 24 * 3600); // 60 days ago
        cap.updated_at = cap.created_at;
        cap.replay_success_count = 0;
        cap.replay_failure_count = 0;
        cap.last_validated = None;
        store.save(&cap).unwrap();

        // Create a fresh capability
        let fresh_cap = create_test_capability("fresh_api", "https://example.com");
        store.save(&fresh_cap).unwrap();

        assert_eq!(store.count("https://example.com").unwrap(), 2);

        let report = run_cleanup(temp.path());
        assert_eq!(report.capabilities_pruned, 1);

        // Both records remain; the stale unused one leaves default views.
        assert_eq!(store.count("https://example.com").unwrap(), 2);
        assert!(store.load("https://example.com", &cap.id).unwrap().hidden);
    }

    #[test]
    fn test_prune_keeps_active_capabilities() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());

        // Create a capability with replays (should not be pruned)
        let mut cap = create_test_capability("active_api", "https://example.com");
        cap.created_at = chrono::Utc::now().timestamp() - (60 * 24 * 3600); // old
        cap.replay_success_count = 5;
        cap.replay_failure_count = 0;
        cap.last_validated = Some(chrono::Utc::now().timestamp()); // recently validated
        store.save(&cap).unwrap();

        let report = run_cleanup(temp.path());
        assert_eq!(report.capabilities_pruned, 0);
        assert_eq!(store.count("https://example.com").unwrap(), 1);
    }

    #[test]
    fn test_enforce_trace_limits() {
        let temp = TempDir::new().unwrap();
        let task_dir = temp.path().join("task1");
        std::fs::create_dir_all(&task_dir).unwrap();

        // Create 10 trace files
        for i in 0..10 {
            let filename = format!("trace_2025_{:02}.jsonl", i);
            std::fs::write(task_dir.join(&filename), format!("line {}", i)).unwrap();
            // Small sleep to ensure different modification times
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let mut report = CleanupReport::default();
        enforce_trace_limits(temp.path(), 5, &mut report);

        assert_eq!(report.traces_deleted, 5);

        // 5 newest files should remain
        let remaining: Vec<_> = std::fs::read_dir(&task_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| is_trace_file(&e.path()))
            .collect();
        assert_eq!(remaining.len(), 5);
    }

    #[test]
    fn test_performance_entry_api_replay() {
        let entry = PerformanceEntry {
            url: "https://example.com/api/data".to_string(),
            capability_id: Some("cap-123".to_string()),
            api_replay_ms: Some(15),
            browser_ms: Some(1200),
            path_used: ExecutionPath::ApiReplay,
            timestamp: chrono::Utc::now().timestamp(),
        };
        // Should not panic
        log_performance_comparison(&entry);
    }

    #[test]
    fn test_performance_entry_browser_fallback() {
        let entry = PerformanceEntry {
            url: "https://example.com/api/data".to_string(),
            capability_id: Some("cap-456".to_string()),
            api_replay_ms: Some(50),
            browser_ms: Some(800),
            path_used: ExecutionPath::BrowserFallback,
            timestamp: chrono::Utc::now().timestamp(),
        };
        log_performance_comparison(&entry);
    }

    #[test]
    fn test_performance_entry_browser_direct() {
        let entry = PerformanceEntry {
            url: "https://example.com/page".to_string(),
            capability_id: None,
            api_replay_ms: None,
            browser_ms: Some(2000),
            path_used: ExecutionPath::BrowserDirect,
            timestamp: chrono::Utc::now().timestamp(),
        };
        log_performance_comparison(&entry);
    }

    #[test]
    fn test_is_trace_file() {
        assert!(is_trace_file(Path::new("trace_20250101.jsonl")));
        assert!(!is_trace_file(Path::new("data.json")));
        assert!(!is_trace_file(Path::new("trace_20250101.json"))); // wrong extension
        assert!(!is_trace_file(Path::new("other_20250101.jsonl"))); // wrong prefix
    }

    #[test]
    fn test_prune_old_traces() {
        let temp = TempDir::new().unwrap();
        let task_dir = temp.path().join("task1");
        std::fs::create_dir_all(&task_dir).unwrap();

        // Create a trace file (will be "new" by default)
        let trace_path = task_dir.join("trace_recent.jsonl");
        std::fs::write(&trace_path, "test data").unwrap();

        let mut report = CleanupReport::default();
        prune_old_traces(temp.path(), 30, &mut report);

        // Recent file should not be deleted
        assert_eq!(report.traces_deleted, 0);
        assert!(trace_path.exists());
    }

    #[test]
    fn test_purge_origin_artifacts_removes_capabilities_traces_and_auth() {
        let temp = TempDir::new().unwrap();
        let store = CapabilityStore::with_base_path(temp.path());
        let trace_storage = TraceStorage::with_base_path(temp.path());
        let secret_store = test_secret_store(temp.path());

        let kept_origin = "https://keep.example.com";
        let purged_origin = "https://purge.example.com";

        store
            .save(&create_test_capability("kept", kept_origin))
            .unwrap();
        store
            .save(&create_test_capability("purged", purged_origin))
            .unwrap();

        trace_storage
            .write_traces(
                "task-1",
                &[
                    create_trace("https://purge.example.com/api/a"),
                    create_trace("https://keep.example.com/api/b"),
                ],
            )
            .unwrap();

        secret_store
            .store_captured(
                purged_origin,
                HashMap::from([("authorization".to_string(), "Bearer secret".to_string())]),
                vec![],
                HashMap::new(),
                HashMap::new(),
            )
            .unwrap();
        secret_store
            .store_captured(
                kept_origin,
                HashMap::from([("authorization".to_string(), "Bearer keep".to_string())]),
                vec![],
                HashMap::new(),
                HashMap::new(),
            )
            .unwrap();

        let report = purge_origin_artifacts(temp.path(), &secret_store, purged_origin).unwrap();

        assert_eq!(report.capabilities_deleted, 1);
        assert!(report.captured_auth_cleared);
        assert_eq!(store.count(purged_origin).unwrap(), 0);
        assert_eq!(store.count(kept_origin).unwrap(), 1);
        assert!(!secret_store.captured_status(purged_origin).has_auth);
        assert!(secret_store.captured_status(kept_origin).has_auth);

        let trace_files = trace_storage.list_trace_files("task-1").unwrap();
        assert_eq!(trace_files.len(), 1);
        let traces = trace_storage.read_traces(&trace_files[0]).unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(extract_origin(&traces[0].url), kept_origin);
    }
}
