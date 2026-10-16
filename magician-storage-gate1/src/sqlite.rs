use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use magician_storage::StorageError;
use rusqlite::{params, Connection, OptionalExtension};

use crate::store::{ChatPage, Gate1Store, TaskRow};

pub struct SqliteStore {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
}

impl SqliteStore {
    pub fn memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory().map_err(sqlite_err)?;
        conn.busy_timeout(std::time::Duration::from_millis(5))
            .map_err(sqlite_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: None,
        })
    }

    pub fn file(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let conn = Connection::open(&path).map_err(sqlite_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sqlite_err)?;
        conn.busy_timeout(std::time::Duration::from_millis(5))
            .map_err(sqlite_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: Some(path),
        })
    }

    pub fn with_immediate_write<F, R>(&self, f: F) -> Result<R, StorageError>
    where
        F: FnOnce() -> R,
    {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute_batch("BEGIN IMMEDIATE").map_err(sqlite_err)?;
        let result = f();
        let _ = conn.execute_batch("ROLLBACK");
        Ok(result)
    }

    pub fn begin_immediate(&self) -> Result<(), StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute_batch("BEGIN IMMEDIATE").map_err(sqlite_err)
    }
}

#[async_trait]
impl Gate1Store for SqliteStore {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    async fn migrate(&self) -> Result<(), StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS tasks (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                task_id TEXT NOT NULL,
                revision INTEGER NOT NULL,
                idempotency_key TEXT NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY (principal, workspace, task_id)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS tasks_idempotency
                ON tasks(principal, workspace, idempotency_key);
            CREATE TABLE IF NOT EXISTS chat_sessions (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                session_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (principal, workspace, session_id)
            );
            CREATE TABLE IF NOT EXISTS chat_messages (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                session_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                body TEXT NOT NULL,
                PRIMARY KEY (principal, workspace, session_id, seq)
            );
            CREATE TABLE IF NOT EXISTS attention_items (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                item_id TEXT NOT NULL,
                score REAL NOT NULL,
                retained_until INTEGER NOT NULL,
                PRIMARY KEY (principal, workspace, item_id)
            );
            CREATE TABLE IF NOT EXISTS outbox (
                principal TEXT NOT NULL,
                workspace TEXT NOT NULL,
                id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                claimed_by TEXT,
                claimed_until INTEGER,
                payload TEXT NOT NULL,
                PRIMARY KEY (principal, workspace, id)
            );
            CREATE TABLE IF NOT EXISTS leases (
                resource TEXT PRIMARY KEY,
                owner TEXT NOT NULL,
                generation INTEGER NOT NULL,
                expires_at INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO schema_migrations(version) VALUES (1);
            "#,
        )
        .map_err(sqlite_err)
    }

