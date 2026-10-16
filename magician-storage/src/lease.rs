use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct LeaseResource(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct OwnerId(String);

impl LeaseResource {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        crate::identifiers::LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl OwnerId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        crate::identifiers::LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for LeaseResource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for OwnerId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseToken {
    pub resource: LeaseResource,
    pub generation: u64,
    pub expires_at: DateTime<Utc>,
    pub owner: OwnerId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseObservation {
    pub token: LeaseToken,
}

#[async_trait]
pub trait LeaseStore: Send + Sync {
    async fn acquire(
        &self,
        resource: LeaseResource,
        owner: OwnerId,
        ttl: Duration,
    ) -> Result<LeaseToken, StorageError>;
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<LeaseToken, StorageError>;
    async fn release(&self, token: LeaseToken) -> Result<(), StorageError>;
    async fn inspect(
        &self,
        resource: &LeaseResource,
    ) -> Result<Option<LeaseObservation>, StorageError>;
}
