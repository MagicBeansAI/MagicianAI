//! Exact-binding persistence bridge for the official SDK's OAuth stores.
//!
//! `rmcp` continues to own discovery, PKCE generation, authorization-code exchange,
//! refresh, issuer validation, and scope-upgrade mechanics. This module only adapts the
//! SDK's store traits to a scoped encrypted vault supplied by the product Auth Broker.

use std::{collections::HashSet, fmt, sync::Arc, time::SystemTime};

use async_trait::async_trait;
use oauth2::TokenResponse;
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, CredentialStore, StateStore, StoredAuthorizationState,
    StoredCredentials,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tool_runtime_core::credential_profiles::{CredentialProfileBinding, CredentialProfileKey};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop};

const OAUTH_ENVELOPE_SCHEMA_VERSION: u32 = 1;
const MAX_SECRET_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CREDENTIAL_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_AUTHORIZATION_STATE_DOCUMENT_BYTES: usize = 128 * 1024;
const MAX_PENDING_AUTHORIZATION_DOCUMENT_BYTES: usize = 128 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 16 * 1024;
const MAX_REDIRECT_URI_BYTES: usize = 16 * 1024;
const MAX_FLOW_ID_BYTES: usize = 64;
const MAX_CSRF_TOKEN_BYTES: usize = 4 * 1024;
const MAX_PKCE_VERIFIER_BYTES: usize = 4 * 1024;
const MAX_SCOPE_COUNT: usize = 256;
const MAX_SCOPE_BYTES: usize = 1_024;
const MAX_TOTAL_SCOPE_BYTES: usize = 64 * 1024;
const AUTHORIZATION_STATE_TTL_SECS: u64 = 10 * 60;
const MAX_CLOCK_SKEW_SECS: u64 = 60;

/// Stable class of encrypted OAuth records owned by the product vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum McpOAuthVaultNamespace {
    Credentials,
    AuthorizationState,
    PendingAuthorization,
}

/// Opaque, domain-separated storage key.
///
/// The digest binds the principal/workspace/provider/profile/resource/issuer tuple.
/// Each namespace has one exact-binding slot; the encrypted authorization-state
/// document still binds and validates the SDK-generated CSRF token.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpOAuthVaultKey {
    namespace: McpOAuthVaultNamespace,
    digest: [u8; 32],
}

impl McpOAuthVaultKey {
    pub fn namespace(&self) -> McpOAuthVaultNamespace {
        self.namespace
    }

    /// Stable lowercase identifier suitable for an encrypted vault record name.
    pub fn storage_id(&self) -> String {
        let mut encoded = String::with_capacity(self.digest.len() * 2);
        for byte in self.digest {
            use fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }
}

impl fmt::Debug for McpOAuthVaultKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthVaultKey")
            .field("namespace", &self.namespace)
            .field("digest", &"[REDACTED]")
            .finish()
    }
}

/// Secret serialized SDK record passed only between this adapter and the encrypted vault.
///
/// The value cannot be cloned, serialized, or printed and is zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct McpOAuthSecretDocument(Vec<u8>);

impl McpOAuthSecretDocument {
    pub fn new(mut bytes: Vec<u8>) -> Result<Self, McpOAuthPersistenceError> {
        if bytes.is_empty() || bytes.len() > MAX_SECRET_DOCUMENT_BYTES {
            bytes.zeroize();
            return Err(McpOAuthPersistenceError::InvalidRecord);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for McpOAuthSecretDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpOAuthSecretDocument([REDACTED])")
    }
}

/// Atomic write behavior required from the encrypted vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpOAuthVaultWriteMode {
    /// Replace the one credential record for an exact binding.
    Replace,
    /// Create a callback state exactly once; collisions must return `Conflict`.
    CreateOnly,
}

/// Stable, value-free vault failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum McpOAuthVaultError {
    #[error("OAuth vault is unavailable")]
    Unavailable,
    #[error("OAuth vault record already exists")]
    Conflict,
    #[error("OAuth vault record is corrupt")]
    Corrupt,
}

/// Scoped encrypted storage supplied by the product Auth Broker.
///
/// Implementations must make `write(CreateOnly)` and `take` atomic. `take` is the
/// callback replay boundary: at most one concurrent callback may receive a state record.
#[async_trait]
pub trait McpOAuthVault: Send + Sync {
    async fn read(
        &self,
        key: McpOAuthVaultKey,
    ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError>;

    async fn write(
        &self,
        key: McpOAuthVaultKey,
        document: McpOAuthSecretDocument,
        mode: McpOAuthVaultWriteMode,
    ) -> Result<(), McpOAuthVaultError>;

    async fn take(
        &self,
        key: McpOAuthVaultKey,
    ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError>;

    async fn delete(&self, key: McpOAuthVaultKey) -> Result<(), McpOAuthVaultError>;
}

/// Stable, secret-free configuration and persistence failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum McpOAuthPersistenceError {
    #[error("MCP OAuth requires an exact resource and issuer profile binding")]
    InvalidBinding,
    #[error("MCP OAuth persistence record is invalid")]
    InvalidRecord,
    #[error("MCP OAuth persistence is unavailable")]
    Unavailable,
    #[error("MCP OAuth authorization state already exists")]
    Conflict,
    #[error("MCP OAuth persistence record is corrupt")]
    Corrupt,
}

impl From<McpOAuthVaultError> for McpOAuthPersistenceError {
    fn from(value: McpOAuthVaultError) -> Self {
        match value {
            McpOAuthVaultError::Unavailable => Self::Unavailable,
            McpOAuthVaultError::Conflict => Self::Conflict,
            McpOAuthVaultError::Corrupt => Self::Corrupt,
        }
    }
}

#[derive(Clone)]
struct ExactBinding {
    fingerprint: [u8; 32],
    authorization_issuer: String,
}

impl ExactBinding {
    fn new(profile: &CredentialProfileKey) -> Result<Self, McpOAuthPersistenceError> {
        let CredentialProfileBinding::McpOauth {
            resource_url,
            authorization_issuer,
        } = &profile.binding
        else {
            return Err(McpOAuthPersistenceError::InvalidBinding);
        };

        let mut hasher = Sha256::new();
        hash_component(&mut hasher, b"magician-mcp-oauth-binding-v1");
        hash_component(&mut hasher, profile.scope.principal.as_str().as_bytes());
        hash_component(&mut hasher, profile.scope.workspace.as_str().as_bytes());
        hash_component(&mut hasher, profile.provider.as_str().as_bytes());
        hash_component(&mut hasher, profile.alias.as_str().as_bytes());
        hash_component(&mut hasher, resource_url.as_str().as_bytes());
        hash_component(&mut hasher, authorization_issuer.as_str().as_bytes());
        Ok(Self {
            fingerprint: hasher.finalize().into(),
            authorization_issuer: authorization_issuer.as_str().to_owned(),
        })
    }

