//! Incremental canonical-history maintenance. Page delivery only schedules it.

use super::*;

const HISTORY_BATCH_ROWS: usize = 4;
const BODY_BATCH_ROWS: usize = 32;
const BODY_BATCH_BYTES: i64 = 4 * 1024 * 1024;
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
type PruneKey = (PathBuf, String, String);
static PRUNES: OnceLock<Mutex<HashMap<PruneKey, (bool, Instant)>>> = OnceLock::new();

struct ScheduledPrune(PruneKey);

impl Drop for ScheduledPrune {
    fn drop(&mut self) {
        if let Some(entry) = PRUNES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_mut(&self.0)
        {
            entry.0 = false;
        }
    }
}

pub(super) fn ensure_indexes(conn: &Connection) -> Result<()> {
    // Run after column migrations. These are reference lookups, not payload
    // indexes; retained JSON must never be scanned once per historical row.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS attention_history_decision_universe_idx
             ON attention_decisions(candidate_set_digest);
         CREATE INDEX IF NOT EXISTS attention_history_delivery_projection_idx
             ON attention_delivery_decisions(projection_id);
         CREATE INDEX IF NOT EXISTS attention_history_outcome_decision_idx
             ON attention_outcomes(decision_id);
         CREATE INDEX IF NOT EXISTS attention_history_rank_decision_idx
             ON attention_rank_recompute_jobs(decision_id);
         CREATE INDEX IF NOT EXISTS attention_history_impression_decision_idx
             ON attention_impressions(decision_id);
         CREATE INDEX IF NOT EXISTS attention_history_member_item_idx
             ON attention_canonical_projection_members(item_digest);
         CREATE INDEX IF NOT EXISTS attention_history_member_rank_idx
             ON attention_canonical_projection_members(rank_digest) WHERE rank_digest IS NOT NULL;
         CREATE INDEX IF NOT EXISTS attention_history_member_decision_idx
             ON attention_canonical_projection_members(decision_digest) WHERE decision_digest IS NOT NULL;
         CREATE INDEX IF NOT EXISTS attention_history_item_feature_idx
             ON attention_decision_items(feature_snapshot_digest) WHERE feature_snapshot_digest IS NOT NULL;
         CREATE INDEX IF NOT EXISTS attention_history_binding_feature_idx
             ON attention_candidate_feature_bindings(content_digest);
         CREATE INDEX IF NOT EXISTS attention_history_update_decision_idx
             ON attention_bandit_updates(decision_id) WHERE decision_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS attention_history_outcome_delivery_idx
             ON attention_outcomes(delivery_id) WHERE delivery_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS attention_history_impression_delivery_idx
             ON attention_impressions(delivery_id) WHERE delivery_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS attention_history_rank_delivery_idx
             ON attention_rank_recompute_jobs(delivery_id) WHERE delivery_id IS NOT NULL;",
    )?;
    Ok(())
}

/// Binds `?1` projection, `?2` principal, `?3` workspace, `?4` normalized
/// schema version, `?5` expired-delivery cutoff (see `delivery_pins_sql!`).
///
/// Retention policy: the PROJECTION (the ~3 MB materialized page set) is the
/// only thing an expired, unattributed delivery stops pinning after
/// `bandit.expired_delivery_retention_hours`. The canonical decision it was
/// served from (same id) and the delivery itself keep their 30-day
/// `delivery_retention_days` life — see `DECISION_UNREFERENCED` — so the
/// decision row no longer pins its projection; what the decision is pinned by
/// (outcomes, jobs, impressions, live or attributed deliveries) pins the
/// projection directly below.
pub(super) const DELETE_PROJECTION: &str = concat!(
    "DELETE FROM attention_canonical_projections
     WHERE projection_id = ?1 AND principal = ?2 AND workspace = ?3
       AND NOT EXISTS (SELECT 1 FROM attention_delivery_decisions d WHERE d.projection_id = ?1
           AND ",
    delivery_pins_sql!("d", "?5"),
    ")
       AND NOT EXISTS (SELECT 1 FROM attention_impressions i WHERE i.decision_id = ?1)
       AND (schema_version >= ?4 OR NOT EXISTS (
           SELECT 1 FROM attention_decisions d
           WHERE d.candidate_set_digest = attention_canonical_projections.universe_digest))
       AND NOT EXISTS (SELECT 1 FROM attention_outcomes o
           WHERE o.principal = ?2 AND o.workspace = ?3 AND o.projection_id = ?1)
       AND NOT EXISTS (SELECT 1 FROM attention_outcomes o
           WHERE o.principal = ?2 AND o.workspace = ?3 AND o.decision_id = ?1)
       AND NOT EXISTS (SELECT 1 FROM attention_impressions i
           WHERE i.principal = ?2 AND i.workspace = ?3 AND i.projection_id = ?1)
       AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j
           WHERE j.principal = ?2 AND j.workspace = ?3 AND j.projection_id = ?1)
       AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j WHERE j.decision_id = ?1)"
);

