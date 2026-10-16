//! File-backed persistence for the shared company backlog.
//!
//! Each item is stored at `programs/backlog/<id>.json` under the scope programs
//! root. The `id` (source_agent+title hash) is the dedupe key: re-proposals
//! refresh `title`/`description`/`priority`/`updated_at` in place rather than
//! appending a new record. Mirrors the `AnomalyStore` atomic-write +
//! tolerant-read pattern and stays sync so it can be called from both sync and
//! async harness contexts.

use chrono::Utc;

use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};

use super::backlog::{
    BacklogDeliveryDisposition, BacklogDeliveryReview, BacklogItem, BacklogPriority, BacklogStatus,
};

pub struct BacklogStore {
    ws: ArtifactV2Workspace,
}

impl BacklogStore {
    pub fn new(ws: ArtifactV2Workspace) -> Self {
        Self { ws }
    }

    /// Upsert by `id`: create on first sight, otherwise refresh
    /// `title`/`description`/`priority`/`updated_at` from the incoming item
    /// while keeping the stored `status` (and `promoted_task_id`). A previously
    /// `Dismissed` item re-surfaced by a fresh proposal is reset to `Proposed`.
    pub fn upsert(&self, item: &BacklogItem) -> Result<(), ArtifactV2Error> {
        let path = self.ws.programs_backlog_path(
            &item.principal,
            &item.workspace,
            &format!("{}.json", item.id),
        );
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let merged = match self.ws.read_json_path_sync::<BacklogItem, _>(&path) {
            Ok(mut existing) => {
                existing.title = item.title.clone();
                existing.description = item.description.clone();
                existing.priority = item.priority;
                existing.updated_at = item.updated_at;
                if existing.status == BacklogStatus::Dismissed
                    && item.status == BacklogStatus::Proposed
                {
                    existing.status = BacklogStatus::Proposed; // re-proposed after dismissal
                }
                existing
            },
            Err(_) => item.clone(),
        };
        self.ws.write_json_atomic_path_sync(&path, &merged)
    }

    /// List all persisted backlog items for a scope. A missing directory yields
    /// an empty list (not an error); unreadable entries are skipped. `Proposed`
    /// items sort first, then by `priority` (High > Medium > Low), then by
    /// most-recent `created_at`.
    pub fn list(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<BacklogItem>, ArtifactV2Error> {
        let dir = self.ws.programs_backlog_dir(principal, workspace);
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
                .programs_backlog_path(principal, workspace, &entry.file_name);
            if let Ok(item) = self.ws.read_json_path_sync::<BacklogItem, _>(&full) {
                out.push(item);
            }
        }
        out.sort_by_key(|item| {
            let priority_rank = match item.priority {
                BacklogPriority::High => 0u8,
                BacklogPriority::Medium => 1,
                BacklogPriority::Low => 2,
            };
            (
                item.status != BacklogStatus::Proposed,
                priority_rank,
                std::cmp::Reverse(item.created_at),
            )
        });
        Ok(out)
    }

    /// Read a single backlog item by `id`. Propagates the reader's not-found
    /// error when the file is absent.
    pub fn get(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
    ) -> Result<BacklogItem, ArtifactV2Error> {
        let path = self
            .ws
            .programs_backlog_path(principal, workspace, &format!("{id}.json"));
        self.ws.read_json_path_sync::<BacklogItem, _>(&path)
    }

    /// Transition a stored item to `status`, stamping `updated_at`. When
    /// promoting, record the `promoted_task_id`.
    pub fn mark(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        status: BacklogStatus,
        promoted_task_id: Option<String>,
    ) -> Result<(), ArtifactV2Error> {
        let path = self
            .ws
            .programs_backlog_path(principal, workspace, &format!("{id}.json"));
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut item = self.ws.read_json_path_sync::<BacklogItem, _>(&path)?;
        item.status = status;
        // Only stamp the promoted task id when actually promoting, so a later
        // Dismiss/re-open never wipes the task linkage (audit trail). Mirrors
        // AnomalyStore::mark, which only records fix_task_id on FixDispatched.
        if status == BacklogStatus::Promoted {
            item.promoted_task_id = promoted_task_id.clone();
            if let Some(task_id) = promoted_task_id {
                if !item.promotion_task_ids.iter().any(|seen| seen == &task_id) {
                    item.promotion_task_ids.push(task_id);
                }
            }
            item.delivery_review = None;
        }
        item.updated_at = Utc::now();
        self.ws.write_json_atomic_path_sync(&path, &item)
    }

