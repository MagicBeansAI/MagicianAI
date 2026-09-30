//! Deduped, file-backed persistence for harness anomalies.
//!
//! Each anomaly is stored at `programs/anomalies/<signature>.json` under the
//! scope programs root. The `signature` (agent+goal+kind hash) is the dedupe
//! key: repeat detections increment `occurrences` and refresh `last_seen`
//! rather than appending a new record. Mirrors the `LearningStore` atomic-write
//! + tolerant-read pattern and stays sync so it can be called from both sync
//! and async harness contexts.

use chrono::Utc;

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};

use super::anomaly::{AnomalyStatus, HarnessAnomaly};

pub struct AnomalyStore {
    ws: ArtifactV2Workspace,
}

impl AnomalyStore {
    pub fn new(ws: ArtifactV2Workspace) -> Self {
        Self { ws }
    }

    /// Upsert by `signature`: create on first sight, otherwise increment
    /// `occurrences`, refresh `last_seen`/`summary`/`detail`, and re-open a
    /// previously `Resolved` or `FixDispatched` anomaly. A repeated
    /// `FixDispatched` anomaly is new evidence; keep its dispatch metadata so
    /// autofix cooldowns still prevent immediate duplicate repair tasks.
    pub fn upsert(&self, a: &HarnessAnomaly) -> Result<(), ArtifactV2Error> {
        let path = self.ws.programs_anomalies_path(
            &a.principal,
            &a.workspace,
            &format!("{}.json", a.signature),
        );
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let merged = match self.ws.read_json_path_sync::<HarnessAnomaly, _>(&path) {
            Ok(mut existing) => {
                existing.occurrences = existing.occurrences.saturating_add(1);
                existing.last_seen = a.last_seen;
                if matches!(
                    existing.status,
                    AnomalyStatus::Resolved | AnomalyStatus::FixDispatched
                ) {
                    existing.status = AnomalyStatus::Open; // regression re-opens
                }
                existing.detail = a.detail.clone();
                existing.summary = a.summary.clone();
                existing
            },
            Err(_) => a.clone(),
        };
        self.ws.write_json_atomic_path_sync(&path, &merged)
    }

