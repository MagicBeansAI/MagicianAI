//! Startup orphan sweep: scan per-task `llm_dispatch/<task_id>.jsonl` files
//! for any `Submitted` / `AttemptStart` entries lacking a matching terminal
//! event (Completed / Failed / Tombstoned) and emit a synthetic
//! `Tombstoned { reason: ProcessRestart }` so executor projections see a
//! resolved terminal event for every accepted job.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use magicllm::dispatch::{LlmCallLedgerEvent, TaskLedgerSink};
use magicllm::{JobId, TaskRef, TombstoneReason};
use serde_json::Value;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{debug, info, warn};

const STARTUP_ORPHAN_SWEEP_MAX_AGE: Duration = Duration::from_secs(60 * 60 * 48);
const STARTUP_ORPHAN_SWEEP_MAX_FILES: usize = 32;
const STARTUP_ORPHAN_SWEEP_PER_FILE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanSweepReport {
    pub total_orphans: usize,
    pub files_scanned: usize,
    pub files_skipped_old: usize,
    pub files_skipped_non_jsonl: usize,
    pub files_failed_metadata: usize,
    pub files_timed_out: usize,
}

/// Startup recovery only needs ledgers that could plausibly contain jobs left
/// in-flight by the previous process. Historical ledgers are still safe for a
/// full explicit sweep, but they should not be startup-critical.
pub async fn run_startup_orphan_sweep(
    base_dir: PathBuf,
    sink: std::sync::Arc<dyn TaskLedgerSink>,
) -> OrphanSweepReport {
    run_orphan_sweep_with_options(
        base_dir,
        sink,
        OrphanSweepOptions {
            max_modified_age: Some(STARTUP_ORPHAN_SWEEP_MAX_AGE),
            max_files_scanned: Some(STARTUP_ORPHAN_SWEEP_MAX_FILES),
            per_file_timeout: Some(STARTUP_ORPHAN_SWEEP_PER_FILE_TIMEOUT),
        },
    )
    .await
}

/// Walk every ledger file under `<base_dir>/llm_dispatch/` and emit
/// synthetic `Tombstoned` events for jobs without terminal records.
pub async fn run_orphan_sweep(
    base_dir: PathBuf,
    sink: std::sync::Arc<dyn TaskLedgerSink>,
) -> usize {
    run_orphan_sweep_with_options(base_dir, sink, OrphanSweepOptions::default())
        .await
        .total_orphans
}

#[derive(Debug, Clone, Copy, Default)]
struct OrphanSweepOptions {
    max_modified_age: Option<Duration>,
    max_files_scanned: Option<usize>,
    per_file_timeout: Option<Duration>,
}

async fn run_orphan_sweep_with_options(
    base_dir: PathBuf,
    sink: std::sync::Arc<dyn TaskLedgerSink>,
    options: OrphanSweepOptions,
) -> OrphanSweepReport {
    let ledger_dir = base_dir.join("llm_dispatch");
    let mut dir = match fs::read_dir(&ledger_dir).await {
        Ok(d) => d,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            debug!(?ledger_dir, "no ledger dir; orphan sweep skipped");
            return OrphanSweepReport::default();
        },
        Err(err) => {
            warn!(?err, "failed to open ledger dir for orphan sweep");
            return OrphanSweepReport::default();
        },
    };

    let now = SystemTime::now();
    let mut report = OrphanSweepReport::default();
    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e != "jsonl")
            .unwrap_or(true)
        {
            report.files_skipped_non_jsonl += 1;
            continue;
        }
        if options
            .max_files_scanned
            .map(|limit| report.files_scanned >= limit)
            .unwrap_or(false)
        {
            break;
        }
        if let Some(max_age) = options.max_modified_age {
            match entry
                .metadata()
                .await
                .and_then(|metadata| metadata.modified())
            {
                Ok(modified) if !modified_within_age(modified, now, max_age) => {
                    report.files_skipped_old += 1;
                    continue;
                },
                Ok(_) => {},
                Err(err) => {
                    report.files_failed_metadata += 1;
                    warn!(
                        ?err,
                        ?path,
                        "failed to read ledger metadata for orphan sweep"
                    );
                    continue;
                },
            }
        }
        let task_id = match path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
        {
            Some(t) => t,
            None => continue,
        };
        report.files_scanned += 1;
        let orphans = if let Some(per_file_timeout) = options.per_file_timeout {
            match tokio::time::timeout(
                per_file_timeout,
                scan_file_and_tombstone(&path, &task_id, sink.clone()),
            )
            .await
            {
                Ok(orphans) => orphans,
                Err(_) => {
                    report.files_timed_out += 1;
                    warn!(
                        ?path,
                        timeout_ms = per_file_timeout.as_millis() as u64,
                        "ledger file scan timed out during orphan sweep"
                    );
                    0
                },
            }
        } else {
            scan_file_and_tombstone(&path, &task_id, sink.clone()).await
        };
        if orphans > 0 {
            info!(task_id, orphans, "tombstoned orphaned LLM dispatch jobs");
        }
        report.total_orphans += orphans;
        tokio::task::yield_now().await;
    }
    info!(
        total_orphans = report.total_orphans,
        files_scanned = report.files_scanned,
        files_skipped_old = report.files_skipped_old,
        files_skipped_non_jsonl = report.files_skipped_non_jsonl,
        files_failed_metadata = report.files_failed_metadata,
        files_timed_out = report.files_timed_out,
        "orphan sweep complete"
    );
    report
}