    /// Record the owning harness's review of a promoted task. Accepted work is
    /// terminal (`Delivered`); rework returns the same deduplicated directive
    /// to `Proposed` so a later cycle can promote a corrected attempt.
    pub fn review_delivery(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        disposition: BacklogDeliveryDisposition,
        summary: &str,
        revised_description: Option<&str>,
    ) -> Result<BacklogItem, ArtifactV2Error> {
        let path = self
            .ws
            .programs_backlog_path(principal, workspace, &format!("{id}.json"));
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut item = self.ws.read_json_path_sync::<BacklogItem, _>(&path)?;
        let task_id = item.promoted_task_id.clone().unwrap_or_default();
        item.delivery_review = Some(BacklogDeliveryReview {
            task_id,
            disposition,
            summary: summary.trim().to_string(),
            reviewed_at: Utc::now(),
        });
        item.status = match disposition {
            BacklogDeliveryDisposition::Accepted => BacklogStatus::Delivered,
            BacklogDeliveryDisposition::Rework => BacklogStatus::Proposed,
        };
        if let Some(description) = revised_description
            .map(str::trim)
            .filter(|description| !description.is_empty())
        {
            item.description = description.to_string();
        }
        item.updated_at = Utc::now();
        self.ws.write_json_atomic_path_sync(&path, &item)?;
        Ok(item)
    }