    /// List all persisted anomalies for a scope. A missing directory yields an
    /// empty list (not an error); unreadable entries are skipped. Open
    /// anomalies sort first, then by most-recent `last_seen`.
    pub fn list(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<HarnessAnomaly>, ArtifactV2Error> {
        let dir = self.ws.programs_anomalies_dir(principal, workspace);
        let mut out = Vec::new();
        // `read_dir_path_sync` returns `Err(NotFound)` when the directory does
        // not exist yet — treat a missing dir as empty. Entries carry a
        // `file_name`; rebuild the resolved path via the scope helper so the
        // provider handles the relative/resolved conversion.
        for entry in self.ws.read_dir_path_sync(&dir).unwrap_or_default() {
            if !entry.is_file || !entry.file_name.ends_with(".json") {
                continue;
            }
            let full = self
                .ws
                .programs_anomalies_path(principal, workspace, &entry.file_name);
            if let Ok(a) = self.ws.read_json_path_sync::<HarnessAnomaly, _>(&full) {
                out.push(a);
            }
        }
        out.sort_by_key(|a| {
            (
                a.status != AnomalyStatus::Open,
                std::cmp::Reverse(a.last_seen),
            )
        });
        Ok(out)
    }

    /// Transition a stored anomaly to `status`. When dispatching a fix, stamp
    /// `last_fix_dispatch_at` and record the `fix_task_id`. Resolving or
    /// dismissing clears the dispatch metadata so a later recurrence (which
    /// `upsert` re-opens) starts a fresh fix cooldown rather than inheriting the
    /// stale `fix_task_id`.
    pub fn mark(
        &self,
        principal: &str,
        workspace: &str,
        signature: &str,
        status: AnomalyStatus,
        fix_task_id: Option<String>,
    ) -> Result<(), ArtifactV2Error> {
        let path =
            self.ws
                .programs_anomalies_path(principal, workspace, &format!("{signature}.json"));
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut a = self.ws.read_json_path_sync::<HarnessAnomaly, _>(&path)?;
        a.status = status;
        match status {
            AnomalyStatus::FixDispatched => {
                a.last_fix_dispatch_at = Some(Utc::now());
                a.fix_task_id = fix_task_id;
            },
            AnomalyStatus::Open => {
                a.last_fix_dispatch_at = None;
                a.fix_task_id = fix_task_id;
            },
            AnomalyStatus::Resolved | AnomalyStatus::Dismissed => {
                // The anomaly is being cleared. Drop the dispatch metadata so a
                // regression that `upsert` re-opens does not appear to already
                // have an in-flight fix and gets a clean autofix cooldown.
                a.last_fix_dispatch_at = None;
                a.fix_task_id = None;
            },
        }
        self.ws.write_json_atomic_path_sync(&path, &a)
    }

    /// Auto-resolve helper: mark every currently `Open`/`FixDispatched` anomaly
    /// for a given `agent_id`+`goal_id` as `Resolved`. Called after a HEALTHY
    /// cycle (or a completed fix) for that agent+goal so a subsequent success
    /// clears the standing anomaly and the reliability queue stops growing.
    /// Returns the number of anomalies transitioned. Missing/unreadable records
    /// are skipped; `Resolved`/`Dismissed` entries are left untouched.
    pub fn resolve_open_for_agent_goal(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
        goal_id: &str,
    ) -> Result<usize, ArtifactV2Error> {
        let mut resolved = 0usize;
        for a in self.list(principal, workspace)? {
            if a.agent_id != agent_id || a.goal_id != goal_id {
                continue;
            }
            if !matches!(a.status, AnomalyStatus::Open | AnomalyStatus::FixDispatched) {
                continue;
            }
            self.mark(
                principal,
                workspace,
                &a.signature,
                AnomalyStatus::Resolved,
                None,
            )?;
            resolved += 1;
        }
        Ok(resolved)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::harness::anomaly::{AnomalyKind, AnomalyStatus, HarnessAnomaly};

    #[test]
    fn upsert_dedupes_and_increments_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnomalyStore::new(ArtifactV2Workspace::new(dir.path()));
        let a = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        store.upsert(&a).unwrap();
        store.upsert(&a).unwrap();
        let all = store.list("anonymous", "default").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].occurrences, 2);
        assert_eq!(all[0].status, AnomalyStatus::Open);
    }

    #[test]
    fn list_survives_missing_dir_and_marks_status() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnomalyStore::new(ArtifactV2Workspace::new(dir.path()));
        assert!(store.list("anonymous", "default").unwrap().is_empty()); // no dir yet → empty, no error
        let a = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::CycleDropped,
            "s",
            "d",
        );
        store.upsert(&a).unwrap();
        store
            .mark(
                "anonymous",
                "default",
                &a.signature,
                AnomalyStatus::FixDispatched,
                Some("task_123".into()),
            )
            .unwrap();
        let got = &store.list("anonymous", "default").unwrap()[0];
        assert_eq!(got.status, AnomalyStatus::FixDispatched);
        assert_eq!(got.fix_task_id.as_deref(), Some("task_123"));
        assert!(got.last_fix_dispatch_at.is_some());
        store
            .mark(
                "anonymous",
                "default",
                &a.signature,
                AnomalyStatus::Open,
                None,
            )
            .unwrap();
        let reopened = &store.list("anonymous", "default").unwrap()[0];
        assert_eq!(reopened.status, AnomalyStatus::Open);
        assert_eq!(reopened.fix_task_id, None);
        assert_eq!(reopened.last_fix_dispatch_at, None);
    }

    #[test]
    fn upsert_reopens_fix_dispatched_recurrence_but_preserves_dispatch_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnomalyStore::new(ArtifactV2Workspace::new(dir.path()));
        let a = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        store.upsert(&a).unwrap();
        store
            .mark(
                "anonymous",
                "default",
                &a.signature,
                AnomalyStatus::FixDispatched,
                Some("task_123".into()),
            )
            .unwrap();
        let dispatched = store.list("anonymous", "default").unwrap()[0].clone();
        assert_eq!(dispatched.status, AnomalyStatus::FixDispatched);
        assert!(dispatched.last_fix_dispatch_at.is_some());

        store.upsert(&a).unwrap();
        let reopened = store.list("anonymous", "default").unwrap()[0].clone();
        assert_eq!(reopened.status, AnomalyStatus::Open);
        assert_eq!(reopened.fix_task_id.as_deref(), Some("task_123"));
        assert_eq!(
            reopened.last_fix_dispatch_at,
            dispatched.last_fix_dispatch_at
        );
        assert_eq!(reopened.occurrences, 2);
    }

    #[test]
    fn mark_resolved_clears_dispatch_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnomalyStore::new(ArtifactV2Workspace::new(dir.path()));
        let a = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        store.upsert(&a).unwrap();
        store
            .mark(
                "anonymous",
                "default",
                &a.signature,
                AnomalyStatus::FixDispatched,
                Some("task_123".into()),
            )
            .unwrap();
        store
            .mark(
                "anonymous",
                "default",
                &a.signature,
                AnomalyStatus::Resolved,
                None,
            )
            .unwrap();
        let got = store.list("anonymous", "default").unwrap()[0].clone();
        assert_eq!(got.status, AnomalyStatus::Resolved);
        assert_eq!(got.fix_task_id, None);
        assert_eq!(got.last_fix_dispatch_at, None);
    }

    #[test]
    fn resolve_open_for_agent_goal_clears_matching_anomalies() {
        let dir = tempfile::tempdir().unwrap();
        let store = AnomalyStore::new(ArtifactV2Workspace::new(dir.path()));
        // Two anomalies on the target agent+goal (different kinds → different
        // signatures) plus one on a different goal that must be left alone.
        let failed = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        let sandbox = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::SandboxDenied,
            "s",
            "d",
        );
        let other_goal = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:review",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        store.upsert(&failed).unwrap();
        store.upsert(&sandbox).unwrap();
        store.upsert(&other_goal).unwrap();

        let n = store
            .resolve_open_for_agent_goal("anonymous", "default", "cto", "harness:cto:standup")
            .unwrap();
        assert_eq!(n, 2);

        let all = store.list("anonymous", "default").unwrap();
        for a in &all {
            if a.goal_id == "harness:cto:standup" {
                assert_eq!(a.status, AnomalyStatus::Resolved);
            } else {
                assert_eq!(a.status, AnomalyStatus::Open); // untouched
            }
        }

        // Idempotent: a second call resolves nothing new.
        let again = store
            .resolve_open_for_agent_goal("anonymous", "default", "cto", "harness:cto:standup")
            .unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn healthy_cycle_auto_resolves_prior_open_anomaly() {
        // A prior CycleFailed anomaly stands Open for (agent, goal). A subsequent
        // HEALTHY cycle for that same agent+goal must clear it (Resolved), so the
        // reliability queue stops carrying a stale failure. Guards the new
        // resolve path used by the healthy-cycle sweep.
        let dir = tempfile::tempdir().unwrap();
        let store = AnomalyStore::new(ArtifactV2Workspace::new(dir.path()));
        let a = HarnessAnomaly::new(
            "anonymous",
            "default",
            "cto",
            "harness:cto:standup",
            AnomalyKind::CycleFailed,
            "s",
            "d",
        );
        store.upsert(&a).unwrap();
        // Precondition: the anomaly is Open before the healthy cycle.
        assert_eq!(
            store.list("anonymous", "default").unwrap()[0].status,
            AnomalyStatus::Open
        );

        // A healthy cycle for this agent+goal auto-resolves the standing anomaly.
        let n = store
            .resolve_open_for_agent_goal("anonymous", "default", "cto", "harness:cto:standup")
            .unwrap();
        assert_eq!(n, 1);

        // list() now reports it Resolved, with dispatch metadata cleared.
        let got = store.list("anonymous", "default").unwrap()[0].clone();
        assert_eq!(got.status, AnomalyStatus::Resolved);
        assert_eq!(got.fix_task_id, None);
        assert_eq!(got.last_fix_dispatch_at, None);
    }
}
