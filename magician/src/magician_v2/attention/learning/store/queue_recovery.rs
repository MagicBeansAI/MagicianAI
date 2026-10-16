//! Versioned, bounded repair of known worker failures through the normal queue.
//! Preserve the prior terminal verdict before permitting one new retry budget.

use super::*;

const RANK_REPAIR: &str = "rank_writer_starvation_v1";
const SEMANTIC_REPAIR: &str = "semantic_schema_output_v1";

pub(super) fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS attention_queue_recoveries (
            queue_kind TEXT NOT NULL, work_id TEXT NOT NULL, repair_key TEXT NOT NULL,
            principal TEXT NOT NULL, workspace TEXT NOT NULL,
            previous_status TEXT NOT NULL, previous_reason TEXT,
            previous_attempts INTEGER NOT NULL, recovered_at INTEGER NOT NULL,
            PRIMARY KEY(queue_kind, work_id, repair_key)
        );
        CREATE INDEX IF NOT EXISTS attention_rank_recovery_idx
            ON attention_rank_recompute_jobs(status, reason, created_at, job_id);",
    )?;
    Ok(())
}

impl AttentionLearningStore {
    /// Older workers erased the cause of every exhausted retry. Re-evaluate
    /// that cohort once under the corrected worker, with the old verdict
    /// retained. A missing served record will now settle honestly as stale;
    /// no historical decision or rank is invented by this migration.
    pub async fn recover_legacy_rank_failures(&self, limit: usize, now: i64) -> Result<u64> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let _progress = install_attention_reconciliation_progress_guard(&tx);
            let rows = {
                let mut stmt = tx.prepare(
                    "SELECT job_id, principal, workspace, status, reason, attempts
                     FROM attention_rank_recompute_jobs j
                     WHERE status = 'dead' AND reason = 'max_retries_exhausted'
                       AND result_json IS NULL
                       AND NOT EXISTS (SELECT 1 FROM attention_queue_recoveries r
                           WHERE r.queue_kind = 'rank' AND r.work_id = j.job_id AND r.repair_key = ?1)
                     ORDER BY created_at, job_id LIMIT ?2"
                )?;
                let rows = stmt.query_map(params![RANK_REPAIR, limit.clamp(1, 100)], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, i64>(5)?))
                })?.collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            for (id, principal, workspace, status, reason, attempts) in &rows {
                tx.execute("INSERT INTO attention_queue_recoveries VALUES ('rank', ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![id, RANK_REPAIR, principal, workspace, status, reason, attempts, now])?;
                tx.execute(
                    "UPDATE attention_rank_recompute_jobs SET status = 'pending', attempts = 0,
                        next_retry_at = NULL, lease_owner = NULL, lease_expires_at = NULL,
                        reason = NULL, completed_at = NULL, updated_at = ? WHERE job_id = ?",
                    params![now, id])?;
            }
            drop(_progress);
            tx.commit()?;
            Ok(rows.len() as u64)
        }).await.context("legacy rank failure recovery task panicked")?
    }
}

/// Called only by discovery of an active candidate, after the ordinary
/// schedule/upsert found an unchanged source and extraction contract. Valid,
/// inactive, in-flight and unrelated terminal failures are never reopened. A
/// successful receipt is repaired only by explicit incompatible-source discovery.
pub(super) fn recover_invalid_semantics(
    conn: &Connection,
    work_id: &str,
    request: &ScheduleSemanticExtraction,
    now: i64,
    incompatible_source_observed: bool,
) -> Result<bool> {
    let key = format!(
        "{SEMANTIC_REPAIR}:{}",
        blake3::hash(
            serde_json::to_string(&serde_json::json!([
                request.source_revision,
                request.source_revision_number,
                request.contract.semantic_schema_version,
                request.contract.extractor_contract,
                request.contract.prompt_version,
                request.contract.model,
                request.contract.profile
            ]))?
            .as_bytes()
        )
        .to_hex()
    );
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO attention_queue_recoveries
         SELECT 'semantic', work_id,
             CASE WHEN status = 'succeeded' THEN 'source_receipt:' || ?1 ELSE ?1 END,
             principal, workspace, status, last_error_code, attempts, ?2
         FROM attention_semantic_extraction_work
         WHERE work_id = ?3 AND (
             (status = 'invalid' AND last_error_code IN
                 ('evidence_count_invalid', 'evidence_ref_not_allowed', 'schema_mismatch'))
             OR (status = 'succeeded' AND ?4 = 1))",
        params![key, now, work_id, incompatible_source_observed],
    )?;
    if inserted == 0 {
        return Ok(false);
    }
    conn.execute(
        "UPDATE attention_semantic_extraction_work SET status = 'pending', attempts = 0,
            next_retry_at = NULL, lease_owner = NULL, lease_expires_at = NULL,
            last_error_code = NULL, updated_at = ? WHERE work_id = ?",
        params![now, work_id],
    )?;
    Ok(true)
}

