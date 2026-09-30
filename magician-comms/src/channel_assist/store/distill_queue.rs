//! Queue age admission and bounded retirement. Reads never acquire the writer;
//! retirement changes only selected queue states and never fetches content.

use super::*;

pub const DISTILL_HISTORY_BATCH: usize = 256;

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct DistillQueueCounts {
    pub history_floor_ms: i64,
    pub pending: u64,
    pub retryable: u64,
    pub outside_history: u64,
    pub expired: u64,
    pub retry_exhausted: u64,
}

impl MailAssistStore {
    /// Counts are disjoint: only pending/retryable work within the window is
    /// eligible. Exhausted failures and retired history are terminal metadata.
    pub async fn distill_queue_counts(
        &self,
        principal: &str,
        workspace: &str,
        floor: i64,
        max_attempts: i64,
    ) -> Result<DistillQueueCounts> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            conn.query_row(
                "SELECT
                   COUNT(*) FILTER (WHERE distill_state = 'pending' AND internal_date >= ?),
                   COUNT(*) FILTER (WHERE distill_state = 'failed' AND distill_attempts < ? AND internal_date >= ?),
                   COUNT(*) FILTER (WHERE (distill_state = 'pending' OR (distill_state = 'failed' AND distill_attempts < ?)) AND internal_date < ?),
                   COUNT(*) FILTER (WHERE distill_state = 'expired'),
                   COUNT(*) FILTER (WHERE distill_state = 'failed' AND distill_attempts >= ?)
                 FROM mail_messages WHERE principal = ? AND workspace = ?",
                params![floor, max_attempts, floor, max_attempts, floor, max_attempts, principal, workspace],
                |row| Ok(DistillQueueCounts {
                    history_floor_ms: floor,
                    pending: row.get(0)?,
                    retryable: row.get(1)?,
                    outside_history: row.get(2)?,
                    expired: row.get(3)?,
                    retry_exhausted: row.get(4)?,
                }),
            ).context("counting eligible distillation work")
        }).await.context("distill queue counts task panicked")?
    }

    /// Retire at most one bounded batch. Selection uses a separate reader and
    /// selects only identity columns; an empty pass never acquires the writer.
    /// The final UPDATE rechecks age/state, so a concurrently completed or
    /// suppressed row wins. No source text, summary, revision or attempt is reset.
    pub async fn expire_distill_history_batch(
        &self,
        principal: &str,
        workspace: &str,
        floor: i64,
        max_attempts: i64,
    ) -> Result<usize> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let keys = {
                let reader = inner.read_connection()?;
                let mut stmt = reader.prepare(
                    "SELECT provider, account_alias, message_id FROM mail_messages
                     WHERE principal = ? AND workspace = ? AND internal_date < ?
                       AND (distill_state = 'pending' OR (distill_state = 'failed' AND distill_attempts < ?))
                     ORDER BY internal_date, provider, account_alias, message_id LIMIT ?",
                )?;
                let mapped = stmt.query_map(params![principal, workspace, floor, max_attempts, DISTILL_HISTORY_BATCH as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                })?;
                mapped.collect::<duckdb::Result<Vec<_>>>()?
            };
            if keys.is_empty() { return Ok(0); }
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner.write_conn.lock().expect("mail assist write connection mutex poisoned");
            expire_selected(&conn, &principal, &workspace, floor, max_attempts, &keys)
        }).await.context("expire distill history task panicked")?
    }
}