    fn credential_key(&self) -> McpOAuthVaultKey {
        self.derive_key(McpOAuthVaultNamespace::Credentials, None)
    }

    fn state_key(&self) -> McpOAuthVaultKey {
        self.derive_key(McpOAuthVaultNamespace::AuthorizationState, None)
    }

    fn pending_key(&self, flow_id: &str) -> McpOAuthVaultKey {
        self.derive_key(
            McpOAuthVaultNamespace::PendingAuthorization,
            Some(flow_id.as_bytes()),
        )
    }

    fn derive_key(
        &self,
        namespace: McpOAuthVaultNamespace,
        discriminator: Option<&[u8]>,
    ) -> McpOAuthVaultKey {
        let mut hasher = Sha256::new();
        hash_component(&mut hasher, b"magician-mcp-oauth-vault-key-v1");
        hash_component(&mut hasher, &self.fingerprint);
        hash_component(
            &mut hasher,
            match namespace {
                McpOAuthVaultNamespace::Credentials => b"credentials",
                McpOAuthVaultNamespace::AuthorizationState => b"authorization-state",
                McpOAuthVaultNamespace::PendingAuthorization => b"pending-authorization",
            },
        );
        if let Some(discriminator) = discriminator {
            hash_component(&mut hasher, discriminator);
        }
        McpOAuthVaultKey {
            namespace,
            digest: hasher.finalize().into(),
        }
    }
}

impl fmt::Debug for ExactBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExactBinding([REDACTED])")
    }
}

