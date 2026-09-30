//! Neutral repository identity types. SQL and pool types stay in owner or
//! state-adapter crates so services never receive a connection.

use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::identifiers::LogicalObjectId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Revision(u64);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SchemaStoreId(String);

impl IdempotencyKey {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Revision {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn get(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl SchemaStoreId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IdempotencyKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for SchemaStoreId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_key_rejects_path() {
        assert!(IdempotencyKey::parse("a/b").is_err());
        assert!(IdempotencyKey::parse("create-1").is_ok());
    }

    #[test]
    fn sibling_ids_deserialize_through_parse() {
        assert!(serde_json::from_str::<IdempotencyKey>("\"a/b\"").is_err());
        assert!(serde_json::from_str::<IdempotencyKey>("\"create-1\"").is_ok());
        assert!(serde_json::from_str::<SchemaStoreId>("\"a/b\"").is_err());
        assert!(serde_json::from_str::<SchemaStoreId>("\"schema-1\"").is_ok());
    }
}