    async fn migrate_v2_add_task_status(&self) -> Result<(), StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        let version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite_err)?;
        if version >= 2 {
            return Ok(());
        }
        conn.execute_batch(
            "ALTER TABLE tasks ADD COLUMN status TEXT NOT NULL DEFAULT 'open';
             INSERT OR IGNORE INTO schema_migrations(version) VALUES (2);",
        )
        .map_err(sqlite_err)
    }

    async fn create_task(&self, task: &TaskRow) -> Result<TaskRow, StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        let result = conn.execute(
            "INSERT INTO tasks(principal, workspace, task_id, revision, idempotency_key, payload)
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            params![
                task.principal,
                task.workspace,
                task.task_id,
                task.idempotency_key,
                task.payload
            ],
        );
        match result {
            Ok(_) => self
                .get_task_locked(&conn, &task.principal, &task.workspace, &task.task_id)?
                .ok_or(StorageError::NotFound),
            Err(err) if is_unique(&err) => {
                let existing = self.get_by_idempotency_locked(
                    &conn,
                    &task.principal,
                    &task.workspace,
                    &task.idempotency_key,
                )?;
                match existing {
                    Some(row) => Ok(row),
                    None => Err(StorageError::Conflict {
                        expected: None,
                        actual: Some("task exists".into()),
                    }),
                }
            },
            Err(err) => Err(sqlite_err(err)),
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
        let conn = self.conn.lock().expect("sqlite");
        let changed = conn
            .execute(
                "UPDATE tasks SET payload = ?1, revision = revision + 1
                 WHERE principal = ?2 AND workspace = ?3 AND task_id = ?4 AND revision = ?5",
                params![payload, principal, workspace, task_id, expected],
            )
            .map_err(sqlite_err)?;
        if changed != 1 {
            let actual = self
                .get_task_locked(&conn, principal, workspace, task_id)?
                .map(|row| row.revision.to_string());
            return Err(StorageError::Conflict {
                expected: Some(expected.to_string()),
                actual,
            });
        }
        self.get_task_locked(&conn, principal, workspace, task_id)?
            .ok_or(StorageError::NotFound)
    }

    async fn get_task(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Result<Option<TaskRow>, StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        self.get_task_locked(&conn, principal, workspace, task_id)
    }

    async fn create_session(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO chat_sessions(principal, workspace, session_id, created_at)
             VALUES (?1, ?2, ?3, 1)",
            params![principal, workspace, session_id],
        )
        .map(|_| ())
        .map_err(sqlite_err)
    }

    async fn append_message(
        &self,
        principal: &str,
        workspace: &str,
        session_id: &str,
        body: &str,
    ) -> Result<i64, StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        let next: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM chat_messages
                 WHERE principal = ?1 AND workspace = ?2 AND session_id = ?3",
                params![principal, workspace, session_id],
                |row| row.get(0),
            )
            .map_err(sqlite_err)?;
        conn.execute(
            "INSERT INTO chat_messages(principal, workspace, session_id, seq, body)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![principal, workspace, session_id, next, body],
        )
        .map_err(sqlite_err)?;
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
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare(
                "SELECT seq, body FROM chat_messages
                 WHERE principal = ?1 AND workspace = ?2 AND session_id = ?3 AND seq > ?4
                 ORDER BY seq ASC LIMIT ?5",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(
                params![principal, workspace, session_id, after_seq, limit],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(sqlite_err)?;
        let mut seqs = Vec::new();
        let mut bodies = Vec::new();
        for row in rows {
            let (seq, body) = row.map_err(sqlite_err)?;
            seqs.push(seq);
            bodies.push(body);
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
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT OR REPLACE INTO attention_items(principal, workspace, item_id, score, retained_until)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![principal, workspace, item_id, score, retained_until],
        )
        .map(|_| ())
        .map_err(sqlite_err)
    }

    async fn retained_attention(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<Vec<String>, StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare(
                "SELECT item_id FROM attention_items
                 WHERE principal = ?1 AND workspace = ?2 AND retained_until > ?3
                 ORDER BY score DESC, item_id ASC",
            )
            .map_err(sqlite_err)?;
        let rows = stmt
            .query_map(params![principal, workspace, now], |row| row.get(0))
            .map_err(sqlite_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sqlite_err)
    }

    async fn enqueue_outbox(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        payload: &str,
    ) -> Result<(), StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO outbox(principal, workspace, id, generation, payload)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![principal, workspace, id, payload],
        )
        .map(|_| ())
        .map_err(sqlite_err)
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
        let conn = self.conn.lock().expect("sqlite");
        let changed = conn
            .execute(
                "UPDATE outbox
                 SET claimed_by = ?1, claimed_until = ?2, generation = generation + 1
                 WHERE principal = ?3 AND workspace = ?4 AND id = ?5
                   AND (claimed_until IS NULL OR claimed_until < ?6)",
                params![owner, now + ttl, principal, workspace, id, now],
            )
            .map_err(sqlite_err)?;
        if changed != 1 {
            return Err(StorageError::Conflict {
                expected: None,
                actual: Some("outbox claimed".into()),
            });
        }
        conn.query_row(
            "SELECT generation FROM outbox WHERE principal = ?1 AND workspace = ?2 AND id = ?3",
            params![principal, workspace, id],
            |row| row.get(0),
        )
        .map_err(sqlite_err)
    }

    async fn acquire_lease(
        &self,
        resource: &str,
        owner: &str,
        now: i64,
        ttl: i64,
    ) -> Result<i64, StorageError> {
        let conn = self.conn.lock().expect("sqlite");
        let existing: Option<(String, i64, i64)> = conn
            .query_row(
                "SELECT owner, generation, expires_at FROM leases WHERE resource = ?1",
                params![resource],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sqlite_err)?;
        match existing {
            Some((held, generation, expires_at)) if held != owner && expires_at >= now => {
                Err(StorageError::Conflict {
                    expected: None,
                    actual: Some(generation.to_string()),
                })
            },
            Some((_, generation, _)) => {
                let next = generation + 1;
                conn.execute(
                    "UPDATE leases SET owner = ?1, generation = ?2, expires_at = ?3 WHERE resource = ?4",
                    params![owner, next, now + ttl, resource],
                )
                .map_err(sqlite_err)?;
                Ok(next)
            },
            None => {
                conn.execute(
                    "INSERT INTO leases(resource, owner, generation, expires_at) VALUES (?1, ?2, 1, ?3)",
                    params![resource, owner, now + ttl],
                )
                .map_err(sqlite_err)?;
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
        let conn = self.conn.lock().expect("sqlite");
        let changed = conn
            .execute(
                "UPDATE leases SET expires_at = ?1
                 WHERE resource = ?2 AND owner = ?3 AND generation = ?4",
                params![now + ttl, resource, owner, expected],
            )
            .map_err(sqlite_err)?;
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
            let mut conn = self.conn.lock().expect("sqlite");
            let tx = conn.transaction().map_err(sqlite_err)?;
            tx.execute(
                "INSERT INTO tasks(principal, workspace, task_id, revision, idempotency_key, payload)
                 VALUES ('probe', 'probe', 'uncommitted', 1, 'uncommitted', 'x')",
                [],
            )
            .map_err(sqlite_err)?;
        }
        let conn = self.conn.lock().expect("sqlite");
        let found: Option<String> = conn
            .query_row(
                "SELECT task_id FROM tasks WHERE task_id = 'uncommitted'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        Ok(found.is_none())
    }

    async fn committed_insert_survives(&self) -> Result<bool, StorageError> {
        {
            let mut conn = self.conn.lock().expect("sqlite");
            let tx = conn.transaction().map_err(sqlite_err)?;
            tx.execute(
                "INSERT INTO tasks(principal, workspace, task_id, revision, idempotency_key, payload)
                 VALUES ('probe', 'probe', 'committed', 1, 'committed', 'x')",
                [],
            )
            .map_err(sqlite_err)?;
            tx.commit().map_err(sqlite_err)?;
        }
        let conn = self.conn.lock().expect("sqlite");
        let found: Option<String> = conn
            .query_row(
                "SELECT task_id FROM tasks WHERE task_id = 'committed'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_err)?;
        Ok(found.is_some())
    }

    async fn backup_restore_round_trip(&self) -> Result<bool, StorageError> {
        let Some(path) = &self.path else {
            return Err(StorageError::UnsupportedCapability);
        };
        let backup_path = path.with_extension("bak");
        {
            let conn = self.conn.lock().expect("sqlite");
            let mut dst = Connection::open(&backup_path).map_err(sqlite_err)?;
            let backup = rusqlite::backup::Backup::new(&conn, &mut dst).map_err(sqlite_err)?;
            backup
                .run_to_completion(5, std::time::Duration::from_millis(1), None)
                .map_err(sqlite_err)?;
        }
        let restored = Connection::open(&backup_path).map_err(sqlite_err)?;
        let tasks: i64 = restored
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .map_err(sqlite_err)?;
        let original: i64 = self
            .conn
            .lock()
            .expect("sqlite")
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
            .map_err(sqlite_err)?;
        Ok(tasks == original && original > 0)
    }
}

impl SqliteStore {
    fn get_task_locked(
        &self,
        conn: &Connection,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Result<Option<TaskRow>, StorageError> {
        conn.query_row(
            "SELECT principal, workspace, task_id, revision, idempotency_key, payload
             FROM tasks WHERE principal = ?1 AND workspace = ?2 AND task_id = ?3",
            params![principal, workspace, task_id],
            row_to_task,
        )
        .optional()
        .map_err(sqlite_err)
    }

    fn get_by_idempotency_locked(
        &self,
        conn: &Connection,
        principal: &str,
        workspace: &str,
        key: &str,
    ) -> Result<Option<TaskRow>, StorageError> {
        conn.query_row(
            "SELECT principal, workspace, task_id, revision, idempotency_key, payload
             FROM tasks WHERE principal = ?1 AND workspace = ?2 AND idempotency_key = ?3",
            params![principal, workspace, key],
            row_to_task,
        )
        .optional()
        .map_err(sqlite_err)
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    Ok(TaskRow {
        principal: row.get(0)?,
        workspace: row.get(1)?,
        task_id: row.get(2)?,
        revision: row.get(3)?,
        idempotency_key: row.get(4)?,
        payload: row.get(5)?,
    })
}

fn sqlite_err(err: rusqlite::Error) -> StorageError {
    StorageError::backend(err.to_string())
}

fn is_unique(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}
