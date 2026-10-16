use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::Path;

use super::token::{SpendToken, TokenStatus};

/// Error type for token store operations.
#[derive(Debug, Clone)]
pub enum TokenStoreError {
    NotFound(String),
    AlreadyExists(String),
    InvalidTransition {
        token_id: String,
        from: TokenStatus,
        to: TokenStatus,
    },
}

impl std::fmt::Display for TokenStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenStoreError::NotFound(id) => write!(f, "Token not found: {}", id),
            TokenStoreError::AlreadyExists(id) => write!(f, "Token already exists: {}", id),
            TokenStoreError::InvalidTransition { token_id, from, to } => {
                write!(
                    f,
                    "Invalid token transition for {}: {:?} -> {:?}",
                    token_id, from, to
                )
            },
        }
    }
}

impl std::error::Error for TokenStoreError {}

/// In-memory store for SpendTokens with JSON file persistence.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenStore {
    pub tokens: HashMap<String, SpendToken>,
}

impl TokenStore {
    /// Create a new empty token store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a new token. Fails if a token with the same ID already exists.
    pub fn create(&mut self, token: SpendToken) -> Result<(), TokenStoreError> {
        if self.tokens.contains_key(&token.id) {
            return Err(TokenStoreError::AlreadyExists(token.id.clone()));
        }
        self.tokens.insert(token.id.clone(), token);
        Ok(())
    }

    /// Get an immutable reference to a token by ID.
    pub fn get(&self, id: &str) -> Result<&SpendToken, TokenStoreError> {
        self.tokens
            .get(id)
            .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))
    }

    /// Get a mutable reference to a token by ID.
    pub fn get_mut(&mut self, id: &str) -> Result<&mut SpendToken, TokenStoreError> {
        self.tokens
            .get_mut(id)
            .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))
    }

    /// Revoke a token: sets status to Revoked.
    pub fn revoke(&mut self, id: &str) -> Result<(), TokenStoreError> {
        let token = self
            .tokens
            .get_mut(id)
            .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))?;
        if token.status == TokenStatus::Revoked {
            return Err(TokenStoreError::InvalidTransition {
                token_id: id.to_string(),
                from: TokenStatus::Revoked,
                to: TokenStatus::Revoked,
            });
        }
        token.status = TokenStatus::Revoked;
        Ok(())
    }

    /// Mark a token as expired: sets status to Expired.
    pub fn mark_expired(&mut self, id: &str) -> Result<(), TokenStoreError> {
        let token = self
            .tokens
            .get_mut(id)
            .ok_or_else(|| TokenStoreError::NotFound(id.to_string()))?;
        if token.status == TokenStatus::Expired {
            // Already expired — idempotent, no error
            return Ok(());
        }
        token.status = TokenStatus::Expired;
        Ok(())
    }

    /// List all tokens with Active status.
    pub fn list_active(&self) -> Vec<&SpendToken> {
        self.tokens
            .values()
            .filter(|t| t.status == TokenStatus::Active)
            .collect()
    }

    /// List all tokens issued to a specific agent.
    pub fn list_by_issued_to(&self, agent_id: &str) -> Vec<&SpendToken> {
        self.tokens
            .values()
            .filter(|t| t.issued_to == agent_id)
            .collect()
    }

    /// Persist the token store to a JSON file.
    ///
    /// Atomic on disk: serialises to a buffer in memory, then
    /// writes-then-renames via `persistence::atomic_write_bytes`.
    /// Required because `TokenStore::save` is called from multiple
    /// paths (REST API `persist_token_store` + dispatch gate via
    /// `ScopedAuthorityBundle::persist_state`), and the previous
    /// "write directly" pattern left a window where a concurrent
    /// reader (or a crash mid-write) could see a partial file —
    /// `TokenStore::load` would then fail to deserialise and the
    /// scope would silently revert to an empty store on next load.
    pub fn save(&self, path: &Path) -> Result<(), io::Error> {
        let json = self.to_json_bytes()?;
        crate::magician_v2::resource_authority::persistence::atomic_write_bytes(path, &json)
    }

    /// Load a token store from a JSON file. Returns an empty store if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self, io::Error> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let json = std::fs::read_to_string(path)?;
        Self::from_json_str(&json)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, io::Error> {
        serde_json::to_vec_pretty(self).map_err(io::Error::other)
    }

    pub fn from_json_str(json: &str) -> Result<Self, io::Error> {
        let store: Self = serde_json::from_str(json)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(store)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::resource_authority::token::{CarryoverPolicy, CeilingPeriod};
    use chrono::Utc;
    use rust_decimal::Decimal;
    use tempfile::NamedTempFile;

    fn make_token(id: &str, issued_to: &str) -> SpendToken {
        SpendToken {
            id: id.to_string(),
            issued_by: "cfo".to_string(),
            issued_to: issued_to.to_string(),
            commodity: "USD".to_string(),
            ceiling: Decimal::new(500, 0),
            period: CeilingPeriod::Total,
            carryover: CarryoverPolicy::None,
            conditions: vec![],
            velocity_limit: None,
            status: TokenStatus::Active,
            expires_at: None,
            created_at: Utc::now(),
            system_ceiling_id: None,
            last_period_start: None,
        }
    }

    #[test]
    fn test_create_and_get() {
        let mut store = TokenStore::new();
        let token = make_token("t1", "agent_a");
        store.create(token).unwrap();
        let retrieved = store.get("t1").unwrap();
        assert_eq!(retrieved.id, "t1");
        assert_eq!(retrieved.issued_to, "agent_a");
    }

    #[test]
    fn test_create_duplicate_fails() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "a")).unwrap();
        assert!(store.create(make_token("t1", "b")).is_err());
    }

    #[test]
    fn test_revoke() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "a")).unwrap();
        store.revoke("t1").unwrap();
        assert_eq!(store.get("t1").unwrap().status, TokenStatus::Revoked);
    }

    #[test]
    fn test_mark_expired() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "a")).unwrap();
        store.mark_expired("t1").unwrap();
        assert_eq!(store.get("t1").unwrap().status, TokenStatus::Expired);
    }

    #[test]
    fn test_list_active() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "a")).unwrap();
        store.create(make_token("t2", "b")).unwrap();
        store.revoke("t1").unwrap();
        let active = store.list_active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "t2");
    }

    #[test]
    fn test_list_by_issued_to() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "agent_a")).unwrap();
        store.create(make_token("t2", "agent_b")).unwrap();
        store.create(make_token("t3", "agent_a")).unwrap();
        let tokens = store.list_by_issued_to("agent_a");
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "a")).unwrap();
        store.create(make_token("t2", "b")).unwrap();

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        store.save(&path).unwrap();

        let loaded = TokenStore::load(&path).unwrap();
        assert_eq!(loaded.tokens.len(), 2);
        assert_eq!(loaded.get("t1").unwrap().issued_to, "a");
        assert_eq!(loaded.get("t2").unwrap().issued_to, "b");
    }

    #[test]
    fn test_load_nonexistent_returns_empty() {
        let store = TokenStore::load(Path::new("/tmp/nonexistent_token_store_12345.json")).unwrap();
        assert!(store.tokens.is_empty());
    }

    #[test]
    fn test_get_mut() {
        let mut store = TokenStore::new();
        store.create(make_token("t1", "a")).unwrap();
        let token = store.get_mut("t1").unwrap();
        token.ceiling = Decimal::new(1000, 0);
        assert_eq!(store.get("t1").unwrap().ceiling, Decimal::new(1000, 0));
    }
}