#[cfg(test)]
mod attention_recovery_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AttentionLearningStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AttentionLearningStore::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn reconstruction_requires_the_served_revision_scope_and_pre_outcome_time() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE attention_decisions (decision_id TEXT, principal TEXT, workspace TEXT, decided_at INTEGER);
            CREATE TABLE attention_decision_items (decision_id TEXT, principal TEXT, workspace TEXT, candidate_id TEXT, source_revision TEXT, selected INTEGER, served_rank INTEGER);").unwrap();
        for (id, time, revision, selected, principal) in [
            ("future", 200, "r1", 1, "p"),
            ("wrong-revision", 20, "r2", 1, "p"),
            ("not-served", 10, "r1", 0, "p"),
            ("other-scope", 15, "r1", 1, "other"),
        ] {
            conn.execute(
                "INSERT INTO attention_decisions VALUES (?, ?, 'w', ?)",
                params![id, principal, time],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO attention_decision_items VALUES (?, ?, 'w', 'follow_up:c', ?, ?, 1)",
                params![id, principal, revision, selected],
            )
            .unwrap();
        }
        assert_eq!(
            reconstruct_served_decision_id(&conn, "p", "w", "follow_up:c", "c", Some("r1"), 100)
                .unwrap(),
            None
        );
        conn.execute(
            "INSERT INTO attention_decisions VALUES ('exact','p','w',5)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_decision_items VALUES ('exact','p','w','follow_up:c','r1',1,2)",
            [],
        )
        .unwrap();
        assert_eq!(
            reconstruct_served_decision_id(&conn, "p", "w", "follow_up:c", "c", Some("r1"), 100)
                .unwrap()
                .as_deref(),
            Some("exact")
        );
    }

    #[tokio::test]
    async fn legacy_rank_recovery_is_bounded_audited_and_never_repeats() {
        let (_dir, store) = fixture();
        {
            let conn = store.conn.lock().unwrap();
            for (id, status, reason) in [
                ("a", "dead", "max_retries_exhausted"),
                ("b", "dead", "max_retries_exhausted"),
                ("c", "dead", "other_error"),
                ("d", "succeeded", "max_retries_exhausted"),
            ] {
                conn.execute("INSERT INTO attention_outcomes
                    (outcome_id,schema_version,event_id,principal,workspace,surface,candidate_id,outcome,label_quality,occurred_at,created_at)
                    VALUES (?,1,?,'p','w','follow_up','candidate','helpful','verified',0,0)", params![id,id]).unwrap();
                conn.execute("INSERT INTO attention_rank_recompute_jobs
                    (job_id,schema_version,outcome_id,principal,workspace,origin_surface,canonical_candidate_id,raw_candidate_id,outcome,status,attempts,reason,created_at,updated_at)
                    VALUES (?,1,?,'p','w','follow_up','follow_up:candidate','candidate','helpful',?,5,?,0,0)",params![id,id,status,reason]).unwrap();
            }
        }
        assert_eq!(store.recover_legacy_rank_failures(1, 10).await.unwrap(), 1);
        assert_eq!(store.recover_legacy_rank_failures(1, 11).await.unwrap(), 1);
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("UPDATE attention_rank_recompute_jobs SET status='dead',reason='max_retries_exhausted',attempts=5 WHERE job_id IN ('a','b')", []).unwrap();
        }
        assert_eq!(store.recover_legacy_rank_failures(20, 12).await.unwrap(), 0);
        let conn = store.conn.lock().unwrap();
        let audit: (i64, i64) = conn.query_row("SELECT COUNT(*),SUM(previous_attempts) FROM attention_queue_recoveries WHERE previous_reason='max_retries_exhausted' AND previous_status='dead'", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(audit, (2, 10));
    }

    #[tokio::test]
    async fn attention_recovery_success_receipt_requires_observed_source_mismatch_and_repairs_once()
    {
        let (_dir, store) = fixture();
        let request = ScheduleSemanticExtraction {
            surface: AttentionSurface::FollowUp,
            candidate_id: "receipt".into(),
            source_revision: "distill:1".into(),
            source_revision_number: 1,
            contract: SemanticExtractionContract {
                semantic_schema_version: 1,
                extractor_contract: "channel_attention_semantics_v1".into(),
                prompt_version: "1.1.0".into(),
                model: Some("luna".into()),
                profile: Some("remote".into()),
            },
        };
        store
            .schedule_semantic_extraction("p", "w", &request, 0)
            .await
            .unwrap();
        {
            let c = store.conn.lock().unwrap();
            c.execute(
                "UPDATE attention_semantic_extraction_work SET status='succeeded',attempts=2",
                [],
            )
            .unwrap();
        }
        assert!(!store
            .schedule_semantic_extraction("p", "w", &request, 1)
            .await
            .unwrap());
        assert!(store
            .schedule_semantic_extraction_from_source("p", "w", &request, 2)
            .await
            .unwrap());
        {
            let c = store.conn.lock().unwrap();
            let audit: (String, i64) = c
                .query_row(
                    "SELECT previous_status,previous_attempts FROM attention_queue_recoveries",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(audit, ("succeeded".into(), 2));
            c.execute(
                "UPDATE attention_semantic_extraction_work SET status='succeeded'",
                [],
            )
            .unwrap();
        }
        assert!(!store
            .schedule_semantic_extraction_from_source("p", "w", &request, 3)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn only_discovered_invalid_semantics_get_one_repair_per_revision() {
        let (_dir, store) = fixture();
        let mut request = ScheduleSemanticExtraction {
            surface: AttentionSurface::FollowUp,
            candidate_id: "active".to_owned(),
            source_revision: "distill:1".to_owned(),
            source_revision_number: 1,
            contract: SemanticExtractionContract {
                semantic_schema_version: 1,
                extractor_contract: "channel_attention_semantics_v1".to_owned(),
                prompt_version: "1.1.0".to_owned(),
                model: Some("luna".to_owned()),
                profile: Some("remote".to_owned()),
            },
        };
        assert!(store
            .schedule_semantic_extraction("p", "w", &request, 0)
            .await
            .unwrap());
        for (id, status) in [
            ("valid", "succeeded"),
            ("unrelated", "invalid"),
            ("inactive", "invalid"),
        ] {
            request.candidate_id = id.to_owned();
            store
                .schedule_semantic_extraction("p", "w", &request, 0)
                .await
                .unwrap();
            let conn = store.conn.lock().unwrap();
            conn.execute("UPDATE attention_semantic_extraction_work SET status=?, last_error_code='stale_source_revision' WHERE candidate_id=?", params![status,id]).unwrap();
        }
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("UPDATE attention_semantic_extraction_work SET status='invalid',attempts=3,last_error_code='evidence_count_invalid' WHERE candidate_id IN ('active','inactive')", []).unwrap();
        }
        for id in ["valid", "unrelated"] {
            request.candidate_id = id.to_owned();
            assert!(!store
                .schedule_semantic_extraction("p", "w", &request, 1)
                .await
                .unwrap());
        }
        request.candidate_id = "active".to_owned();
        assert!(store
            .schedule_semantic_extraction("p", "w", &request, 1)
            .await
            .unwrap());
        {
            let conn = store.conn.lock().unwrap();
            conn.execute("UPDATE attention_semantic_extraction_work SET status='invalid',last_error_code='evidence_count_invalid' WHERE candidate_id='active'", []).unwrap();
            let inactive: String = conn.query_row("SELECT status FROM attention_semantic_extraction_work WHERE candidate_id='inactive'", [], |r| r.get(0)).unwrap();
            assert_eq!(inactive, "invalid");
        }
        assert!(!store
            .schedule_semantic_extraction("p", "w", &request, 2)
            .await
            .unwrap());
        request.source_revision = "distill:2".to_owned();
        request.source_revision_number = 2;
        assert!(store
            .schedule_semantic_extraction("p", "w", &request, 3)
            .await
            .unwrap());
        let conn = store.conn.lock().unwrap();
        let attempts: i64 = conn.query_row("SELECT previous_attempts FROM attention_queue_recoveries WHERE queue_kind='semantic'", [], |r| r.get(0)).unwrap();
        assert_eq!(attempts, 3);
    }
}
