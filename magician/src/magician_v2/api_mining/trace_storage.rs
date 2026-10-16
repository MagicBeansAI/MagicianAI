//! Persistent storage for network traces
//!
//! Writes traces to JSONL files organized by task ID and timestamp.
//! Path format: storage/magician/api_mining/traces/{task_id}/trace_{timestamp}.jsonl

use super::types::NetworkTraceEvent;
use crate::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};
use serde_json;
use std::path::{Path, PathBuf};

// ─────────────────────── Sensitive Data Redaction ────────────────────────

/// Placeholder value for redacted sensitive data
const REDACTED: &str = "[REDACTED]";

/// Header names that contain sensitive authentication/session data (lowercase).
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "x-auth-token",
    "x-access-token",
    "x-csrf-token",
    "x-xsrf-token",
    "proxy-authorization",
    "www-authenticate",
    "x-amz-security-token",
    "x-framework-xsrf-token", // Gmail CSRF
    "x-google-btd",           // Google session
    "x-gmail-btai",           // Gmail session
    "x-xsrf-asfe-token",      // Google XSRF variant
];

pub fn is_sensitive_header_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SENSITIVE_HEADERS.contains(&lower.as_str())
        || lower == "api-key"
        || lower.ends_with("-api-key")
        || lower.ends_with("-apikey")
        || lower.contains("auth")
        || lower.contains("token")
        || lower.contains("csrf")
        || lower.contains("xsrf")
        || lower.contains("secret")
}

/// JSON field names that likely contain sensitive data (case-insensitive match).
const SENSITIVE_BODY_FIELDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "access_token",
    "refresh_token",
    "api_key",
    "apikey",
    "private_key",
    "client_secret",
    "ssn",
    "credit_card",
    "card_number",
    "cvv",
    "pin",
];

/// Redact sensitive data from a trace event before writing to disk.
///
/// Scrubs:
/// - Sensitive request/response header values (Authorization, Cookie, etc.)
/// - Password-like fields in JSON request/response bodies
///
/// Returns a new trace with sensitive values replaced by `[REDACTED]`.
pub fn redact_trace(trace: &NetworkTraceEvent) -> NetworkTraceEvent {
    let mut redacted = trace.clone();

    // Redact sensitive request headers
    for (key, value) in &mut redacted.request_headers {
        if is_sensitive_header_name(key) {
            *value = REDACTED.to_string();
        }
    }

    // Redact sensitive response headers
    for (key, value) in &mut redacted.response_headers {
        if is_sensitive_header_name(key) {
            *value = REDACTED.to_string();
        }
    }

    // Redact sensitive fields in request body (JSON only)
    if let Some(ref body) = redacted.request_body {
        if let Ok(mut json_val) = serde_json::from_str::<serde_json::Value>(body) {
            if redact_json_value(&mut json_val) {
                if let Ok(scrubbed) = serde_json::to_string(&json_val) {
                    redacted.request_body = Some(scrubbed);
                }
            }
        }
    }

    // Redact sensitive fields in response body (JSON only)
    if let Some(ref body) = redacted.response_body {
        if let Ok(mut json_val) = serde_json::from_str::<serde_json::Value>(body) {
            if redact_json_value(&mut json_val) {
                if let Ok(scrubbed) = serde_json::to_string(&json_val) {
                    redacted.response_body = Some(scrubbed);
                }
            }
        }
    }

    // Redact sensitive query parameters in URL (e.g., ?api_key=xxx)
    redacted.url = redact_url_params(&redacted.url);
    if let Some(url) = redacted.initiator.url.as_deref() {
        redacted.initiator.url = Some(redact_url_params(url));
    }
    if let Some(stack) = redacted.initiator.stack.as_mut() {
        for frame in stack {
            frame.url = redact_url_params(&frame.url);
        }
    }

    redacted
}

fn is_sensitive_body_field(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    SENSITIVE_BODY_FIELDS
        .iter()
        .any(|sensitive| lower.contains(sensitive))
        || lower.contains("authorization")
        || matches!(compact.as_str(), "apikey" | "clientsecret")
        || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("csrf")
        || compact.ends_with("xsrf")
}

