use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::Semaphore;
use uuid::Uuid;

use super::ffmpeg::run_ffmpeg;

const MAX_CONCURRENT_JOBS: usize = 2;
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug)]
pub enum MediaJobStatus {
    Running { progress_pct: Option<f32> },
    Completed { output_path: PathBuf },
    Failed { error: String },
}

struct MediaJobState {
    status: MediaJobStatus,
}

pub struct MediaJobRegistry {
    jobs: RwLock<HashMap<String, MediaJobState>>,
    semaphore: Arc<Semaphore>,
}

impl MediaJobRegistry {
    pub fn new() -> Self {
        Self {
            jobs: RwLock::new(HashMap::new()),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_JOBS)),
        }
    }

    /// Spawn a background ffmpeg job. `on_complete` runs after a successful
    /// ffmpeg exit, before the status flips to `Completed` — this is where a
    /// caller hooks post-processing (e.g. registering the output as a chat
    /// attachment) without this module needing to know what that means.
    pub async fn spawn(
        self: &Arc<Self>,
        ffmpeg_bin: String,
        args: Vec<String>,
        total_duration_secs: Option<f64>,
        output_path: PathBuf,
        on_complete: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<String, String> {
        let permit = match Arc::clone(&self.semaphore).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return Err(format!(
                    "too many concurrent media_edit jobs (max {MAX_CONCURRENT_JOBS}); retry later"
                ))
            },
        };

        let job_id = Uuid::new_v4().simple().to_string();
        lock_jobs(&self.jobs).insert(
            job_id.clone(),
            MediaJobState {
                status: MediaJobStatus::Running {
                    progress_pct: Some(0.0),
                },
            },
        );

        let registry = Arc::clone(self);
        let job_id_task = job_id.clone();
        tokio::spawn(async move {
            let _permit = permit; // released when this task ends

            // Synchronous, in-order updates: `on_progress` runs inline inside
            // `run_ffmpeg`'s stdout-reading loop (this same task), so each
            // tick applies strictly before the next one is read and strictly
            // before `run` returns. A `tokio::sync::RwLock` would need each
            // tick to `.await` a lock from inside a plain `FnMut`, which can
            // only be bridged by spawning a detached task per tick — and
            // detached tasks have no ordering guarantee relative to each
            // other or to the terminal status write below, so a late one
            // could silently revert a completed job back to "running". A
            // `std::sync::RwLock` held only across the synchronous
            // read-modify-write (never across an `.await`) removes the race
            // entirely and is the standard idiom for a critical section this
            // short.
            let progress_registry = Arc::clone(&registry);
            let progress_job_id = job_id_task.clone();
            let run = run_ffmpeg(&ffmpeg_bin, &args, total_duration_secs, move |pct| {
                if let Some(state) = lock_jobs(&progress_registry.jobs).get_mut(&progress_job_id) {
                    state.status = MediaJobStatus::Running { progress_pct: pct };
                }
            });

            let outcome = tokio::time::timeout(JOB_TIMEOUT, run).await;
            let final_status = match outcome {
                Ok(Ok(())) => match on_complete() {
                    Ok(()) => MediaJobStatus::Completed { output_path },
                    Err(error) => MediaJobStatus::Failed {
                        error: format!("ffmpeg succeeded but post-processing failed: {error}"),
                    },
                },
                Ok(Err(error)) => MediaJobStatus::Failed { error },
                Err(_) => MediaJobStatus::Failed {
                    error: format!("ffmpeg job timed out after {}s", JOB_TIMEOUT.as_secs()),
                },
            };
            if let Some(state) = lock_jobs(&registry.jobs).get_mut(&job_id_task) {
                state.status = final_status;
            }
        });

        Ok(job_id)
    }

    pub async fn status(&self, job_id: &str) -> Option<MediaJobStatus> {
        lock_jobs_read(&self.jobs)
            .get(job_id)
            .map(|s| s.status.clone())
    }
}

