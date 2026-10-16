use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use magician_storage::lease::{LeaseObservation, LeaseResource, LeaseStore, LeaseToken, OwnerId};
use magician_storage::StorageError;
use rusqlite::OptionalExtension;

use crate::postgres::PostgresPool;
use crate::sqlite::{sql_err, SqlitePool};

pub struct SqliteLeaseStore {
    pool: Arc<SqlitePool>,
}

impl SqliteLeaseStore {
    pub fn new(pool: Arc<SqlitePool>) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        self.pool
            .apply_migration(
                "leases",
                1,
                "CREATE TABLE IF NOT EXISTS leases (
                    resource TEXT PRIMARY KEY,
                    owner TEXT NOT NULL,
                    generation INTEGER NOT NULL,
                    expires_at INTEGER NOT NULL
                );",
            )
            .await
    }

    /// Test helper to break fencing. Not a service API.
    pub async fn force_generation(
        &self,
        resource: &str,
        generation: i64,
    ) -> Result<(), StorageError> {
        let resource = resource.to_string();
        self.pool
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE leases SET generation = ?1 WHERE resource = ?2",
                    rusqlite::params![generation, resource],
                )
                .map_err(sql_err)?;
                Ok(())
            })
            .await
    }
}

#[async_trait]
impl LeaseStore for SqliteLeaseStore {
    async fn acquire(
        &self,
        resource: LeaseResource,
        owner: OwnerId,
        ttl: Duration,
    ) -> Result<LeaseToken, StorageError> {
        let now = Utc::now().timestamp_millis();
        let ttl_ms = ttl.as_millis() as i64;
        let resource_s = resource.as_str().to_string();
        let owner_s = owner.as_str().to_string();
        let generation = self
            .pool
            .with_conn(move |conn| {
                let tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(sql_err)?;
                let generation: i64 = match tx.query_row(
                    "INSERT INTO leases(resource, owner, generation, expires_at)
                     VALUES (?1, ?2, 1, ?3)
                     ON CONFLICT(resource) DO UPDATE SET
                       owner = excluded.owner,
                       generation = leases.generation + 1,
                       expires_at = excluded.expires_at
                     WHERE leases.owner = excluded.owner
                        OR leases.expires_at < ?4
                     RETURNING generation",
                    rusqlite::params![resource_s, owner_s, now + ttl_ms, now],
                    |row| row.get(0),
                ) {
                    Ok(generation) => generation,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        return Err(StorageError::Conflict {
                            expected: None,
                            actual: None,
                        });
                    },
                    Err(err) => return Err(sql_err(err)),
                };
                tx.commit().map_err(sql_err)?;
                Ok(generation)
            })
            .await?;
        Ok(LeaseToken {
            resource,
            generation: generation as u64,
            expires_at: millis_to_utc(now + ttl_ms),
            owner,
        })
    }

    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<LeaseToken, StorageError> {
        let now = Utc::now().timestamp_millis();
        let ttl_ms = ttl.as_millis() as i64;
        let resource = token.resource.as_str().to_string();
        let owner = token.owner.as_str().to_string();
        let expected = token.generation as i64;
        let changed = self
            .pool
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE leases SET expires_at = ?1
                     WHERE resource = ?2 AND owner = ?3 AND generation = ?4",
                    rusqlite::params![now + ttl_ms, resource, owner, expected],
                )
                .map_err(sql_err)
            })
            .await?;
        if changed != 1 {
            return Err(StorageError::LeaseLost {
                resource: token.resource.as_str().to_string(),
                generation: token.generation,
            });
        }
        let mut next = token.clone();
        next.expires_at = millis_to_utc(now + ttl_ms);
        Ok(next)
    }

    async fn release(&self, token: LeaseToken) -> Result<(), StorageError> {
        let resource = token.resource.as_str().to_string();
        let expected = token.generation as i64;
        self.pool
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE leases SET expires_at = 0
                     WHERE resource = ?1 AND generation = ?2",
                    rusqlite::params![resource, expected],
                )
                .map_err(sql_err)?;
                Ok(())
            })
            .await
    }

    async fn inspect(
        &self,
        resource: &LeaseResource,
    ) -> Result<Option<LeaseObservation>, StorageError> {
        let resource_s = resource.as_str().to_string();
        self.pool
            .with_conn(move |conn| {
                let row: Option<(String, i64, i64)> = conn
                    .query_row(
                        "SELECT owner, generation, expires_at FROM leases WHERE resource = ?1",
                        [resource_s.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(sql_err)?;
                Ok(row.map(|(owner, generation, expires_at)| LeaseObservation {
                    token: LeaseToken {
                        resource: LeaseResource::parse(&resource_s)
                            .unwrap_or_else(|_| LeaseResource::parse("unknown").expect("static")),
                        generation: generation as u64,
                        expires_at: millis_to_utc(expires_at),
                        owner: OwnerId::parse(&owner)
                            .unwrap_or_else(|_| OwnerId::parse("unknown").expect("static")),
                    },
                }))
            })
            .await
    }
}

pub struct PostgresLeaseStore {
    pool: PostgresPool,
}

impl PostgresLeaseStore {
    pub fn new(pool: PostgresPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        let client = self.pool.connect().await?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS leases (
                    resource TEXT PRIMARY KEY,
                    owner TEXT NOT NULL,
                    generation BIGINT NOT NULL,
                    expires_at BIGINT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS schema_ledger (
                    store_id TEXT PRIMARY KEY,
                    version INTEGER NOT NULL
                );
                INSERT INTO schema_ledger(store_id, version) VALUES ('leases', 1)
                    ON CONFLICT (store_id) DO NOTHING;",
            )
            .await
            .map_err(crate::postgres::pg_err)?;
        Ok(())
    }
}

#[async_trait]
impl LeaseStore for PostgresLeaseStore {
    async fn acquire(
        &self,
        resource: LeaseResource,
        owner: OwnerId,
        ttl: Duration,
    ) -> Result<LeaseToken, StorageError> {
        let client = self.pool.connect().await?;
        let ttl_ms = ttl.as_millis() as i64;
        let row = client
            .query_opt(
                "INSERT INTO leases(resource, owner, generation, expires_at)
                 VALUES (
                   $1,
                   $2,
                   1,
                   (EXTRACT(EPOCH FROM CLOCK_TIMESTAMP()) * 1000)::bigint + $3
                 )
                 ON CONFLICT (resource) DO UPDATE SET
                   owner = EXCLUDED.owner,
                   generation = leases.generation + 1,
                   expires_at = EXCLUDED.expires_at
                 WHERE leases.owner = EXCLUDED.owner
                    OR leases.expires_at
                       < (EXTRACT(EPOCH FROM CLOCK_TIMESTAMP()) * 1000)::bigint
                 RETURNING generation, expires_at",
                &[&resource.as_str(), &owner.as_str(), &ttl_ms],
            )
            .await
            .map_err(crate::postgres::pg_err)?;
        let Some(row) = row else {
            return Err(StorageError::Conflict {
                expected: None,
                actual: None,
            });
        };
        let generation: i64 = row.get(0);
        let expires_at: i64 = row.get(1);
        Ok(LeaseToken {
            resource,
            generation: generation as u64,
            expires_at: millis_to_utc(expires_at),
            owner,
        })
    }

    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<LeaseToken, StorageError> {
        let client = self.pool.connect().await?;
        let ttl_ms = ttl.as_millis() as i64;
        let expected = token.generation as i64;
        let row = client
            .query_opt(
                "UPDATE leases SET expires_at =
                   (EXTRACT(EPOCH FROM CLOCK_TIMESTAMP()) * 1000)::bigint + $1
                 WHERE resource = $2 AND owner = $3 AND generation = $4
                 RETURNING expires_at",
                &[
                    &ttl_ms,
                    &token.resource.as_str(),
                    &token.owner.as_str(),
                    &expected,
                ],
            )
            .await
            .map_err(crate::postgres::pg_err)?;
        let Some(row) = row else {
            return Err(StorageError::LeaseLost {
                resource: token.resource.as_str().to_string(),
                generation: token.generation,
            });
        };
        let expires_at: i64 = row.get(0);
        let mut next = token.clone();
        next.expires_at = millis_to_utc(expires_at);
        Ok(next)
    }

    async fn release(&self, token: LeaseToken) -> Result<(), StorageError> {
        let client = self.pool.connect().await?;
        let expected = token.generation as i64;
        client
            .execute(
                "UPDATE leases SET expires_at = 0 WHERE resource = $1 AND generation = $2",
                &[&token.resource.as_str(), &expected],
            )
            .await
            .map_err(crate::postgres::pg_err)?;
        Ok(())
    }

    async fn inspect(
        &self,
        resource: &LeaseResource,
    ) -> Result<Option<LeaseObservation>, StorageError> {
        let client = self.pool.connect().await?;
        let row = client
            .query_opt(
                "SELECT owner, generation, expires_at FROM leases WHERE resource = $1",
                &[&resource.as_str()],
            )
            .await
            .map_err(crate::postgres::pg_err)?;
        Ok(row.map(|row| {
            let owner: String = row.get(0);
            let generation: i64 = row.get(1);
            let expires_at: i64 = row.get(2);
            LeaseObservation {
                token: LeaseToken {
                    resource: resource.clone(),
                    generation: generation as u64,
                    expires_at: millis_to_utc(expires_at),
                    owner: OwnerId::parse(&owner)
                        .unwrap_or_else(|_| OwnerId::parse("unknown").expect("static")),
                },
            }
        }))
    }
}

fn millis_to_utc(ms: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Utc::now)
}
