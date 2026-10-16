//! Per-workspace JSONL log of [`AgentUpdate`] events.
//!
//! One file per (principal, workspace) pair at:
//! `<data_root>/scopes/<principal>/<workspace>/updates.jsonl`
//!
//! Writer: subscribes to the shared `RuntimeTransportBroadcaster`, filters on
//! event_type == "agent.update", appends one JSON line per event. O_APPEND
//! writes under PIPE_BUF are atomic on Linux/macOS; we rely on single-writer
//! semantics by running exactly one journal task per process.
//!
//! Rotation: when the current file exceeds `max_bytes`, it is renamed to
//! `updates-<ISO8601>.jsonl` and a fresh file is started.
//!
//! Reader: the [`tail`] / [`tail_matching`] functions read the current file,
//! filter by `since_ts` and optional caller predicates, then apply `limit` and
//! return events in reverse-chronological order.
//! Older rotated files are archival — the REST API does not read them.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use tokio::task::JoinHandle;

use super::agent_update_emitter::AGENT_UPDATE_EVENT_TYPE;
use crate::magician_v2::{
    artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error},
    realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

/// Default maximum bytes per file before rotation (50 MB).
pub const DEFAULT_MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// Per-workspace journal writer configuration.
#[derive(Debug, Clone)]
pub struct AgentUpdateJournalConfig {
    /// Root data directory. The journal writes to
    /// `<data_root>/scopes/<principal>/<workspace>/updates.jsonl`.
    pub data_root: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    /// File size threshold for rotation.
    pub max_file_bytes: u64,
}

impl AgentUpdateJournalConfig {
    pub fn new(data_root: PathBuf) -> Self {
        let workspace_layout = ArtifactV2Workspace::with_local_file_provider(&data_root);
        Self {
            data_root,
            workspace_layout,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            data_root: workspace_layout.base_root().to_path_buf(),
            workspace_layout,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
        }
    }

    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_file_bytes = max;
        self
    }

    /// Path for the currently-writable journal file.
    pub fn journal_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.data_root
            .join("scopes")
            .join(principal)
            .join(workspace)
            .join("updates.jsonl")
    }
}

/// Spawn a background task that subscribes to `broadcaster` and appends every
/// `agent.update` event to the appropriate per-workspace JSONL file.
///
/// Events without a principal or workspace are dropped — they cannot be
/// routed. This matches the operator-feed scope model: untagged events are
/// not observable in the feed.
pub fn spawn_journal(
    config: AgentUpdateJournalConfig,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
) -> JoinHandle<()> {
    // Subscribe BEFORE spawning so events emitted immediately after the
    // caller receives the JoinHandle are not lost to a subscribe race.
    let mut rx = broadcaster.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let RuntimeTransportEvent::AgentEvent { event } = event {
                        if event.event_type == AGENT_UPDATE_EVENT_TYPE {
                            if let Err(err) = write_event(&config, &event) {
                                tracing::error!(
                                    error = %err,
                                    "agent_update_journal: failed to write event"
                                );
                            }
                        }
                    }
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "agent_update_journal: receiver lagged; events dropped"
                    );
                },
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                },
            }
        }
    })
}

/// Synchronously append one envelope payload to the owning workspace's log.
///
/// Used by [`spawn_journal`]. Exposed for tests and direct-write paths.
pub fn write_event(
    config: &AgentUpdateJournalConfig,
    envelope: &AgentEventEnvelope,
) -> io::Result<()> {
    let principal = envelope
        .principal
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "envelope missing principal"))?;
    let workspace = envelope
        .workspace
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "envelope missing workspace"))?;
    let path = config.journal_path(principal, workspace);
    let workspace_layout = &config.workspace_layout;

    // Rotate BEFORE writing if the current file is already oversize.
    if let Ok(Some(meta)) = workspace_layout.metadata_path_sync(&path) {
        if meta.len() >= config.max_file_bytes {
            rotate(workspace_layout, &path)?;
        }
    }

    // Single atomic append: format line + newline into one buffer so a
    // concurrent reader never sees a half-line. Under O_APPEND on Linux /
    // macOS, a single write under PIPE_BUF (4 KB) is atomic — typical
    // AgentUpdate payloads comfortably fit.
    let mut line = serde_json::to_string(&envelope.payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    line.push('\n');
    workspace_layout
        .append_path_sync(&path, line.as_bytes())
        .map_err(artifact_io_error)?;
    Ok(())
}

/// Rename `path` to `updates-<ISO8601>.jsonl` in the same directory.
fn rotate(workspace_layout: &ArtifactV2Workspace, path: &Path) -> io::Result<()> {
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "journal path has no parent directory",
        )
    })?;
    let rotated = parent.join(format!("updates-{}.jsonl", stamp));
    workspace_layout
        .rename_path_sync(path, &rotated)
        .map_err(artifact_io_error)?;
    Ok(())
}

/// Read the current journal file for (principal, workspace), return events
/// newer than `since_ts` (unix millis), newest first, up to `limit`.
///
/// Rotated files are not read. The assumption is that rotation boundaries
/// mark archival points; recent history is what operators watch.
pub fn tail(
    config: &AgentUpdateJournalConfig,
    principal: &str,
    workspace: &str,
    since_ts: Option<i64>,
    limit: usize,
) -> io::Result<Vec<Value>> {
    tail_matching(config, principal, workspace, since_ts, limit, |_| true)
}