/// Recursively redact sensitive fields in a JSON value.
///
/// Returns true if any field was redacted.
fn redact_json_value(value: &mut serde_json::Value) -> bool {
    let mut changed = false;
    match value {
        serde_json::Value::Object(map) => {
            let keys_to_redact: Vec<String> = map
                .keys()
                .filter(|key| is_sensitive_body_field(key))
                .cloned()
                .collect();

            for key in keys_to_redact {
                map.insert(key, serde_json::Value::String(REDACTED.to_string()));
                changed = true;
            }

            // Recurse into remaining object values
            for (_key, val) in map.iter_mut() {
                if redact_json_value(val) {
                    changed = true;
                }
            }
        },
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                if redact_json_value(item) {
                    changed = true;
                }
            }
        },
        _ => {},
    }
    changed
}

/// Redact sensitive query parameters from a URL.
///
/// Replaces values of known-sensitive parameter names while preserving URL structure.
fn redact_url_params(url: &str) -> String {
    // Split on '?' to find query string
    let parts: Vec<&str> = url.splitn(2, '?').collect();
    if parts.len() < 2 {
        return url.to_string();
    }

    let base = parts[0];
    let query = parts[1];

    // Split on '#' to preserve fragment
    let query_parts: Vec<&str> = query.splitn(2, '#').collect();
    let query_str = query_parts[0];
    let fragment = query_parts.get(1);

    let redacted_params: Vec<String> = query_str
        .split('&')
        .map(|param| {
            let kv: Vec<&str> = param.splitn(2, '=').collect();
            if kv.len() == 2 {
                let key_lower = kv[0].to_lowercase();
                if SENSITIVE_BODY_FIELDS.iter().any(|s| key_lower.contains(s))
                    || key_lower.contains("key")
                    || key_lower.contains("auth")
                    || key_lower.contains("csrf")
                    || key_lower.contains("xsrf")
                {
                    format!("{}={}", kv[0], REDACTED)
                } else {
                    param.to_string()
                }
            } else {
                param.to_string()
            }
        })
        .collect();

    let mut result = format!("{}?{}", base, redacted_params.join("&"));
    if let Some(frag) = fragment {
        result = format!("{}#{}", result, frag);
    }
    result
}

fn default_trace_storage_base() -> PathBuf {
    crate::magician_v2::process_storage::workspace().api_mining_root("__unbound__", "__unbound__")
}

/// Trace storage manager
pub struct TraceStorage {
    base_path: PathBuf,
    workspace_layout: ArtifactV2Workspace,
}