    /// Record a cross-owner promotion request on an item: a sibling officer wants this
    /// item promoted to `requested_owner_agent` (an agent outside the requester's harness
    /// scope). Keeps the item `Proposed` (no cross-scope task is created) and stamps
    /// `updated_at`, so the request surfaces in the shared "Company Backlog" block the
    /// target agent's owner sees on its next harness cycle and can promote in its own
    /// scope.
    pub fn set_owner_request(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        requested_owner_agent: &str,
        requested_by_officer: &str,
    ) -> Result<(), ArtifactV2Error> {
        let path = self
            .ws
            .programs_backlog_path(principal, workspace, &format!("{id}.json"));
        let lock = super::harness_record_lock(&path);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut item = self.ws.read_json_path_sync::<BacklogItem, _>(&path)?;
        item.requested_owner_agent = Some(requested_owner_agent.to_string());
        item.requested_by_officer = Some(requested_by_officer.to_string());
        item.updated_at = Utc::now();
        self.ws.write_json_atomic_path_sync(&path, &item)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::harness::backlog::{BacklogItem, BacklogPriority, BacklogStatus};

    #[test]
    fn upsert_list_round_trip_returns_item() {
        let dir = tempfile::tempdir().unwrap();
        let store = BacklogStore::new(ArtifactV2Workspace::new(dir.path()));
        let item = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest",
            "d",
            BacklogPriority::Medium,
        );
        store.upsert(&item).unwrap();
        let all = store.list("anonymous", "default").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, item.id);
        assert_eq!(all[0].title, "Ship weekly digest");
        assert_eq!(all[0].status, BacklogStatus::Proposed);
    }

    /// Every mutator here is read `{id}.json`, change a field, write it whole
    /// back, and the store is built per request holding no lock — so two
    /// handlers touching **different fields of the same item** each read the
    /// same record and each wrote their own version. The later write took the
    /// whole record and the earlier field was gone, with the earlier call
    /// reporting success.
    ///
    /// It has to be two different mutators: `set_owner_request` writes both of
    /// its fields from one read, so racing it against itself leaves a
    /// consistent record even when an update is lost. A first version of this
    /// test did exactly that and passed with the lock removed.
    #[test]
    fn a_concurrent_upsert_does_not_drop_an_owner_request() {
        for round in 0..24 {
            let dir = tempfile::tempdir().unwrap();
            let ws = ArtifactV2Workspace::new(dir.path());
            let store = BacklogStore::new(ws.clone());
            let item = BacklogItem::new(
                "anonymous",
                "default",
                "growth",
                "original title",
                "d",
                BacklogPriority::Medium,
            );
            store.upsert(&item).unwrap();
            let id = item.id.clone();

            // One writer stamps the owner-request fields; the other refreshes
            // the title through `upsert`, which preserves owner fields *if* it
            // reads a record that already has them.
            let owner_writer = {
                let ws = ws.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    BacklogStore::new(ws)
                        .set_owner_request("anonymous", "default", &id, "agent-x", "officer-x")
                        .unwrap();
                })
            };
            let title_writer = {
                let ws = ws.clone();
                let mut refreshed = item.clone();
                refreshed.title = "refreshed title".to_string();
                std::thread::spawn(move || {
                    BacklogStore::new(ws).upsert(&refreshed).unwrap();
                })
            };
            owner_writer.join().expect("owner writer");
            title_writer.join().expect("title writer");

            let stored = store.get("anonymous", "default", &id).unwrap();
            assert_eq!(
                stored.requested_owner_agent.as_deref(),
                Some("agent-x"),
                "round {round}: the owner request was dropped by the concurrent upsert"
            );
            assert_eq!(
                stored.title, "refreshed title",
                "round {round}: the title refresh was dropped by the concurrent owner request"
            );
        }
    }

    #[test]
    fn upsert_dedupes_and_refreshes_description_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let store = BacklogStore::new(ArtifactV2Workspace::new(dir.path()));
        let first = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest",
            "original description",
            BacklogPriority::Medium,
        );
        store.upsert(&first).unwrap();
        let second = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest",
            "updated description",
            BacklogPriority::Medium,
        );
        store.upsert(&second).unwrap();
        let all = store.list("anonymous", "default").unwrap();
        assert_eq!(all.len(), 1, "same source_agent+title → one item");
        assert_eq!(all[0].description, "updated description");
    }

    #[test]
    fn mark_promoted_reflects_status_and_task_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = BacklogStore::new(ArtifactV2Workspace::new(dir.path()));
        let item = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest",
            "d",
            BacklogPriority::Medium,
        );
        store.upsert(&item).unwrap();
        store
            .mark(
                "anonymous",
                "default",
                &item.id,
                BacklogStatus::Promoted,
                Some("task_456".into()),
            )
            .unwrap();
        let got = store.get("anonymous", "default", &item.id).unwrap();
        assert_eq!(got.status, BacklogStatus::Promoted);
        assert_eq!(got.promoted_task_id.as_deref(), Some("task_456"));
        assert_eq!(got.promotion_task_ids, vec!["task_456"]);
    }

    #[test]
    fn delivery_review_accepts_or_returns_item_for_rework() {
        let dir = tempfile::tempdir().unwrap();
        let store = BacklogStore::new(ArtifactV2Workspace::new(dir.path()));
        let item = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Ship weekly digest",
            "original scope",
            BacklogPriority::Medium,
        );
        store.upsert(&item).unwrap();
        store
            .mark(
                "anonymous",
                "default",
                &item.id,
                BacklogStatus::Promoted,
                Some("task_1".into()),
            )
            .unwrap();

        let rework = store
            .review_delivery(
                "anonymous",
                "default",
                &item.id,
                BacklogDeliveryDisposition::Rework,
                "The run only produced discovery notes.",
                Some("Build and publish the digest artifact."),
            )
            .unwrap();
        assert_eq!(rework.status, BacklogStatus::Proposed);
        assert_eq!(rework.description, "Build and publish the digest artifact.");
        assert_eq!(
            rework
                .delivery_review
                .as_ref()
                .map(|review| review.disposition),
            Some(BacklogDeliveryDisposition::Rework)
        );

        store
            .mark(
                "anonymous",
                "default",
                &item.id,
                BacklogStatus::Promoted,
                Some("task_2".into()),
            )
            .unwrap();
        let delivered = store
            .review_delivery(
                "anonymous",
                "default",
                &item.id,
                BacklogDeliveryDisposition::Accepted,
                "The requested digest now exists and runs on schedule.",
                None,
            )
            .unwrap();
        assert_eq!(delivered.status, BacklogStatus::Delivered);
        assert_eq!(delivered.promotion_task_ids, vec!["task_1", "task_2"]);
    }

    #[test]
    fn list_sorts_proposed_high_priority_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = BacklogStore::new(ArtifactV2Workspace::new(dir.path()));
        let medium = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "Medium item",
            "d",
            BacklogPriority::Medium,
        );
        let high = BacklogItem::new(
            "anonymous",
            "default",
            "growth",
            "High item",
            "d",
            BacklogPriority::High,
        );
        store.upsert(&medium).unwrap();
        store.upsert(&high).unwrap();
        let all = store.list("anonymous", "default").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, high.id, "High priority sorts before Medium");
        assert_eq!(all[1].id, medium.id);
    }

    #[test]
    fn set_owner_request_records_cross_owner_flag_and_keeps_proposed() {
        let dir = tempfile::tempdir().unwrap();
        let store = BacklogStore::new(ArtifactV2Workspace::new(dir.path()));
        let item = BacklogItem::new(
            "anonymous",
            "default",
            "cro",
            "Instrument funnel events",
            "d",
            BacklogPriority::High,
        );
        store.upsert(&item).unwrap();
        store
            .set_owner_request("anonymous", "default", &item.id, "cto-agent", "cro")
            .unwrap();
        let got = store.get("anonymous", "default", &item.id).unwrap();
        assert_eq!(got.requested_owner_agent.as_deref(), Some("cto-agent"));
        assert_eq!(got.requested_by_officer.as_deref(), Some("cro"));
        // Cross-owner request does NOT promote — no cross-scope task; stays reviewable.
        assert_eq!(got.status, BacklogStatus::Proposed);
    }
}
