use std::path::PathBuf;

use anyhow::Context;

use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use crate::magician_v2::progress_channel_seam::types::{EventLocator, ProgressMessage};

/// Byte window for one recent-history tail read.
///
/// Sized from the live store rather than guessed: across ~300 progress shards
/// the median shard needs ~43 KB to hold its last 60 records and the 95th
/// percentile ~111 KB, so a single 256 KB read answers the execution panel for
/// all but one shard. A shard fatter than that escalates through
/// `RECENT_TAIL_MAX_BYTES`; it does not fall back to reading the whole file.
const RECENT_TAIL_WINDOW_BYTES: u64 = 256 * 1024;

/// Ceiling for the escalated recent-history window.
///
/// Reached only when a shard's records are fat enough that the first window
/// cannot prove `limit` complete records are present. Beyond this the read
/// returns however many records the ceiling-sized window held: a shard is an
/// append-only lifetime log with no upper bound on size, and a recent-activity
/// view must have an I/O cost that does not grow with it.
const RECENT_TAIL_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Window for the pending-retry lookup.
///
/// A locator is written when a delivery fails and is replayed by the 60-second
/// sweep, so the record it names is always near the end of its shard. A
/// locator older than this window is reported absent — the same outcome the
/// caller already handles for any locator whose record it cannot find.
const REPLAY_TAIL_MAX_BYTES: u64 = 1024 * 1024;

/// Record cap for the pending-retry lookup, so a shard of tiny records cannot
/// turn one window into an unbounded deserialize.
const REPLAY_TAIL_MAX_RECORDS: usize = 4096;

#[derive(Debug, Clone)]
pub struct EventLog {
    workspace_layout: ArtifactV2Workspace,
}

impl EventLog {
    pub async fn new(base_root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let base_root = base_root.into();
        Self::with_workspace_layout(ArtifactV2Workspace::new(&base_root)).await
    }

    pub async fn with_workspace_layout(
        workspace_layout: ArtifactV2Workspace,
    ) -> anyhow::Result<Self> {
        let base_root = workspace_layout.base_root().to_path_buf();
        workspace_layout
            .ensure_root()
            .await
            .with_context(|| format!("failed to create event log root at {:?}", base_root))?;
        Ok(Self { workspace_layout })
    }

    pub async fn append(&self, message: &ProgressMessage) -> anyhow::Result<()> {
        let path = self.shard_path(&message.principal, &message.workspace, &message.log_key);
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.workspace_layout
            .append_path(&path, &line)
            .await
            .with_context(|| format!("failed to append event shard {:?}", path))?;
        Ok(())
    }

    /// Resolve one locator recorded by a failed delivery.
    ///
    /// Reads a bounded tail rather than the whole shard: a shard is an
    /// append-only lifetime log that reaches tens of megabytes, and this runs
    /// once per pending locator on every replay sweep. A missing shard is
    /// reported by the provider as an empty tail, so it lands on `Ok(None)`
    /// exactly as it did before.
    pub async fn load(&self, locator: &EventLocator) -> anyhow::Result<Option<ProgressMessage>> {
        let path = self.shard_path(&locator.principal, &locator.workspace, &locator.log_key);
        let tail = self
            .workspace_layout
            .read_jsonl_tail_recent_path::<ProgressMessage, _>(
                &path,
                REPLAY_TAIL_MAX_RECORDS,
                REPLAY_TAIL_MAX_BYTES,
                REPLAY_TAIL_MAX_BYTES,
            )
            .await
            .with_context(|| format!("failed to read event shard {:?}", path))?;

        Ok(tail
            .records
            .into_iter()
            .find(|message| match locator.message_id.as_deref() {
                Some(message_id) => message.id == message_id,
                None => message.seq == locator.seq,
            }))
    }

    /// The last `limit` records of a shard, oldest-first.
    ///
    /// Reads a bounded window from the end of the file. The previous
    /// implementation read the shard whole and kept the last `limit` lines,
    /// which on the largest live shard meant allocating and UTF-8-validating
    /// 36 MB to return 60 records — on a 4-second poll.
    ///
    /// Fewer than `limit` records come back when the shard holds fewer, and
    /// also when the window ceiling could not hold `limit`. This is a
    /// recent-activity view; both callers merge the result into a capped
    /// timeline and neither treats the count as a completeness claim.
    pub async fn list_recent(
        &self,
        principal: &str,
        workspace: &str,
        log_key: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<ProgressMessage>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let path = self.shard_path(principal, workspace, log_key);
        let tail = self
            .workspace_layout
            .read_jsonl_tail_recent_path::<ProgressMessage, _>(
                &path,
                limit,
                RECENT_TAIL_WINDOW_BYTES,
                RECENT_TAIL_MAX_BYTES,
            )
            .await
            .with_context(|| format!("failed to read event shard {:?}", path))?;
        Ok(tail.records)
    }

