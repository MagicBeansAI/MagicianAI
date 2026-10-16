//! Async Auth Broker vault adapter for the official MCP SDK persistence boundary.

// Phase 5C2 intentionally lands the durable vault before Phase 5C3 wires the SDK
// OAuth coordinator. Keep this dependency-ordered dormant slice warning-free and
// remove the allowance when the coordinator constructs the adapter.
#![cfg_attr(not(test), allow(dead_code))]

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use magician_mcp_client::{
    McpOAuthSecretDocument, McpOAuthVault, McpOAuthVaultError, McpOAuthVaultKey,
    McpOAuthVaultNamespace, McpOAuthVaultWriteMode,
};

use super::{SecretStore, SecretStoreError};

/// Scope-owned encrypted OAuth vault backed by the existing platform-key SecretStore.
///
/// Synchronous encrypted-file operations run on Tokio's blocking pool rather than an
/// Actix/Tokio async worker. The wrapped `SecretStore` is already resolved to one exact
/// principal/workspace scope before this adapter is constructed.
#[derive(Clone)]
pub struct SecretStoreMcpOAuthVault {
    store: Arc<SecretStore>,
}

impl SecretStoreMcpOAuthVault {
    pub fn new(store: Arc<SecretStore>) -> Self {
        Self { store }
    }
}

impl fmt::Debug for SecretStoreMcpOAuthVault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretStoreMcpOAuthVault([REDACTED])")
    }
}

#[async_trait]
impl McpOAuthVault for SecretStoreMcpOAuthVault {
    async fn read(
        &self,
        key: McpOAuthVaultKey,
    ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError> {
        let store = Arc::clone(&self.store);
        let record_id = record_id(key)?;
        let bytes = tokio::task::spawn_blocking(move || store.read_mcp_oauth(&record_id))
            .await
            .map_err(|_| McpOAuthVaultError::Unavailable)?
            .map_err(map_store_error)?;
        bytes
            .map(McpOAuthSecretDocument::new)
            .transpose()
            .map_err(|_| McpOAuthVaultError::Corrupt)
    }

    async fn write(
        &self,
        key: McpOAuthVaultKey,
        document: McpOAuthSecretDocument,
        mode: McpOAuthVaultWriteMode,
    ) -> Result<(), McpOAuthVaultError> {
        let store = Arc::clone(&self.store);
        let record_id = record_id(key)?;
        tokio::task::spawn_blocking(move || {
            store.write_mcp_oauth(
                &record_id,
                document.as_bytes(),
                mode == McpOAuthVaultWriteMode::CreateOnly,
            )
        })
        .await
        .map_err(|_| McpOAuthVaultError::Unavailable)?
        .map_err(map_store_error)
    }

    async fn take(
        &self,
        key: McpOAuthVaultKey,
    ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError> {
        let store = Arc::clone(&self.store);
        let record_id = record_id(key)?;
        let bytes = tokio::task::spawn_blocking(move || store.take_mcp_oauth(&record_id))
            .await
            .map_err(|_| McpOAuthVaultError::Unavailable)?
            .map_err(map_store_error)?;
        bytes
            .map(McpOAuthSecretDocument::new)
            .transpose()
            .map_err(|_| McpOAuthVaultError::Corrupt)
    }

    async fn delete(&self, key: McpOAuthVaultKey) -> Result<(), McpOAuthVaultError> {
        let store = Arc::clone(&self.store);
        let record_id = record_id(key)?;
        tokio::task::spawn_blocking(move || store.delete_mcp_oauth(&record_id))
            .await
            .map_err(|_| McpOAuthVaultError::Unavailable)?
            .map_err(map_store_error)
    }
}

fn record_id(key: McpOAuthVaultKey) -> Result<String, McpOAuthVaultError> {
    Ok(format!(
        "{}{}",
        namespace_prefix(key.namespace())?,
        key.storage_id()
    ))
}

fn namespace_prefix(namespace: McpOAuthVaultNamespace) -> Result<&'static str, McpOAuthVaultError> {
    Ok(match namespace {
        McpOAuthVaultNamespace::Credentials => "credentials:",
        McpOAuthVaultNamespace::AuthorizationState => "authorization-state:",
        McpOAuthVaultNamespace::PendingAuthorization => "pending-authorization:",
        _ => return Err(McpOAuthVaultError::Corrupt),
    })
}

fn map_store_error(error: SecretStoreError) -> McpOAuthVaultError {
    match error {
        SecretStoreError::McpOAuthConflict => McpOAuthVaultError::Conflict,
        SecretStoreError::McpOAuthInvalidRecord => McpOAuthVaultError::Corrupt,
        SecretStoreError::Encryption(_)
        | SecretStoreError::Io(_)
        | SecretStoreError::Serde(_)
        | SecretStoreError::McpOAuthCapacity
        | SecretStoreError::McpOAuthUnavailable
        | SecretStoreError::McpOAuthCommitStateUnknown
        | SecretStoreError::FeatureDisabled { .. } => McpOAuthVaultError::Unavailable,
        _ => McpOAuthVaultError::Unavailable,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::secrets::InMemoryKeyProvider;

    #[test]
    fn adapter_debug_output_does_not_expose_store_details() {
        let store = Arc::new(SecretStore::new_empty(
            Box::new(InMemoryKeyProvider::new()),
            std::path::PathBuf::from("/sensitive/scope/path"),
        ));
        let adapter = SecretStoreMcpOAuthVault::new(store);
        assert_eq!(
            format!("{adapter:?}"),
            "SecretStoreMcpOAuthVault([REDACTED])"
        );
    }

    #[test]
    fn adapter_maps_store_failures_to_stable_value_free_classes() {
        assert_eq!(
            map_store_error(SecretStoreError::McpOAuthConflict),
            McpOAuthVaultError::Conflict
        );
        assert_eq!(
            map_store_error(SecretStoreError::McpOAuthInvalidRecord),
            McpOAuthVaultError::Corrupt
        );
        assert_eq!(
            map_store_error(SecretStoreError::McpOAuthCommitStateUnknown),
            McpOAuthVaultError::Unavailable
        );
        assert_eq!(
            map_store_error(SecretStoreError::McpOAuthUnavailable),
            McpOAuthVaultError::Unavailable
        );
        assert_eq!(
            map_store_error(SecretStoreError::McpOAuthCapacity),
            McpOAuthVaultError::Unavailable
        );
    }

    #[test]
    fn every_oauth_namespace_has_a_distinct_opaque_record_prefix() {
        assert_eq!(
            namespace_prefix(McpOAuthVaultNamespace::Credentials).expect("credentials"),
            "credentials:"
        );
        assert_eq!(
            namespace_prefix(McpOAuthVaultNamespace::AuthorizationState).expect("state"),
            "authorization-state:"
        );
        assert_eq!(
            namespace_prefix(McpOAuthVaultNamespace::PendingAuthorization).expect("pending"),
            "pending-authorization:"
        );
    }
}