fn modified_within_age(modified: SystemTime, now: SystemTime, max_age: Duration) -> bool {
    match now.duration_since(modified) {
        Ok(age) => age <= max_age,
        // If the file appears to be from the future because of clock skew,
        // scan it. Future mtimes are more likely active than historical.
        Err(_) => true,
    }
}

async fn scan_file_and_tombstone(
    path: &std::path::Path,
    task_id: &str,
    sink: std::sync::Arc<dyn TaskLedgerSink>,
) -> usize {
    let file = match fs::File::open(path).await {
        Ok(f) => f,
        Err(err) => {
            warn!(?err, ?path, "failed to open ledger file");
            return 0;
        },
    };
    let mut reader = BufReader::new(file).lines();

    let mut submitted: HashSet<String> = HashSet::new();
    let mut terminated: HashSet<String> = HashSet::new();
    while let Ok(Some(line)) = reader.next_line().await {
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event = match value.get("event") {
            Some(e) => e,
            None => continue,
        };
        let kind = event.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let job_id = event
            .get("job_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if job_id.is_empty() {
            continue;
        }
        match kind {
            "submitted" | "attempt_start" => {
                submitted.insert(job_id);
            },
            "completed" | "failed" | "tombstoned" => {
                terminated.insert(job_id);
            },
            _ => {},
        }
    }
    let orphans: Vec<String> = submitted.difference(&terminated).cloned().collect();
    if orphans.is_empty() {
        return 0;
    }
    let task_ref = TaskRef::task(task_id);
    let count = orphans.len();
    for job_id in orphans {
        sink.append(
            &task_ref,
            LlmCallLedgerEvent::Tombstoned {
                job_id: JobId::from(job_id),
                reason: TombstoneReason::ProcessRestart,
                attempts_so_far: 0,
            },
        )
        .await;
    }
    count
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use magicllm::dispatch::LlmCallLedgerEvent;
    use magicllm::TaskRef;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<(TaskRef, LlmCallLedgerEvent)>>,
    }

    #[async_trait]
    impl TaskLedgerSink for RecordingSink {
        async fn append(&self, task_ref: &TaskRef, event: LlmCallLedgerEvent) {
            self.events
                .lock()
                .expect("recording sink poisoned")
                .push((task_ref.clone(), event));
        }
    }

    #[tokio::test]
    async fn startup_orphan_sweep_tombstones_recent_unterminated_jobs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ledger_dir = temp.path().join("llm_dispatch");
        tokio::fs::create_dir_all(&ledger_dir)
            .await
            .expect("ledger dir");
        tokio::fs::write(
            ledger_dir.join("task-a.jsonl"),
            concat!(
                "{\"event\":{\"kind\":\"submitted\",\"job_id\":\"job-open\"}}\n",
                "{\"event\":{\"kind\":\"attempt_start\",\"job_id\":\"job-open\"}}\n",
                "{\"event\":{\"kind\":\"submitted\",\"job_id\":\"job-done\"}}\n",
                "{\"event\":{\"kind\":\"completed\",\"job_id\":\"job-done\"}}\n",
            ),
        )
        .await
        .expect("ledger file");

        let sink = Arc::new(RecordingSink::default());
        let report = run_startup_orphan_sweep(temp.path().to_path_buf(), sink.clone()).await;

        assert_eq!(report.total_orphans, 1);
        assert_eq!(report.files_scanned, 1);
        let events = sink.events.lock().expect("recording sink poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.task_id, "task-a");
        match &events[0].1 {
            LlmCallLedgerEvent::Tombstoned { job_id, .. } => {
                assert_eq!(job_id.to_string(), "job-open");
            },
            other => panic!("expected tombstone, got {other:?}"),
        }
    }

    #[test]
    fn modified_within_age_skips_historical_files_only() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(modified_within_age(
            now - Duration::from_secs(10),
            now,
            Duration::from_secs(60)
        ));
        assert!(!modified_within_age(
            now - Duration::from_secs(120),
            now,
            Duration::from_secs(60)
        ));
        assert!(modified_within_age(
            now + Duration::from_secs(120),
            now,
            Duration::from_secs(60)
        ));
    }
}