    fn shard_path(&self, principal: &str, workspace: &str, log_key: &str) -> PathBuf {
        self.workspace_layout
            .progress_events_dir(principal, workspace)
            .join(format!("{log_key}.jsonl"))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::path::PathBuf;

    use super::{EventLog, RECENT_TAIL_MAX_BYTES, RECENT_TAIL_WINDOW_BYTES};
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::progress_channel_seam::types::EventLocator;
    use crate::magician_v2::progress_channel_seam::*;

    use crate::magician_v2::progress_channel_seam::types::{
        ProgressMessageKind, ProgressSeverity, ProgressSource,
    };

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";

    fn message(seq: u64, log_key: &str, filler: usize) -> ProgressMessage {
        ProgressMessage {
            id: format!("msg-{seq}"),
            seq,
            log_key: log_key.to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: None,
            task_id: None,
            root_task_id: None,
            root_execution_id: None,
            parent_execution_id: None,
            agent_id: None,
            ui_thread_id: None,
            step_id: None,
            routing_keys: Vec::new(),
            principal: PRINCIPAL.to_string(),
            workspace: WORKSPACE.to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "running".to_string(),
                summary: Some("x".repeat(filler)),
            },
            timestamp: seq as i64,
        }
    }

    /// Write a shard directly. The read path is what changed, and one write is
    /// far cheaper than `count` durable appends.
    async fn shard_fixture(
        workspace_layout: &ArtifactV2Workspace,
        log_key: &str,
        count: u64,
        filler: usize,
    ) -> PathBuf {
        let dir = workspace_layout.progress_events_dir(PRINCIPAL, WORKSPACE);
        tokio::fs::create_dir_all(&dir).await.expect("shard dir");
        let path = dir.join(format!("{log_key}.jsonl"));
        let mut body = Vec::new();
        for seq in 1..=count {
            body.extend(serde_json::to_vec(&message(seq, log_key, filler)).expect("encode"));
            body.push(b'\n');
        }
        tokio::fs::write(&path, body).await.expect("shard fixture");
        path
    }

    async fn event_log(root: &std::path::Path) -> (EventLog, ArtifactV2Workspace) {
        let workspace_layout = ArtifactV2Workspace::new(root);
        let log = EventLog::with_workspace_layout(workspace_layout.clone())
            .await
            .expect("event log");
        (log, workspace_layout)
    }