/// Read the current journal file for (principal, workspace), return matching
/// events newer than `since_ts` (unix millis), newest first, up to `limit`.
///
/// Rotated files are not read. The assumption is that rotation boundaries
/// mark archival points; recent history is what operators watch.
///
/// The caller predicate runs before the limit is applied, so API filters such
/// as `agent`, `kind`, and `thread` can still return `limit` matching events
/// even when newer non-matching events exist.
pub fn tail_matching<F>(
    config: &AgentUpdateJournalConfig,
    principal: &str,
    workspace: &str,
    since_ts: Option<i64>,
    limit: usize,
    mut predicate: F,
) -> io::Result<Vec<Value>>
where
    F: FnMut(&Value) -> bool,
{
    let path = config.journal_path(principal, workspace);
    let contents = match config.workspace_layout.read_to_string_path_sync(&path) {
        Ok(contents) => contents,
        Err(ArtifactV2Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        },
        Err(error) => return Err(artifact_io_error(error)),
    };
    let mut events: Vec<Value> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| match since_ts {
            Some(ts) => v
                .get("ts")
                .and_then(|t| t.as_i64())
                .is_some_and(|event_ts| event_ts > ts),
            None => true,
        })
        .filter(|v| predicate(v))
        .collect();
    events.sort_by(|a, b| {
        let a_ts = a.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        let b_ts = b.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    events.truncate(limit);
    Ok(events)
}

fn artifact_io_error(error: ArtifactV2Error) -> io::Error {
    match error {
        ArtifactV2Error::Io(error) => error,
        other => io::Error::other(other),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::agent_update::{AgentUpdate, AgentUpdateKind, CycleOutcome};
    use crate::magician_v2::agents::agent_update_emitter::{
        publish_agent_update, AGENT_UPDATE_EVENT_TYPE,
    };
    use serde_json::json;
    use tempfile::TempDir;

    fn make_envelope(principal: &str, workspace: &str, payload: Value) -> AgentEventEnvelope {
        AgentEventEnvelope::new_scoped(
            AGENT_UPDATE_EVENT_TYPE,
            "cfo",
            principal,
            workspace,
            payload,
        )
    }

    #[test]
    fn write_event_creates_nested_directories_and_appends_line() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let payload = json!({"id": "01H", "ts": 1, "kind": "agent_resumed"});
        let env = make_envelope("alpha", "ws_a", payload.clone());

        write_event(&cfg, &env).unwrap();

        let path = cfg.journal_path("alpha", "ws_a");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), serde_json::to_string(&payload).unwrap());
    }

    #[test]
    fn write_event_appends_subsequent_events_with_newline_separator() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let a = make_envelope("alpha", "ws_a", json!({"id": "A", "ts": 1}));
        let b = make_envelope("alpha", "ws_a", json!({"id": "B", "ts": 2}));
        write_event(&cfg, &a).unwrap();
        write_event(&cfg, &b).unwrap();

        let contents = std::fs::read_to_string(cfg.journal_path("alpha", "ws_a")).unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn write_event_rejects_envelope_without_scope() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let env = AgentEventEnvelope::new(AGENT_UPDATE_EVENT_TYPE, "cfo", json!({}));
        let err = write_event(&cfg, &env).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rotation_happens_when_file_exceeds_max_bytes() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf()).with_max_bytes(64);
        let env1 = make_envelope(
            "alpha",
            "ws_a",
            json!({"id": "A", "ts": 1, "big": "x".repeat(100)}),
        );
        write_event(&cfg, &env1).unwrap();
        // Now file > 64 bytes. Next write rotates first.
        let env2 = make_envelope("alpha", "ws_a", json!({"id": "B", "ts": 2}));
        write_event(&cfg, &env2).unwrap();

        let workspace_dir = cfg.data_root.join("scopes/alpha/ws_a");
        let entries: Vec<_> = std::fs::read_dir(&workspace_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        // Expect one rotated file + the fresh current.
        assert!(entries.iter().any(|n| n == "updates.jsonl"));
        assert!(
            entries
                .iter()
                .any(|n| n.starts_with("updates-") && n.ends_with(".jsonl")),
            "expected rotated file, got {:?}",
            entries
        );
    }

    #[test]
    fn tail_returns_events_newest_first_filtered_by_since_and_limit() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        for ts in [10_i64, 20, 30, 40, 50] {
            let env = make_envelope("alpha", "ws_a", json!({"id": format!("e{}", ts), "ts": ts}));
            write_event(&cfg, &env).unwrap();
        }
        let got = tail(&cfg, "alpha", "ws_a", Some(15), 3).unwrap();
        let timestamps: Vec<i64> = got
            .iter()
            .map(|v| v.get("ts").and_then(|t| t.as_i64()).unwrap())
            .collect();
        assert_eq!(timestamps, vec![50, 40, 30]);
    }

    #[test]
    fn tail_returns_empty_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let got = tail(&cfg, "alpha", "ws_missing", None, 10).unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn spawn_journal_writes_events_to_disk() {
        let dir = TempDir::new().unwrap();
        let cfg = AgentUpdateJournalConfig::new(dir.path().to_path_buf());
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let handle = spawn_journal(cfg.clone(), broadcaster.clone());

        // Emit one event through the public helper.
        let update = AgentUpdate::new(
            "ws_a".to_string(),
            "cfo".to_string(),
            AgentUpdateKind::CycleCompleted {
                outcome: CycleOutcome::Succeeded,
                duration_ms: 100,
            },
        );
        publish_agent_update(&broadcaster, Some("alpha"), Some("ws_a"), update);

        // Wait briefly for the writer to consume and flush.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let path = cfg.journal_path("alpha", "ws_a");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("cycle_completed"),
            "expected event on disk, got: {}",
            contents
        );

        // Cleanup.
        handle.abort();
    }
}
