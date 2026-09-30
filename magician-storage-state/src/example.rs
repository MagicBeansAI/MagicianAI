use std::sync::Arc;

use magician_storage::identifiers::ScopeId;
use magician_storage::{IdempotencyKey, Revision, StorageError};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::sqlite::{sql_err, SqlitePool};
use crate::EXAMPLE_STORE_ID;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleItem {
    pub principal: String,
    pub workspace: String,
    pub id: String,
    pub revision: u64,
    pub idempotency_key: String,
    pub payload: String,
}

/// Copyable repository skeleton. Not a Magician product owner.
pub struct ExampleRepository {
    pool: Arc<SqlitePool>,
}

impl ExampleRepository {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        self.pool
            .apply_migration(
                EXAMPLE_STORE_ID,
                1,
                "CREATE TABLE IF NOT EXISTS example_items (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    id TEXT NOT NULL,
                    revision INTEGER NOT NULL,
                    idempotency_key TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    PRIMARY KEY (principal, workspace, id)
                );
                CREATE UNIQUE INDEX IF NOT EXISTS example_items_idempotency
                    ON example_items(principal, workspace, idempotency_key);
                CREATE TABLE IF NOT EXISTS example_outbox (
                    principal TEXT NOT NULL,
                    workspace TEXT NOT NULL,
                    id TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    PRIMARY KEY (principal, workspace, id)
                );",
            )
            .await
    }

    pub async fn create(
        &self,
        scope: &ScopeId,
        id: &str,
        idempotency: &IdempotencyKey,
        payload: &str,
    ) -> Result<ExampleItem, StorageError> {
        let principal = scope.principal.as_str().to_string();
        let workspace = scope.workspace.as_str().to_string();
        let id = id.to_string();
        let key = idempotency.as_str().to_string();
        let payload = payload.to_string();
        self.pool
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(sql_err)?;
                let insert = tx.execute(
                    "INSERT INTO example_items(principal, workspace, id, revision, idempotency_key, payload)
                     VALUES (?1, ?2, ?3, 1, ?4, ?5)",
                    rusqlite::params![principal, workspace, id, key, payload],
                );
                let item = match insert {
                    Ok(_) => {
                        tx.execute(
                            "INSERT INTO example_outbox(principal, workspace, id, generation, payload)
                             VALUES (?1, ?2, ?3, 1, ?4)",
                            rusqlite::params![principal, workspace, id, payload],
                        )
                        .map_err(sql_err)?;
                        get_item(&tx, &principal, &workspace, &id)?
                            .ok_or(StorageError::NotFound)?
                    }
                    Err(err) if is_unique(&err) => {
                        let item = get_by_idempotency(&tx, &principal, &workspace, &key)?
                            .ok_or(StorageError::Conflict {
                                expected: None,
                                actual: Some("exists".into()),
                            })?;
                        tx.execute(
                            "INSERT INTO example_outbox(principal, workspace, id, generation, payload)
                             VALUES (?1, ?2, ?3, 1, ?4)
                             ON CONFLICT(principal, workspace, id) DO NOTHING",
                            rusqlite::params![item.principal, item.workspace, item.id, item.payload],
                        )
                        .map_err(sql_err)?;
                        item
                    }
                    Err(err) => return Err(sql_err(err)),
                };
                tx.commit().map_err(sql_err)?;
                Ok(item)
            })
            .await
    }

    pub async fn get(
        &self,
        scope: &ScopeId,
        id: &str,
    ) -> Result<Option<ExampleItem>, StorageError> {
        let principal = scope.principal.as_str().to_string();
        let workspace = scope.workspace.as_str().to_string();
        let id = id.to_string();
        self.pool
            .with_conn(move |conn| get_item(conn, &principal, &workspace, &id))
            .await
    }

    pub async fn compare_and_update(
        &self,
        scope: &ScopeId,
        id: &str,
        expected: Revision,
        payload: &str,
    ) -> Result<ExampleItem, StorageError> {
        let principal = scope.principal.as_str().to_string();
        let workspace = scope.workspace.as_str().to_string();
        let id = id.to_string();
        let payload = payload.to_string();
        let expected = expected.get() as i64;
        self.pool
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(sql_err)?;
                let item = match tx.query_row(
                    "UPDATE example_items SET payload = ?1, revision = revision + 1
                     WHERE principal = ?2 AND workspace = ?3 AND id = ?4 AND revision = ?5
                     RETURNING principal, workspace, id, revision, idempotency_key, payload",
                    rusqlite::params![payload, principal, workspace, id, expected],
                    row_to_item,
                ) {
                    Ok(item) => item,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        let actual = get_item(&tx, &principal, &workspace, &id)?
                            .map(|item| item.revision.to_string());
                        return Err(StorageError::Conflict {
                            expected: Some(expected.to_string()),
                            actual,
                        });
                    },
                    Err(err) => return Err(sql_err(err)),
                };
                tx.commit().map_err(sql_err)?;
                Ok(item)
            })
            .await
    }

    pub async fn export_scope(&self, scope: &ScopeId) -> Result<Vec<u8>, StorageError> {
        let principal = scope.principal.as_str().to_string();
        let workspace = scope.workspace.as_str().to_string();
        let items = self
            .pool
            .with_conn(move |conn| {
                let mut stmt = conn
                    .prepare(
                        "SELECT principal, workspace, id, revision, idempotency_key, payload
                         FROM example_items WHERE principal = ?1 AND workspace = ?2
                         ORDER BY id",
                    )
                    .map_err(sql_err)?;
                let rows = stmt
                    .query_map(rusqlite::params![principal, workspace], row_to_item)
                    .map_err(sql_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
            })
            .await?;
        serde_json::to_vec(&items).map_err(|err| StorageError::backend(err.to_string()))
    }

    pub async fn import_scope(&self, scope: &ScopeId, bytes: &[u8]) -> Result<(), StorageError> {
        let items: Vec<ExampleItem> =
            serde_json::from_slice(bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })?;
        let principal = scope.principal.as_str();
        if items
            .iter()
            .any(|item| item.principal != principal || item.workspace != scope.workspace.as_str())
        {
            return Err(StorageError::invalid_key("export scope mismatch"));
        }
        let principal = principal.to_string();
        let workspace = scope.workspace.as_str().to_string();
        self.pool
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(sql_err)?;
                tx.execute(
                    "DELETE FROM example_items WHERE principal = ?1 AND workspace = ?2",
                    rusqlite::params![principal, workspace],
                )
                .map_err(sql_err)?;
                for item in items {
                    tx.execute(
                        "INSERT INTO example_items(principal, workspace, id, revision, idempotency_key, payload)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![
                            item.principal,
                            item.workspace,
                            item.id,
                            item.revision as i64,
                            item.idempotency_key,
                            item.payload
                        ],
                    )
                    .map_err(sql_err)?;
                }
                tx.commit().map_err(sql_err)?;
                Ok(())
            })
            .await
    }

    pub async fn uncommitted_is_dropped(&self) -> Result<bool, StorageError> {
        self.pool
            .with_conn(|conn| {
                let tx = conn.unchecked_transaction().map_err(sql_err)?;
                tx.execute(
                    "INSERT INTO example_items(principal, workspace, id, revision, idempotency_key, payload)
                     VALUES ('probe', 'probe', 'uncommitted', 1, 'uncommitted', 'x')",
                    [],
                )
                .map_err(sql_err)?;
                drop(tx);
                let found: Option<String> = conn
                    .query_row(
                        "SELECT id FROM example_items WHERE id = 'uncommitted'",
                        [],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(sql_err)?;
                Ok(found.is_none())
            })
            .await
    }

    pub async fn committed_survives(&self) -> Result<bool, StorageError> {
        self.pool
            .with_conn(|conn| {
                {
                    let tx = conn.unchecked_transaction().map_err(sql_err)?;
                    tx.execute(
                        "INSERT INTO example_items(principal, workspace, id, revision, idempotency_key, payload)
                         VALUES ('probe', 'probe', 'committed', 1, 'committed', 'x')",
                        [],
                    )
                    .map_err(sql_err)?;
                    tx.commit().map_err(sql_err)?;
                }
                let found: Option<String> = conn
                    .query_row(
                        "SELECT id FROM example_items WHERE id = 'committed'",
                        [],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(sql_err)?;
                Ok(found.is_some())
            })
            .await
    }
}

fn get_item(
    conn: &rusqlite::Connection,
    principal: &str,
    workspace: &str,
    id: &str,
) -> Result<Option<ExampleItem>, StorageError> {
    conn.query_row(
        "SELECT principal, workspace, id, revision, idempotency_key, payload
         FROM example_items WHERE principal = ?1 AND workspace = ?2 AND id = ?3",
        rusqlite::params![principal, workspace, id],
        row_to_item,
    )
    .optional()
    .map_err(sql_err)
}

fn get_by_idempotency(
    conn: &rusqlite::Connection,
    principal: &str,
    workspace: &str,
    key: &str,
) -> Result<Option<ExampleItem>, StorageError> {
    conn.query_row(
        "SELECT principal, workspace, id, revision, idempotency_key, payload
         FROM example_items WHERE principal = ?1 AND workspace = ?2 AND idempotency_key = ?3",
        rusqlite::params![principal, workspace, key],
        row_to_item,
    )
    .optional()
    .map_err(sql_err)
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExampleItem> {
    Ok(ExampleItem {
        principal: row.get(0)?,
        workspace: row.get(1)?,
        id: row.get(2)?,
        revision: row.get::<_, i64>(3)? as u64,
        idempotency_key: row.get(4)?,
        payload: row.get(5)?,
    })
}

fn is_unique(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}