fn hash_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialEnvelope {
    schema_version: u32,
    binding_fingerprint: [u8; 32],
    credentials: StoredCredentials,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizationStateEnvelope {
    schema_version: u32,
    binding_fingerprint: [u8; 32],
    state: StoredAuthorizationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingClientRegistration {
    DynamicPublic,
    ClientMetadata,
    PreRegistered,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingAuthorizationRecord {
    pub flow_id: String,
    pub attempt_id: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub registration: PendingClientRegistration,
    pub credential_revision_before: Option<[u8; 32]>,
    pub created_at: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingAuthorizationEnvelope {
    schema_version: u32,
    binding_fingerprint: [u8; 32],
    pending: PendingAuthorizationRecord,
}

/// Installs exact-binding durable stores on an SDK authorization manager.
///
/// This object has no OAuth protocol implementation and no browser/callback listener.
/// The SDK remains the sole owner of those mechanics.
#[derive(Clone)]
pub struct McpOAuthPersistence {
    binding: Arc<ExactBinding>,
    vault: Arc<dyn McpOAuthVault>,
}

/// Secret-free view of the credential record for one exact binding.
///
/// Token values and client identity never leave the persistence boundary. The
/// coordinator uses only this metadata to project status and decide whether an
/// explicit refresh or scope upgrade is possible.
pub(crate) struct McpOAuthCredentialObservation {
    pub granted_scopes: Vec<String>,
    pub token_present: bool,
    pub refresh_available: bool,
    pub expires_at: Option<u64>,
    pub(crate) revision: [u8; 32],
}

impl McpOAuthPersistence {
    pub fn new(
        profile: &CredentialProfileKey,
        vault: Arc<dyn McpOAuthVault>,
    ) -> Result<Self, McpOAuthPersistenceError> {
        Ok(Self {
            binding: Arc::new(ExactBinding::new(profile)?),
            vault,
        })
    }

    pub(crate) fn install(&self, manager: &mut AuthorizationManager) {
        manager.set_credential_store(BrokerCredentialStore {
            binding: Arc::clone(&self.binding),
            vault: Arc::clone(&self.vault),
        });
        manager.set_state_store(BrokerAuthorizationStateStore {
            binding: Arc::clone(&self.binding),
            vault: Arc::clone(&self.vault),
        });
    }

    pub(crate) fn callback_binding_id(&self) -> String {
        let mut encoded = String::with_capacity(self.binding.fingerprint.len() * 2);
        for byte in self.binding.fingerprint {
            use fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }

    /// Stable opaque callback slot for this exact binding.
    ///
    /// OAuth clients require a pre-registered redirect URI, so an attempt-random path
    /// cannot be used for dynamic-client reuse or later scope upgrades. Per-attempt
    /// correlation and replay protection remain the SDK-generated CSRF state.
    pub(crate) fn callback_flow_id(&self) -> String {
        let mut hasher = Sha256::new();
        hash_component(&mut hasher, b"magician-mcp-oauth-callback-slot-v1");
        hash_component(&mut hasher, &self.binding.fingerprint);
        let digest: [u8; 32] = hasher.finalize().into();
        let mut encoded = String::with_capacity(32);
        for byte in digest.into_iter().take(16) {
            use fmt::Write as _;
            let _ = write!(encoded, "{byte:02x}");
        }
        encoded
    }

    pub(crate) async fn prepare_new_authorization(&self) -> Result<(), McpOAuthPersistenceError> {
        let now = now_epoch_secs();
        if self.active_pending_at(now).await?.is_some() {
            return Err(McpOAuthPersistenceError::Conflict);
        }
        // A cancelled start can leave SDK PKCE state before the durable pending route
        // is written. With no active pending flow, all exact-binding state is orphaned
        // and can be reclaimed before the next start instead of consuming vault capacity
        // for the full TTL.
        self.vault.delete(self.binding.state_key()).await?;
        Ok(())
    }

    pub(crate) async fn credential_observation(
        &self,
    ) -> Result<Option<McpOAuthCredentialObservation>, McpOAuthPersistenceError> {
        let Some(document) = self.vault.read(self.binding.credential_key()).await? else {
            return Ok(None);
        };
        let revision: [u8; 32] = Sha256::digest(document.as_bytes()).into();
        let credentials = decode_credentials(&self.binding, document)?;
        let (token_present, refresh_available, expires_at) = credentials
            .token_response
            .as_ref()
            .map(|token| {
                (
                    true,
                    token.refresh_token().is_some(),
                    credentials
                        .token_received_at
                        .zip(token.expires_in())
                        .map(|(received_at, ttl)| received_at.saturating_add(ttl.as_secs())),
                )
            })
            .unwrap_or((false, false, None));
        Ok(Some(McpOAuthCredentialObservation {
            granted_scopes: credentials.granted_scopes,
            token_present,
            refresh_available,
            expires_at,
            revision,
        }))
    }

    pub(crate) async fn clear_credentials(&self) -> Result<(), McpOAuthPersistenceError> {
        self.vault
            .delete(self.binding.credential_key())
            .await
            .map_err(Into::into)
    }

    /// Preserve the previously observed grant when a refresh response omits `scope`.
    ///
    /// `rmcp` 3.1.0 correctly sends the stored grant on refresh, but a manager restored
    /// from its store does not repopulate its in-memory scope vector. Consequently an
    /// RFC 6749 response that omits `scope` can otherwise persist an empty grant. This
    /// adapter repairs only that metadata field; the SDK still owns the token exchange.
    pub(crate) async fn restore_granted_scopes_if_empty(
        &self,
        previous_scopes: &[String],
    ) -> Result<(), McpOAuthPersistenceError> {
        if previous_scopes.is_empty() {
            return Ok(());
        }
        validate_scopes(previous_scopes)?;
        let Some(document) = self.vault.read(self.binding.credential_key()).await? else {
            return Err(McpOAuthPersistenceError::InvalidRecord);
        };
        let mut credentials = decode_credentials(&self.binding, document)?;
        if !credentials.granted_scopes.is_empty() {
            return Ok(());
        }
        credentials.granted_scopes = previous_scopes.to_vec();
        let document = encode_credentials(&self.binding, credentials)?;
        self.vault
            .write(
                self.binding.credential_key(),
                document,
                McpOAuthVaultWriteMode::Replace,
            )
            .await
            .map_err(Into::into)
    }

    /// Remove every local OAuth record belonging to this exact binding.
    ///
    /// All records use stable exact-binding keys, so logout performs a fixed number of
    /// vault operations and cannot be poisoned by another binding's malformed record.
    pub(crate) async fn clear_exact_binding(&self) -> Result<(), McpOAuthPersistenceError> {
        self.clear_credentials().await?;
        self.vault.delete(self.binding.state_key()).await?;
        self.vault
            .delete(self.binding.pending_key(&self.callback_flow_id()))
            .await?;
        Ok(())
    }

    pub(crate) async fn active_pending(
        &self,
    ) -> Result<Option<PendingAuthorizationRecord>, McpOAuthPersistenceError> {
        self.active_pending_at(now_epoch_secs()).await
    }

    pub(crate) async fn purge_expired_authorizations(
        &self,
    ) -> Result<(), McpOAuthPersistenceError> {
        let now = now_epoch_secs();
        self.purge_expired_authorization_state(now).await?;
        self.active_pending_at(now).await?;
        Ok(())
    }

    async fn purge_expired_authorization_state(
        &self,
        now: u64,
    ) -> Result<(), McpOAuthPersistenceError> {
        let key = self.binding.state_key();
        let Some(document) = self.vault.read(key).await? else {
            return Ok(());
        };
        if document.as_bytes().len() > MAX_AUTHORIZATION_STATE_DOCUMENT_BYTES {
            return Err(McpOAuthPersistenceError::Corrupt);
        }
        let envelope: AuthorizationStateEnvelope = serde_json::from_slice(document.as_bytes())
            .map_err(|_| McpOAuthPersistenceError::Corrupt)?;
        if envelope.schema_version != OAUTH_ENVELOPE_SCHEMA_VERSION
            || envelope.binding_fingerprint != self.binding.fingerprint
        {
            return Err(McpOAuthPersistenceError::Corrupt);
        }
        if authorization_record_expired(envelope.state.created_at, now) {
            self.vault.delete(key).await?;
        }
        Ok(())
    }

    async fn active_pending_at(
        &self,
        now: u64,
    ) -> Result<Option<PendingAuthorizationRecord>, McpOAuthPersistenceError> {
        let flow_id = self.callback_flow_id();
        let key = self.binding.pending_key(&flow_id);
        let Some(document) = self.vault.read(key).await? else {
            return Ok(None);
        };
        if document.as_bytes().len() > MAX_PENDING_AUTHORIZATION_DOCUMENT_BYTES {
            return Err(McpOAuthPersistenceError::Corrupt);
        }
        let envelope: PendingAuthorizationEnvelope = serde_json::from_slice(document.as_bytes())
            .map_err(|_| McpOAuthPersistenceError::Corrupt)?;
        if envelope.schema_version != OAUTH_ENVELOPE_SCHEMA_VERSION
            || envelope.binding_fingerprint != self.binding.fingerprint
            || envelope.pending.flow_id != flow_id
        {
            return Err(McpOAuthPersistenceError::Corrupt);
        }
        if authorization_record_expired(envelope.pending.created_at, now) {
            self.vault.delete(key).await?;
            self.vault.delete(self.binding.state_key()).await?;
            return Ok(None);
        }
        validate_pending_authorization(&envelope.pending, now)?;
        Ok(Some(envelope.pending))
    }

    pub(crate) async fn save_pending(
        &self,
        pending: PendingAuthorizationRecord,
    ) -> Result<(), McpOAuthPersistenceError> {
        validate_pending_authorization(&pending, now_epoch_secs())?;
        let flow_id = pending.flow_id.clone();
        let envelope = PendingAuthorizationEnvelope {
            schema_version: OAUTH_ENVELOPE_SCHEMA_VERSION,
            binding_fingerprint: self.binding.fingerprint,
            pending,
        };
        let bytes =
            serde_json::to_vec(&envelope).map_err(|_| McpOAuthPersistenceError::InvalidRecord)?;
        if bytes.len() > MAX_PENDING_AUTHORIZATION_DOCUMENT_BYTES {
            return Err(McpOAuthPersistenceError::InvalidRecord);
        }
        let document = McpOAuthSecretDocument::new(bytes)?;
        self.vault
            .write(
                self.binding.pending_key(&flow_id),
                document,
                McpOAuthVaultWriteMode::CreateOnly,
            )
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn load_pending(
        &self,
        flow_id: &str,
    ) -> Result<Option<PendingAuthorizationRecord>, McpOAuthPersistenceError> {
        validate_flow_id(flow_id)?;
        let Some(document) = self.vault.read(self.binding.pending_key(flow_id)).await? else {
            return Ok(None);
        };
        if document.as_bytes().len() > MAX_PENDING_AUTHORIZATION_DOCUMENT_BYTES {
            return Err(McpOAuthPersistenceError::InvalidRecord);
        }
        let envelope: PendingAuthorizationEnvelope = serde_json::from_slice(document.as_bytes())
            .map_err(|_| McpOAuthPersistenceError::Corrupt)?;
        if envelope.schema_version != OAUTH_ENVELOPE_SCHEMA_VERSION
            || envelope.binding_fingerprint != self.binding.fingerprint
            || envelope.pending.flow_id != flow_id
        {
            return Err(McpOAuthPersistenceError::InvalidRecord);
        }
        validate_pending_authorization(&envelope.pending, now_epoch_secs())?;
        Ok(Some(envelope.pending))
    }

    pub(crate) async fn delete_pending(
        &self,
        flow_id: &str,
    ) -> Result<(), McpOAuthPersistenceError> {
        validate_flow_id(flow_id)?;
        self.vault
            .delete(self.binding.pending_key(flow_id))
            .await
            .map_err(Into::into)
    }

    pub(crate) async fn authorization_state_exists(
        &self,
        csrf_token: &str,
    ) -> Result<bool, McpOAuthPersistenceError> {
        validate_csrf_token(csrf_token)?;
        let Some(document) = self.vault.read(self.binding.state_key()).await? else {
            return Ok(false);
        };
        decode_authorization_state(&self.binding, csrf_token, document, now_epoch_secs())?;
        Ok(true)
    }

    pub(crate) async fn delete_authorization_state(
        &self,
        csrf_token: &str,
    ) -> Result<(), McpOAuthPersistenceError> {
        validate_csrf_token(csrf_token)?;
        self.vault
            .delete(self.binding.state_key())
            .await
            .map_err(Into::into)
    }
}

impl fmt::Debug for McpOAuthPersistence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpOAuthPersistence([REDACTED])")
    }
}

#[derive(Clone)]
struct BrokerCredentialStore {
    binding: Arc<ExactBinding>,
    vault: Arc<dyn McpOAuthVault>,
}

#[async_trait]
impl CredentialStore for BrokerCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let key = self.binding.credential_key();
        let Some(document) = self.vault.read(key).await.map_err(map_vault_error)? else {
            return Ok(None);
        };
        decode_credentials(&self.binding, document)
            .map(Some)
            .map_err(map_persistence_error)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        validate_credentials(&self.binding, &credentials).map_err(map_persistence_error)?;
        let document =
            encode_credentials(&self.binding, credentials).map_err(map_persistence_error)?;
        self.vault
            .write(
                self.binding.credential_key(),
                document,
                McpOAuthVaultWriteMode::Replace,
            )
            .await
            .map_err(map_vault_error)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.vault
            .delete(self.binding.credential_key())
            .await
            .map_err(map_vault_error)
    }
}

#[derive(Clone)]
struct BrokerAuthorizationStateStore {
    binding: Arc<ExactBinding>,
    vault: Arc<dyn McpOAuthVault>,
}

#[async_trait]
impl StateStore for BrokerAuthorizationStateStore {
    async fn save(
        &self,
        csrf_token: &str,
        state: StoredAuthorizationState,
    ) -> Result<(), AuthError> {
        validate_authorization_state(&self.binding, csrf_token, &state, now_epoch_secs())
            .map_err(map_persistence_error)?;
        let document =
            encode_authorization_state(&self.binding, state).map_err(map_persistence_error)?;
        self.vault
            .write(
                self.binding.state_key(),
                document,
                McpOAuthVaultWriteMode::CreateOnly,
            )
            .await
            .map_err(map_vault_error)
    }

    async fn load(&self, csrf_token: &str) -> Result<Option<StoredAuthorizationState>, AuthError> {
        validate_csrf_token(csrf_token).map_err(map_persistence_error)?;
        let key = self.binding.state_key();
        let Some(document) = self.vault.take(key).await.map_err(map_vault_error)? else {
            return Ok(None);
        };
        decode_authorization_state(&self.binding, csrf_token, document, now_epoch_secs())
            .map(Some)
            .map_err(map_persistence_error)
    }

    async fn delete(&self, csrf_token: &str) -> Result<(), AuthError> {
        validate_csrf_token(csrf_token).map_err(map_persistence_error)?;
        self.vault
            .delete(self.binding.state_key())
            .await
            .map_err(map_vault_error)
    }
}

fn encode_credentials(
    binding: &ExactBinding,
    credentials: StoredCredentials,
) -> Result<McpOAuthSecretDocument, McpOAuthPersistenceError> {
    let envelope = CredentialEnvelope {
        schema_version: OAUTH_ENVELOPE_SCHEMA_VERSION,
        binding_fingerprint: binding.fingerprint,
        credentials,
    };
    let bytes =
        serde_json::to_vec(&envelope).map_err(|_| McpOAuthPersistenceError::InvalidRecord)?;
    if bytes.len() > MAX_CREDENTIAL_DOCUMENT_BYTES {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    McpOAuthSecretDocument::new(bytes)
}

fn decode_credentials(
    binding: &ExactBinding,
    document: McpOAuthSecretDocument,
) -> Result<StoredCredentials, McpOAuthPersistenceError> {
    if document.as_bytes().len() > MAX_CREDENTIAL_DOCUMENT_BYTES {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    let envelope: CredentialEnvelope = serde_json::from_slice(document.as_bytes())
        .map_err(|_| McpOAuthPersistenceError::Corrupt)?;
    if envelope.schema_version != OAUTH_ENVELOPE_SCHEMA_VERSION
        || envelope.binding_fingerprint != binding.fingerprint
    {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    validate_credentials(binding, &envelope.credentials)?;
    Ok(envelope.credentials)
}

fn encode_authorization_state(
    binding: &ExactBinding,
    state: StoredAuthorizationState,
) -> Result<McpOAuthSecretDocument, McpOAuthPersistenceError> {
    let envelope = AuthorizationStateEnvelope {
        schema_version: OAUTH_ENVELOPE_SCHEMA_VERSION,
        binding_fingerprint: binding.fingerprint,
        state,
    };
    let bytes =
        serde_json::to_vec(&envelope).map_err(|_| McpOAuthPersistenceError::InvalidRecord)?;
    if bytes.len() > MAX_AUTHORIZATION_STATE_DOCUMENT_BYTES {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    McpOAuthSecretDocument::new(bytes)
}

fn decode_authorization_state(
    binding: &ExactBinding,
    csrf_token: &str,
    document: McpOAuthSecretDocument,
    now: u64,
) -> Result<StoredAuthorizationState, McpOAuthPersistenceError> {
    if document.as_bytes().len() > MAX_AUTHORIZATION_STATE_DOCUMENT_BYTES {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    let envelope: AuthorizationStateEnvelope = serde_json::from_slice(document.as_bytes())
        .map_err(|_| McpOAuthPersistenceError::Corrupt)?;
    if envelope.schema_version != OAUTH_ENVELOPE_SCHEMA_VERSION
        || envelope.binding_fingerprint != binding.fingerprint
    {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    validate_authorization_state(binding, csrf_token, &envelope.state, now)?;
    Ok(envelope.state)
}

fn validate_credentials(
    binding: &ExactBinding,
    credentials: &StoredCredentials,
) -> Result<(), McpOAuthPersistenceError> {
    validate_bounded_text(&credentials.client_id, MAX_CLIENT_ID_BYTES)?;
    if !credentials
        .issuer
        .as_deref()
        .is_some_and(|issuer| issuer_matches(issuer, &binding.authorization_issuer))
    {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    validate_scopes(&credentials.granted_scopes)?;
    // `rmcp` deliberately supports stored tokens that predate `token_received_at` and
    // skips proactive expiry for them. A timestamp without a token is still invalid.
    if credentials.token_response.is_none() && credentials.token_received_at.is_some() {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    Ok(())
}

fn validate_authorization_state(
    binding: &ExactBinding,
    csrf_token: &str,
    state: &StoredAuthorizationState,
    now: u64,
) -> Result<(), McpOAuthPersistenceError> {
    validate_csrf_token(csrf_token)?;
    if state.csrf_token != csrf_token {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    validate_bounded_text(&state.pkce_verifier, MAX_PKCE_VERIFIER_BYTES)?;
    if !state
        .expected_issuer
        .as_deref()
        .is_some_and(|issuer| issuer_matches(issuer, &binding.authorization_issuer))
    {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    validate_scopes(&state.requested_scopes)?;
    if authorization_record_expired(state.created_at, now) {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    Ok(())
}

fn validate_pending_authorization(
    pending: &PendingAuthorizationRecord,
    now: u64,
) -> Result<(), McpOAuthPersistenceError> {
    validate_flow_id(&pending.flow_id)?;
    validate_flow_id(&pending.attempt_id)?;
    validate_bounded_text(&pending.client_id, MAX_CLIENT_ID_BYTES)?;
    validate_bounded_text(&pending.redirect_uri, MAX_REDIRECT_URI_BYTES)?;
    validate_scopes(&pending.scopes)?;
    if authorization_record_expired(pending.created_at, now) {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    Ok(())
}

fn authorization_record_expired(created_at: u64, now: u64) -> bool {
    created_at == 0
        || created_at > now.saturating_add(MAX_CLOCK_SKEW_SECS)
        || now.saturating_sub(created_at) > AUTHORIZATION_STATE_TTL_SECS
}

pub(crate) fn validate_flow_id(value: &str) -> Result<(), McpOAuthPersistenceError> {
    if value.len() != 32
        || value.len() > MAX_FLOW_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    Ok(())
}

fn validate_csrf_token(value: &str) -> Result<(), McpOAuthPersistenceError> {
    validate_bounded_text(value, MAX_CSRF_TOKEN_BYTES)
}

fn issuer_matches(candidate: &str, expected: &str) -> bool {
    Url::parse(candidate)
        .ok()
        .zip(Url::parse(expected).ok())
        .is_some_and(|(candidate, expected)| candidate == expected)
}

fn validate_bounded_text(value: &str, max_bytes: usize) -> Result<(), McpOAuthPersistenceError> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    Ok(())
}

fn validate_scopes(scopes: &[String]) -> Result<(), McpOAuthPersistenceError> {
    if scopes.len() > MAX_SCOPE_COUNT {
        return Err(McpOAuthPersistenceError::InvalidRecord);
    }
    let mut total = 0usize;
    let mut seen = HashSet::with_capacity(scopes.len());
    for scope in scopes {
        total = total
            .checked_add(scope.len())
            .ok_or(McpOAuthPersistenceError::InvalidRecord)?;
        if scope.is_empty()
            || scope.len() > MAX_SCOPE_BYTES
            || total > MAX_TOTAL_SCOPE_BYTES
            || !scope.bytes().all(|byte| {
                byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
            })
            || !seen.insert(scope.as_str())
        {
            return Err(McpOAuthPersistenceError::InvalidRecord);
        }
    }
    Ok(())
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn map_vault_error(error: McpOAuthVaultError) -> AuthError {
    map_persistence_error(error.into())
}

fn map_persistence_error(error: McpOAuthPersistenceError) -> AuthError {
    AuthError::InternalError(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex, thread};

    use oauth2::{basic::BasicTokenType, AccessToken, CsrfToken, PkceCodeVerifier};
    use rmcp::transport::auth::{OAuthTokenResponse, VendorExtraTokenFields};
    use static_assertions::assert_not_impl_any;
    use tool_runtime_core::credential_profiles::{
        CanonicalCredentialUrl, CredentialProfileBinding, CredentialProfileKey, CredentialScope,
    };

    use super::*;

    #[derive(Default)]
    struct MemoryVault {
        records: Mutex<HashMap<McpOAuthVaultKey, Vec<u8>>>,
    }

    #[async_trait]
    impl McpOAuthVault for MemoryVault {
        async fn read(
            &self,
            key: McpOAuthVaultKey,
        ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError> {
            let bytes = self
                .records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?
                .get(&key)
                .cloned();
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
            let mut records = self
                .records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?;
            if mode == McpOAuthVaultWriteMode::CreateOnly && records.contains_key(&key) {
                return Err(McpOAuthVaultError::Conflict);
            }
            records.insert(key, document.as_bytes().to_vec());
            Ok(())
        }

        async fn take(
            &self,
            key: McpOAuthVaultKey,
        ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError> {
            let bytes = self
                .records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?
                .remove(&key);
            bytes
                .map(McpOAuthSecretDocument::new)
                .transpose()
                .map_err(|_| McpOAuthVaultError::Corrupt)
        }

        async fn delete(&self, key: McpOAuthVaultKey) -> Result<(), McpOAuthVaultError> {
            self.records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?
                .remove(&key);
            Ok(())
        }
    }

    fn profile(alias: &str, resource: &str, issuer: &str) -> CredentialProfileKey {
        CredentialProfileKey::new(
            CredentialScope::new("owner", "default").expect("scope"),
            "provider-mcp",
            alias,
            CredentialProfileBinding::McpOauth {
                resource_url: CanonicalCredentialUrl::new(resource).expect("resource"),
                authorization_issuer: CanonicalCredentialUrl::new(issuer).expect("issuer"),
            },
        )
        .expect("profile")
    }

    fn state(csrf: &str, issuer: &str) -> StoredAuthorizationState {
        StoredAuthorizationState::new_with_expected_issuer(
            &PkceCodeVerifier::new("verifier-value".to_owned()),
            &CsrfToken::new(csrf.to_owned()),
            Some(issuer.to_owned()),
            true,
        )
        .with_requested_scopes(vec!["messages.read".to_owned()])
    }

    fn stores(
        profile: &CredentialProfileKey,
        vault: Arc<MemoryVault>,
    ) -> (BrokerCredentialStore, BrokerAuthorizationStateStore) {
        let persistence = McpOAuthPersistence::new(profile, vault).expect("persistence");
        (
            BrokerCredentialStore {
                binding: Arc::clone(&persistence.binding),
                vault: Arc::clone(&persistence.vault),
            },
            BrokerAuthorizationStateStore {
                binding: persistence.binding,
                vault: persistence.vault,
            },
        )
    }

    #[test]
    fn public_secret_surfaces_are_not_cloneable_or_serializable() {
        assert_not_impl_any!(McpOAuthSecretDocument: Clone, Serialize);
        assert_not_impl_any!(McpOAuthPersistence: Serialize);
        let key = ExactBinding::new(&profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        ))
        .expect("binding")
        .credential_key();
        assert!(!format!("{key:?}").contains(&key.storage_id()));
        assert_eq!(key.storage_id().len(), 64);
        assert!(key
            .storage_id()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn binding_keys_separate_profile_resource_issuer_and_namespace() {
        let base_profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let other_profile_key = profile(
            "work",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let base = ExactBinding::new(&base_profile).unwrap();
        let other_profile = ExactBinding::new(&other_profile_key).unwrap();
        let other_resource = ExactBinding::new(&profile(
            "personal",
            "https://other.example/mcp",
            "https://issuer.example/tenant",
        ))
        .unwrap();
        let other_issuer = ExactBinding::new(&profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/other",
        ))
        .unwrap();

        assert_ne!(base.credential_key(), other_profile.credential_key());
        assert_ne!(base.credential_key(), other_resource.credential_key());
        assert_ne!(base.credential_key(), other_issuer.credential_key());
        assert_ne!(base.credential_key(), base.state_key());
        assert_ne!(base.state_key(), other_profile.state_key());
        assert_ne!(
            base.credential_key(),
            base.pending_key("0123456789abcdef0123456789abcdef")
        );
        let vault = Arc::new(MemoryVault::default());
        let base_persistence =
            McpOAuthPersistence::new(&base_profile, vault.clone()).expect("base persistence");
        let same_persistence =
            McpOAuthPersistence::new(&base_profile, vault.clone()).expect("same persistence");
        let other_persistence =
            McpOAuthPersistence::new(&other_profile_key, vault).expect("other persistence");
        assert_eq!(
            base_persistence.callback_flow_id(),
            same_persistence.callback_flow_id()
        );
        assert_ne!(
            base_persistence.callback_flow_id(),
            other_persistence.callback_flow_id()
        );
        validate_flow_id(&base_persistence.callback_flow_id()).expect("callback slot shape");
        assert_ne!(
            base.state_key(),
            base.pending_key("0123456789abcdef0123456789abcdef")
        );
    }

    #[tokio::test]
    async fn pending_callback_metadata_is_exact_bound_create_only_and_deletable() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let persistence = McpOAuthPersistence::new(&profile, vault).expect("persistence");
        let flow_id = persistence.callback_flow_id();
        let pending = || PendingAuthorizationRecord {
            flow_id: flow_id.clone(),
            attempt_id: "11111111111111111111111111111111".to_owned(),
            client_id: "client-id".to_owned(),
            redirect_uri: format!("http://127.0.0.1/callback/{flow_id}"),
            scopes: vec!["messages.read".to_owned()],
            registration: PendingClientRegistration::PreRegistered,
            credential_revision_before: None,
            created_at: now_epoch_secs(),
        };

        persistence.save_pending(pending()).await.expect("save");
        assert_eq!(
            persistence
                .save_pending(pending())
                .await
                .expect_err("create-only collision"),
            McpOAuthPersistenceError::Conflict
        );
        let loaded = persistence
            .load_pending(&flow_id)
            .await
            .expect("load")
            .expect("pending");
        assert_eq!(loaded.client_id, "client-id");
        assert_eq!(loaded.flow_id, flow_id);

        persistence.delete_pending(&flow_id).await.expect("delete");
        assert!(persistence
            .load_pending(&flow_id)
            .await
            .expect("load after delete")
            .is_none());
    }

    #[tokio::test]
    async fn only_one_unexpired_authorization_is_allowed_per_exact_binding() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let persistence = McpOAuthPersistence::new(&profile, vault).expect("persistence");
        let flow_id = persistence.callback_flow_id();
        persistence
            .save_pending(PendingAuthorizationRecord {
                flow_id,
                attempt_id: "11111111111111111111111111111111".to_owned(),
                client_id: "client-id".to_owned(),
                redirect_uri: "http://127.0.0.1/callback".to_owned(),
                scopes: Vec::new(),
                registration: PendingClientRegistration::DynamicPublic,
                credential_revision_before: None,
                created_at: now_epoch_secs(),
            })
            .await
            .expect("save");
        assert_eq!(
            persistence
                .prepare_new_authorization()
                .await
                .expect_err("active flow"),
            McpOAuthPersistenceError::Conflict
        );
    }

    #[tokio::test]
    async fn stale_pending_records_are_iteratively_reclaimed_before_start() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let persistence = McpOAuthPersistence::new(&profile, vault.clone()).expect("persistence");
        let flow_id = persistence.callback_flow_id();
        let envelope = PendingAuthorizationEnvelope {
            schema_version: OAUTH_ENVELOPE_SCHEMA_VERSION,
            binding_fingerprint: persistence.binding.fingerprint,
            pending: PendingAuthorizationRecord {
                flow_id: flow_id.clone(),
                attempt_id: "11111111111111111111111111111111".to_owned(),
                client_id: "client-id".to_owned(),
                redirect_uri: "http://127.0.0.1/callback".to_owned(),
                scopes: Vec::new(),
                registration: PendingClientRegistration::DynamicPublic,
                credential_revision_before: None,
                created_at: now_epoch_secs().saturating_sub(AUTHORIZATION_STATE_TTL_SECS + 1),
            },
        };
        vault
            .write(
                persistence.binding.pending_key(&flow_id),
                McpOAuthSecretDocument::new(serde_json::to_vec(&envelope).expect("encode"))
                    .expect("document"),
                McpOAuthVaultWriteMode::CreateOnly,
            )
            .await
            .expect("seed stale");

        persistence
            .prepare_new_authorization()
            .await
            .expect("stale flow reclaimed");
        assert!(persistence
            .load_pending(&flow_id)
            .await
            .expect("load")
            .is_none());
    }

    #[tokio::test]
    async fn orphaned_pkce_state_is_reclaimed_before_a_replacement_start() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let persistence = McpOAuthPersistence::new(&profile, vault.clone()).expect("persistence");
        let (_, state_store) = stores(&profile, vault);
        state_store
            .save(
                "cancelled-csrf",
                state("cancelled-csrf", "https://issuer.example/tenant"),
            )
            .await
            .expect("seed cancelled start");
        assert!(persistence
            .authorization_state_exists("cancelled-csrf")
            .await
            .expect("state exists"));

        persistence
            .prepare_new_authorization()
            .await
            .expect("orphan reclaimed");
        assert!(!persistence
            .authorization_state_exists("cancelled-csrf")
            .await
            .expect("state removed"));
    }

    #[tokio::test]
    async fn malformed_unrelated_binding_cannot_poison_exact_pending_or_cleanup() {
        let selected_profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let unrelated_profile = profile(
            "work",
            "https://other.example/mcp",
            "https://issuer.example/other",
        );
        let vault = Arc::new(MemoryVault::default());
        let selected =
            McpOAuthPersistence::new(&selected_profile, vault.clone()).expect("selected");
        let unrelated =
            McpOAuthPersistence::new(&unrelated_profile, vault.clone()).expect("unrelated");
        vault.records.lock().expect("records").insert(
            unrelated.binding.pending_key(&unrelated.callback_flow_id()),
            b"{malformed".to_vec(),
        );

        assert!(selected
            .active_pending()
            .await
            .expect("exact status")
            .is_none());
        selected
            .prepare_new_authorization()
            .await
            .expect("exact preparation");
        selected.clear_exact_binding().await.expect("exact cleanup");
    }

    #[test]
    fn pending_callback_metadata_rejects_bad_ids_and_expiry() {
        assert_eq!(
            validate_flow_id("NOT-A-FLOW-ID").expect_err("bad flow id"),
            McpOAuthPersistenceError::InvalidRecord
        );
        let now = now_epoch_secs();
        let expired = PendingAuthorizationRecord {
            flow_id: "0123456789abcdef0123456789abcdef".to_owned(),
            attempt_id: "11111111111111111111111111111111".to_owned(),
            client_id: "client-id".to_owned(),
            redirect_uri: "http://127.0.0.1/callback".to_owned(),
            scopes: Vec::new(),
            registration: PendingClientRegistration::DynamicPublic,
            credential_revision_before: None,
            created_at: now.saturating_sub(AUTHORIZATION_STATE_TTL_SECS + 1),
        };
        assert_eq!(
            validate_pending_authorization(&expired, now).expect_err("expired"),
            McpOAuthPersistenceError::InvalidRecord
        );
    }

    #[tokio::test]
    async fn sdk_credentials_round_trip_only_for_the_exact_issuer() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let (credential_store, _) = stores(&profile, Arc::clone(&vault));
        let credentials = StoredCredentials::new("client-id".to_owned(), None, vec![], None)
            .with_issuer(Some("https://issuer.example/tenant".to_owned()));
        credential_store.save(credentials).await.expect("save");
        let loaded = credential_store
            .load()
            .await
            .expect("load")
            .expect("record");
        assert_eq!(loaded.client_id, "client-id");

        let invalid = StoredCredentials::new("client-id".to_owned(), None, vec![], None)
            .with_issuer(Some("https://issuer.example/other".to_owned()));
        assert!(credential_store.save(invalid).await.is_err());
    }

    #[tokio::test]
    async fn sdk_credentials_from_before_received_at_tracking_remain_compatible() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let (credential_store, _) = stores(&profile, vault);
        let token = OAuthTokenResponse::new(
            AccessToken::new("access-token".to_owned()),
            BasicTokenType::Bearer,
            VendorExtraTokenFields::default(),
        );
        let credentials = StoredCredentials::new(
            "client-id".to_owned(),
            Some(token),
            vec!["messages.read".to_owned()],
            None,
        )
        .with_issuer(Some("https://issuer.example/tenant".to_owned()));
        credential_store.save(credentials).await.expect("save");
        assert!(credential_store.load().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn credential_envelope_cannot_be_replayed_under_another_binding() {
        let source = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let target = profile(
            "work",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let (source_store, _) = stores(&source, Arc::clone(&vault));
        let (target_store, _) = stores(&target, Arc::clone(&vault));
        source_store
            .save(
                StoredCredentials::new("client-id".to_owned(), None, vec![], None)
                    .with_issuer(Some("https://issuer.example/tenant".to_owned())),
            )
            .await
            .expect("save");

        let source_key = ExactBinding::new(&source).unwrap().credential_key();
        let target_key = ExactBinding::new(&target).unwrap().credential_key();
        let copied = vault.read(source_key).await.unwrap().unwrap();
        vault
            .write(target_key, copied, McpOAuthVaultWriteMode::Replace)
            .await
            .unwrap();
        assert!(target_store.load().await.is_err());
        assert!(vault.read(target_key).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn authorization_state_is_create_only_and_consumed_once() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let (_, state_store) = stores(&profile, vault);
        state_store
            .save(
                "csrf-token",
                state("csrf-token", "https://issuer.example/tenant"),
            )
            .await
            .expect("save");
        assert!(state_store
            .save(
                "csrf-token",
                state("csrf-token", "https://issuer.example/tenant"),
            )
            .await
            .is_err());
        assert!(state_store.load("csrf-token").await.unwrap().is_some());
        assert!(state_store.load("csrf-token").await.unwrap().is_none());
        state_store
            .delete("csrf-token")
            .await
            .expect("idempotent delete");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callbacks_have_one_atomic_winner() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let (_, state_store) = stores(&profile, vault);
        state_store
            .save(
                "csrf-token",
                state("csrf-token", "https://issuer.example/tenant"),
            )
            .await
            .unwrap();
        let store = Arc::new(state_store);
        let mut tasks = Vec::new();
        for _ in 0..16 {
            let store = Arc::clone(&store);
            tasks.push(tokio::spawn(async move {
                store.load("csrf-token").await.unwrap().is_some()
            }));
        }
        let mut winners = 0;
        for task in tasks {
            winners += usize::from(task.await.unwrap());
        }
        assert_eq!(winners, 1);
    }

    #[tokio::test]
    async fn mismatched_and_expired_callback_state_fails_closed_after_consumption() {
        let profile = profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        );
        let vault = Arc::new(MemoryVault::default());
        let (_, state_store) = stores(&profile, Arc::clone(&vault));
        assert!(state_store
            .save(
                "csrf-token",
                state("other-token", "https://issuer.example/tenant"),
            )
            .await
            .is_err());
        assert!(state_store
            .save(
                "csrf-token",
                state("csrf-token", "https://issuer.example/other"),
            )
            .await
            .is_err());

        let mut expired = state("csrf-token", "https://issuer.example/tenant");
        expired.created_at = now_epoch_secs().saturating_sub(AUTHORIZATION_STATE_TTL_SECS + 1);
        let binding = ExactBinding::new(&profile).unwrap();
        let document = encode_authorization_state(&binding, expired).unwrap();
        let key = binding.state_key();
        vault
            .write(key, document, McpOAuthVaultWriteMode::CreateOnly)
            .await
            .unwrap();
        assert!(state_store.load("csrf-token").await.is_err());
        assert!(vault.read(key).await.unwrap().is_none());
    }

    #[test]
    fn scope_and_document_limits_fail_with_value_free_diagnostics() {
        let binding = ExactBinding::new(&profile(
            "personal",
            "https://provider.example/mcp",
            "https://issuer.example/tenant",
        ))
        .unwrap();
        let canary = "SECRET_SCOPE_CANARY";
        let invalid = vec![format!("{canary} whitespace")];
        let error = validate_scopes(&invalid).expect_err("invalid scope");
        assert!(!error.to_string().contains(canary));
        assert!(McpOAuthSecretDocument::new(vec![0; MAX_SECRET_DOCUMENT_BYTES + 1]).is_err());
        assert!(validate_credentials(
            &binding,
            &StoredCredentials::new("client-id".to_owned(), None, invalid, None)
                .with_issuer(Some("https://issuer.example/tenant".to_owned()))
        )
        .is_err());
    }

    #[test]
    fn validation_and_key_derivation_fit_a_small_stack() {
        thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let profile = profile(
                    "personal",
                    "https://provider.example/mcp",
                    "https://issuer.example/tenant",
                );
                let binding = ExactBinding::new(&profile).unwrap();
                for index in 0..1_000 {
                    let token = format!("csrf-{index}");
                    let state = state(&token, "https://issuer.example/tenant");
                    validate_authorization_state(&binding, &token, &state, now_epoch_secs())
                        .unwrap();
                    let _ = binding.state_key();
                }
            })
            .expect("spawn")
            .join()
            .expect("small-stack validation");
    }
}
