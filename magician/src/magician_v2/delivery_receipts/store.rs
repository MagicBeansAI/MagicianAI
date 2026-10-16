//! Typed sent-index boundary. Default construction is still the JSONL adapter.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::sent_index::{SendBinding, SentIndexRecord, SentMessage, SentMessageIndex};
use super::SendIdentifier;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::DeliveryScope;

/// Durable sent-index contract. Local JSONL remains the default adapter.
pub trait SentMessageStore: Send + Sync {
    fn record(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
        sent: &SentMessage,
        now: DateTime<Utc>,
    ) -> Result<()>;

    fn lookup(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
    ) -> Result<SendBinding>;

    fn export_scope(&self, scope: &DeliveryScope) -> Result<Vec<u8>>;
    fn import_scope(&self, scope: &DeliveryScope, bytes: &[u8]) -> Result<()>;
    fn list_scope(&self, scope: &DeliveryScope) -> Result<Vec<SentIndexRecord>>;
}

/// Default/canonical local adapter. Production composition uses this.
pub fn open_local_sent_index(workspace: ArtifactV2Workspace) -> Arc<dyn SentMessageStore> {
    Arc::new(SentMessageIndex::new(workspace))
}

/// Explicit remote-qualification adapter. Not selected by default startup.
pub fn open_sqlite_sent_index(path: impl AsRef<Path>) -> Result<Arc<dyn SentMessageStore>> {
    Ok(Arc::new(super::sqlite::SqliteSentMessageStore::open(path)?))
}

impl SentMessageStore for Arc<dyn SentMessageStore> {
    fn record(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
        sent: &SentMessage,
        now: DateTime<Utc>,
    ) -> Result<()> {
        (**self).record(scope, provider, identifier, sent, now)
    }

    fn lookup(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
    ) -> Result<SendBinding> {
        (**self).lookup(scope, provider, identifier)
    }

    fn export_scope(&self, scope: &DeliveryScope) -> Result<Vec<u8>> {
        (**self).export_scope(scope)
    }

    fn import_scope(&self, scope: &DeliveryScope, bytes: &[u8]) -> Result<()> {
        (**self).import_scope(scope, bytes)
    }

    fn list_scope(&self, scope: &DeliveryScope) -> Result<Vec<SentIndexRecord>> {
        (**self).list_scope(scope)
    }
}

impl SentMessageStore for SentMessageIndex {
    fn record(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
        sent: &SentMessage,
        now: DateTime<Utc>,
    ) -> Result<()> {
        SentMessageIndex::record(self, scope, provider, identifier, sent, now)
    }

    fn lookup(
        &self,
        scope: &DeliveryScope,
        provider: &str,
        identifier: &SendIdentifier,
    ) -> Result<SendBinding> {
        SentMessageIndex::lookup(self, scope, provider, identifier)
    }

    fn export_scope(&self, scope: &DeliveryScope) -> Result<Vec<u8>> {
        SentMessageIndex::export_scope(self, scope)
    }

    fn import_scope(&self, scope: &DeliveryScope, bytes: &[u8]) -> Result<()> {
        SentMessageIndex::import_scope(self, scope, bytes)
    }

    fn list_scope(&self, scope: &DeliveryScope) -> Result<Vec<SentIndexRecord>> {
        SentMessageIndex::list_scope(self, scope)
    }
}