/// A panic while holding the write lock (there is none in this module's own
/// code, but a future change could add one) poisons the lock; recovering the
/// inner guard rather than propagating keeps one bad job from wedging every
/// other job's status forever.
fn lock_jobs(
    jobs: &RwLock<HashMap<String, MediaJobState>>,
) -> std::sync::RwLockWriteGuard<'_, HashMap<String, MediaJobState>> {
    jobs.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_jobs_read(
    jobs: &RwLock<HashMap<String, MediaJobState>>,
) -> std::sync::RwLockReadGuard<'_, HashMap<String, MediaJobState>> {
    jobs.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_ffmpeg_args(output: &std::path::Path) -> Vec<String> {
        [
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=64x64:rate=5",
        ]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(output.display().to_string()))
        .collect()
    }

    #[tokio::test]
    async fn spawns_and_completes_a_job() {
        let registry = Arc::new(MediaJobRegistry::new());
        let dir = std::env::temp_dir().join(format!("media_edit_job_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mp4");
        let args = synthetic_ffmpeg_args(&output);

        let job_id = registry
            .spawn(
                super::super::ffmpeg::ffmpeg_bin(),
                args,
                Some(1.0),
                output.clone(),
                || Ok(()),
            )
            .await
            .unwrap();

        let mut status = registry.status(&job_id).await;
        for _ in 0..100 {
            if matches!(
                status,
                Some(MediaJobStatus::Completed { .. }) | Some(MediaJobStatus::Failed { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            status = registry.status(&job_id).await;
        }

        match status {
            Some(MediaJobStatus::Completed { output_path }) => assert_eq!(output_path, output),
            other => panic!("expected Completed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn failed_ffmpeg_invocation_reports_failed_status() {
        let registry = Arc::new(MediaJobRegistry::new());
        let bad_args = vec![
            "-y".to_string(),
            "-i".to_string(),
            "/no/such/input.mp4".to_string(),
            "/tmp/never.mp4".to_string(),
        ];

        let job_id = registry
            .spawn(
                super::super::ffmpeg::ffmpeg_bin(),
                bad_args,
                None,
                PathBuf::from("/tmp/never.mp4"),
                || Ok(()),
            )
            .await
            .unwrap();

        let mut status = registry.status(&job_id).await;
        for _ in 0..100 {
            if matches!(
                status,
                Some(MediaJobStatus::Completed { .. }) | Some(MediaJobStatus::Failed { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            status = registry.status(&job_id).await;
        }
        assert!(matches!(status, Some(MediaJobStatus::Failed { .. })));
    }

    #[tokio::test]
    async fn on_complete_failure_marks_the_job_failed() {
        let registry = Arc::new(MediaJobRegistry::new());
        let dir =
            std::env::temp_dir().join(format!("media_edit_oncomplete_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mp4");
        let args = synthetic_ffmpeg_args(&output);

        let job_id = registry
            .spawn(
                super::super::ffmpeg::ffmpeg_bin(),
                args,
                Some(1.0),
                output,
                || Err("post-processing exploded".to_string()),
            )
            .await
            .unwrap();

        let mut status = registry.status(&job_id).await;
        for _ in 0..100 {
            if matches!(
                status,
                Some(MediaJobStatus::Completed { .. }) | Some(MediaJobStatus::Failed { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            status = registry.status(&job_id).await;
        }
        match status {
            Some(MediaJobStatus::Failed { error }) => {
                assert!(error.contains("post-processing exploded"))
            },
            other => panic!("expected Failed, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rejects_spawn_past_concurrency_cap() {
        let registry = Arc::new(MediaJobRegistry::new());
        let dir = std::env::temp_dir().join(format!("media_edit_cap_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Fill the cap with slow-ish jobs (2s synthetic clips) so they're still
        // running when we try the one that should be rejected.
        let mut job_ids = Vec::new();
        for i in 0..MAX_CONCURRENT_JOBS {
            let output = dir.join(format!("slow_{i}.mp4"));
            let args: Vec<String> = [
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=64x64:rate=5",
            ]
            .into_iter()
            .map(String::from)
            .chain(std::iter::once(output.display().to_string()))
            .collect();
            job_ids.push(
                registry
                    .spawn(
                        super::super::ffmpeg::ffmpeg_bin(),
                        args,
                        Some(2.0),
                        output,
                        || Ok(()),
                    )
                    .await
                    .unwrap(),
            );
        }

        let overflow = registry
            .spawn(
                super::super::ffmpeg::ffmpeg_bin(),
                synthetic_ffmpeg_args(&dir.join("overflow.mp4")),
                Some(1.0),
                dir.join("overflow.mp4"),
                || Ok(()),
            )
            .await;
        assert!(overflow.is_err());

        // Drain so the temp dir can be cleaned up without racing running jobs.
        for job_id in job_ids {
            for _ in 0..100 {
                if matches!(
                    registry.status(&job_id).await,
                    Some(MediaJobStatus::Completed { .. }) | Some(MediaJobStatus::Failed { .. })
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression test for the progress-update race: polling status
    /// throughout a run must never observe `progress_pct` go backward, and
    /// once the job reaches a terminal state it must never revert to
    /// `Running`. Both were possible when progress updates were applied via
    /// detached `tokio::spawn` tasks racing against each other and against
    /// the terminal status write; synchronous, in-order updates rule it out.
    #[tokio::test]
    async fn progress_never_regresses_and_terminal_status_never_reverts() {
        let registry = Arc::new(MediaJobRegistry::new());
        let dir = std::env::temp_dir().join(format!(
            "media_edit_progress_order_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mp4");
        // Long enough (3s of source at a real frame rate) that -progress
        // emits several ticks before the job finishes, giving this test a
        // real chance to observe more than one Running sample.
        let args: Vec<String> = [
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=3:size=64x64:rate=25",
        ]
        .into_iter()
        .map(String::from)
        .chain(std::iter::once(output.display().to_string()))
        .collect();

        let job_id = registry
            .spawn(
                super::super::ffmpeg::ffmpeg_bin(),
                args,
                Some(3.0),
                output,
                || Ok(()),
            )
            .await
            .unwrap();

        let mut last_pct: Option<f32> = None;
        let mut saw_completed = false;
        for _ in 0..300 {
            match registry.status(&job_id).await {
                Some(MediaJobStatus::Running {
                    progress_pct: Some(pct),
                }) => {
                    assert!(
                        !saw_completed,
                        "status reverted to Running after already observing Completed"
                    );
                    if let Some(last) = last_pct {
                        assert!(pct >= last, "progress regressed: saw {last} then {pct}");
                    }
                    last_pct = Some(pct);
                },
                Some(MediaJobStatus::Running { progress_pct: None }) => {
                    assert!(!saw_completed, "status reverted to Running after Completed");
                },
                Some(MediaJobStatus::Completed { .. }) => {
                    saw_completed = true;
                    break;
                },
                Some(MediaJobStatus::Failed { error }) => panic!("job failed: {error}"),
                None => panic!("job_id vanished from the registry"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            saw_completed,
            "job did not reach Completed within the poll budget"
        );

        // Polling after completion must keep reporting Completed, never
        // Running — the exact symptom a late stray progress-update task
        // would have produced.
        for _ in 0..5 {
            assert!(matches!(
                registry.status(&job_id).await,
                Some(MediaJobStatus::Completed { .. })
            ));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
