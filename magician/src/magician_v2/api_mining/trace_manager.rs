//! Trace manager coordinates between buffer and storage
//!
//! Manages per-tab trace buffers and periodic persistence to disk.

use super::trace_buffer::TraceBuffer;
use super::trace_storage::TraceStorage;
use super::types::{NetworkTraceEvent, TraceCaptureStats};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::warn;

#[derive(Debug, Clone)]
struct TraceScope {
    principal: String,
    workspace: String,
}

/// Manager for trace buffers across multiple tabs
pub struct TraceManager {
    /// Per-tab trace buffers
    buffers: Arc<RwLock<HashMap<String, Arc<TraceBuffer>>>>,

    /// Persistent storage
    workspace_layout: ArtifactV2Workspace,

    /// Current task ID (for associating traces with tasks)
    task_id: Arc<RwLock<Option<String>>>,

    /// Current scoped V3 principal/workspace for associating traces.
    scope: Arc<RwLock<Option<TraceScope>>>,
}

impl TraceManager {
    /// Create a new trace manager with the default base path
    pub fn new() -> Self {
        Self {
            buffers: Arc::new(RwLock::new(HashMap::new())),
            workspace_layout: ArtifactV2Workspace::new(std::path::PathBuf::from(
                "magician_data_v3",
            )),
            task_id: Arc::new(RwLock::new(None)),
            scope: Arc::new(RwLock::new(None)),
        }
    }

