use async_trait::async_trait;
use magician_storage::StorageError;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

use crate::store::{ChatPage, Gate1Store, TaskRow};

pub struct PostgresStore {
    client: Mutex<Client>,
}

impl PostgresStore {
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self {
            client: Mutex::new(client),
        })
    }
}

#[async_trait]
impl Gate1Store for PostgresStore {
    fn name(&self) -> &'static str {
        "postgres"
    }

    async fn migrate(&self) -> Result<(), StorageError> {
        let client = self.client.lock().await;
        client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);
                CREATE TABLE IF NOT EXISTS tasks (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    task_id TEXT NOT NULL,
                    revision BIGINT NOT NULL,
                    idempotency_key TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    PRIMARY KEY (principal, workspace, task_id),
                    UNIQUE (principal, workspace, idempotency_key)
                );
                CREATE TABLE IF NOT EXISTS chat_sessions (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    created_at BIGINT NOT NULL,
                    PRIMARY KEY (principal, workspace, session_id)
                );
                CREATE TABLE IF NOT EXISTS chat_messages (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    seq BIGINT NOT NULL,
                    body TEXT NOT NULL,
                    PRIMARY KEY (principal, workspace, session_id, seq)
                );
                CREATE TABLE IF NOT EXISTS attention_items (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    score DOUBLE PRECISION NOT NULL,
                    retained_until BIGINT NOT NULL,
                    PRIMARY KEY (principal, workspace, item_id)
                );
                CREATE TABLE IF NOT EXISTS outbox (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    id TEXT NOT NULL,
                    generation BIGINT NOT NULL,
                    claimed_by TEXT,
                    claimed_until BIGINT,
                    payload TEXT NOT NULL,
                    PRIMARY KEY (principal, workspace, id)
                );
                CREATE TABLE IF NOT EXISTS leases (
                    resource TEXT PRIMARY KEY,
                    owner TEXT NOT NULL,
                    generation BIGINT NOT NULL,
                    expires_at BIGINT NOT NULL
                );
                INSERT INTO schema_migrations(version) VALUES (1)
                    ON CONFLICT (version) DO NOTHING;
                "#,
            )
            .await
            .map_err(pg_err)
    }

    async fn migrate_v2_add_task_status(&self) -> Result<(), StorageError> {
        let mut client = self.client.lock().await;
        let tx = client.transaction().await.map_err(pg_err)?;
        tx.batch_execute(
            "ALTER TABLE tasks ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'open';
             INSERT INTO schema_migrations(version) VALUES (2) ON CONFLICT (version) DO NOTHING;",
        )
        .await
        .map_err(pg_err)?;
        tx.commit().await.map_err(pg_err)
    }

    async fn create_task(&self, task: &TaskRow) -> Result<TaskRow, StorageError> {
        let client = self.client.lock().await;
        let result = client
            .execute(
                "INSERT INTO tasks(principal, workspace, task_id, revision, idempotency_key, payload)
                 VALUES ($1, $2, $3, 1, $4, $5)",
                &[
                    &task.principal,
                    &task.workspace,
                    &task.task_id,
                    &task.idempotency_key,
                    &task.payload,
                ],
            )
            .await;
        match result {
            Ok(_) => get_task(&client, &task.principal, &task.workspace, &task.task_id)
                .await?
                .ok_or(StorageError::NotFound),
            Err(err) if is_unique(&err) => {
                match get_by_idempotency(
                    &client,
                    &task.principal,
                    &task.workspace,
                    &task.idempotency_key,
                )
                .await?
                {
                    Some(row) => Ok(row),
                    None => Err(StorageError::Conflict {
                        expected: None,
                        actual: Some("task exists".into()),
                    }),
                }
            },
            Err(err) => Err(pg_err(err)),
        }
    }

    async fn cas_task(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        expected: i64,
        payload: &str,
    ) -> Result<TaskRow, StorageError> {
        let client = self.client.lock().await;
        let changed = client
            .execute(
                "UPDATE tasks SET payload = $1, revision = revision + 1
                 WHERE principal = $2 AND workspace = $3 AND task_id = $4 AND revision = $5",
                &[&payload, &principal, &workspace, &task_id, &expected],
            )
            .await
            .map_err(pg_err)?;
        if changed != 1 {
            let actual = get_task(&client, principal, workspace, task_id)
                .await?
                .map(|row| row.revision.to_string());
            return Err(StorageError::Conflict {
                expected: Some(expected.to_string()),
                actual,
            });
        }
        get_task(&client, principal, workspace, task_id)
            .await?
            .ok_or(StorageError::NotFound)
    }

    async fn get_task(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Result<Option<TaskRow>, StorageError> {
        let client = self.client.lock().await;
        get_task(&client, principal, workspace, task_id).await
    }

    async fn create_session(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> Result<(), StorageError> {
        let client = self.client.lock().await;
        client
            .execute(
                "INSERT INTO chat_sessions(principal, workspace, session_id, created_at)
                 VALUES ($1, $2, $3, 1)",
                &[&principal, &workspace, &session_id],
            )
            .await
            .map(|_| ())
            .map_err(pg_err)
    }

    async fn append_message(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
        body: &str,
    ) -> Result<i64, StorageError> {
        let client = self.client.lock().await;
        let next: i64 = client
            .query_one(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM chat_messages
                 WHERE principal = $1 AND workspace = $2 AND session_id = $3",
                &[&principal, &workspace, &session_id],
            )
            .await
            .map_err(pg_err)?
            .get(0);
        client
            .execute(
                "INSERT INTO chat_messages(principal, workspace, session_id, seq, body)
                 VALUES ($1, $2, $3, $4, $5)",
                &[&principal, &workspace, &session_id, &next, &body],
            )
            .await
            .map_err(pg_err)?;
        Ok(next)
    }

    async fn page_messages(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<ChatPage, StorageError> {
        let client = self.client.lock().await;
        let rows = client
            .query(
                "SELECT seq, body FROM chat_messages
                 WHERE principal = $1 AND workspace = $2 AND session_id = $3 AND seq > $4
                 ORDER BY seq ASC LIMIT $5",
                &[&principal, &workspace, &session_id, &after_seq, &limit],
            )
            .await
            .map_err(pg_err)?;
        let mut seqs = Vec::new();
        let mut bodies = Vec::new();
        for row in rows {
            seqs.push(row.get(0));
            bodies.push(row.get(1));
        }
        Ok(ChatPage { seqs, bodies })
    }

    async fn put_attention(
        &self,
        principal: &str,
        workspace: &str,
        item_id: &str,
        score: f64,
        retained_until: i64,
    ) -> Result<(), StorageError> {
        let client = self.client.lock().await;
        client
            .execute(
                "INSERT INTO attention_items(principal, workspace, item_id, score, retained_until)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (principal, workspace, item_id) DO UPDATE
                   SET score = EXCLUDED.score, retained_until = EXCLUDED.retained_until",
                &[&principal, &workspace, &item_id, &score, &retained_until],
            )
            .await
            .map(|_| ())
            .map_err(pg_err)
    }

    async fn retained_attention(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<Vec<String>, StorageError> {
        let client = self.client.lock().await;
        let rows = client
            .query(
                "SELECT item_id FROM attention_items
                 WHERE principal = $1 AND workspace = $2 AND retained_until > $3
                 ORDER BY score DESC, item_id ASC",
                &[&principal, &workspace, &now],
            )
            .await
            .map_err(pg_err)?;
        Ok(rows.iter().map(|row| row.get(0)).collect())
    }

    async fn enqueue_outbox(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        payload: &str,
    ) -> Result<(), StorageError> {
        let client = self.client.lock().await;
        client
            .execute(
                "INSERT INTO outbox(principal, workspace, id, generation, payload)
                 VALUES ($1, $2, $3, 0, $4)",
                &[&principal, &workspace, &id, &payload],
            )
            .await
            .map(|_| ())
            .map_err(pg_err)
    }

    async fn claim_outbox(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        owner: &str,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError> {
        let client = self.client.lock().await;
        let row = client
            .query_opt(
                "UPDATE outbox
                 SET claimed_by = $1, claimed_until = $2, generation = generation + 1
                 WHERE principal = $3 AND workspace = $4 AND id = $5
                   AND (claimed_until IS NULL OR claimed_until < $6)
                 RETURNING generation",
                &[&owner, &(now + ttl), &principal, &workspace, &id, &now],
            )
            .await
            .map_err(pg_err)?;
        match row {
            Some(row) => Ok(row.get(0)),
            None => Err(StorageError::Conflict {
                expected: None,
                actual: Some("outbox claimed".into()),
            }),
        }
    }

    async fn acquire_lease(
        &self,
        resource: &str,
        owner: &str,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError> {
        let client = self.client.lock().await;
        let existing = client
            .query_opt(
                "SELECT owner, generation, expires_at FROM leases WHERE resource = $1",
                &[&resource],
            )
            .await
            .map_err(pg_err)?;
        match existing {
            Some(row) => {
                let held: String = row.get(0);
                let generation: i64 = row.get(1);
                let expires_at: i64 = row.get(2);
                if held != owner && expires_at >= now {
                    return Err(StorageError::Conflict {
                        expected: None,
                        actual: Some(generation.to_string()),
                    });
                }
                let next = generation + 1;
                client
                    .execute(
                        "UPDATE leases SET owner = $1, generation = $2, expires_at = $3 WHERE resource = $4",
                        &[&owner, &next, &(now + ttl), &resource],
                    )
                    .await
                    .map_err(pg_err)?;
                Ok(next)
            },
            None => {
                client
                    .execute(
                        "INSERT INTO leases(resource, owner, generation, expires_at)
                         VALUES ($1, $2, 1, $3)",
                        &[&resource, &owner, &(now + ttl)],
                    )
                    .await
                    .map_err(pg_err)?;
                Ok(1)
            },
        }
    }

    async fn renew_lease(
        &self,
        resource: &str,
        owner: &str,
        expected: i64,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError> {
        let client = self.client.lock().await;
        let changed = client
            .execute(
                "UPDATE leases SET expires_at = $1
                 WHERE resource = $2 AND owner = $3 AND generation = $4",
                &[&(now + ttl), &resource, &owner, &expected],
            )
            .await
            .map_err(pg_err)?;
        if changed != 1 {
            return Err(StorageError::LeaseLost {
                resource: resource.into(),
                generation: expected as u64,
            });
        }
        Ok(expected)
    }

    async fn uncommitted_insert_dropped(&self) -> Result<bool, StorageError> {
        {
            let mut client = self.client.lock().await;
            let tx = client.transaction().await.map_err(pg_err)?;
            tx.execute(
                "INSERT INTO tasks(principal, workspace, task_id, revision, idempotency_key, payload)
                 VALUES ('probe', 'probe', 'uncommitted', 1, 'uncommitted', 'x')",
                &[],
            )
            .await
            .map_err(pg_err)?;
        }
        let client = self.client.lock().await;
        let found = client
            .query_opt(
                "SELECT task_id FROM tasks WHERE task_id = 'uncommitted'",
                &[],
            )
            .await
            .map_err(pg_err)?;
        Ok(found.is_none())
    }

    async fn committed_insert_survives(&self) -> Result<bool, StorageError> {
        {
            let mut client = self.client.lock().await;
            let tx = client.transaction().await.map_err(pg_err)?;
            tx.execute(
                "INSERT INTO tasks(principal, workspace, task_id, revision, idempotency_key, payload)
                 VALUES ('probe', 'probe', 'committed', 1, 'committed', 'x')",
                &[],
            )
            .await
            .map_err(pg_err)?;
            tx.commit().await.map_err(pg_err)?;
        }
        let client = self.client.lock().await;
        let found = client
            .query_opt("SELECT task_id FROM tasks WHERE task_id = 'committed'", &[])
            .await
            .map_err(pg_err)?;
        Ok(found.is_some())
    }

    async fn backup_restore_round_trip(&self) -> Result<bool, StorageError> {
        // File-copy restore is not the Postgres backup story. Operators use
        // pg_dump/PITR. The spike records this as a distinct capability.
        Err(StorageError::UnsupportedCapability)
    }
}

async fn get_task(
    client: &Client,
    principal: &str,
    workspace: &str,
    task_id: &str,
) -> Result<Option<TaskRow>, StorageError> {
    let row = client
        .query_opt(
            "SELECT principal, workspace, task_id, revision, idempotency_key, payload
             FROM tasks WHERE principal = $1 AND workspace = $2 AND task_id = $3",
            &[&principal, &workspace, &task_id],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.map(|row| TaskRow {
        principal: row.get(0),
        workspace: row.get(1),
        task_id: row.get(2),
        revision: row.get(3),
        idempotency_key: row.get(4),
        payload: row.get(5),
    }))
}

async fn get_by_idempotency(
    client: &Client,
    principal: &str,
    workspace: &str,
    key: &str,
) -> Result<Option<TaskRow>, StorageError> {
    let row = client
        .query_opt(
            "SELECT principal, workspace, task_id, revision, idempotency_key, payload
             FROM tasks WHERE principal = $1 AND workspace = $2 AND idempotency_key = $3",
            &[&principal, &workspace, &key],
        )
        .await
        .map_err(pg_err)?;
    Ok(row.map(|row| TaskRow {
        principal: row.get(0),
        workspace: row.get(1),
        task_id: row.get(2),
        revision: row.get(3),
        idempotency_key: row.get(4),
        payload: row.get(5),
    }))
}

fn pg_err(err: tokio_postgres::Error) -> StorageError {
    StorageError::backend(err.to_string())
}

fn is_unique(err: &tokio_postgres::Error) -> bool {
    err.code()
        .is_some_and(|code| code == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
}