/// Binds `?1` decision. Any surviving delivery served from the decision pins
/// it — deliveries live `delivery_retention_days` (30) past expiry, retired by
/// `compact_attention_deliveries` — so a late outcome still binds to the
/// served decision items (bandit attribution, `attribution_ref`,
/// `reconstruct_served_decision_id`) for the whole learning window.
const DECISION_UNREFERENCED: &str =
    "NOT EXISTS (SELECT 1 FROM attention_impressions i WHERE i.decision_id = ?1)
     AND NOT EXISTS (SELECT 1 FROM attention_outcomes o WHERE o.decision_id = ?1)
     AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j WHERE j.decision_id = ?1)
     AND NOT EXISTS (SELECT 1 FROM attention_delivery_decisions d WHERE d.decision_id = ?1)
     AND NOT EXISTS (SELECT 1 FROM attention_delivery_decisions d WHERE d.projection_id = ?1)";

#[derive(Clone, Copy)]
enum Phase {
    Decisions,
    Projections,
    Items,
    Diagnostics,
}

#[derive(Default)]
struct Cursor {
    time: i64,
    id: String,
    // Freeze the first batch's upper bound so new writes cannot extend a pass.
    upper: Option<(i64, String)>,
}

impl AttentionLearningStore {
    /// Coalesce maintenance by database AND scope. Scheduling cannot wait for
    /// the SQLite writer or extend a page's projection/cache lease.
    pub fn schedule_canonical_history_prune(&self, principal: &str, workspace: &str, keep: usize) {
        let key = (
            self.path.as_ref().clone(),
            principal.to_owned(),
            workspace.to_owned(),
        );
        let mut running = PRUNES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        running.retain(|_, (active, started)| *active || started.elapsed().as_secs() < 3600);
        if running
            .get(&key)
            .is_some_and(|(active, started)| *active || started.elapsed() < PRUNE_INTERVAL)
        {
            return;
        }
        running.insert(key.clone(), (true, Instant::now()));
        drop(running);
        let store = self.clone();
        let guard = ScheduledPrune(key);
        tokio::spawn(async move {
            let _guard = guard;
            match store
                .prune_canonical_history(&_guard.0 .1, &_guard.0 .2, keep)
                .await
            {
                Ok(report) => tracing::debug!(
                    ?report,
                    "completed incremental canonical history maintenance"
                ),
                Err(error) => {
                    tracing::warn!(error = %error, "canonical history maintenance failed; a later pass may retry")
                },
            }
        });
    }

    /// Drain a finite keyset pass in bounded transactions. Every batch releases
    /// the fair writer ticket before the next one is requested. Protected rows
    /// advance the cursor too, so old attribution cannot starve later garbage.
    pub async fn prune_canonical_history(
        &self,
        principal: &str,
        workspace: &str,
        keep: usize,
    ) -> Result<AttentionHistoryPruneReport> {
        let mut report = AttentionHistoryPruneReport::default();
        // Retired decisions may be the last reference to an old projection.
        // Bodies are released by the projection batches themselves (only the
        // digests the retired members held are probed); the full body walk is
        // `sweep_orphan_canonical_bodies`, run by the reclaim path.
        for phase in [Phase::Decisions, Phase::Projections] {
            let mut cursor = Cursor {
                time: i64::MIN,
                id: String::new(),
                upper: None,
            };
            loop {
                let store = self.clone();
                let principal = principal.to_owned();
                let workspace = workspace.to_owned();
                let (next, batch) = tokio::task::spawn_blocking(move || {
                    store.prune_history_batch(&principal, &workspace, keep.max(1), phase, cursor)
                })
                .await
                .context("canonical history maintenance batch panicked")??;
                report.projections_pruned += batch.projections_pruned;
                report.decisions_pruned += batch.decisions_pruned;
                report.decision_items_pruned += batch.decision_items_pruned;
                report.item_revisions_pruned += batch.item_revisions_pruned;
                report.diagnostics_pruned += batch.diagnostics_pruned;
                let Some(next) = next else { break };
                cursor = next;
                tokio::task::yield_now().await;
            }
        }
        Ok(report)
    }