impl TraceStorage {
    /// Create a new trace storage manager
    pub fn new() -> Self {
        let base_path = default_trace_storage_base();
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base_path),
            base_path,
        }
    }

    /// Create storage with custom base path (for testing)
    pub fn with_base_path<P: AsRef<Path>>(path: P) -> Self {
        let base_path = path.as_ref().to_path_buf();
        Self {
            workspace_layout: ArtifactV2Workspace::with_local_file_provider(&base_path),
            base_path,
        }
    }

    /// Ensure storage directory exists for a task
    fn ensure_task_dir(&self, task_id: &str) -> Result<PathBuf, String> {
        let task_dir = super::types::safe_join(&self.base_path, task_id)?;
        self.workspace_layout
            .create_dir_all_path_sync(&task_dir)
            .map_err(|e| format!("Failed to create task directory: {}", e))?;
        Ok(task_dir)
    }

    fn entry_path(&self, entry: &WorkspaceFileEntry) -> PathBuf {
        self.base_path.join(&entry.relative_path)
    }

    fn path_exists(&self, path: &Path) -> Result<bool, String> {
        self.workspace_layout
            .metadata_path_sync(path)
            .map(|metadata| metadata.is_some())
            .map_err(|e| e.to_string())
    }

    fn is_not_found(error: &ArtifactV2Error) -> bool {
        matches!(error, ArtifactV2Error::Io(inner) if inner.kind() == std::io::ErrorKind::NotFound)
    }

    fn traces_to_jsonl(traces: &[NetworkTraceEvent]) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        for trace in traces {
            let safe_trace = redact_trace(trace);
            let json = serde_json::to_string(&safe_trace)
                .map_err(|e| format!("Failed to serialize trace: {}", e))?;
            bytes.extend_from_slice(json.as_bytes());
            bytes.push(b'\n');
        }
        Ok(bytes)
    }

    /// Write traces to a new JSONL file for a task
    pub fn write_traces(
        &self,
        task_id: &str,
        traces: &[NetworkTraceEvent],
    ) -> Result<PathBuf, String> {
        if traces.is_empty() {
            return Err("No traces to write".to_string());
        }

        let task_dir = self.ensure_task_dir(task_id)?;

        // Generate filename with timestamp (millisecond precision to avoid collisions)
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f").to_string();
        let filename = format!("trace_{}.jsonl", timestamp);
        let filepath = task_dir.join(&filename);

        let bytes = Self::traces_to_jsonl(traces)?;
        self.workspace_layout
            .write_atomic_path_sync(&filepath, &bytes)
            .map_err(|e| format!("Failed to write trace file: {}", e))?;

        Ok(filepath)
    }

    /// Persist the browser action events for a task, beside its traces.
    ///
    /// Action events were previously live-only: the correlator consumed them
    /// from an in-memory slot during dispatch and they were dropped when the
    /// execution ended. That made "which user action caused this request?"
    /// unanswerable after the fact — and that linkage is the single strongest
    /// signal for telling an app's real API calls apart from background
    /// telemetry, which fires on timers rather than in response to intent.
    ///
    /// Written into the same `task_id` directory as traces and with the same
    /// filename convention, so post-processing joins them by task + timestamp
    /// exactly as it joins traces.
    ///
    /// Privacy note: `user_values` carries text the automation typed. This is
    /// the same exposure class as the request/response bodies already stored
    /// beside it, so it is written as-is rather than partially redacted, which
    /// would be inconsistent. Retention/redaction policy for the whole
    /// api-mining corpus is a single decision, not a per-field one.
    pub fn write_action_events(
        &self,
        task_id: &str,
        events: &[crate::magician_v2::api_mining::correlator::ActionEvent],
    ) -> Result<PathBuf, String> {
        if events.is_empty() {
            return Err("No action events to write".to_string());
        }

        let task_dir = self.ensure_task_dir(task_id)?;
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f").to_string();
        let filepath = task_dir.join(format!("actions_{}.jsonl", timestamp));

        let mut bytes = Vec::new();
        for event in events {
            let line = serde_json::to_vec(event)
                .map_err(|e| format!("Failed to serialize action event: {}", e))?;
            bytes.extend_from_slice(&line);
            bytes.push(b'\n');
        }

        self.workspace_layout
            .write_atomic_path_sync(&filepath, &bytes)
            .map_err(|e| format!("Failed to write action event file: {}", e))?;

        Ok(filepath)
    }

    /// Append traces to existing file or create new one
    pub fn append_traces(
        &self,
        task_id: &str,
        traces: &[NetworkTraceEvent],
    ) -> Result<PathBuf, String> {
        if traces.is_empty() {
            return Err("No traces to append".to_string());
        }

        let task_dir = self.ensure_task_dir(task_id)?;

        // Find most recent trace file or create new one
        let latest_file = self.get_latest_trace_file(task_id)?;

        let filepath = if let Some(existing) = latest_file {
            existing
        } else {
            let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
            task_dir.join(format!("trace_{}.jsonl", timestamp))
        };

        let bytes = Self::traces_to_jsonl(traces)?;
        self.workspace_layout
            .append_path_sync(&filepath, &bytes)
            .map_err(|e| format!("Failed to append trace file: {}", e))?;

        Ok(filepath)
    }

    /// Get the latest trace file for a task
    fn get_latest_trace_file(&self, task_id: &str) -> Result<Option<PathBuf>, String> {
        let task_dir = super::types::safe_join(&self.base_path, task_id)?;

        if !self.path_exists(&task_dir)? {
            return Ok(None);
        }

        let entries = self
            .workspace_layout
            .read_dir_path_sync(&task_dir)
            .map_err(|e| format!("Failed to read task directory: {}", e))?;

        let mut trace_files: Vec<PathBuf> = entries
            .into_iter()
            .filter(|e| e.is_file)
            .map(|e| self.entry_path(&e))
            .filter(|p| {
                p.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("trace_"))
            })
            .collect();

        trace_files.sort();
        Ok(trace_files.last().cloned())
    }

    /// Read traces from a specific file
    pub fn read_traces<P: AsRef<Path>>(
        &self,
        filepath: P,
    ) -> Result<Vec<NetworkTraceEvent>, String> {
        let content = self
            .workspace_layout
            .read_to_string_path_sync(filepath.as_ref())
            .map_err(|e| format!("Failed to read trace file: {}", e))?;

        let mut traces = Vec::new();
        let mut skipped = 0usize;
        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<NetworkTraceEvent>(line) {
                Ok(trace) => traces.push(trace),
                Err(e) => {
                    // Skip malformed lines instead of failing the entire read
                    skipped += 1;
                    tracing::warn!(
                        "[API_MINING] Skipping malformed trace at line {}: {}",
                        line_num + 1,
                        e
                    );
                },
            }
        }

        if skipped > 0 {
            tracing::info!(
                "[API_MINING] Read {} traces, skipped {} malformed lines from {:?}",
                traces.len(),
                skipped,
                filepath.as_ref()
            );
        }

        Ok(traces)
    }

    /// Read complete compiler evidence, rejecting any missing/corrupt record
    /// or exceeded budget instead of silently omitting a possible mutation.
    /// Aggregation/UI readers intentionally retain the tolerant API above.
    pub async fn read_complete_traces<P: AsRef<Path>>(
        &self,
        filepath: P,
        max_bytes: u64,
        max_events: usize,
    ) -> Result<(Vec<NetworkTraceEvent>, u64), String> {
        let content = self
            .workspace_layout
            .read_to_string_bounded_path(filepath, max_bytes)
            .await
            .map_err(|error| format!("could not read complete trace evidence: {error}"))?;
        if !content.is_empty() && !content.ends_with('\n') {
            return Err("trace evidence has an uncommitted final record".into());
        }
        let mut traces = Vec::new();
        for (line_number, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            if traces.len() >= max_events {
                return Err("trace evidence exceeds the event limit".into());
            }
            let trace = serde_json::from_str(line).map_err(|_| {
                // Do not echo a malformed line containing captured data.
                format!("trace evidence is malformed at line {}", line_number + 1)
            })?;
            traces.push(trace);
        }
        Ok((traces, content.len() as u64))
    }

    /// List all trace files for a task
    pub fn list_trace_files(&self, task_id: &str) -> Result<Vec<PathBuf>, String> {
        let task_dir = super::types::safe_join(&self.base_path, task_id)?;

        if !self.path_exists(&task_dir)? {
            return Ok(Vec::new());
        }

        let entries = self
            .workspace_layout
            .read_dir_path_sync(&task_dir)
            .map_err(|e| format!("Failed to read task directory: {}", e))?;

        let mut trace_files: Vec<PathBuf> = entries
            .into_iter()
            .filter(|e| e.is_file)
            .map(|e| self.entry_path(&e))
            .filter(|p| {
                p.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("trace_"))
            })
            .collect();

        trace_files.sort();
        Ok(trace_files)
    }

    /// List all trace files across ALL task subdirectories, filtered by recency.
    ///
    /// Walks every subdirectory under `base_path` that contains trace JSONL files.
    /// Only includes files modified within `max_age_days` (0 = no age filter).
    /// Origin-keyed directories (e.g., `https___mail_google_com/`) are skipped
    /// since those contain capability files, not traces.
    pub fn list_all_recent_trace_files(&self, max_age_days: u32) -> Result<Vec<PathBuf>, String> {
        if !self.path_exists(&self.base_path)? {
            return Ok(Vec::new());
        }

        let cutoff = if max_age_days > 0 {
            Some(
                std::time::SystemTime::now()
                    - std::time::Duration::from_secs(max_age_days as u64 * 24 * 3600),
            )
        } else {
            None
        };

        let entries = self
            .workspace_layout
            .read_dir_path_sync(&self.base_path)
            .map_err(|e| format!("Failed to read api_mining dir: {}", e))?;

        let mut all_files = Vec::new();

        for entry in entries {
            let path = self.entry_path(&entry);
            if !entry.is_dir {
                continue;
            }

            // Skip origin-keyed directories (they contain capabilities, not traces).
            // Origin dirs have names like `https___mail_google_com`. Task dirs are UUIDs
            // or `deleg-thread-*` prefixed.
            let dir_name = entry
                .relative_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_string();
            if dir_name.starts_with("http") || dir_name == "registry_index.json" {
                continue;
            }

            // Collect trace files from this task directory
            let Ok(files) = self.workspace_layout.read_dir_path_sync(&path) else {
                continue;
            };

            for file_entry in files {
                if !file_entry.is_file {
                    continue;
                }
                let fpath = self.entry_path(&file_entry);
                let is_trace = fpath.extension().and_then(|s| s.to_str()) == Some("jsonl")
                    && fpath
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| s.starts_with("trace_"));

                if !is_trace {
                    continue;
                }

                // Apply age filter (fail-closed: exclude files whose age can't be determined)
                if let Some(cutoff_time) = cutoff {
                    let recent_enough = self
                        .workspace_layout
                        .metadata_path_sync(&fpath)
                        .map_err(|e| std::io::Error::other(e.to_string()))
                        .and_then(|metadata| {
                            metadata.ok_or_else(|| {
                                std::io::Error::new(
                                    std::io::ErrorKind::NotFound,
                                    "missing trace metadata",
                                )
                            })
                        })
                        .and_then(|m| m.modified())
                        .map(|modified| modified >= cutoff_time)
                        .unwrap_or(false);
                    if !recent_enough {
                        continue;
                    }
                }

                all_files.push(fpath);
            }
        }

        all_files.sort();
        Ok(all_files)
    }

    /// Delete all traces for a task
    pub fn delete_task_traces(&self, task_id: &str) -> Result<(), String> {
        let task_dir = super::types::safe_join(&self.base_path, task_id)?;

        match self.workspace_layout.remove_dir_all_path_sync(&task_dir) {
            Ok(()) => {},
            Err(error) if Self::is_not_found(&error) => {},
            Err(error) => return Err(format!("Failed to delete task traces: {}", error)),
        }

        Ok(())
    }
}

