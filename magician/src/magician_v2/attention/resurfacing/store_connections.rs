//! Connection decisions share the resurfacing DB and scope; they are not memory.
use super::*;
use crate::magician_v2::attention::resurfacing::memory_connections::{
    activity_revision, evidence_text, ConnectionRecord, ConnectionState, ConnectionSurface,
};

/// A live connection owns its explanation at read time, independently of a late
/// ordinary curator write. Candidate content and lifecycle are checked in the
/// same SQLite read; stale or withdrawn connection phrasing is hidden.
pub(super) fn overlay_connection_phrasing(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    ids: &[String],
    out: &mut std::collections::HashMap<String, (String, String)>,
) -> Result<()> {
    for ids in ids.chunks(EMBEDDING_SNAPSHOT_SQL_CHUNK_SIZE) {
        if ids.is_empty() {
            continue;
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt=conn.prepare(&format!("SELECT r.candidate_id,r.record_json,c.source_kind,c.source_ref,c.title,c.content_digest,c.content_revision,c.state FROM resurfacing_memory_connections r JOIN resurfacing_candidates c ON c.principal=r.principal AND c.workspace=r.workspace AND c.candidate_id=r.candidate_id WHERE r.principal=? AND r.workspace=? AND r.candidate_id IN ({placeholders})"))?;
        let values = std::iter::once(principal)
            .chain(std::iter::once(workspace))
            .chain(ids.iter().map(String::as_str));
        let mut rows = stmt.query(params_from_iter(values))?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let raw: String = row.get(1)?;
            let Ok(record) = serde_json::from_str::<ConnectionRecord>(&raw) else {
                out.remove(&id);
                continue;
            };
            let Some(connection) = record
                .connection
                .as_ref()
                .filter(|c| c.surface == ConnectionSurface::WorthALook)
            else {
                continue;
            };
            let key = format!("{}:{}", row.get::<_, String>(2)?, row.get::<_, String>(3)?);
            let revision = activity_revision(
                &row.get::<_, String>(4)?,
                &row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?.as_deref(),
            );
            let state: String = row.get(7)?;
            let phrasing = (connection.title.clone(), evidence_text(&record));
            let current = record.state == ConnectionState::Published
                && state == "surfaced"
                && chrono::Utc::now().timestamp() <= record.attempted_at + 7 * 86400
                && record
                    .sources
                    .iter()
                    .any(|s| s.id == "activity" && s.key == key && s.revision == revision);
            if current {
                out.insert(id, phrasing);
            } else if out.get(&id) == Some(&phrasing) {
                out.remove(&id);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reservation(id: &str, at: i64) -> ConnectionRecord {
        ConnectionRecord {
            candidate_id: id.into(),
            fingerprint: String::new(),
            attempted_at: at,
            state: ConnectionState::Reviewing,
            sources: vec![],
            connection: None,
            request_id: String::new(),
            feed_id: String::new(),
            source_route: None,
            decision_policy: None,
            decision_origin: None,
        }
    }

    #[tokio::test]
    async fn memory_connections_reservations_are_atomic_scoped_and_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let first = ResurfacingStore::open(temp.path()).unwrap();
        let second = ResurfacingStore::open(temp.path()).unwrap();
        let mut workers = Vec::new();
        for index in 0..8 {
            let store = if index % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            workers.push(tokio::spawn(async move {
                store
                    .reserve_connection_review("p", "w", &reservation(&format!("c{index}"), 10_000))
                    .await
                    .unwrap()
            }));
        }
        let mut accepted = 0;
        for worker in workers {
            accepted += usize::from(worker.await.unwrap());
        }
        assert_eq!(accepted, 3);
        let reopened = ResurfacingStore::open(temp.path()).unwrap();
        assert_eq!(
            reopened
                .connection_calls_since("p", "w", 6400)
                .await
                .unwrap(),
            3
        );
        assert!(!reopened
            .reserve_connection_review("p", "w", &reservation("extra", 10_001))
            .await
            .unwrap());
        assert!(reopened
            .reserve_connection_review("other", "w", &reservation("extra", 10_001))
            .await
            .unwrap());
        assert!(reopened
            .reserve_connection_review("p", "w", &reservation("extra", 13_601))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn memory_connections_old_generation_cannot_overwrite_new_review() {
        let store = ResurfacingStore::open_in_temp();
        let old = reservation("one", 10_000);
        assert!(store
            .reserve_connection_review("p", "w", &old)
            .await
            .unwrap());
        let new = reservation("one", 13_601);
        assert!(store
            .reserve_connection_review("p", "w", &new)
            .await
            .unwrap());
        store.put_connection("p", "w", &old).await.unwrap();
        assert_eq!(
            store
                .get_connection("p", "w", "one")
                .await
                .unwrap()
                .unwrap()
                .attempted_at,
            new.attempted_at
        );
    }
}

impl ResurfacingStore {
    /// Serialize the budget check and debit even when two workers share a DB.
    pub async fn reserve_connection_review(
        &self,
        principal: &str,
        workspace: &str,
        record: &ConnectionRecord,
    ) -> Result<bool> {
        let store = self.clone();
        let (principal, workspace, record) =
            (principal.to_owned(), workspace.to_owned(), record.clone());
        tokio::task::spawn_blocking(move || {
            let mut conn=store.conn.lock().unwrap_or_else(|p|p.into_inner());
            let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let previous:Option<(i64,String)>=tx.query_row("SELECT attempted_at,state FROM resurfacing_memory_connections WHERE principal=? AND workspace=? AND candidate_id=?",params![principal,workspace,record.candidate_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
            if previous.is_some_and(|(at,state)| record.attempted_at-at<3600 || matches!(state.as_str(),"ready"|"published"|"done"|"withdrawing")) {return Ok(false);}
            let spent:i64=tx.query_row("SELECT COUNT(*) FROM resurfacing_memory_connections WHERE principal=? AND workspace=? AND attempted_at>=?",params![principal,workspace,record.attempted_at-3600],|row|row.get(0))?;
            if spent>=crate::magician_v2::attention::resurfacing::memory_connections::MAX_CALLS_PER_HOUR as i64 {return Ok(false);}
            tx.execute("INSERT INTO resurfacing_memory_connections (principal,workspace,candidate_id,attempted_at,state,record_json) VALUES (?,?,?,?,'reviewing',?) ON CONFLICT(principal,workspace,candidate_id) DO UPDATE SET attempted_at=excluded.attempted_at,state=excluded.state,record_json=excluded.record_json",params![principal,workspace,record.candidate_id,record.attempted_at,serde_json::to_string(&record)?])?;
            tx.commit()?;
            Ok(true)
        }).await?
    }
    pub async fn release_connection_hold(
        &self,
        principal: &str,
        workspace: &str,
        record: &ConnectionRecord,
    ) -> Result<()> {
        let store = self.clone();
        let (principal, workspace, id, until) = (
            principal.to_owned(),
            workspace.to_owned(),
            record.candidate_id.clone(),
            record.attempted_at + 7 * 86400,
        );
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute("UPDATE resurfacing_candidates SET cooldown_until=0 WHERE principal=? AND workspace=? AND candidate_id=? AND state='candidate' AND cooldown_until=?", params![principal,workspace,id,until])?;
            Ok(())
        }).await?
    }
    /// Move a source out of Worth a look while its connection occupies another
    /// attention lane. Preserve feedback and refuse concurrent source changes.
    pub async fn hold_connection_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate: &Candidate,
        until: i64,
    ) -> Result<bool> {
        let store = self.clone();
        let (principal, workspace, candidate) = (
            principal.to_owned(),
            workspace.to_owned(),
            candidate.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            Ok(conn.execute("UPDATE resurfacing_candidates SET state='candidate',cooldown_until=MAX(cooldown_until,?) WHERE principal=? AND workspace=? AND candidate_id=? AND state IN ('candidate','surfaced') AND title=? AND content_digest=? AND content_revision IS ?", params![until,principal,workspace,candidate.candidate_id,candidate.title,candidate.content_digest,candidate.content_revision])? == 1)
        }).await?
    }

    pub async fn get_connection(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
    ) -> Result<Option<ConnectionRecord>> {
        let store = self.clone();
        let (principal, workspace, id) =
            (principal.to_owned(), workspace.to_owned(), id.to_owned());
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let raw: Option<String> = conn.query_row("SELECT record_json FROM resurfacing_memory_connections WHERE principal=? AND workspace=? AND candidate_id=?", params![principal,workspace,id], |row| row.get(0)).optional()?;
            raw.map(|s| serde_json::from_str(&s).map_err(Into::into)).transpose()
        }).await?
    }

    pub async fn put_connection(
        &self,
        principal: &str,
        workspace: &str,
        record: &ConnectionRecord,
    ) -> Result<()> {
        let store = self.clone();
        let (principal, workspace, record) =
            (principal.to_owned(), workspace.to_owned(), record.clone());
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let state = serde_json::to_value(record.state)?.as_str().unwrap().to_owned();
            conn.execute("INSERT INTO resurfacing_memory_connections (principal,workspace,candidate_id,attempted_at,state,record_json) VALUES (?,?,?,?,?,?) ON CONFLICT(principal,workspace,candidate_id) DO UPDATE SET attempted_at=excluded.attempted_at,state=excluded.state,record_json=excluded.record_json WHERE excluded.attempted_at >= resurfacing_memory_connections.attempted_at", params![principal,workspace,record.candidate_id,record.attempted_at,state,serde_json::to_string(&record)?])?;
            Ok(())
        }).await?
    }

    pub async fn active_connections(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<ConnectionRecord>> {
        let store = self.clone();
        let (principal, workspace) = (principal.to_owned(), workspace.to_owned());
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let mut stmt = conn.prepare("SELECT record_json FROM resurfacing_memory_connections WHERE principal=? AND workspace=? AND state IN ('ready','published','withdrawing') ORDER BY attempted_at LIMIT 100")?;
            let rows = stmt.query_map(params![principal,workspace], |row| row.get::<_,String>(0))?;
            rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
        }).await?
    }

    pub async fn connection_calls_since(
        &self,
        principal: &str,
        workspace: &str,
        since: i64,
    ) -> Result<usize> {
        let store = self.clone();
        let (principal, workspace) = (principal.to_owned(), workspace.to_owned());
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            Ok(conn.query_row("SELECT COUNT(*) FROM resurfacing_memory_connections WHERE principal=? AND workspace=? AND attempted_at>=?", params![principal,workspace,since], |row| row.get::<_,i64>(0))? as usize)
        }).await?
    }

    /// Remove only this connection's phrasing. A subsequent curator write wins.
    pub async fn clear_connection_phrasing(
        &self,
        principal: &str,
        workspace: &str,
        record: &ConnectionRecord,
    ) -> Result<()> {
        let Some(connection) = &record.connection else {
            return Ok(());
        };
        let store = self.clone();
        let (principal, workspace, id, title) = (
            principal.to_owned(),
            workspace.to_owned(),
            record.candidate_id.clone(),
            connection.title.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute("DELETE FROM resurfacing_phrasing WHERE principal=? AND workspace=? AND candidate_id=? AND line=?", params![principal,workspace,id,title])?;
            Ok(())
        }).await?
    }
}
