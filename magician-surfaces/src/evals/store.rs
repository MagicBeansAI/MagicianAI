//! File-backed history of eval runs.
//!
//! One JSON file per run at `evals/runs/<lane_id>/<run_id>.json` under the scope
//! root. Mirrors [`magician::magician_v2::harness::backlog_store::BacklogStore`]'s
//! atomic-write + tolerant-read pattern and stays sync so it can be called from
//! both the sync runner path and async request handlers.
//!
//! # Why append-only
//!
//! This is the one thing the `/evals` feature persists, and the only reason it
//! persists anything is the trend: "this lane has failed the last three times",
//! "this lane got 4× slower last week". That series cannot be backfilled — the
//! runs are gone and re-running them is minutes of model time and, for the
//! `provider_keys` lanes, real money. So a new run never replaces an old one:
//! ids are unique by construction ([`EvalRun::new_run_id`]) and
//! [`EvalRunStore::append`] refuses to write over an existing record rather than
//! silently dropping a data point.
//!
//! # Why a directory per lane
//!
//! The lane is the unit everything reads by: the page renders one lane's
//! history, and the trend is per-lane. A directory per lane makes that read
//! touch only that lane's files instead of every run ever recorded, and it makes
//! the lane filter a fact about the layout rather than a predicate that could be
//! forgotten.
//!
//! # Why a corrupt record is skipped rather than fatal
//!
//! A half-written or hand-edited file is one bad row. Propagating it would blank
//! the whole page — including the 30 lanes that are fine — which is a strictly
//! worse outcome than rendering 30 of 31 lanes. It is logged at `warn!` so the
//! gap is discoverable instead of merely invisible.

use std::path::Path;

use tracing::warn;

use magician::magician_v2::artifact_v2::{
    workspace::{ArtifactV2Workspace, WorkspaceFileEntry},
    ArtifactV2Error,
};

use super::run::EvalRun;

pub struct EvalRunStore {
    ws: ArtifactV2Workspace,
}

impl EvalRunStore {
    pub fn new(ws: ArtifactV2Workspace) -> Self {
        Self { ws }
    }