fn expire_selected(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    floor: i64,
    max_attempts: i64,
    keys: &[(String, String, String)],
) -> Result<usize> {
    if keys.is_empty() {
        return Ok(0);
    }
    let mut values = vec![
        DuckValue::Text(principal.to_owned()),
        DuckValue::Text(workspace.to_owned()),
        DuckValue::BigInt(floor),
        DuckValue::BigInt(max_attempts),
    ];
    for (provider, account, message) in keys {
        values.extend([
            DuckValue::Text(provider.clone()),
            DuckValue::Text(account.clone()),
            DuckValue::Text(message.clone()),
        ]);
    }
    let placeholders = vec!["(?, ?, ?)"; keys.len()].join(",");
    conn.execute(
        &format!(
            "UPDATE mail_messages SET distill_state = 'expired'
         WHERE principal = ? AND workspace = ? AND internal_date < ?
           AND (distill_state = 'pending' OR (distill_state = 'failed' AND distill_attempts < ?))
           AND (provider, account_alias, message_id) IN (VALUES {placeholders})"
        ),
        params_from_iter(values),
    )
    .context("retiring out-of-range distillation work")
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample_message;
    use super::*;

    #[tokio::test]
    async fn distill_history_retirement_is_bounded_preserves_sources_and_survives_reappend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut old = sample_message("a", "t", "old");
        old.internal_date = 999;
        old.summary = Some("retained safe summary".into());
        old.distill_attempts = 1;
        old.distill_state = DistillState::Failed;
        let mut records = vec![old.clone()];
        for i in 0..DISTILL_HISTORY_BATCH {
            let mut r = old.clone();
            r.message_id = format!("pending-{i:03}");
            r.distill_state = DistillState::Pending;
            records.push(r);
        }
        for (id, state, date, attempts) in [
            ("boundary", DistillState::Pending, 1000, 0),
            ("done", DistillState::Done, 999, 0),
            ("suppressed", DistillState::Suppressed, 999, 0),
            ("exhausted", DistillState::Failed, 999, 3),
        ] {
            let mut r = old.clone();
            r.message_id = id.into();
            r.distill_state = state;
            r.internal_date = date;
            r.distill_attempts = attempts;
            records.push(r);
        }
        store
            .append_messages("p", "w", records.clone())
            .await
            .unwrap();
        store
            .append_messages("other", "w", vec![old.clone()])
            .await
            .unwrap();
        assert_eq!(
            store
                .expire_distill_history_batch("p", "w", 1000, 3)
                .await
                .unwrap(),
            DISTILL_HISTORY_BATCH
        );
        assert_eq!(
            store
                .expire_distill_history_batch("p", "w", 1000, 3)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .expire_distill_history_batch("p", "w", 1000, 3)
                .await
                .unwrap(),
            0
        );
        store.append_messages("p", "w", vec![old]).await.unwrap();
        let counts = store.distill_queue_counts("p", "w", 1000, 3).await.unwrap();
        assert_eq!(
            counts,
            DistillQueueCounts {
                history_floor_ms: 1000,
                pending: 1,
                expired: 257,
                retry_exhausted: 1,
                ..Default::default()
            }
        );
        for original in records {
            let row = store
                .get_message("p", "w", "gmail", "a", &original.message_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.summary, original.summary);
            assert_eq!(row.distill_attempts, original.distill_attempts);
            assert_eq!(row.internal_date, original.internal_date);
            if original.internal_date >= 1000
                || matches!(
                    original.distill_state,
                    DistillState::Done | DistillState::Suppressed
                )
                || original.distill_attempts >= 3
            {
                assert_eq!(row.distill_state, original.distill_state);
            } else {
                assert_eq!(row.distill_state, DistillState::Expired);
            }
        }
        assert_eq!(
            store
                .get_message("other", "w", "gmail", "a", "old")
                .await
                .unwrap()
                .unwrap()
                .distill_state,
            DistillState::Failed
        );
    }

    #[tokio::test]
    async fn distill_history_empty_retirement_and_selection_do_not_take_writer() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut row = sample_message("a", "t", "recent");
        row.internal_date = 1000;
        store.append_messages("p", "w", vec![row]).await.unwrap();
        let inner = store.scope_inner("p", "w").unwrap();
        // Hold the actual writer while independent reader/empty cleanup calls run.
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = tokio::task::spawn_blocking(move || {
            let _guard = inner.acquire_write_guard().unwrap();
            let _conn = inner.write_conn.lock().unwrap();
            locked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        locked_rx.await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            assert_eq!(
                store
                    .expire_distill_history_batch("p", "w", 1000, 3)
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(
                store
                    .list_pending_distill_since("p", "w", 8, 1000)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                store
                    .distill_queue_counts("p", "w", 1000, 3)
                    .await
                    .unwrap()
                    .pending,
                1
            );
        })
        .await;
        release_tx.send(()).unwrap();
        writer.await.unwrap();
        result.unwrap();
    }

    #[test]
    fn distill_history_retirement_rechecks_completed_state() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE mail_messages (principal TEXT, workspace TEXT, provider TEXT, account_alias TEXT, message_id TEXT, internal_date BIGINT, distill_state TEXT, distill_attempts BIGINT); INSERT INTO mail_messages VALUES ('p','w','gmail','a','r',1,'done',0)").unwrap();
        assert_eq!(
            expire_selected(
                &conn,
                "p",
                "w",
                1000,
                3,
                &[("gmail".into(), "a".into(), "r".into())]
            )
            .unwrap(),
            0
        );
    }
}