impl Default for TraceStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_trace(id: &str) -> NetworkTraceEvent {
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
            response_body: Some("{\"data\": \"test\"}".to_string()),
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
            timestamp: chrono::Utc::now().timestamp(),
            request_size: 0,
            response_size: 15,
            capture_source: None,
        }
    }

    #[test]
    fn test_write_and_read_traces() {
        let temp_dir = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp_dir.path());

        let traces = vec![create_test_trace("req1"), create_test_trace("req2")];

        let filepath = storage.write_traces("task1", &traces).unwrap();
        assert!(filepath.exists());

        let read_traces = storage.read_traces(&filepath).unwrap();
        assert_eq!(read_traces.len(), 2);
        assert_eq!(read_traces[0].request_id, "req1");
        assert_eq!(read_traces[1].request_id, "req2");
    }

    #[test]
    fn test_append_traces() {
        let temp_dir = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp_dir.path());

        // Write initial traces
        let traces1 = vec![create_test_trace("req1")];
        storage.write_traces("task1", &traces1).unwrap();

        // Append more traces
        let traces2 = vec![create_test_trace("req2")];
        let filepath = storage.append_traces("task1", &traces2).unwrap();

        // Read all traces
        let all_traces = storage.read_traces(&filepath).unwrap();
        assert_eq!(all_traces.len(), 2);
    }

    #[tokio::test]
    async fn complete_trace_reads_reject_corruption_without_changing_tolerant_reads() {
        let temp = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp.path());
        let trace = serde_json::to_string(&create_test_trace("answer-read")).unwrap();
        let path = temp.path().join("trace_partial.jsonl");
        std::fs::write(
            &path,
            format!("{trace}\n{{corrupt-private-mutation\n{trace}\n"),
        )
        .unwrap();

        assert_eq!(storage.read_traces(&path).unwrap().len(), 2);
        let error = storage
            .read_complete_traces(&path, 16_384, 10)
            .await
            .unwrap_err();
        assert!(error.contains("line 2"));
        assert!(!error.contains("private-mutation"));
    }

    #[tokio::test]
    async fn complete_trace_reads_enforce_bytes_events_and_committed_tail() {
        let temp = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp.path());
        let trace = serde_json::to_string(&create_test_trace("read")).unwrap();
        let path = temp.path().join("trace_bounded.jsonl");
        let content = format!("{trace}\n\n{trace}\n");
        std::fs::write(&path, &content).unwrap();
        let (traces, bytes) = storage
            .read_complete_traces(&path, content.len() as u64, 2)
            .await
            .unwrap();
        assert_eq!(traces.len(), 2);
        assert_eq!(bytes, content.len() as u64);
        assert!(storage
            .read_complete_traces(&path, bytes - 1, 2)
            .await
            .is_err());
        assert!(storage.read_complete_traces(&path, bytes, 1).await.is_err());
        std::fs::write(&path, &trace).unwrap();
        assert!(storage
            .read_complete_traces(&path, bytes, 2)
            .await
            .unwrap_err()
            .contains("uncommitted"));
        assert!(storage
            .read_complete_traces(temp.path().join("missing.jsonl"), bytes, 2)
            .await
            .is_err());
    }

    #[test]
    fn test_list_trace_files() {
        let temp_dir = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp_dir.path());

        // Write multiple trace files
        storage
            .write_traces("task1", &[create_test_trace("req1")])
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        storage
            .write_traces("task1", &[create_test_trace("req2")])
            .unwrap();

        let files = storage.list_trace_files("task1").unwrap();
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_delete_task_traces() {
        let temp_dir = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp_dir.path());

        storage
            .write_traces("task1", &[create_test_trace("req1")])
            .unwrap();
        assert!(!storage.list_trace_files("task1").unwrap().is_empty());

        storage.delete_task_traces("task1").unwrap();
        assert!(storage.list_trace_files("task1").unwrap().is_empty());
    }

    // ─── Redaction Tests ────────────────────────────────────────

    #[test]
    fn test_redact_authorization_header() {
        let mut trace = create_test_trace("req1");
        trace.request_headers.insert(
            "Authorization".to_string(),
            "Bearer secret_token_123".to_string(),
        );
        trace
            .request_headers
            .insert("Content-Type".to_string(), "application/json".to_string());

        let redacted = redact_trace(&trace);

        assert_eq!(
            redacted.request_headers.get("Authorization").unwrap(),
            "[REDACTED]"
        );
        // Non-sensitive headers preserved
        assert_eq!(
            redacted.request_headers.get("Content-Type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn test_redact_cookie_headers() {
        let mut trace = create_test_trace("req1");
        trace
            .request_headers
            .insert("cookie".to_string(), "SID=abc123; HSID=def456".to_string());
        trace.response_headers.insert(
            "Set-Cookie".to_string(),
            "session=xyz789; HttpOnly".to_string(),
        );

        let redacted = redact_trace(&trace);

        assert_eq!(
            redacted.request_headers.get("cookie").unwrap(),
            "[REDACTED]"
        );
        assert_eq!(
            redacted.response_headers.get("Set-Cookie").unwrap(),
            "[REDACTED]"
        );
    }

    #[test]
    fn test_redact_vendor_api_key_header() {
        let mut trace = create_test_trace("req1");
        trace
            .request_headers
            .insert("x-algolia-api-key".to_string(), "search-key".to_string());
        trace
            .request_headers
            .insert("x-algolia-application-id".to_string(), "APPID".to_string());

        let redacted = redact_trace(&trace);

        assert_eq!(
            redacted.request_headers.get("x-algolia-api-key").unwrap(),
            "[REDACTED]"
        );
        assert_eq!(
            redacted
                .request_headers
                .get("x-algolia-application-id")
                .unwrap(),
            "APPID"
        );
    }

    #[test]
    fn test_redact_password_in_request_body() {
        let mut trace = create_test_trace("req1");
        trace.request_body =
            Some(r#"{"username": "alice", "password": "s3cret!", "remember": true}"#.to_string());

        let redacted = redact_trace(&trace);

        let body: serde_json::Value =
            serde_json::from_str(redacted.request_body.as_ref().unwrap()).unwrap();
        assert_eq!(body["username"], "alice");
        assert_eq!(body["password"], "[REDACTED]");
        assert_eq!(body["remember"], true);
    }

    #[test]
    fn test_redact_nested_sensitive_fields() {
        let mut trace = create_test_trace("req1");
        trace.response_body = Some(
            r#"{"user": {"name": "Bob", "api_key": "key_12345"}, "token": "jwt_abc"}"#.to_string(),
        );

        let redacted = redact_trace(&trace);

        let body: serde_json::Value =
            serde_json::from_str(redacted.response_body.as_ref().unwrap()).unwrap();
        assert_eq!(body["user"]["name"], "Bob");
        assert_eq!(body["user"]["api_key"], "[REDACTED]");
        assert_eq!(body["token"], "[REDACTED]");
    }

    #[test]
    fn test_redact_camel_case_tokens_and_initiator_urls() {
        let mut trace = create_test_trace("req1");
        trace.response_body =
            Some(r#"{"refreshToken":"response-secret","profile":{"name":"Bob"}}"#.to_string());
        trace.initiator.url =
            Some("https://example.com/loader.js?access_token=initiator-secret".to_string());
        trace.initiator.stack = Some(vec![super::super::types::StackFrame {
            function_name: "load".to_string(),
            script_id: "1".to_string(),
            url: "https://example.com/app.js?api_key=stack-secret".to_string(),
            line_number: 1,
            column_number: 1,
        }]);

        let redacted = redact_trace(&trace);
        let serialized = serde_json::to_string(&redacted).unwrap();

        assert!(!serialized.contains("response-secret"));
        assert!(!serialized.contains("initiator-secret"));
        assert!(!serialized.contains("stack-secret"));
        assert!(serialized.contains("[REDACTED]"));
        assert!(serialized.contains("Bob"));
    }

    #[test]
    fn test_redact_url_query_params() {
        let mut trace = create_test_trace("req1");
        trace.url = "https://api.example.com/v1/data?api_key=secret123&page=1".to_string();

        let redacted = redact_trace(&trace);

        assert!(redacted.url.contains("api_key=[REDACTED]"));
        assert!(redacted.url.contains("page=1"));
        assert!(!redacted.url.contains("secret123"));
    }

    #[test]
    fn test_redact_preserves_non_json_body() {
        let mut trace = create_test_trace("req1");
        trace.request_body = Some("plain text body with password inside".to_string());

        let redacted = redact_trace(&trace);

        // Non-JSON bodies are preserved as-is (can't reliably parse)
        assert_eq!(
            redacted.request_body.as_ref().unwrap(),
            "plain text body with password inside"
        );
    }

    #[test]
    fn test_redact_multiple_sensitive_headers() {
        let mut trace = create_test_trace("req1");
        trace
            .request_headers
            .insert("X-Api-Key".to_string(), "key_abc".to_string());
        trace
            .request_headers
            .insert("x-auth-token".to_string(), "tok_xyz".to_string());
        trace
            .request_headers
            .insert("Proxy-Authorization".to_string(), "Basic abc==".to_string());
        trace
            .request_headers
            .insert("Accept".to_string(), "application/json".to_string());

        let redacted = redact_trace(&trace);

        assert_eq!(
            redacted.request_headers.get("X-Api-Key").unwrap(),
            "[REDACTED]"
        );
        assert_eq!(
            redacted.request_headers.get("x-auth-token").unwrap(),
            "[REDACTED]"
        );
        assert_eq!(
            redacted.request_headers.get("Proxy-Authorization").unwrap(),
            "[REDACTED]"
        );
        assert_eq!(
            redacted.request_headers.get("Accept").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn test_redact_traces_written_to_disk() {
        let temp_dir = TempDir::new().unwrap();
        let storage = TraceStorage::with_base_path(temp_dir.path());

        let mut trace = create_test_trace("req1");
        trace
            .request_headers
            .insert("Authorization".to_string(), "Bearer secret123".to_string());
        trace.request_body = Some(r#"{"password": "hunter2", "user": "admin"}"#.to_string());

        let filepath = storage.write_traces("task1", &[trace]).unwrap();
        let read_traces = storage.read_traces(&filepath).unwrap();

        assert_eq!(read_traces.len(), 1);
        assert_eq!(
            read_traces[0].request_headers.get("Authorization").unwrap(),
            "[REDACTED]"
        );
        let body: serde_json::Value =
            serde_json::from_str(read_traces[0].request_body.as_ref().unwrap()).unwrap();
        assert_eq!(body["password"], "[REDACTED]");
        assert_eq!(body["user"], "admin");
    }

    #[test]
    fn test_redact_url_without_query_params() {
        let url = "https://example.com/api/v1/users";
        let result = redact_url_params(url);
        assert_eq!(result, url);
    }

    #[test]
    fn test_redact_url_with_fragment() {
        let url = "https://example.com/page?auth_token=secret#section1";
        let result = redact_url_params(url);
        assert!(result.contains("auth_token=[REDACTED]"));
        assert!(result.contains("#section1"));
    }
}