    #[tokio::test]
    async fn list_recent_reads_a_bounded_window_of_a_far_larger_shard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, workspace_layout) = event_log(tmp.path()).await;
        let path = shard_fixture(&workspace_layout, "task:big", 4_000, 1_024).await;

        let recent = log
            .list_recent(PRINCIPAL, WORKSPACE, "task:big", 60)
            .await
            .expect("recent");

        assert_eq!(recent.len(), 60);
        assert_eq!(recent.first().map(|entry| entry.seq), Some(3_941));
        assert_eq!(recent.last().map(|entry| entry.seq), Some(4_000));

        // The point of the fix: cost tracks the window, not the shard.
        let file_len = tokio::fs::metadata(&path).await.expect("metadata").len();
        assert!(
            file_len > 4 * RECENT_TAIL_WINDOW_BYTES,
            "fixture must dwarf the window; got {file_len} bytes"
        );
        let tail = workspace_layout
            .read_jsonl_tail_recent_path::<ProgressMessage, _>(
                &path,
                60,
                RECENT_TAIL_WINDOW_BYTES,
                RECENT_TAIL_MAX_BYTES,
            )
            .await
            .expect("bounded tail");
        assert!(
            tail.bytes_read <= RECENT_TAIL_WINDOW_BYTES,
            "read {} bytes to return 60 records",
            tail.bytes_read
        );
        assert!(
            tail.file_len > tail.bytes_read,
            "a bounded view must still report that more history exists"
        );
    }

    #[tokio::test]
    async fn list_recent_returns_every_record_when_the_shard_fits_in_one_window() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, workspace_layout) = event_log(tmp.path()).await;
        shard_fixture(&workspace_layout, "task:small", 5, 16).await;

        let recent = log
            .list_recent(PRINCIPAL, WORKSPACE, "task:small", 60)
            .await
            .expect("recent");

        assert_eq!(
            recent.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[tokio::test]
    async fn list_recent_returns_nothing_for_a_shard_that_was_never_written() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, _) = event_log(tmp.path()).await;

        let recent = log
            .list_recent(PRINCIPAL, WORKSPACE, "task:absent", 60)
            .await
            .expect("missing shard reads as empty");

        assert!(recent.is_empty());
    }

    #[tokio::test]
    async fn list_recent_escalates_past_a_record_larger_than_the_first_window() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, workspace_layout) = event_log(tmp.path()).await;
        let dir = workspace_layout.progress_events_dir(PRINCIPAL, WORKSPACE);
        tokio::fs::create_dir_all(&dir).await.expect("shard dir");
        let path = dir.join("task:fat.jsonl");

        // The newest record alone is wider than the first window, so a single
        // 256 KB read cannot prove two records are present.
        let mut body = Vec::new();
        body.extend(serde_json::to_vec(&message(1, "task:fat", 32)).expect("encode"));
        body.push(b'\n');
        body.extend(
            serde_json::to_vec(&message(
                2,
                "task:fat",
                (RECENT_TAIL_WINDOW_BYTES as usize) * 2,
            ))
            .expect("encode"),
        );
        body.push(b'\n');
        tokio::fs::write(&path, body).await.expect("fixture");

        let recent = log
            .list_recent(PRINCIPAL, WORKSPACE, "task:fat", 2)
            .await
            .expect("escalated tail");

        assert_eq!(
            recent.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, 2],
            "escalation must recover both records rather than fail or truncate"
        );
    }

    #[tokio::test]
    async fn list_recent_degrades_rather_than_failing_when_records_exceed_the_ceiling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, workspace_layout) = event_log(tmp.path()).await;
        let dir = workspace_layout.progress_events_dir(PRINCIPAL, WORKSPACE);
        tokio::fs::create_dir_all(&dir).await.expect("shard dir");
        let path = dir.join("task:huge.jsonl");

        // Three records, each half the ceiling: no window inside the ceiling
        // can hold all three.
        let filler = (RECENT_TAIL_MAX_BYTES as usize) / 2;
        let mut body = Vec::new();
        for seq in 1..=3 {
            body.extend(serde_json::to_vec(&message(seq, "task:huge", filler)).expect("encode"));
            body.push(b'\n');
        }
        tokio::fs::write(&path, body).await.expect("fixture");

        let recent = log
            .list_recent(PRINCIPAL, WORKSPACE, "task:huge", 3)
            .await
            .expect("a recent-activity view must degrade, not error");

        assert!(
            !recent.is_empty() && recent.len() < 3,
            "expected a short but non-empty window, got {} records",
            recent.len()
        );
        assert_eq!(
            recent.last().map(|entry| entry.seq),
            Some(3),
            "whatever fits must still end at the newest record"
        );
    }

    #[tokio::test]
    async fn load_resolves_a_locator_near_the_end_of_a_large_shard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, workspace_layout) = event_log(tmp.path()).await;
        shard_fixture(&workspace_layout, "task:big", 4_000, 1_024).await;

        let by_id = log
            .load(&EventLocator {
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                log_key: "task:big".to_string(),
                seq: 0,
                message_id: Some("msg-4000".to_string()),
            })
            .await
            .expect("load by id");
        assert_eq!(by_id.map(|entry| entry.seq), Some(4_000));

        let by_seq = log
            .load(&EventLocator {
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                log_key: "task:big".to_string(),
                seq: 3_999,
                message_id: None,
            })
            .await
            .expect("load by seq");
        assert_eq!(by_seq.map(|entry| entry.id), Some("msg-3999".to_string()));
    }

    #[tokio::test]
    async fn load_reports_absent_for_a_locator_older_than_the_replay_window() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (log, workspace_layout) = event_log(tmp.path()).await;
        shard_fixture(&workspace_layout, "task:big", 4_000, 1_024).await;

        // Deliberate: a pending retry is replayed within one 60-second sweep,
        // so a locator this old is already unreachable in practice. The caller
        // skips an unresolved locator, which is what it already did for any
        // locator it could not find.
        let ancient = log
            .load(&EventLocator {
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                log_key: "task:big".to_string(),
                seq: 1,
                message_id: Some("msg-1".to_string()),
            })
            .await
            .expect("load");

        assert!(ancient.is_none());
    }
}