    fn prune_history_batch(
        &self,
        principal: &str,
        workspace: &str,
        keep: usize,
        phase: Phase,
        cursor: Cursor,
    ) -> Result<(Option<Cursor>, AttentionHistoryPruneReport)> {
        let delivery_cutoff =
            self.expired_delivery_cutoff_at(chrono::Utc::now().timestamp_millis());
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut report = AttentionHistoryPruneReport::default();
        let mut next = None;
        match phase {
            Phase::Decisions | Phase::Projections => {
                let (table, id, time) = match phase {
                    Phase::Decisions => ("attention_decisions", "decision_id", "decided_at"),
                    _ => (
                        "attention_canonical_projections",
                        "projection_id",
                        "created_at",
                    ),
                };
                let boundary: Option<(i64, String)> = tx.query_row(
                    &format!("SELECT {time}, {id} FROM {table} WHERE principal = ? AND workspace = ? ORDER BY {time} DESC, {id} DESC LIMIT 1 OFFSET ?"),
                    params![principal, workspace, i64::try_from(keep.saturating_sub(1)).unwrap_or(i64::MAX)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ).optional()?;
                if let Some(boundary) = boundary {
                    let upper = cursor
                        .upper
                        .unwrap_or_else(|| boundary.clone())
                        .min(boundary);
                    let (boundary_time, boundary_id) = &upper;
                    let rows = {
                        let mut statement = tx.prepare(&format!(
                            "SELECT {time}, {id} FROM {table} WHERE principal = ?1 AND workspace = ?2 AND ({time}, {id}) > (?3, ?4) AND ({time}, {id}) < (?5, ?6) ORDER BY {time}, {id} LIMIT ?7"
                        ))?;
                        let rows = statement
                            .query_map(
                                params![
                                    principal,
                                    workspace,
                                    cursor.time,
                                    cursor.id,
                                    boundary_time,
                                    boundary_id,
                                    HISTORY_BATCH_ROWS
                                ],
                                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
                            )?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        rows
                    };
                    for (time, id) in rows {
                        match phase {
                            Phase::Projections => {
                                let retired = retire_projection_and_bodies(
                                    &tx,
                                    &id,
                                    principal,
                                    workspace,
                                    delivery_cutoff,
                                )?;
                                report.projections_pruned += retired.projections;
                                report.item_revisions_pruned += retired.item_bodies;
                                report.diagnostics_pruned += retired.diagnostic_bodies;
                            },
                            _ => {
                                let unreferenced: bool = tx.query_row(
                                    &format!("SELECT {DECISION_UNREFERENCED}"),
                                    [&id],
                                    |r| r.get(0),
                                )?;
                                if unreferenced {
                                    report.decision_items_pruned += tx.execute("DELETE FROM attention_decision_items WHERE decision_id = ? AND principal = ? AND workspace = ?", params![id, principal, workspace])? as u64;
                                    report.decisions_pruned += tx.execute("DELETE FROM attention_decisions WHERE decision_id = ? AND principal = ? AND workspace = ? AND NOT EXISTS (SELECT 1 FROM attention_decision_items i WHERE i.decision_id = attention_decisions.decision_id)", params![id, principal, workspace])? as u64;
                                }
                            },
                        }
                        next = Some(Cursor {
                            time,
                            id,
                            upper: Some(upper.clone()),
                        });
                    }
                }
            },
            Phase::Items | Phase::Diagnostics => {
                let (table, column, guard) = match phase {
                    Phase::Items => ("attention_canonical_item_revisions", "item_digest", "NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m WHERE m.item_digest = ?1)"),
                    _ => ("attention_canonical_diagnostic_revisions", "diagnostic_digest", "NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m WHERE m.rank_digest = ?1) AND NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m WHERE m.decision_digest = ?1)"),
                };
                let upper = match cursor.upper {
                    Some(upper) => upper,
                    None => (
                        0,
                        tx.query_row(
                            &format!("SELECT COALESCE(MAX({column}), '') FROM {table}"),
                            [],
                            |r| r.get::<_, String>(0),
                        )?,
                    ),
                };
                let rows = {
                    let mut statement = tx.prepare(&format!("SELECT {column}, size_bytes FROM {table} WHERE {column} > ? AND {column} <= ? ORDER BY {column} LIMIT ?"))?;
                    let rows = statement
                        .query_map(params![cursor.id, upper.1, BODY_BATCH_ROWS], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    rows
                };
                let mut bytes = 0i64;
                for (id, size) in rows {
                    if next.is_some() && bytes.saturating_add(size.max(0)) > BODY_BATCH_BYTES {
                        break;
                    }
                    let removed = tx.execute(
                        &format!("DELETE FROM {table} WHERE {column} = ?1 AND {guard}"),
                        [&id],
                    )? as u64;
                    match phase {
                        Phase::Items => report.item_revisions_pruned += removed,
                        _ => report.diagnostics_pruned += removed,
                    }
                    bytes = bytes.saturating_add(size.max(0));
                    next = Some(Cursor {
                        time: 0,
                        id,
                        upper: Some(upper.clone()),
                    });
                }
            },
        }
        tx.commit()?;
        if report.pruned_anything() {
            // Hand the pages this batch freed back while the writer is still
            // ours; a no-op unless the file is in incremental auto-vacuum.
            if let Err(error) = attention_incremental_vacuum_step(&conn) {
                tracing::debug!(error = %error, "attention incremental vacuum step failed; a later batch retries");
            }
        }
        Ok((next, report))
    }

    /// FULL walk of every content-addressed body — item revisions, then
    /// decision/rank diagnostics — in short keyset batches, releasing the
    /// writer between batches. ~70k transactions on a store with ~2M
    /// diagnostics, so it is reserved for the operator reclaim path; routine
    /// retention and pruning probe only the digests the projections they
    /// retired held (`retire_projection_and_bodies`).
    pub(super) fn sweep_orphan_canonical_bodies(&self) -> Result<AttentionHistoryPruneReport> {
        let mut report = AttentionHistoryPruneReport::default();
        for phase in [Phase::Items, Phase::Diagnostics] {
            let mut cursor = Cursor::default();
            loop {
                let (next, batch) = self.prune_history_batch("", "", 1, phase, cursor)?;
                report.item_revisions_pruned += batch.item_revisions_pruned;
                report.diagnostics_pruned += batch.diagnostics_pruned;
                let Some(next) = next else { break };
                cursor = next;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod attention_recovery_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AttentionLearningStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = AttentionLearningStore::open(dir.path()).unwrap();
        (dir, store)
    }

    fn projection(conn: &Connection, id: &str, scope: &str, time: i64, schema: i64) {
        conn.execute("INSERT INTO attention_canonical_projections VALUES (?, ?, ?, 'w', ?, 'policy', '{}', ?, ?)",
            params![id, schema, scope, id, time, time]).unwrap();
    }

    fn decision(conn: &Connection, id: &str, universe: &str, time: i64) {
        conn.execute("INSERT INTO attention_decisions
            (decision_id, schema_version, principal, workspace, surface, decided_at, policy_mode,
             candidate_set_digest, eligible_item_count, selected_item_count, returned_item_count,
             complete_universe_recorded, complete_cross_lane_universe, context_json,
             policy_seed_identity, canary_assigned, baseline_route_summary_json,
             learned_route_summary_json, latency_ms, decision_json, created_at)
            VALUES (?, 1, 'p', 'w', 'follow_up', ?, 'off', ?, 0, 0, 0, 1, 1, '{}', '', 0, '{}', '{}', 0, '{}', ?)",
            params![id, time, universe, time]).unwrap();
    }

    fn member(conn: &Connection, projection: &str, body: &str) {
        conn.execute("INSERT INTO attention_canonical_projection_manifests VALUES (?, 2, '{}', NULL, 1, 2, 0, 0)", [projection]).unwrap();
        conn.execute(
            "INSERT INTO attention_canonical_projection_members
            (projection_id, lane, position, canonical_id, item_digest, rank_digest)
            VALUES (?, 'follow_up', 1, 'candidate', ?, 'shared-rank')",
            params![projection, body],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn pruning_preserves_legacy_attribution_newest_scope_and_shared_bodies() {
        let (_dir, store) = fixture();
        {
            let conn = store.conn.lock().unwrap();
            for n in 0..12 {
                projection(
                    &conn,
                    &format!("p{n:02}"),
                    "p",
                    n,
                    if n == 0 { 1 } else { 2 },
                );
            }
            projection(&conn, "other", "other", 0, 2);
            decision(&conn, "keep-decision", "p00", 100);
            decision(&conn, "retired-decision", "irrelevant", 0);
            for id in ["shared", "orphan"] {
                conn.execute("INSERT INTO attention_canonical_item_revisions VALUES (?, 'candidate', 'r', '{}', 2, 0)", [id]).unwrap();
            }
            conn.execute("INSERT INTO attention_canonical_diagnostic_revisions VALUES ('shared-rank', 'rank', '{}', 2, 0)", []).unwrap();
            member(&conn, "p01", "shared");
            member(&conn, "other", "shared");
            let mut stmt = conn
                .prepare(&format!("EXPLAIN QUERY PLAN {DELETE_PROJECTION}"))
                .unwrap();
            let plan = stmt
                .query_map(params!["p01", "p", "w", 2, 0], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            assert!(
                plan.iter().all(|row| !row.contains("SCAN d")
                    && !row.contains("SCAN m")
                    && !row.contains("SCAN j")
                    && !row.contains("SCAN pin_")),
                "{plan:?}"
            );
        }
        let report = store.prune_canonical_history("p", "w", 1).await.unwrap();
        assert_eq!(report.projections_pruned, 10);
        assert_eq!(report.decisions_pruned, 1);
        // Only bodies a retired member referenced are probed: "shared" is
        // still referenced by "other", and "orphan" was never referenced by
        // a retired projection, so the routine prune leaves it to the full
        // sweep that the reclaim path runs.
        assert_eq!(report.item_revisions_pruned, 0);
        assert_eq!(report.diagnostics_pruned, 0);
        let full = store.sweep_orphan_canonical_bodies().unwrap();
        assert_eq!(full.item_revisions_pruned, 1);
        assert!(!store
            .prune_canonical_history("p", "w", 1)
            .await
            .unwrap()
            .pruned_anything());
        let conn = store.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM attention_canonical_projections WHERE projection_id IN ('p00', 'p11', 'other')", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 3);
    }

    fn delivery(conn: &Connection, id: &str, projection: &str, expires_at: i64) {
        conn.execute(
            "INSERT INTO attention_delivery_decisions (decision_id, schema_version, principal, \
                workspace, lane, projection_id, universe_digest, posterior_version, seed_identity, \
                universe_size, page_size, status, health_json, min_visible_ms, \
                visibility_rule_version, context_json, decision_json, created_at, expires_at) \
             VALUES (?1, 1, 'p', 'w', 'follow_up', ?2, ?2, 0, 'seed', 1, 1, 'succeeded', '{}', 1, \
                'v1', '{}', '{}', 0, ?3)",
            params![id, projection, expires_at],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_delivery_decision_items (decision_id, position, candidate_id, \
                root_policy_propensity, exposure_token, item_json) \
             VALUES (?1, 1, 'candidate', 1.0, ?1, '')",
            [id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_delivery_pages (delivery_id, decision_id, page_index, page_start, \
                page_size, has_more, expires_at, page_json) \
             VALUES (?1 || '-page', ?1, 0, 0, 1, 0, ?2, '{}')",
            params![id, expires_at],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_delivery_page_items (delivery_id, decision_id, position) \
             VALUES (?1 || '-page', ?1, 1)",
            [id],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn expired_unattributed_delivery_releases_its_projection_but_not_its_decision() {
        let (_dir, store) = fixture();
        let now = chrono::Utc::now().timestamp_millis();
        {
            let conn = store.conn.lock().unwrap();
            // Oldest first; `newest` is inside the keep bound regardless.
            for (n, id) in [
                "p-expired",
                "p-attributed",
                "p-recent",
                "p-live",
                "p-newest",
            ]
            .iter()
            .enumerate()
            {
                projection(&conn, id, "p", n as i64, 2);
            }
            for id in ["retired-body", "unrelated-orphan"] {
                conn.execute("INSERT INTO attention_canonical_item_revisions VALUES (?, 'candidate', 'r', '{}', 2, 0)", [id]).unwrap();
            }
            member(&conn, "p-expired", "retired-body");
            // The served canonical decision shares the projection's id.
            decision(&conn, "p-expired", "p-expired", 0);
            // A newer decision puts `p-expired` outside the keep-1 bound.
            decision(&conn, "decision-newest", "u", 10);
            // Expired two days ago, nothing attributes to it: releases the
            // projection, keeps the decision (30-day learning evidence).
            delivery(&conn, "d-expired", "p-expired", now - 2 * 86_400_000);
            // Expired long ago, but an outcome names it: still pins.
            delivery(&conn, "d-attributed", "p-attributed", 100);
            conn.execute(
                "INSERT INTO attention_outcomes (outcome_id, schema_version, event_id, principal, \
                    workspace, surface, candidate_id, outcome, label_quality, occurred_at, \
                    created_at, decision_id) \
                 VALUES ('o1', 1, 'e1', 'p', 'w', 'follow_up', 'candidate', 'dismissed', \
                    'explicit', 1, 1, 'd-attributed')",
                [],
            )
            .unwrap();
            // Expired an hour ago: still inside the 24 h window, still pins.
            delivery(&conn, "d-recent", "p-recent", now - 3_600_000);
            // Not expired at all.
            delivery(&conn, "d-live", "p-live", now + 3_600_000);
        }
        let report = store.prune_canonical_history("p", "w", 1).await.unwrap();
        assert_eq!(report.projections_pruned, 1);
        assert_eq!(report.decisions_pruned, 0, "a delivery keeps its decision");
        assert_eq!(
            report.item_revisions_pruned, 1,
            "the retired member's body is probed"
        );
        let conn = store.conn.lock().unwrap();
        let ids = |sql: &str| -> Vec<String> {
            conn.prepare(sql)
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            ids("SELECT projection_id FROM attention_canonical_projections ORDER BY created_at"),
            ["p-attributed", "p-recent", "p-live", "p-newest"]
        );
        assert_eq!(
            ids("SELECT decision_id FROM attention_decisions ORDER BY decision_id"),
            ["decision-newest", "p-expired"]
        );
        assert_eq!(
            ids("SELECT decision_id FROM attention_delivery_decisions ORDER BY decision_id"),
            ["d-attributed", "d-expired", "d-live", "d-recent"],
            "deliveries keep their delivery_retention_days life"
        );
        assert_eq!(
            ids("SELECT item_digest FROM attention_canonical_item_revisions"),
            ["unrelated-orphan"],
            "only digests the retired projection held are probed"
        );
    }

    #[test]
    fn batches_freeze_the_horizon_and_allow_one_oversized_body() {
        let (_dir, store) = fixture();
        {
            let conn = store.conn.lock().unwrap();
            for n in 0..12 {
                projection(&conn, &format!("p{n:02}"), "p", n, 2);
            }
        }
        let (cursor, report) = store
            .prune_history_batch(
                "p",
                "w",
                1,
                Phase::Projections,
                Cursor {
                    time: i64::MIN,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(report.projections_pruned, HISTORY_BATCH_ROWS as u64);
        {
            let conn = store.conn.lock().unwrap();
            projection(&conn, "newer", "p", 20, 2);
        }
        let mut next = cursor;
        while let Some(cursor) = next {
            next = store
                .prune_history_batch("p", "w", 1, Phase::Projections, cursor)
                .unwrap()
                .0;
        }
        {
            let conn = store.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_canonical_projections",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                count, 2,
                "the original newest row is outside this pass's horizon"
            );
            for id in ["a", "b"] {
                conn.execute("INSERT INTO attention_canonical_item_revisions VALUES (?, 'candidate', 'r', '{}', ?, 0)", params![id, BODY_BATCH_BYTES * 2]).unwrap();
            }
        }
        let (cursor, report) = store
            .prune_history_batch("p", "w", 1, Phase::Items, Cursor::default())
            .unwrap();
        assert_eq!(report.item_revisions_pruned, 1);
        assert_eq!(cursor.unwrap().id, "a");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scheduling_does_not_wait_for_writer_and_other_writes_pass_between_batches() {
        let (_dir, store) = fixture();
        let guard = store.conn.lock().unwrap();
        for n in 0..20 {
            projection(&guard, &format!("p{n:02}"), "p", n, 2);
        }
        let issued = store.conn.issued_ticket_count();
        // This must return while the writer remains locked by this test.
        store.schedule_canonical_history_prune("p", "w", 1);
        let wait_for_ticket = |target| {
            let start = Instant::now();
            while store.conn.issued_ticket_count() < target {
                assert!(
                    start.elapsed().as_secs() < 5,
                    "writer never requested a ticket"
                );
                std::thread::yield_now();
            }
        };
        wait_for_ticket(issued + 1);
        let concurrent = store.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let conn = concurrent.conn.lock().unwrap();
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_canonical_projections",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            tx.send(count).unwrap();
        });
        wait_for_ticket(issued + 2);
        drop(guard);
        assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap() > 1);
        thread.join().unwrap();
        // Let the coalesced pass finish before its temporary DB is dropped.
        let key = (
            store.path.as_ref().clone(),
            "p".to_string(),
            "w".to_string(),
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !PRUNES.get().unwrap().lock().unwrap().get(&key).unwrap().0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}