    /// Create a trace manager with a custom V3 workspace root.
    pub fn with_workspace_root<P: AsRef<std::path::Path>>(base: P) -> Self {
        Self {
            buffers: Arc::new(RwLock::new(HashMap::new())),
            workspace_layout: ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(
                base.as_ref(),
            )),
            task_id: Arc::new(RwLock::new(None)),
            scope: Arc::new(RwLock::new(None)),
        }
    }

    /// Backward-compatible alias used in tests and legacy helpers.
    pub fn with_base_path<P: AsRef<std::path::Path>>(base: P) -> Self {
        Self::with_workspace_root(base)
    }

    /// Set the current task ID
    pub fn set_task_id(&self, task_id: String) -> Result<(), String> {
        let mut current_task = self.task_id.write().map_err(|e| e.to_string())?;
        *current_task = Some(task_id);
        Ok(())
    }

    pub fn set_scope(&self, principal: String, workspace: String) -> Result<(), String> {
        let mut current_scope = self.scope.write().map_err(|e| e.to_string())?;
        *current_scope = Some(TraceScope {
            principal,
            workspace,
        });
        Ok(())
    }

    pub fn set_trace_target(
        &self,
        principal: String,
        workspace: String,
        task_id: String,
    ) -> Result<(), String> {
        self.set_scope(principal, workspace)?;
        self.set_task_id(task_id)
    }

    /// Get the current task ID
    pub fn get_task_id(&self) -> Result<Option<String>, String> {
        let task = self.task_id.read().map_err(|e| e.to_string())?;
        Ok(task.clone())
    }

    /// Get or create a trace buffer for a tab
    pub fn get_buffer(&self, tab_id: &str) -> Result<Arc<TraceBuffer>, String> {
        let mut buffers = self.buffers.write().map_err(|e| e.to_string())?;

        if let Some(buffer) = buffers.get(tab_id) {
            return Ok(Arc::clone(buffer));
        }

        // Create new buffer
        let buffer = Arc::new(TraceBuffer::new(tab_id.to_string()));
        buffers.insert(tab_id.to_string(), Arc::clone(&buffer));
        Ok(buffer)
    }

    /// Add a trace event to a tab's buffer
    pub fn add_trace(&self, tab_id: &str, event: NetworkTraceEvent) -> Result<(), String> {
        let buffer = self.get_buffer(tab_id)?;
        buffer.push(event)
    }

    /// Get all traces from a tab's buffer
    pub fn get_traces(&self, tab_id: &str) -> Result<Vec<NetworkTraceEvent>, String> {
        let buffer = self.get_buffer(tab_id)?;
        buffer.get_all()
    }

    /// Get recent N traces from a tab's buffer
    pub fn get_recent_traces(
        &self,
        tab_id: &str,
        n: usize,
    ) -> Result<Vec<NetworkTraceEvent>, String> {
        let buffer = self.get_buffer(tab_id)?;
        buffer.get_recent(n)
    }

    /// Flush traces from a tab to persistent storage
    pub fn flush_traces(&self, tab_id: &str) -> Result<usize, String> {
        let task_id = self
            .get_task_id()?
            .ok_or_else(|| "No task ID set".to_string())?;
        let scope = self
            .scope
            .read()
            .map_err(|e| e.to_string())?
            .clone()
            .ok_or_else(|| "No principal/workspace scope set".to_string())?;

        let buffer = self.get_buffer(tab_id)?;
        let traces = buffer.get_all()?;

        if traces.is_empty() {
            return Ok(0);
        }

        let count = traces.len();
        let storage = TraceStorage::with_base_path(
            self.workspace_layout
                .api_mining_root(&scope.principal, &scope.workspace),
        );
        storage.append_traces(&task_id, &traces)?;

        // Clear buffer after successful write
        buffer.clear()?;

        Ok(count)
    }

    /// Flush all tab buffers to storage
    pub fn flush_all(&self) -> Result<usize, String> {
        let buffers = self.buffers.read().map_err(|e| e.to_string())?;
        let tab_ids: Vec<String> = buffers.keys().cloned().collect();
        drop(buffers);

        let mut total_flushed = 0;
        for tab_id in tab_ids {
            match self.flush_traces(&tab_id) {
                Ok(count) => total_flushed += count,
                Err(e) => {
                    warn!("Failed to flush traces for tab {}: {}", tab_id, e);
                },
            }
        }

        Ok(total_flushed)
    }

    /// Get capture statistics for a tab
    pub fn get_stats(&self, tab_id: &str) -> Result<TraceCaptureStats, String> {
        let buffer = self.get_buffer(tab_id)?;
        buffer.get_stats()
    }

    /// Get aggregate statistics across all tabs
    pub fn get_aggregate_stats(&self) -> Result<TraceCaptureStats, String> {
        let buffers = self.buffers.read().map_err(|e| e.to_string())?;

        let mut aggregate = TraceCaptureStats::default();

        for buffer in buffers.values() {
            let stats = buffer.get_stats()?;
            aggregate.total_requests += stats.total_requests;
            aggregate.dropped_oversized += stats.dropped_oversized;
            aggregate.dropped_buffer_full += stats.dropped_buffer_full;
            aggregate.total_bytes_captured += stats.total_bytes_captured;

            // Update max overhead
            if stats.max_overhead_us > aggregate.max_overhead_us {
                aggregate.max_overhead_us = stats.max_overhead_us;
            }
        }

        // Calculate weighted average overhead
        if aggregate.total_requests > 0 {
            let mut total_overhead = 0.0;
            for buffer in buffers.values() {
                let stats = buffer.get_stats()?;
                total_overhead += stats.avg_overhead_us * stats.total_requests as f64;
            }
            aggregate.avg_overhead_us = total_overhead / aggregate.total_requests as f64;
        }

        Ok(aggregate)
    }

    /// Remove a tab's buffer
    pub fn remove_buffer(&self, tab_id: &str) -> Result<(), String> {
        let mut buffers = self.buffers.write().map_err(|e| e.to_string())?;
        buffers.remove(tab_id);
        Ok(())
    }

    /// Clear all buffers
    pub fn clear_all_buffers(&self) -> Result<(), String> {
        let mut buffers = self.buffers.write().map_err(|e| e.to_string())?;
        buffers.clear();
        Ok(())
    }
}

impl Default for TraceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn create_test_event(id: &str) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: id.to_string(),
            method: "GET".to_string(),
            url: "https://example.com".to_string(),
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
            timing: super::super::types::RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 100.0,
            },
            initiator: super::super::types::RequestInitiator {
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

    #[test]
    fn test_manager_creation() {
        let manager = TraceManager::new();
        assert!(manager.get_task_id().unwrap().is_none());
    }

    #[test]
    fn test_buffer_management() {
        let manager = TraceManager::new();

        // Add trace to new buffer
        let event = create_test_event("req1");
        manager.add_trace("tab1", event).unwrap();

        // Verify buffer exists
        let traces = manager.get_traces("tab1").unwrap();
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn test_aggregate_stats() {
        let manager = TraceManager::new();

        // Add traces to multiple tabs
        for tab in ["tab1", "tab2", "tab3"] {
            for i in 0..10 {
                manager
                    .add_trace(tab, create_test_event(&format!("req{}", i)))
                    .unwrap();
            }
        }

        let stats = manager.get_aggregate_stats().unwrap();
        assert_eq!(stats.total_requests, 30);
    }
}