    /// Records a completed run. The scope and lane come from the record itself,
    /// so a run can only ever be filed under the scope it names.
    ///
    /// Refuses to overwrite an existing record for the same `(lane_id, run_id)`.
    /// The check is not atomic with the write, so it is a guard against a
    /// caller that reused an id — not against two processes racing on the same
    /// id, which [`EvalRun::new_run_id`]'s random suffix is what actually
    /// prevents. It is here because the failure it catches is invisible: an
    /// overwritten run is a silently missing data point in a series whose whole
    /// value is being complete.
    pub fn append(&self, run: &EvalRun) -> Result<(), ArtifactV2Error> {
        let path = self.ws.evals_run_path(
            &run.principal,
            &run.workspace,
            &run.lane_id,
            &format!("{}.json", run.run_id),
        );
        if self.ws.exists_path_sync(&path)? {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "duplicate eval run id `{}` for lane `{}`: run history is append-only \
                 and this would overwrite an existing record",
                run.run_id, run.lane_id
            )));
        }
        // The record's fields are public, so the log-tail bound cannot depend on
        // the caller having gone through `set_log_tail`. Applying it here makes
        // it a property of what is on disk.
        let mut record = run.clone();
        record.enforce_log_tail_bound();
        self.ws.write_json_atomic_path_sync(&path, &record)
    }

    /// Runs in this scope, newest first. `lane` `None` returns every lane.
    ///
    /// A scope that has never run anything lists empty rather than erroring —
    /// that is the normal state of a fresh install, not a fault. Unreadable
    /// individual records are skipped with a `warn!`.
    pub fn list(
        &self,
        principal: &str,
        workspace: &str,
        lane: Option<&str>,
    ) -> Result<Vec<EvalRun>, ArtifactV2Error> {
        let mut out = Vec::new();
        match lane {
            Some(lane) => self.collect_lane(principal, workspace, lane, &mut out),
            None => {
                let root = self.ws.evals_runs_dir(principal, workspace);
                for entry in self.read_dir_or_empty(&root) {
                    if !entry.is_dir {
                        continue;
                    }
                    // The directory name is the (already path-safe) lane id, so
                    // re-deriving the path through the helper is a no-op that
                    // keeps every read going through one place.
                    self.collect_lane(principal, workspace, &entry.file_name, &mut out);
                }
            },
        }
        // Newest first, tie-broken by id so two runs that started in the same
        // millisecond still have one stable order — an unstable order would make
        // the page's rows shuffle between reloads for no visible reason.
        out.sort_by(|left, right| {
            right
                .started_at_ms
                .cmp(&left.started_at_ms)
                .then_with(|| right.run_id.cmp(&left.run_id))
        });
        Ok(out)
    }

    fn collect_lane(&self, principal: &str, workspace: &str, lane: &str, out: &mut Vec<EvalRun>) {
        let dir = self.ws.evals_run_lane_dir(principal, workspace, lane);
        for entry in self.read_dir_or_empty(&dir) {
            // `.json` also excludes the `.<name>.<uuid>.tmp` files an atomic
            // write leaves in this directory while it is in flight.
            if !entry.is_file || !entry.file_name.ends_with(".json") {
                continue;
            }
            let path = self
                .ws
                .evals_run_path(principal, workspace, lane, &entry.file_name);
            match self.ws.read_json_path_sync::<EvalRun, _>(&path) {
                Ok(run) => out.push(run),
                Err(error) => warn!(
                    path = %path.display(),
                    %error,
                    "skipping unreadable eval run record"
                ),
            }
        }
    }

    /// Directory entries, distinguishing "nothing has run yet" (empty, normal)
    /// from "the directory could not be read" (empty, and loud about it).
    fn read_dir_or_empty(&self, dir: &Path) -> Vec<WorkspaceFileEntry> {
        match self.ws.read_dir_path_sync(dir) {
            Ok(entries) => entries,
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Vec::new()
            },
            Err(error) => {
                warn!(
                    dir = %dir.display(),
                    %error,
                    "eval run directory could not be listed; reporting no runs for it"
                );
                Vec::new()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::registry::EvalRequirement;
    use crate::evals::run::{EvalRunStatus, MAX_LOG_TAIL_BYTES};

    fn store() -> (tempfile::TempDir, EvalRunStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = EvalRunStore::new(ArtifactV2Workspace::new(dir.path()));
        (dir, store)
    }

    fn sample_run(lane: &str, run_id: &str, started_at_ms: i64) -> EvalRun {
        EvalRun::new(run_id, lane, "anonymous", "default", started_at_ms)
    }

    #[test]
    fn a_written_run_reads_back() {
        let (_dir, store) = store();
        let mut written = sample_run("eval-monitor-golden", "evr_1", 1_700_000_000_000);
        written.status = EvalRunStatus::Failed;
        written.duration_ms = 4_200;
        written.exit_code = Some(2);
        written.task_id = Some("task_42".into());
        written.services = vec![EvalRequirement::Ollama];
        written.report_href = Some("/evals/reports/monitor".into());
        written.set_log_tail("assertion failed\n");
        store.append(&written).unwrap();

        let all = store.list("anonymous", "default", None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], written, "every field must survive the round trip");
    }

    /// Append-only: a second run of the same lane must NOT replace the first.
    /// Trend cannot be backfilled — this is why the record exists at all.
    #[test]
    fn runs_accumulate_rather_than_overwrite() {
        let (_dir, store) = store();
        let mut first = sample_run("eval-monitor-golden", "evr_1", 1_000);
        first.status = EvalRunStatus::Passed;
        let mut second = sample_run("eval-monitor-golden", "evr_2", 2_000);
        second.status = EvalRunStatus::Failed;
        store.append(&first).unwrap();
        store.append(&second).unwrap();

        let all = store
            .list("anonymous", "default", Some("eval-monitor-golden"))
            .unwrap();
        assert_eq!(all.len(), 2, "the earlier run was destroyed");
        // The point of keeping both: the series says pass-then-fail.
        assert_eq!(
            all.iter().map(|r| r.status).collect::<Vec<_>>(),
            vec![EvalRunStatus::Failed, EvalRunStatus::Passed]
        );
    }

    /// Reusing a run id is the one way the append-only guarantee can be broken
    /// from the outside, and an overwritten run is invisible once it is gone.
    #[test]
    fn appending_a_duplicate_run_id_is_refused_rather_than_silently_overwriting() {
        let (_dir, store) = store();
        let mut first = sample_run("lane-a", "evr_same", 1_000);
        first.status = EvalRunStatus::Passed;
        store.append(&first).unwrap();

        let mut again = sample_run("lane-a", "evr_same", 2_000);
        again.status = EvalRunStatus::Failed;
        let error = store
            .append(&again)
            .expect_err("a reused run id must not overwrite history");
        let message = match &error {
            ArtifactV2Error::InvalidRequest(message) => message.clone(),
            other => panic!("expected an InvalidRequest, got {other}"),
        };
        assert!(message.contains("evr_same"), "{message}");

        let all = store.list("anonymous", "default", None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].status, EvalRunStatus::Passed, "the original stands");
    }

    #[test]
    fn listing_filters_by_lane() {
        let (_dir, store) = store();
        store.append(&sample_run("lane-a", "evr_1", 1_000)).unwrap();
        store.append(&sample_run("lane-a", "evr_2", 2_000)).unwrap();
        store.append(&sample_run("lane-b", "evr_3", 3_000)).unwrap();

        let lane_a = store.list("anonymous", "default", Some("lane-a")).unwrap();
        assert_eq!(
            lane_a.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
            vec!["evr_2", "evr_1"]
        );
        assert!(lane_a.iter().all(|r| r.lane_id == "lane-a"));

        let everything = store.list("anonymous", "default", None).unwrap();
        assert_eq!(everything.len(), 3, "None must mean all lanes, not none");

        // A lane nobody has run is empty, not an error.
        assert!(store
            .list("anonymous", "default", Some("lane-never-run"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn runs_are_newest_first() {
        let (_dir, store) = store();
        // Appended out of order, and across two lanes, so the ordering cannot be
        // an accident of insertion order or of directory traversal order.
        store
            .append(&sample_run("lane-b", "evr_mid", 2_000))
            .unwrap();
        store
            .append(&sample_run("lane-a", "evr_old", 1_000))
            .unwrap();
        store
            .append(&sample_run("lane-a", "evr_new", 3_000))
            .unwrap();

        let all = store.list("anonymous", "default", None).unwrap();
        assert_eq!(
            all.iter().map(|r| r.started_at_ms).collect::<Vec<_>>(),
            vec![3_000, 2_000, 1_000]
        );
    }

    /// Two runs in the same millisecond must still have one stable order, or the
    /// page's rows shuffle between reloads.
    #[test]
    fn runs_that_started_in_the_same_millisecond_have_a_stable_order() {
        let (_dir, store) = store();
        store
            .append(&sample_run("lane-a", "evr_aaa", 5_000))
            .unwrap();
        store
            .append(&sample_run("lane-a", "evr_bbb", 5_000))
            .unwrap();

        let first = store.list("anonymous", "default", None).unwrap();
        let second = store.list("anonymous", "default", None).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
            vec!["evr_bbb", "evr_aaa"]
        );
    }

    /// Scope isolation, matching every other surface in this codebase.
    #[test]
    fn runs_never_cross_scope() {
        let (_dir, store) = store();
        store
            .append(&sample_run("lane-a", "evr_mine", 1_000))
            .unwrap();

        let mut other_principal = sample_run("lane-a", "evr_theirs", 2_000);
        other_principal.principal = "someone-else".into();
        store.append(&other_principal).unwrap();

        let mut other_workspace = sample_run("lane-a", "evr_elsewhere", 3_000);
        other_workspace.workspace = "other-workspace".into();
        store.append(&other_workspace).unwrap();

        for lane in [None, Some("lane-a")] {
            let mine = store.list("anonymous", "default", lane).unwrap();
            assert_eq!(
                mine.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
                vec!["evr_mine"],
                "lane filter {lane:?}"
            );
            assert!(mine
                .iter()
                .all(|r| r.principal == "anonymous" && r.workspace == "default"));
        }

        // And the neighbours can still see their own.
        assert_eq!(
            store.list("someone-else", "default", None).unwrap().len(),
            1
        );
        assert_eq!(
            store
                .list("anonymous", "other-workspace", None)
                .unwrap()
                .len(),
            1
        );
    }

    /// A corrupt file must not take out the whole listing — one bad run record
    /// cannot be allowed to blank the page.
    #[test]
    fn a_corrupt_run_file_is_skipped_not_fatal() {
        let (dir, store) = store();
        store
            .append(&sample_run("lane-a", "evr_good_1", 1_000))
            .unwrap();
        store
            .append(&sample_run("lane-a", "evr_good_2", 3_000))
            .unwrap();

        let ws = ArtifactV2Workspace::new(dir.path());
        let truncated = ws.evals_run_path("anonymous", "default", "lane-a", "evr_torn.json");
        std::fs::write(&truncated, b"{\"schema_version\": 1, \"run_id\"").unwrap();
        // Valid JSON, wrong shape — the other way a record goes bad.
        let wrong_shape = ws.evals_run_path("anonymous", "default", "lane-a", "evr_alien.json");
        std::fs::write(&wrong_shape, b"{\"totally\": \"different\"}").unwrap();
        // Neither a record nor a `.json` file; must simply be ignored.
        let stray = ws.evals_run_path("anonymous", "default", "lane-a", "notes.txt");
        std::fs::write(&stray, b"scratch").unwrap();

        for lane in [None, Some("lane-a")] {
            let all = store.list("anonymous", "default", lane).unwrap();
            assert_eq!(
                all.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>(),
                vec!["evr_good_2", "evr_good_1"],
                "lane filter {lane:?}: a bad row must cost one row, not the page"
            );
        }
    }

    /// Multi-byte safety: truncation must not split a UTF-8 char — and the bound
    /// has to hold for what is written to DISK, not only for what
    /// `set_log_tail` produced, because the field is public.
    #[test]
    fn an_oversized_log_tail_is_truncated_on_a_char_boundary() {
        let (_dir, store) = store();
        let mut written = sample_run("lane-a", "evr_1", 1_000);
        // 4-byte characters, so a naive byte cut lands mid-character.
        written.log_tail = Some("\u{1F4A5}".repeat(MAX_LOG_TAIL_BYTES));
        store.append(&written).unwrap();

        let all = store.list("anonymous", "default", None).unwrap();
        let stored = all[0].log_tail.as_deref().unwrap();
        assert!(
            stored.len() <= MAX_LOG_TAIL_BYTES,
            "stored {} bytes on disk, bound is {MAX_LOG_TAIL_BYTES}",
            stored.len()
        );
        // Reading it back at all proves it was valid UTF-8 and valid JSON: a
        // split character would have failed one or the other.
        assert!(stored.ends_with('\u{1F4A5}'));
        assert!(!stored.contains('\u{FFFD}'));
    }

    /// A fresh install has no `evals/` directory at all. That is the normal
    /// state, not a fault, and it must not surface as an error on the page.
    #[test]
    fn a_scope_that_has_never_run_anything_lists_empty() {
        let (_dir, store) = store();
        assert!(store.list("anonymous", "default", None).unwrap().is_empty());
        assert!(store
            .list("anonymous", "default", Some("lane-a"))
            .unwrap()
            .is_empty());
    }

    /// The status is the whole point of the record, so the distinction between
    /// "judged and failed" and "never judged" has to survive the disk.
    #[test]
    fn an_interrupted_run_reads_back_as_interrupted() {
        let (_dir, store) = store();
        let interrupted = sample_run("lane-a", "evr_1", 1_000); // defaults to Interrupted
        store.append(&interrupted).unwrap();

        let all = store.list("anonymous", "default", None).unwrap();
        assert_eq!(all[0].status, EvalRunStatus::Interrupted);
        assert!(!all[0].status.is_judgement());
    }

    /// The lane id is a directory name. A lane whose id is not path-shaped is
    /// already a `parse_error` in the registry, but it must not be able to write
    /// outside the scope even so.
    #[test]
    fn a_lane_id_that_is_not_path_shaped_cannot_escape_the_scope() {
        let (dir, store) = store();
        store
            .append(&sample_run("../../escaped", "evr_1", 1_000))
            .unwrap();
        assert!(
            !dir.path().join("escaped").exists(),
            "a lane id must not be able to write outside its scope"
        );
        assert_eq!(store.list("anonymous", "default", None).unwrap().len(), 1);
    }
}
