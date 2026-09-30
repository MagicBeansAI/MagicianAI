//! SDK-owned MCP OAuth authorization coordinator.
//!
//! The official `rmcp` SDK remains responsible for discovery, registration, PKCE,
//! callback parsing, issuer validation, and token exchange. This module supplies the
//! product boundary around those mechanics: exact profile binding, opaque callback
//! routing, durable restart metadata, stable errors, and a browser URL capability that
//! cannot be serialized or printed.

use std::{collections::HashSet, fmt, mem, sync::Arc, time::SystemTime};

use rand::{rngs::OsRng, RngCore};
use rmcp::transport::auth::{
    AuthError, AuthorizationCallback, AuthorizationManager, AuthorizationMetadata,
    AuthorizationRequest, AuthorizationSession, OAuthClientConfig, OAuthHttpClient,
};
use serde::Serialize;
use tokio::sync::Mutex;
use tool_runtime_core::{
    credential_profiles::{CredentialProfileBinding, CredentialProfileKey},
    manifest::AuthState,
};
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::oauth_persistence::{
    validate_flow_id, McpOAuthPersistence, McpOAuthPersistenceError, PendingAuthorizationRecord,
    PendingClientRegistration,
};
use crate::{BearerToken, McpOAuthVault};

const MAX_CALLBACK_URL_BYTES: usize = 32 * 1024;
const MAX_AUTHORIZATION_URL_BYTES: usize = 64 * 1024;
const MAX_CLIENT_ID_BYTES: usize = 16 * 1024;
const MAX_CLIENT_SECRET_BYTES: usize = 64 * 1024;
const MAX_CLIENT_METADATA_URL_BYTES: usize = 16 * 1024;
const MAX_CLIENT_NAME_BYTES: usize = 512;
const MAX_SCOPE_COUNT: usize = 256;
const MAX_SCOPE_BYTES: usize = 1_024;
const MAX_TOTAL_SCOPE_BYTES: usize = 64 * 1024;

/// Stable, value-free OAuth coordinator failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum McpOAuthCoordinatorErrorCode {
    InvalidConfiguration,
    DiscoveryUnavailable,
    MetadataRequired,
    IssuerMismatch,
    RegistrationFailed,
    AuthorizationUnsupported,
    PendingAuthorizationConflict,
    PendingAuthorizationNotFound,
    CallbackRejected,
    TokenExchangeFailed,
    AuthorizationRequired,
    RefreshFailed,
    RefreshRejected,
    ScopeUpgradeNotRequired,
    PersistenceUnavailable,
}

/// Stable OAuth coordinator error safe for logs, APIs, and strategy projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpOAuthCoordinatorError {
    pub code: McpOAuthCoordinatorErrorCode,
    pub message: &'static str,
}

impl McpOAuthCoordinatorError {
    const fn new(code: McpOAuthCoordinatorErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }
}

/// Confidential pre-registered client secret.
///
/// The value has no `Clone`, serialization, or value-bearing `Debug` implementation and
/// is zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct McpOAuthClientSecret(String);

impl McpOAuthClientSecret {
    pub fn new(value: impl Into<String>) -> Result<Self, McpOAuthCoordinatorError> {
        let mut value = value.into();
        if value.is_empty()
            || value.len() > MAX_CLIENT_SECRET_BYTES
            || value.chars().any(char::is_control)
        {
            value.zeroize();
            return Err(invalid_configuration());
        }
        Ok(Self(value))
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for McpOAuthClientSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpOAuthClientSecret([REDACTED])")
    }
}

/// Client-registration material available to the SDK.
#[non_exhaustive]
pub enum McpOAuthClientIdentity {
    /// Ask the SDK to use dynamic registration for a public native client.
    DynamicPublic,
    /// Use an HTTPS Client ID Metadata Document when the provider advertises it.
    ClientMetadata { url: String },
    /// Use an out-of-band client registration. The optional secret stays in the
    /// coordinator and is never written into pending callback metadata.
    PreRegistered {
        client_id: String,
        client_secret: Option<McpOAuthClientSecret>,
    },
}

impl fmt::Debug for McpOAuthClientIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DynamicPublic => formatter.write_str("McpOAuthClientIdentity::DynamicPublic"),
            Self::ClientMetadata { .. } => {
                formatter.write_str("McpOAuthClientIdentity::ClientMetadata([REDACTED])")
            },
            Self::PreRegistered { client_secret, .. } => formatter
                .debug_struct("McpOAuthClientIdentity::PreRegistered")
                .field("client_id", &"[REDACTED]")
                .field("has_client_secret", &client_secret.is_some())
                .finish(),
        }
    }
}

/// Opaque, non-secret callback route identity.
#[derive(Clone, PartialEq, Eq)]
pub struct McpOAuthCallbackRoute {
    binding_id: String,
    flow_id: String,
    attempt_id: String,
    redirect_uri: String,
}

impl McpOAuthCallbackRoute {
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }

    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }
}

impl fmt::Debug for McpOAuthCallbackRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCallbackRoute")
            .field("binding_id", &self.binding_id)
            .field("flow_id", &self.flow_id)
            .field("attempt_id", &self.attempt_id)
            .field("redirect_uri", &self.redirect_uri)
            .finish()
    }
}

/// Successful authorization projection. Tokens and authorization codes never cross this
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpOAuthAuthorizationOutcome {
    pub granted_scopes: Vec<String>,
    pub attempt_id: String,
}

/// Secret-free status for one exact MCP OAuth binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpOAuthCredentialStatus {
    pub state: AuthState,
    pub granted_scopes: Vec<String>,
    pub refresh_available: bool,
}

/// Lifecycle mutation represented in a product-safe audit projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthLifecycleOperation {
    Refresh,
    DefinitiveRejectionInvalidation,
    LocalLogout,
    ScopeUpgrade,
}

/// Secret-free result of an exact-binding lifecycle mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct McpOAuthLifecycleAudit {
    pub operation: McpOAuthLifecycleOperation,
    pub previous_state: AuthState,
    pub current_state: AuthState,
    pub granted_scope_count: usize,
    pub refresh_available: bool,
}

/// A scope-upgrade browser flow and the safe audit record for starting it.
pub struct McpOAuthScopeUpgradeStart {
    authorization: McpOAuthAuthorizationStart,
    audit: McpOAuthLifecycleAudit,
}

impl McpOAuthScopeUpgradeStart {
    pub fn authorization(&self) -> &McpOAuthAuthorizationStart {
        &self.authorization
    }

    pub fn audit(&self) -> &McpOAuthLifecycleAudit {
        &self.audit
    }

    pub fn into_authorization(self) -> McpOAuthAuthorizationStart {
        self.authorization
    }
}

impl fmt::Debug for McpOAuthScopeUpgradeStart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthScopeUpgradeStart")
            .field("authorization", &self.authorization)
            .field("audit", &self.audit)
            .finish()
    }
}

/// A live authorization session plus its trusted browser-launch capability.
///
/// Callers may pass `browser_url()` directly to a product-owned browser opener, then keep
/// this value in an exact-route callback registry. It intentionally cannot be cloned,
/// serialized, or printed with its URL.
pub struct McpOAuthAuthorizationStart {
    route: McpOAuthCallbackRoute,
    browser_url: Zeroizing<String>,
    session: AuthorizationSession,
    persistence: McpOAuthPersistence,
    lifecycle: Arc<Mutex<()>>,
}

impl McpOAuthAuthorizationStart {
    pub fn route(&self) -> &McpOAuthCallbackRoute {
        &self.route
    }

    /// Expose only to a trusted product browser opener. Do not place this value in model,
    /// analytics, audit, or generic diagnostic surfaces.
    pub fn browser_url(&self) -> &str {
        self.browser_url.as_str()
    }

    /// Complete a live callback. The SDK atomically consumes the PKCE state; the durable
    /// pending route is removed only after token persistence succeeds.
    pub async fn complete(
        &self,
        callback_url: &str,
    ) -> Result<McpOAuthAuthorizationOutcome, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        validate_callback_url(&self.route, callback_url)?;
        let callback = parse_callback(callback_url)?;
        self.persistence
            .purge_expired_authorizations()
            .await
            .map_err(map_persistence_error)?;
        let pending = load_matching_pending(&self.persistence, &self.route).await?;
        if !self
            .persistence
            .authorization_state_exists(&callback.csrf_token)
            .await
            .map_err(map_callback_state_error)?
        {
            return Err(callback_rejected());
        }
        let (live_client_id, _) = self
            .session
            .get_credentials()
            .await
            .map_err(map_callback_error)?;
        if live_client_id != pending.client_id {
            return Err(callback_rejected());
        }

        self.session
            .handle_callback_url(callback_url)
            .await
            .map_err(map_callback_error)?;
        if self
            .persistence
            .delete_pending(self.route.flow_id())
            .await
            .is_err()
        {
            tracing::warn!(
                error_class = "oauth_pending_cleanup_deferred",
                "MCP OAuth token exchange committed; pending cleanup will reconcile from the credential revision"
            );
        }
        let granted_scopes = self.session.auth_manager.get_current_scopes().await;
        Ok(McpOAuthAuthorizationOutcome {
            granted_scopes,
            attempt_id: self.route.attempt_id.clone(),
        })
    }

    /// Complete a live callback from the raw HTTP query string.
    ///
    /// Product callback handlers should prefer this method over reconstructing a full
    /// URL from request headers. The trusted redirect origin and path come from the
    /// coordinator-created route; only the bounded query is supplied by the request.
    pub async fn complete_query(
        &self,
        callback_query: &str,
    ) -> Result<McpOAuthAuthorizationOutcome, McpOAuthCoordinatorError> {
        let callback_url = callback_url_from_query(&self.route, callback_query)?;
        self.complete(callback_url.as_str()).await
    }

    /// Cancel this exact browser attempt and reclaim its durable pending/PKCE state.
    /// Existing credentials, other exact bindings, and remote provider grants are not
    /// changed. Product dismiss actions should call this instead of only dropping the
    /// in-memory flow.
    pub async fn cancel(&self) -> Result<(), McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        self.persistence
            .purge_expired_authorizations()
            .await
            .map_err(map_persistence_error)?;
        let _pending = load_matching_pending(&self.persistence, &self.route).await?;
        let authorization_state = authorization_request_state(self.browser_url())?;

        // Delete the pending marker first. If the subsequent state deletion fails, the
        // next begin can safely reclaim the now-orphaned exact-binding PKCE record.
        self.persistence
            .delete_pending(self.route.flow_id())
            .await
            .map_err(map_persistence_error)?;
        self.persistence
            .delete_authorization_state(&authorization_state)
            .await
            .map_err(map_persistence_error)
    }
}

impl fmt::Debug for McpOAuthAuthorizationStart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthAuthorizationStart")
            .field("route", &self.route)
            .field("browser_url", &"[REDACTED]")
            .field("session", &"[REDACTED]")
            .finish()
    }
}

/// Exact-profile OAuth coordinator around the official SDK.
pub struct McpOAuthCoordinator {
    profile: CredentialProfileKey,
    resource_url: String,
    authorization_issuer: String,
    callback_base: Url,
    identity: McpOAuthClientIdentity,
    client_name: String,
    scopes: Vec<String>,
    persistence: McpOAuthPersistence,
    oauth_http_client: Option<Arc<dyn OAuthHttpClient>>,
    lifecycle: Arc<Mutex<()>>,
}

impl McpOAuthCoordinator {
    pub fn new(
        profile: CredentialProfileKey,
        vault: Arc<dyn McpOAuthVault>,
        callback_base: impl AsRef<str>,
        identity: McpOAuthClientIdentity,
    ) -> Result<Self, McpOAuthCoordinatorError> {
        let CredentialProfileBinding::McpOauth {
            resource_url,
            authorization_issuer,
        } = &profile.binding
        else {
            return Err(invalid_configuration());
        };
        let resource_url = resource_url.as_str().to_owned();
        let authorization_issuer = authorization_issuer.as_str().to_owned();
        validate_identity(&identity)?;
        let callback_base = validate_callback_base(callback_base.as_ref())?;
        let persistence =
            McpOAuthPersistence::new(&profile, vault).map_err(map_persistence_error)?;
        Ok(Self {
            profile,
            resource_url,
            authorization_issuer,
            callback_base,
            identity,
            client_name: "MCP Client".to_owned(),
            scopes: Vec::new(),
            persistence,
            oauth_http_client: None,
            lifecycle: Arc::new(Mutex::new(())),
        })
    }

    pub fn with_client_name(
        mut self,
        client_name: impl Into<String>,
    ) -> Result<Self, McpOAuthCoordinatorError> {
        let client_name = client_name.into();
        validate_bounded_text(&client_name, MAX_CLIENT_NAME_BYTES)?;
        self.client_name = client_name;
        Ok(self)
    }

    pub fn with_scopes(
        mut self,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, McpOAuthCoordinatorError> {
        let scopes = scopes.into_iter().map(Into::into).collect::<Vec<_>>();
        validate_scopes(&scopes)?;
        self.scopes = scopes;
        Ok(self)
    }

    pub fn callback_binding_id(&self) -> String {
        self.persistence.callback_binding_id()
    }

    /// Exact non-secret profile identity bound to this coordinator.
    ///
    /// Product callback registries use this to derive scope for user-facing pending and
    /// resolved events. Callers must not substitute request- or model-provided scope.
    pub fn profile_key(&self) -> &CredentialProfileKey {
        &self.profile
    }

    /// Observe exact-binding credential metadata without exposing client or token
    /// material. An active browser flow takes precedence over a previously usable
    /// token because the grant is currently being changed.
    pub async fn status(&self) -> Result<McpOAuthCredentialStatus, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        self.status_unlocked().await
    }

    /// Resolve one connection-scoped bearer credential through the official SDK.
    ///
    /// The token never crosses the product API or model boundary: callers can only
    /// move the zeroizing [`BearerToken`] directly into an MCP transport config.
    /// SDK refresh semantics remain authoritative and all lifecycle mutations for
    /// this exact profile binding are serialized by the coordinator lock.
    pub async fn bearer_token(&self) -> Result<BearerToken, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        let manager = self
            .initialized_manager(self.callback_base.as_str())
            .await?;
        let token = manager
            .get_access_token()
            .await
            .map_err(map_lifecycle_error)?;
        Ok(BearerToken::new(token))
    }

    /// Ask the official SDK to refresh this exact binding. Concurrent refresh,
    /// logout, invalidation, and scope-upgrade operations on this coordinator are
    /// serialized. A definitive `invalid_grant` clears only this binding; transient
    /// provider failures preserve the credential for a later retry.
    pub async fn refresh(&self) -> Result<McpOAuthLifecycleAudit, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        let before = self.status_unlocked().await?;
        if before.state == AuthState::Missing || before.state == AuthState::Authenticating {
            return Err(authorization_required());
        }

        let manager = self
            .initialized_manager(self.callback_base.as_str())
            .await?;
        match manager.refresh_token().await {
            Ok(_) => {},
            Err(AuthError::TokenRefreshRejected(_)) => {
                self.persistence
                    .clear_credentials()
                    .await
                    .map_err(map_persistence_error)?;
                return Err(McpOAuthCoordinatorError::new(
                    McpOAuthCoordinatorErrorCode::RefreshRejected,
                    "the MCP OAuth refresh was rejected and local credentials were invalidated",
                ));
            },
            Err(AuthError::AuthorizationRequired) => return Err(authorization_required()),
            Err(_) => {
                return Err(McpOAuthCoordinatorError::new(
                    McpOAuthCoordinatorErrorCode::RefreshFailed,
                    "the MCP OAuth refresh is temporarily unavailable",
                ))
            },
        }
        self.persistence
            .restore_granted_scopes_if_empty(&before.granted_scopes)
            .await
            .map_err(map_persistence_error)?;
        let after = self.status_unlocked().await?;
        Ok(lifecycle_audit(
            McpOAuthLifecycleOperation::Refresh,
            &before,
            &after,
        ))
    }

    /// Invalidate only this exact binding after a trusted caller has classified a
    /// provider response as a definitive token rejection.
    pub async fn invalidate_definitive_rejection(
        &self,
    ) -> Result<McpOAuthLifecycleAudit, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        let before = self.status_unlocked().await?;
        self.persistence
            .clear_credentials()
            .await
            .map_err(map_persistence_error)?;
        let after = self.status_unlocked().await?;
        Ok(lifecycle_audit(
            McpOAuthLifecycleOperation::DefinitiveRejectionInvalidation,
            &before,
            &after,
        ))
    }

    /// Perform idempotent local logout for this exact binding. The pinned SDK does
    /// not expose provider revocation; therefore this method truthfully removes local
    /// credentials and abandoned callback state without claiming remote revocation.
    pub async fn logout_local(&self) -> Result<McpOAuthLifecycleAudit, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        let before = self.status_unlocked().await?;
        self.persistence
            .clear_exact_binding()
            .await
            .map_err(map_persistence_error)?;
        let after = self.status_unlocked().await?;
        Ok(lifecycle_audit(
            McpOAuthLifecycleOperation::LocalLogout,
            &before,
            &after,
        ))
    }

    /// Begin a bounded explicit scope upgrade through the SDK. Existing granted
    /// scopes are retained, duplicate/no-op requests are rejected, and PKCE/browser
    /// capability handling remains identical to a first authorization.
    pub async fn begin_scope_upgrade(
        &self,
        required_scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<McpOAuthScopeUpgradeStart, McpOAuthCoordinatorError> {
        let required_scopes = required_scopes
            .into_iter()
            .map(Into::into)
            .collect::<Vec<_>>();
        validate_scopes(&required_scopes)?;
        if required_scopes.is_empty() {
            return Err(scope_upgrade_not_required());
        }

        let _guard = self.lifecycle.lock().await;
        let before = self.status_unlocked().await?;
        if before.state == AuthState::Missing || before.state == AuthState::Authenticating {
            return Err(authorization_required());
        }
        let mut union = before.granted_scopes.clone();
        let mut seen = union.iter().cloned().collect::<HashSet<_>>();
        let mut added = false;
        for scope in required_scopes {
            if seen.insert(scope.clone()) {
                union.push(scope);
                added = true;
            }
        }
        validate_scopes(&union)?;
        if !added {
            return Err(scope_upgrade_not_required());
        }

        self.persistence
            .prepare_new_authorization()
            .await
            .map_err(map_persistence_error)?;
        let credential_revision_before = self
            .persistence
            .credential_observation()
            .await
            .map_err(map_persistence_error)?
            .map(|observation| observation.revision);
        let flow_id = self.persistence.callback_flow_id();
        let attempt_id = generate_attempt_id();
        let route = self.callback_route(&flow_id, &attempt_id)?;
        let manager = self.initialized_manager(route.redirect_uri()).await?;
        let (client_id, _) = manager
            .get_credentials()
            .await
            .map_err(map_scope_upgrade_error)?;
        let required = union.join(" ");
        let auth_url = manager
            .request_scope_upgrade(&required)
            .await
            .map_err(map_scope_upgrade_error)?;
        let authorization_state = authorization_request_state(&auth_url)?;
        if let Err(error) = validate_authorization_url(&auth_url) {
            self.persistence
                .delete_authorization_state(&authorization_state)
                .await
                .map_err(map_persistence_error)?;
            return Err(error);
        }
        if let Err(error) = self
            .persistence
            .save_pending(PendingAuthorizationRecord {
                flow_id,
                attempt_id,
                client_id,
                redirect_uri: route.redirect_uri.clone(),
                scopes: union.clone(),
                registration: self.registration_kind(),
                credential_revision_before,
                created_at: now_epoch_secs(),
            })
            .await
        {
            self.persistence
                .delete_authorization_state(&authorization_state)
                .await
                .map_err(map_persistence_error)?;
            return Err(map_persistence_error(error));
        }
        let after = McpOAuthCredentialStatus {
            state: AuthState::Authenticating,
            granted_scopes: union,
            refresh_available: before.refresh_available,
        };
        Ok(McpOAuthScopeUpgradeStart {
            authorization: McpOAuthAuthorizationStart {
                route: route.clone(),
                browser_url: Zeroizing::new(auth_url.clone()),
                session: AuthorizationSession::for_scope_upgrade(
                    manager,
                    auth_url,
                    route.redirect_uri(),
                ),
                persistence: self.persistence.clone(),
                lifecycle: Arc::clone(&self.lifecycle),
            },
            audit: lifecycle_audit(McpOAuthLifecycleOperation::ScopeUpgrade, &before, &after),
        })
    }

    /// Begin SDK discovery/registration/PKCE and persist enough non-token client metadata
    /// to finish a callback after a process restart.
    pub async fn begin(&self) -> Result<McpOAuthAuthorizationStart, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        self.persistence
            .prepare_new_authorization()
            .await
            .map_err(map_persistence_error)?;
        let credential_revision_before = self
            .persistence
            .credential_observation()
            .await
            .map_err(map_persistence_error)?
            .map(|observation| observation.revision);
        let flow_id = self.persistence.callback_flow_id();
        let attempt_id = generate_attempt_id();
        let route = self.callback_route(&flow_id, &attempt_id)?;
        let mut manager = self.new_manager().await?;
        self.persistence.install(&mut manager);
        resolve_exact_metadata(
            &mut manager,
            &self.authorization_issuer,
            McpOAuthCoordinatorErrorCode::DiscoveryUnavailable,
        )
        .await?;

        let request = self.authorization_request(route.redirect_uri());
        let mut session = AuthorizationSession::new(manager, request)
            .await
            .map_err(|(_, error)| map_start_error(error))?;
        let (client_id, _) = match session.get_credentials().await {
            Ok(credentials) => credentials,
            Err(error) => {
                let authorization_state = authorization_request_state(&session.auth_url)?;
                self.persistence
                    .delete_authorization_state(&authorization_state)
                    .await
                    .map_err(map_persistence_error)?;
                return Err(map_start_error(error));
            },
        };
        validate_bounded_text(&client_id, MAX_CLIENT_ID_BYTES)?;
        let auth_url = mem::take(&mut session.auth_url);
        let authorization_state = authorization_request_state(&auth_url)?;
        if let Err(error) = validate_authorization_url(&auth_url) {
            self.persistence
                .delete_authorization_state(&authorization_state)
                .await
                .map_err(map_persistence_error)?;
            return Err(error);
        }

        if let Err(error) = self
            .persistence
            .save_pending(PendingAuthorizationRecord {
                flow_id,
                attempt_id,
                client_id,
                redirect_uri: route.redirect_uri.clone(),
                scopes: self.scopes.clone(),
                registration: self.registration_kind(),
                credential_revision_before,
                created_at: now_epoch_secs(),
            })
            .await
        {
            self.persistence
                .delete_authorization_state(&authorization_state)
                .await
                .map_err(map_persistence_error)?;
            return Err(map_persistence_error(error));
        }

        Ok(McpOAuthAuthorizationStart {
            route,
            browser_url: Zeroizing::new(auth_url),
            session,
            persistence: self.persistence.clone(),
            lifecycle: Arc::clone(&self.lifecycle),
        })
    }

    /// Finish a callback after restart when the in-memory live session no longer exists.
    /// The exact profile and opaque route must both match the durable pending record.
    pub async fn complete_after_restart(
        &self,
        binding_id: &str,
        flow_id: &str,
        callback_url: &str,
    ) -> Result<McpOAuthAuthorizationOutcome, McpOAuthCoordinatorError> {
        let _guard = self.lifecycle.lock().await;
        if binding_id != self.persistence.callback_binding_id()
            || validate_flow_id(flow_id).is_err()
        {
            return Err(callback_rejected());
        }
        self.persistence
            .purge_expired_authorizations()
            .await
            .map_err(map_persistence_error)?;
        let pending = load_pending_for_restart(&self.persistence, flow_id).await?;
        let route = self.callback_route(flow_id, &pending.attempt_id)?;
        validate_callback_url(&route, callback_url)?;
        let callback = parse_callback(callback_url)?;
        validate_pending_route(&pending, &route)?;
        if !self
            .persistence
            .authorization_state_exists(&callback.csrf_token)
            .await
            .map_err(map_callback_state_error)?
        {
            return Err(callback_rejected());
        }
        self.validate_pending_identity(&pending)?;

        let mut manager = self.new_manager().await?;
        self.persistence.install(&mut manager);
        resolve_exact_metadata(
            &mut manager,
            &self.authorization_issuer,
            McpOAuthCoordinatorErrorCode::DiscoveryUnavailable,
        )
        .await?;
        let mut client = OAuthClientConfig::new(&pending.client_id, &pending.redirect_uri)
            .with_scopes(pending.scopes.clone())
            .with_application_type("native");
        if let McpOAuthClientIdentity::PreRegistered {
            client_secret: Some(secret),
            ..
        } = &self.identity
        {
            client = client.with_client_secret(secret.expose());
        }
        manager
            .configure_client(client)
            .map_err(map_callback_error)?;
        let session =
            AuthorizationSession::for_scope_upgrade(manager, String::new(), &pending.redirect_uri);
        session
            .handle_callback_url(callback_url)
            .await
            .map_err(map_callback_error)?;
        if self.persistence.delete_pending(flow_id).await.is_err() {
            tracing::warn!(
                error_class = "oauth_pending_cleanup_deferred",
                "MCP OAuth restart exchange committed; pending cleanup will reconcile from the credential revision"
            );
        }
        let granted_scopes = session.auth_manager.get_current_scopes().await;
        Ok(McpOAuthAuthorizationOutcome {
            granted_scopes,
            attempt_id: pending.attempt_id,
        })
    }

    /// Finish a restarted callback from the raw HTTP query string without trusting the
    /// incoming Host, scheme, or path. The exact callback URL is rebuilt from this
    /// coordinator's configured base and the opaque route identifiers.
    pub async fn complete_after_restart_query(
        &self,
        binding_id: &str,
        flow_id: &str,
        callback_query: &str,
    ) -> Result<McpOAuthAuthorizationOutcome, McpOAuthCoordinatorError> {
        if binding_id != self.persistence.callback_binding_id()
            || validate_flow_id(flow_id).is_err()
        {
            return Err(callback_rejected());
        }
        let route = self.callback_route(flow_id, flow_id)?;
        let callback_url = callback_url_from_query(&route, callback_query)?;
        self.complete_after_restart(binding_id, flow_id, callback_url.as_str())
            .await
    }

    fn authorization_request(&self, redirect_uri: &str) -> AuthorizationRequest {
        let mut request = AuthorizationRequest::new(redirect_uri)
            .with_client_name(&self.client_name)
            .with_application_type("native");
        if !self.scopes.is_empty() {
            request = request.with_scopes(self.scopes.clone());
        }
        match &self.identity {
            McpOAuthClientIdentity::DynamicPublic => request,
            McpOAuthClientIdentity::ClientMetadata { url } => request.with_client_metadata_url(url),
            McpOAuthClientIdentity::PreRegistered {
                client_id,
                client_secret,
            } => {
                let request = request.with_preregistered_client(client_id);
                match client_secret {
                    Some(secret) => request.with_client_secret(secret.expose()),
                    None => request,
                }
            },
        }
    }

    fn registration_kind(&self) -> PendingClientRegistration {
        match &self.identity {
            McpOAuthClientIdentity::DynamicPublic => PendingClientRegistration::DynamicPublic,
            McpOAuthClientIdentity::ClientMetadata { .. } => {
                PendingClientRegistration::ClientMetadata
            },
            McpOAuthClientIdentity::PreRegistered { .. } => {
                PendingClientRegistration::PreRegistered
            },
        }
    }

    fn validate_pending_identity(
        &self,
        pending: &PendingAuthorizationRecord,
    ) -> Result<(), McpOAuthCoordinatorError> {
        let matches = match (&self.identity, pending.registration) {
            (McpOAuthClientIdentity::DynamicPublic, PendingClientRegistration::DynamicPublic) => {
                true
            },
            (
                McpOAuthClientIdentity::ClientMetadata { url },
                PendingClientRegistration::ClientMetadata,
            ) => pending.client_id == *url,
            (
                McpOAuthClientIdentity::PreRegistered { client_id, .. },
                PendingClientRegistration::PreRegistered,
            ) => pending.client_id == *client_id,
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err(callback_rejected())
        }
    }

    fn callback_route(
        &self,
        flow_id: &str,
        attempt_id: &str,
    ) -> Result<McpOAuthCallbackRoute, McpOAuthCoordinatorError> {
        validate_flow_id(flow_id).map_err(map_persistence_error)?;
        validate_flow_id(attempt_id).map_err(map_persistence_error)?;
        let binding_id = self.persistence.callback_binding_id();
        let mut redirect = self.callback_base.clone();
        let base_path = redirect.path().trim_end_matches('/');
        redirect.set_path(&format!("{base_path}/{binding_id}/{flow_id}"));
        Ok(McpOAuthCallbackRoute {
            binding_id,
            flow_id: flow_id.to_owned(),
            attempt_id: attempt_id.to_owned(),
            redirect_uri: redirect.to_string(),
        })
    }

    async fn new_manager(&self) -> Result<AuthorizationManager, McpOAuthCoordinatorError> {
        match &self.oauth_http_client {
            Some(client) => AuthorizationManager::new_with_oauth_http_client(
                &self.resource_url,
                Arc::clone(client),
            )
            .await
            .map_err(map_start_error),
            None => AuthorizationManager::new(&self.resource_url)
                .await
                .map_err(map_start_error),
        }
    }

    async fn initialized_manager(
        &self,
        redirect_uri: &str,
    ) -> Result<AuthorizationManager, McpOAuthCoordinatorError> {
        let mut manager = self.new_manager().await?;
        self.persistence.install(&mut manager);
        resolve_exact_metadata(
            &mut manager,
            &self.authorization_issuer,
            McpOAuthCoordinatorErrorCode::DiscoveryUnavailable,
        )
        .await?;
        if !manager
            .initialize_from_store()
            .await
            .map_err(map_lifecycle_error)?
        {
            return Err(authorization_required());
        }
        let (client_id, _) = manager
            .get_credentials()
            .await
            .map_err(map_lifecycle_error)?;
        let mut client = OAuthClientConfig::new(client_id, redirect_uri)
            .with_scopes(self.scopes.clone())
            .with_application_type("native");
        if let McpOAuthClientIdentity::PreRegistered {
            client_secret: Some(secret),
            ..
        } = &self.identity
        {
            client = client.with_client_secret(secret.expose());
        }
        manager
            .configure_client(client)
            .map_err(map_lifecycle_error)?;
        Ok(manager)
    }

    async fn status_unlocked(&self) -> Result<McpOAuthCredentialStatus, McpOAuthCoordinatorError> {
        let pending = self
            .persistence
            .active_pending()
            .await
            .map_err(map_persistence_error)?;
        let observation = self
            .persistence
            .credential_observation()
            .await
            .map_err(map_persistence_error)?;
        if let Some(pending) = pending {
            let exchange_committed = observation.as_ref().is_some_and(|current| {
                current.token_present
                    && pending
                        .credential_revision_before
                        .is_none_or(|before| before != current.revision)
            });
            if exchange_committed {
                if self
                    .persistence
                    .delete_pending(&pending.flow_id)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        error_class = "oauth_pending_cleanup_deferred",
                        "MCP OAuth status reconciled a committed exchange with deferred cleanup"
                    );
                }
            } else {
                return Ok(McpOAuthCredentialStatus {
                    state: AuthState::Authenticating,
                    granted_scopes: observation
                        .as_ref()
                        .map(|value| value.granted_scopes.clone())
                        .unwrap_or_default(),
                    refresh_available: observation
                        .as_ref()
                        .is_some_and(|value| value.refresh_available),
                });
            }
        }
        let Some(observation) = observation else {
            return Ok(McpOAuthCredentialStatus {
                state: AuthState::Missing,
                granted_scopes: Vec::new(),
                refresh_available: false,
            });
        };
        let state = if !observation.token_present {
            AuthState::Missing
        } else if observation
            .expires_at
            .is_some_and(|expires_at| expires_at <= now_epoch_secs())
        {
            AuthState::Expired
        } else {
            AuthState::Ready
        };
        Ok(McpOAuthCredentialStatus {
            state,
            granted_scopes: observation.granted_scopes,
            refresh_available: observation.refresh_available,
        })
    }

    #[cfg(test)]
    fn with_oauth_http_client(mut self, client: Arc<dyn OAuthHttpClient>) -> Self {
        self.oauth_http_client = Some(client);
        self
    }
}

impl fmt::Debug for McpOAuthCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCoordinator")
            .field("profile", &self.profile)
            .field("resource_url", &self.resource_url)
            .field("authorization_issuer", &self.authorization_issuer)
            .field("callback_base", &self.callback_base)
            .field("identity", &self.identity)
            .field("scope_count", &self.scopes.len())
            .finish()
    }
}

async fn resolve_exact_metadata(
    manager: &mut AuthorizationManager,
    expected_issuer: &str,
    unavailable_code: McpOAuthCoordinatorErrorCode,
) -> Result<(), McpOAuthCoordinatorError> {
    let resolution = manager.resolve_metadata().await.map_err(|_| {
        McpOAuthCoordinatorError::new(unavailable_code, "MCP OAuth discovery is unavailable")
    })?;
    if !resolution.source.is_discovered() {
        return Err(McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::MetadataRequired,
            "the MCP provider did not publish required OAuth metadata",
        ));
    }
    validate_metadata_issuer(&resolution.metadata, expected_issuer)?;
    manager.set_metadata(resolution.metadata);
    Ok(())
}

fn validate_metadata_issuer(
    metadata: &AuthorizationMetadata,
    expected_issuer: &str,
) -> Result<(), McpOAuthCoordinatorError> {
    let Some(issuer) = metadata.issuer.as_deref() else {
        return Err(McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::MetadataRequired,
            "the MCP authorization metadata is missing its issuer",
        ));
    };
    let issuer = Url::parse(issuer).map_err(|_| {
        McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::IssuerMismatch,
            "the MCP authorization issuer did not match the selected profile",
        )
    })?;
    let expected = Url::parse(expected_issuer).map_err(|_| invalid_configuration())?;
    if issuer != expected {
        return Err(McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::IssuerMismatch,
            "the MCP authorization issuer did not match the selected profile",
        ));
    }
    Ok(())
}

async fn load_matching_pending(
    persistence: &McpOAuthPersistence,
    route: &McpOAuthCallbackRoute,
) -> Result<PendingAuthorizationRecord, McpOAuthCoordinatorError> {
    let Some(pending) = persistence
        .load_pending(route.flow_id())
        .await
        .map_err(map_persistence_error)?
    else {
        return Err(McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::PendingAuthorizationNotFound,
            "the MCP OAuth authorization is missing or has expired",
        ));
    };
    validate_pending_route(&pending, route)?;
    Ok(pending)
}

async fn load_pending_for_restart(
    persistence: &McpOAuthPersistence,
    flow_id: &str,
) -> Result<PendingAuthorizationRecord, McpOAuthCoordinatorError> {
    persistence
        .load_pending(flow_id)
        .await
        .map_err(map_persistence_error)?
        .ok_or_else(|| {
            McpOAuthCoordinatorError::new(
                McpOAuthCoordinatorErrorCode::PendingAuthorizationNotFound,
                "the MCP OAuth authorization is missing or has expired",
            )
        })
}

fn validate_pending_route(
    pending: &PendingAuthorizationRecord,
    route: &McpOAuthCallbackRoute,
) -> Result<(), McpOAuthCoordinatorError> {
    if pending.redirect_uri != route.redirect_uri
        || pending.flow_id != route.flow_id
        || pending.attempt_id != route.attempt_id
    {
        return Err(callback_rejected());
    }
    Ok(())
}

fn validate_identity(identity: &McpOAuthClientIdentity) -> Result<(), McpOAuthCoordinatorError> {
    match identity {
        McpOAuthClientIdentity::DynamicPublic => Ok(()),
        McpOAuthClientIdentity::ClientMetadata { url } => {
            validate_bounded_text(url, MAX_CLIENT_METADATA_URL_BYTES)?;
            let parsed = Url::parse(url).map_err(|_| invalid_configuration())?;
            if parsed.scheme() != "https"
                || parsed.host_str().is_none()
                || parsed.path() == "/"
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(invalid_configuration());
            }
            Ok(())
        },
        McpOAuthClientIdentity::PreRegistered { client_id, .. } => {
            validate_bounded_text(client_id, MAX_CLIENT_ID_BYTES)
        },
    }
}

fn validate_callback_base(value: &str) -> Result<Url, McpOAuthCoordinatorError> {
    if value.is_empty() || value.len() > MAX_CALLBACK_URL_BYTES {
        return Err(invalid_configuration());
    }
    let parsed = Url::parse(value).map_err(|_| invalid_configuration())?;
    let loopback_http = parsed.scheme() == "http"
        && parsed.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if (parsed.scheme() != "https" && !loopback_http)
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid_configuration());
    }
    Ok(parsed)
}

fn validate_authorization_url(value: &str) -> Result<(), McpOAuthCoordinatorError> {
    if value.is_empty() || value.len() > MAX_AUTHORIZATION_URL_BYTES {
        return Err(McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "the MCP provider returned an invalid authorization URL",
        ));
    }
    let parsed = Url::parse(value).map_err(|_| {
        McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "the MCP provider returned an invalid authorization URL",
        )
    })?;
    let loopback_http = parsed.scheme() == "http"
        && parsed.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if (parsed.scheme() != "https" && !loopback_http)
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "the MCP provider returned an invalid authorization URL",
        ));
    }
    Ok(())
}

fn authorization_request_state(value: &str) -> Result<String, McpOAuthCoordinatorError> {
    let parsed = Url::parse(value).map_err(|_| {
        McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "the MCP provider returned an invalid authorization URL",
        )
    })?;
    let mut state = None;
    for (key, value) in parsed.query_pairs() {
        if key == "state" {
            if state.is_some() {
                return Err(McpOAuthCoordinatorError::new(
                    McpOAuthCoordinatorErrorCode::RegistrationFailed,
                    "the MCP provider returned an invalid authorization URL",
                ));
            }
            validate_callback_component(&value, 4 * 1024).map_err(|_| {
                McpOAuthCoordinatorError::new(
                    McpOAuthCoordinatorErrorCode::RegistrationFailed,
                    "the MCP provider returned an invalid authorization URL",
                )
            })?;
            state = Some(value.into_owned());
        }
    }
    state.ok_or_else(|| {
        McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "the MCP provider returned an invalid authorization URL",
        )
    })
}

fn parse_callback(callback_url: &str) -> Result<AuthorizationCallback, McpOAuthCoordinatorError> {
    let parsed = Url::parse(callback_url).map_err(|_| callback_rejected())?;
    let mut code_count = 0usize;
    let mut state_count = 0usize;
    let mut issuer_count = 0usize;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => {
                code_count = code_count.saturating_add(1);
                validate_callback_component(&value, 16 * 1024)?;
            },
            "state" => {
                state_count = state_count.saturating_add(1);
                validate_callback_component(&value, 4 * 1024)?;
            },
            "iss" => {
                issuer_count = issuer_count.saturating_add(1);
                validate_callback_component(&value, 16 * 1024)?;
            },
            "error" | "error_description" => return Err(callback_rejected()),
            _ => {},
        }
    }
    if code_count != 1 || state_count != 1 || issuer_count > 1 {
        return Err(callback_rejected());
    }
    AuthorizationCallback::from_redirect_url(callback_url).map_err(|_| callback_rejected())
}

fn validate_callback_component(
    value: &str,
    maximum: usize,
) -> Result<(), McpOAuthCoordinatorError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(callback_rejected());
    }
    Ok(())
}

fn validate_callback_url(
    route: &McpOAuthCallbackRoute,
    callback_url: &str,
) -> Result<(), McpOAuthCoordinatorError> {
    if callback_url.is_empty() || callback_url.len() > MAX_CALLBACK_URL_BYTES {
        return Err(callback_rejected());
    }
    let actual = Url::parse(callback_url).map_err(|_| callback_rejected())?;
    let expected = Url::parse(route.redirect_uri()).map_err(|_| invalid_configuration())?;
    if actual.scheme() != expected.scheme()
        || actual.host_str() != expected.host_str()
        || actual.port_or_known_default() != expected.port_or_known_default()
        || actual.path() != expected.path()
        || !actual.username().is_empty()
        || actual.password().is_some()
        || actual.fragment().is_some()
        || actual.query().is_none()
    {
        return Err(callback_rejected());
    }
    Ok(())
}

fn callback_url_from_query(
    route: &McpOAuthCallbackRoute,
    callback_query: &str,
) -> Result<Url, McpOAuthCoordinatorError> {
    if callback_query.is_empty()
        || callback_query.len() > MAX_CALLBACK_URL_BYTES
        || callback_query.chars().any(char::is_control)
    {
        return Err(callback_rejected());
    }
    let mut callback = Url::parse(route.redirect_uri()).map_err(|_| invalid_configuration())?;
    callback.set_query(Some(callback_query));
    if callback.as_str().len() > MAX_CALLBACK_URL_BYTES {
        return Err(callback_rejected());
    }
    Ok(callback)
}

fn validate_scopes(scopes: &[String]) -> Result<(), McpOAuthCoordinatorError> {
    if scopes.len() > MAX_SCOPE_COUNT {
        return Err(invalid_configuration());
    }
    let mut total = 0usize;
    let mut seen = std::collections::HashSet::with_capacity(scopes.len());
    for scope in scopes {
        total = total
            .checked_add(scope.len())
            .ok_or_else(invalid_configuration)?;
        if scope.is_empty()
            || scope.len() > MAX_SCOPE_BYTES
            || total > MAX_TOTAL_SCOPE_BYTES
            || !scope.bytes().all(|byte| {
                byte == 0x21 || (0x23..=0x5b).contains(&byte) || (0x5d..=0x7e).contains(&byte)
            })
            || !seen.insert(scope.as_str())
        {
            return Err(invalid_configuration());
        }
    }
    Ok(())
}

fn validate_bounded_text(value: &str, maximum: usize) -> Result<(), McpOAuthCoordinatorError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        return Err(invalid_configuration());
    }
    Ok(())
}

fn generate_attempt_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    let mut encoded = String::with_capacity(32);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn invalid_configuration() -> McpOAuthCoordinatorError {
    McpOAuthCoordinatorError::new(
        McpOAuthCoordinatorErrorCode::InvalidConfiguration,
        "the MCP OAuth coordinator configuration is invalid",
    )
}

fn callback_rejected() -> McpOAuthCoordinatorError {
    McpOAuthCoordinatorError::new(
        McpOAuthCoordinatorErrorCode::CallbackRejected,
        "the MCP OAuth callback was rejected",
    )
}

fn authorization_required() -> McpOAuthCoordinatorError {
    McpOAuthCoordinatorError::new(
        McpOAuthCoordinatorErrorCode::AuthorizationRequired,
        "MCP OAuth authorization is required",
    )
}

fn scope_upgrade_not_required() -> McpOAuthCoordinatorError {
    McpOAuthCoordinatorError::new(
        McpOAuthCoordinatorErrorCode::ScopeUpgradeNotRequired,
        "the requested MCP OAuth scopes are already granted",
    )
}

fn lifecycle_audit(
    operation: McpOAuthLifecycleOperation,
    before: &McpOAuthCredentialStatus,
    after: &McpOAuthCredentialStatus,
) -> McpOAuthLifecycleAudit {
    McpOAuthLifecycleAudit {
        operation,
        previous_state: before.state,
        current_state: after.state,
        granted_scope_count: after.granted_scopes.len(),
        refresh_available: after.refresh_available,
    }
}

fn map_persistence_error(error: McpOAuthPersistenceError) -> McpOAuthCoordinatorError {
    match error {
        McpOAuthPersistenceError::Conflict => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::PendingAuthorizationConflict,
            "the MCP OAuth callback route already exists",
        ),
        McpOAuthPersistenceError::InvalidBinding | McpOAuthPersistenceError::InvalidRecord => {
            invalid_configuration()
        },
        McpOAuthPersistenceError::Unavailable | McpOAuthPersistenceError::Corrupt => {
            McpOAuthCoordinatorError::new(
                McpOAuthCoordinatorErrorCode::PersistenceUnavailable,
                "MCP OAuth persistence is unavailable",
            )
        },
    }
}

fn map_callback_state_error(error: McpOAuthPersistenceError) -> McpOAuthCoordinatorError {
    match error {
        // The callback supplies the CSRF token. A syntactically valid but non-matching
        // token must remain an ordinary rejected callback, not look like trusted
        // coordinator configuration drift.
        McpOAuthPersistenceError::InvalidRecord => callback_rejected(),
        other => map_persistence_error(other),
    }
}

fn map_start_error(error: AuthError) -> McpOAuthCoordinatorError {
    match error {
        AuthError::NoAuthorizationSupport => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::AuthorizationUnsupported,
            "the MCP provider does not support OAuth authorization",
        ),
        AuthError::PkceUnsupported => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::AuthorizationUnsupported,
            "the MCP provider does not support required PKCE authorization",
        ),
        AuthError::RegistrationFailed(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "MCP OAuth client registration failed",
        ),
        AuthError::AuthorizationServerMismatch { .. }
        | AuthError::AuthorizationServerMissingIssuer { .. } => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::IssuerMismatch,
            "the MCP authorization issuer did not match the selected profile",
        ),
        AuthError::MetadataError(_) | AuthError::HttpError(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::DiscoveryUnavailable,
            "MCP OAuth discovery is unavailable",
        ),
        AuthError::InternalError(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::PersistenceUnavailable,
            "MCP OAuth persistence is unavailable",
        ),
        _ => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RegistrationFailed,
            "MCP OAuth authorization could not be started",
        ),
    }
}

fn map_lifecycle_error(error: AuthError) -> McpOAuthCoordinatorError {
    match error {
        AuthError::AuthorizationRequired => authorization_required(),
        AuthError::TokenRefreshRejected(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RefreshRejected,
            "the MCP OAuth refresh was rejected",
        ),
        AuthError::AuthorizationServerMismatch { .. }
        | AuthError::AuthorizationServerMissingIssuer { .. } => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::IssuerMismatch,
            "the MCP authorization issuer did not match the selected profile",
        ),
        AuthError::MetadataError(_)
        | AuthError::HttpError(_)
        | AuthError::TokenRefreshFailed(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RefreshFailed,
            "the MCP OAuth provider is temporarily unavailable",
        ),
        AuthError::InternalError(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::PersistenceUnavailable,
            "MCP OAuth persistence is unavailable",
        ),
        _ => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::RefreshFailed,
            "the MCP OAuth lifecycle operation failed",
        ),
    }
}

fn map_scope_upgrade_error(error: AuthError) -> McpOAuthCoordinatorError {
    match error {
        AuthError::AuthorizationRequired => authorization_required(),
        AuthError::InvalidScope(_) | AuthError::InsufficientScope { .. } => {
            McpOAuthCoordinatorError::new(
                McpOAuthCoordinatorErrorCode::AuthorizationUnsupported,
                "the MCP provider rejected the requested scope upgrade",
            )
        },
        other => map_lifecycle_error(other),
    }
}

fn map_callback_error(error: AuthError) -> McpOAuthCoordinatorError {
    match error {
        AuthError::AuthorizationServerMismatch { .. }
        | AuthError::AuthorizationServerMissingIssuer { .. } => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::IssuerMismatch,
            "the MCP OAuth callback issuer did not match the selected profile",
        ),
        AuthError::TokenExchangeFailed(_)
        | AuthError::AuthorizationFailed(_)
        | AuthError::OAuthError(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::TokenExchangeFailed,
            "the MCP OAuth token exchange failed",
        ),
        AuthError::InternalError(_) => callback_rejected(),
        AuthError::HttpError(_) => McpOAuthCoordinatorError::new(
            McpOAuthCoordinatorErrorCode::TokenExchangeFailed,
            "the MCP OAuth token exchange failed",
        ),
        _ => callback_rejected(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use async_trait::async_trait;
    use oauth2::HttpResponse;
    use rmcp::transport::auth::{
        OAuthHttpClientError, OAuthHttpClientFuture, OAuthHttpRedirectPolicy, OAuthHttpRequest,
    };
    use static_assertions::{assert_impl_all, assert_not_impl_any};
    use tool_runtime_core::credential_profiles::{
        CanonicalCredentialUrl, CredentialProfileBinding, CredentialProfileKey, CredentialScope,
    };

    use super::*;
    use crate::{
        McpOAuthSecretDocument, McpOAuthVaultError, McpOAuthVaultKey, McpOAuthVaultWriteMode,
    };

    #[derive(Default)]
    struct MemoryVault {
        records: Mutex<HashMap<McpOAuthVaultKey, Vec<u8>>>,
        fail_next_pending_delete: AtomicBool,
    }

    #[async_trait]
    impl McpOAuthVault for MemoryVault {
        async fn read(
            &self,
            key: McpOAuthVaultKey,
        ) -> Result<Option<McpOAuthSecretDocument>, McpOAuthVaultError> {
            self.records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?
                .get(&key)
                .cloned()
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
            self.records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?
                .remove(&key)
                .map(McpOAuthSecretDocument::new)
                .transpose()
                .map_err(|_| McpOAuthVaultError::Corrupt)
        }

        async fn delete(&self, key: McpOAuthVaultKey) -> Result<(), McpOAuthVaultError> {
            if key.namespace() == crate::McpOAuthVaultNamespace::PendingAuthorization
                && self.fail_next_pending_delete.swap(false, Ordering::AcqRel)
            {
                return Err(McpOAuthVaultError::Unavailable);
            }
            self.records
                .lock()
                .map_err(|_| McpOAuthVaultError::Unavailable)?
                .remove(&key);
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedRequest {
        uri: String,
        redirect_policy: OAuthHttpRedirectPolicy,
        body: Vec<u8>,
    }

    #[derive(Clone, Default)]
    struct FakeOAuthHttpClient {
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    }

    impl FakeOAuthHttpClient {
        fn with_responses(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                ..Self::default()
            }
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().expect("requests").clone()
        }
    }

    impl OAuthHttpClient for FakeOAuthHttpClient {
        fn execute(&self, request: OAuthHttpRequest) -> OAuthHttpClientFuture<'_> {
            self.requests
                .lock()
                .expect("requests")
                .push(RecordedRequest {
                    uri: request.request.uri().to_string(),
                    redirect_policy: request.redirect_policy,
                    body: request.request.body().clone(),
                });
            let response = self.responses.lock().expect("responses").pop_front();
            Box::pin(async move {
                response.ok_or_else(|| -> OAuthHttpClientError { "missing fake response".into() })
            })
        }
    }

    fn response(status: u16, body: serde_json::Value) -> HttpResponse {
        oauth2::http::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&body).expect("JSON"))
            .expect("response")
    }

    fn challenge_response(resource_metadata_url: &str) -> HttpResponse {
        oauth2::http::Response::builder()
            .status(401)
            .header(
                "www-authenticate",
                format!(r#"Bearer resource_metadata="{resource_metadata_url}""#),
            )
            .body(Vec::new())
            .expect("response")
    }

    fn discovery_responses(issuer: &str) -> Vec<HttpResponse> {
        discovery_responses_for("https://mcp.example.com/mcp", issuer)
    }

    fn discovery_responses_for(resource: &str, issuer: &str) -> Vec<HttpResponse> {
        let resource_url = Url::parse(resource).expect("resource URL");
        let resource_metadata_url = format!(
            "{}://{}/.well-known/oauth-protected-resource",
            resource_url.scheme(),
            resource_url.host_str().expect("resource host")
        );
        vec![
            challenge_response(&resource_metadata_url),
            response(
                200,
                serde_json::json!({
                    "resource": resource,
                    "authorization_servers": [issuer]
                }),
            ),
            response(
                200,
                serde_json::json!({
                    "issuer": issuer,
                    "authorization_endpoint": format!("{issuer}/authorize"),
                    "token_endpoint": format!("{issuer}/token"),
                    "registration_endpoint": format!("{issuer}/register"),
                    "scopes_supported": ["read", "offline_access"],
                    "response_types_supported": ["code"],
                    "code_challenge_methods_supported": ["S256"],
                    "authorization_response_iss_parameter_supported": true
                }),
            ),
        ]
    }

    fn token_response() -> HttpResponse {
        response(
            200,
            serde_json::json!({
                "access_token": "canary-access-token",
                "token_type": "Bearer",
                "expires_in": 3600,
                "scope": "read"
            }),
        )
    }

    fn refreshable_token_response(
        access_token: &str,
        refresh_token: Option<&str>,
        scope: Option<&str>,
    ) -> HttpResponse {
        let mut body = serde_json::json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": 3600
        });
        let object = body.as_object_mut().expect("object");
        if let Some(refresh_token) = refresh_token {
            object.insert(
                "refresh_token".to_owned(),
                serde_json::Value::String(refresh_token.to_owned()),
            );
        }
        if let Some(scope) = scope {
            object.insert(
                "scope".to_owned(),
                serde_json::Value::String(scope.to_owned()),
            );
        }
        response(200, body)
    }

    fn rejected_refresh_response() -> HttpResponse {
        response(
            400,
            serde_json::json!({
                "error": "invalid_grant",
                "error_description": "canary provider detail"
            }),
        )
    }

    fn profile(issuer: &str) -> CredentialProfileKey {
        profile_for("https://mcp.example.com/mcp", issuer, "personal")
    }

    fn profile_for(resource: &str, issuer: &str, alias: &str) -> CredentialProfileKey {
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

    fn coordinator(vault: Arc<MemoryVault>, client: FakeOAuthHttpClient) -> McpOAuthCoordinator {
        McpOAuthCoordinator::new(
            profile("https://auth.example.com"),
            vault,
            "http://127.0.0.1:3002/api/magician/v2/auth/mcp/oauth/callback",
            McpOAuthClientIdentity::PreRegistered {
                client_id: "registered-client".to_owned(),
                client_secret: Some(
                    McpOAuthClientSecret::new("canary-client-secret").expect("secret"),
                ),
            },
        )
        .expect("coordinator")
        .with_scopes(["read"])
        .expect("scopes")
        .with_oauth_http_client(Arc::new(client))
    }

    fn callback_url(start: &McpOAuthAuthorizationStart) -> String {
        let authorization = Url::parse(start.browser_url()).expect("authorization URL");
        let issuer = match authorization.port() {
            Some(port) => format!(
                "{}://{}:{port}",
                authorization.scheme(),
                authorization.host_str().expect("authorization host")
            ),
            None => format!(
                "{}://{}",
                authorization.scheme(),
                authorization.host_str().expect("authorization host")
            ),
        };
        let state = authorization
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("state");
        let mut callback = Url::parse(start.route().redirect_uri()).expect("callback URL");
        callback
            .query_pairs_mut()
            .append_pair("code", "canary-authorization-code")
            .append_pair("state", &state)
            .append_pair("iss", &issuer);
        callback.to_string()
    }

    fn callback_query(start: &McpOAuthAuthorizationStart) -> String {
        Url::parse(&callback_url(start))
            .expect("callback URL")
            .query()
            .expect("callback query")
            .to_owned()
    }

    #[test]
    fn secret_and_live_flow_surfaces_are_not_cloneable_or_serializable() {
        assert_not_impl_any!(McpOAuthClientSecret: Clone, Serialize);
        assert_not_impl_any!(McpOAuthAuthorizationStart: Clone, Serialize);
        assert_not_impl_any!(McpOAuthScopeUpgradeStart: Clone, Serialize);
        assert_impl_all!(McpOAuthAuthorizationStart: Send, Sync);
        assert_impl_all!(McpOAuthCoordinator: Send, Sync);
        let secret = McpOAuthClientSecret::new("canary-client-secret").expect("secret");
        assert_eq!(format!("{secret:?}"), "McpOAuthClientSecret([REDACTED])");
    }

    #[test]
    fn invalid_callback_and_identity_configuration_fail_closed() {
        let vault = Arc::new(MemoryVault::default());
        assert!(McpOAuthCoordinator::new(
            profile("https://auth.example.com"),
            vault.clone(),
            "http://provider.example/callback",
            McpOAuthClientIdentity::DynamicPublic,
        )
        .is_err());
        assert!(validate_authorization_url("http://provider.example/authorize?state=x").is_err());
        assert!(McpOAuthCoordinator::new(
            profile("https://auth.example.com"),
            vault,
            "http://127.0.0.1:3002/callback?unexpected=true",
            McpOAuthClientIdentity::PreRegistered {
                client_id: "bad\nclient".to_owned(),
                client_secret: None,
            },
        )
        .is_err());
    }

    #[test]
    fn callback_query_is_bound_to_the_preconfigured_origin_and_path() {
        let route = McpOAuthCallbackRoute {
            binding_id: "a".repeat(64),
            flow_id: "b".repeat(32),
            attempt_id: "c".repeat(32),
            redirect_uri: format!(
                "http://127.0.0.1:3002/api/magician/v2/auth/mcp/oauth/callback/{}/{}",
                "a".repeat(64),
                "b".repeat(32)
            ),
        };
        let callback = callback_url_from_query(&route, "code=one&state=two").expect("callback");
        assert_eq!(callback.scheme(), "http");
        assert_eq!(callback.host_str(), Some("127.0.0.1"));
        assert_eq!(callback.port(), Some(3002));
        assert_eq!(
            callback.path(),
            format!(
                "/api/magician/v2/auth/mcp/oauth/callback/{}/{}",
                "a".repeat(64),
                "b".repeat(32)
            )
        );
        assert_eq!(callback.query(), Some("code=one&state=two"));
        assert!(callback_url_from_query(&route, "").is_err());
        assert!(callback_url_from_query(&route, "code=one\n&state=two").is_err());
        assert!(callback_url_from_query(&route, &"x".repeat(MAX_CALLBACK_URL_BYTES + 1)).is_err());
    }

    #[tokio::test]
    async fn live_sdk_callback_never_projects_codes_or_tokens() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.push(token_response());
        let client = FakeOAuthHttpClient::with_responses(responses);
        let coordinator = coordinator(vault, client.clone());
        let start = coordinator.begin().await.expect("begin");
        let query = callback_query(&start);

        let debug = format!("{start:?}");
        assert!(!debug.contains("canary-client-secret"));
        assert!(!debug.contains("state="));
        let outcome = start.complete_query(&query).await.expect("complete");
        assert_eq!(outcome.granted_scopes, vec!["read"]);
        let token_request = client.requests().last().expect("token request").clone();
        assert!(token_request.uri.ends_with("/token"));
    }

    #[tokio::test]
    async fn committed_live_exchange_survives_pending_cleanup_failure_and_reconciles() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.push(token_response());
        let coordinator = coordinator(
            vault.clone(),
            FakeOAuthHttpClient::with_responses(responses),
        );
        let start = coordinator.begin().await.expect("begin");
        vault
            .fail_next_pending_delete
            .store(true, Ordering::Release);

        let outcome = start
            .complete_query(&callback_query(&start))
            .await
            .expect("committed exchange must not be masked by cleanup");
        assert_eq!(outcome.granted_scopes, ["read"]);
        assert_eq!(
            coordinator.status().await.expect("reconciled status").state,
            AuthState::Ready
        );
    }

    #[tokio::test]
    async fn callback_completes_after_restart_from_exact_durable_route() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.extend(discovery_responses("https://auth.example.com"));
        responses.push(token_response());
        let client = FakeOAuthHttpClient::with_responses(responses);

        let first = coordinator(vault.clone(), client.clone());
        let start = first.begin().await.expect("begin");
        let route = start.route().clone();
        let query = callback_query(&start);
        drop(start);
        drop(first);

        let restarted = coordinator(vault, client);
        let outcome = restarted
            .complete_after_restart_query(route.binding_id(), route.flow_id(), &query)
            .await
            .expect("restart completion");
        assert_eq!(outcome.granted_scopes, vec!["read"]);

        let replay = restarted
            .complete_after_restart_query(route.binding_id(), route.flow_id(), &query)
            .await
            .expect_err("replay must fail");
        assert_eq!(
            replay.code,
            McpOAuthCoordinatorErrorCode::PendingAuthorizationNotFound
        );
    }

    #[tokio::test]
    async fn committed_restart_exchange_survives_pending_cleanup_failure_and_reconciles() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.extend(discovery_responses("https://auth.example.com"));
        responses.push(token_response());
        let client = FakeOAuthHttpClient::with_responses(responses);
        let first = coordinator(vault.clone(), client.clone());
        let start = first.begin().await.expect("begin");
        let route = start.route().clone();
        let query = callback_query(&start);
        drop(start);
        drop(first);
        vault
            .fail_next_pending_delete
            .store(true, Ordering::Release);

        let restarted = coordinator(vault, client);
        restarted
            .complete_after_restart_query(route.binding_id(), route.flow_id(), &query)
            .await
            .expect("restart exchange must not be masked by cleanup");
        assert_eq!(
            restarted.status().await.expect("reconciled status").state,
            AuthState::Ready
        );
    }

    #[tokio::test]
    async fn wrong_route_is_rejected_without_consuming_the_real_callback() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.push(token_response());
        let client = FakeOAuthHttpClient::with_responses(responses);
        let coordinator = coordinator(vault, client);
        let start = coordinator.begin().await.expect("begin");
        let callback = callback_url(&start);
        let wrong = callback.replace("/callback/", "/other/");

        let rejected = start.complete(&wrong).await.expect_err("wrong route");
        assert_eq!(
            rejected.code,
            McpOAuthCoordinatorErrorCode::CallbackRejected
        );

        let mut wrong_state = Url::parse(&callback).expect("callback");
        wrong_state
            .query_pairs_mut()
            .clear()
            .append_pair("code", "canary-authorization-code")
            .append_pair("state", "wrong-state")
            .append_pair("iss", "https://auth.example.com");
        let rejected = start
            .complete(wrong_state.as_str())
            .await
            .expect_err("wrong state");
        assert_eq!(
            rejected.code,
            McpOAuthCoordinatorErrorCode::CallbackRejected
        );

        let duplicated_state = format!("{callback}&state=another-state");
        let rejected = start
            .complete(&duplicated_state)
            .await
            .expect_err("duplicate state");
        assert_eq!(
            rejected.code,
            McpOAuthCoordinatorErrorCode::CallbackRejected
        );
        let outcome = start.complete(&callback).await.expect("real callback");
        assert_eq!(outcome.granted_scopes, vec!["read"]);
    }

    #[tokio::test]
    async fn a_second_flow_for_the_same_binding_stops_before_network_discovery() {
        let vault = Arc::new(MemoryVault::default());
        let client =
            FakeOAuthHttpClient::with_responses(discovery_responses("https://auth.example.com"));
        let coordinator = coordinator(vault, client.clone());
        let _first = coordinator.begin().await.expect("first flow");
        let request_count = client.requests().len();

        let error = coordinator.begin().await.expect_err("second flow");
        assert_eq!(
            error.code,
            McpOAuthCoordinatorErrorCode::PendingAuthorizationConflict
        );
        assert_eq!(client.requests().len(), request_count);
    }

    #[tokio::test]
    async fn exact_issuer_mismatch_stops_before_browser_launch() {
        let vault = Arc::new(MemoryVault::default());
        let client = FakeOAuthHttpClient::with_responses(discovery_responses(
            "https://other-auth.example.com",
        ));
        let coordinator = coordinator(vault, client);
        let error = coordinator.begin().await.expect_err("issuer mismatch");
        assert_eq!(error.code, McpOAuthCoordinatorErrorCode::IssuerMismatch);
    }

    #[tokio::test]
    async fn status_refresh_and_logout_are_secret_free_and_preserve_omitted_scopes() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.push(refreshable_token_response(
            "canary-initial-access",
            Some("canary-refresh"),
            Some("read"),
        ));
        responses.extend(discovery_responses("https://auth.example.com"));
        responses.push(refreshable_token_response(
            "canary-refreshed-access",
            None,
            None,
        ));
        let client = FakeOAuthHttpClient::with_responses(responses);
        let coordinator = coordinator(vault, client.clone());

        assert_eq!(
            coordinator.status().await.expect("missing").state,
            AuthState::Missing
        );
        let start = coordinator.begin().await.expect("begin");
        assert_eq!(
            coordinator.status().await.expect("pending").state,
            AuthState::Authenticating
        );
        start
            .complete_query(&callback_query(&start))
            .await
            .expect("complete");
        let ready = coordinator.status().await.expect("ready");
        assert_eq!(ready.state, AuthState::Ready);
        assert!(ready.refresh_available);
        assert_eq!(ready.granted_scopes, ["read"]);

        let refreshed = coordinator.refresh().await.expect("refresh");
        assert_eq!(refreshed.operation, McpOAuthLifecycleOperation::Refresh);
        assert_eq!(refreshed.current_state, AuthState::Ready);
        assert_eq!(
            coordinator
                .status()
                .await
                .expect("refreshed")
                .granted_scopes,
            ["read"]
        );
        let refresh_request = client.requests().last().expect("refresh request").clone();
        assert!(refresh_request.uri.ends_with("/token"));
        assert!(String::from_utf8_lossy(&refresh_request.body).contains("grant_type=refresh_token"));

        let provider_request_count = client.requests().len();
        let logout = coordinator.logout_local().await.expect("logout");
        assert_eq!(logout.current_state, AuthState::Missing);
        assert_eq!(
            coordinator.status().await.expect("logged out").state,
            AuthState::Missing
        );
        let encoded = serde_json::to_string(&logout).expect("audit JSON");
        assert!(!encoded.contains("canary"));
        assert_eq!(
            client.requests().len(),
            provider_request_count,
            "local logout must not claim or attempt provider revocation"
        );
    }

    #[tokio::test]
    async fn independent_resource_endpoints_and_profiles_never_cross_callbacks_or_refresh() {
        let vault = Arc::new(MemoryVault::default());
        let resource_a = "https://mcp-a.example.com/mcp";
        let issuer_a = "https://auth-a.example.com";
        let resource_b = "https://mcp-b.example.com/mcp";
        let issuer_b = "https://auth-b.example.com";

        let mut responses_a = discovery_responses_for(resource_a, issuer_a);
        responses_a.push(refreshable_token_response(
            "a-access",
            Some("a-refresh"),
            Some("read"),
        ));
        responses_a.extend(discovery_responses_for(resource_a, issuer_a));
        responses_a.push(refreshable_token_response("a-access-next", None, None));
        let client_a = FakeOAuthHttpClient::with_responses(responses_a);

        let mut responses_b = discovery_responses_for(resource_b, issuer_b);
        responses_b.push(refreshable_token_response(
            "b-access",
            Some("b-refresh"),
            Some("read"),
        ));
        responses_b.extend(discovery_responses_for(resource_b, issuer_b));
        responses_b.push(refreshable_token_response("b-access-next", None, None));
        let client_b = FakeOAuthHttpClient::with_responses(responses_b);

        let coordinator_a = McpOAuthCoordinator::new(
            profile_for(resource_a, issuer_a, "personal"),
            vault.clone(),
            "http://127.0.0.1:3002/api/magician/v2/auth/mcp/oauth/callback",
            McpOAuthClientIdentity::PreRegistered {
                client_id: "client-a".to_owned(),
                client_secret: None,
            },
        )
        .expect("coordinator A")
        .with_scopes(["read"])
        .expect("scopes A")
        .with_oauth_http_client(Arc::new(client_a.clone()));
        let coordinator_b = McpOAuthCoordinator::new(
            profile_for(resource_b, issuer_b, "work"),
            vault,
            "http://127.0.0.1:3002/api/magician/v2/auth/mcp/oauth/callback",
            McpOAuthClientIdentity::PreRegistered {
                client_id: "client-b".to_owned(),
                client_secret: None,
            },
        )
        .expect("coordinator B")
        .with_scopes(["read"])
        .expect("scopes B")
        .with_oauth_http_client(Arc::new(client_b.clone()));

        let start_a = coordinator_a.begin().await.expect("begin A");
        let start_b = coordinator_b.begin().await.expect("begin B");
        start_b
            .complete_query(&callback_query(&start_b))
            .await
            .expect("callback B");
        start_a
            .complete_query(&callback_query(&start_a))
            .await
            .expect("callback A");

        coordinator_a.refresh().await.expect("refresh A");
        coordinator_b.refresh().await.expect("refresh B");
        assert_eq!(
            coordinator_a.status().await.expect("status A").state,
            AuthState::Ready
        );
        assert_eq!(
            coordinator_b.status().await.expect("status B").state,
            AuthState::Ready
        );

        let a_requests = client_a.requests();
        let b_requests = client_b.requests();
        assert!(a_requests.iter().all(|request| {
            !matches!(
                Url::parse(&request.uri).expect("request URL").host_str(),
                Some("mcp-b.example.com" | "auth-b.example.com")
            )
        }));
        assert!(b_requests.iter().all(|request| {
            !matches!(
                Url::parse(&request.uri).expect("request URL").host_str(),
                Some("mcp-a.example.com" | "auth-a.example.com")
            )
        }));
        let a_token_requests = a_requests
            .iter()
            .filter(|request| request.uri.ends_with("/token"))
            .collect::<Vec<_>>();
        let b_token_requests = b_requests
            .iter()
            .filter(|request| request.uri.ends_with("/token"))
            .collect::<Vec<_>>();
        assert_eq!(a_token_requests.len(), 2);
        assert_eq!(b_token_requests.len(), 2);
        assert!(String::from_utf8_lossy(&a_token_requests[1].body).contains("a-refresh"));
        assert!(String::from_utf8_lossy(&b_token_requests[1].body).contains("b-refresh"));
        assert!(a_token_requests
            .iter()
            .all(|request| !String::from_utf8_lossy(&request.body).contains("b-refresh")));
        assert!(b_token_requests
            .iter()
            .all(|request| !String::from_utf8_lossy(&request.body).contains("a-refresh")));
    }

    #[tokio::test]
    async fn definitive_refresh_rejection_invalidates_only_the_exact_binding() {
        let first_vault = Arc::new(MemoryVault::default());
        let other_vault = Arc::clone(&first_vault);
        let mut first_responses = discovery_responses("https://auth.example.com");
        first_responses.push(refreshable_token_response(
            "first-access",
            Some("first-refresh"),
            Some("read"),
        ));
        first_responses.extend(discovery_responses("https://auth.example.com"));
        first_responses.push(rejected_refresh_response());
        let first = coordinator(
            first_vault,
            FakeOAuthHttpClient::with_responses(first_responses),
        );
        let first_start = first.begin().await.expect("first begin");
        first_start
            .complete_query(&callback_query(&first_start))
            .await
            .expect("first complete");

        let mut other_responses = discovery_responses("https://other-auth.example.com");
        other_responses.push(refreshable_token_response(
            "other-access",
            Some("other-refresh"),
            Some("read"),
        ));
        let other = McpOAuthCoordinator::new(
            profile("https://other-auth.example.com"),
            other_vault,
            "http://127.0.0.1:3002/api/magician/v2/auth/mcp/oauth/callback",
            McpOAuthClientIdentity::PreRegistered {
                client_id: "registered-client".to_owned(),
                client_secret: None,
            },
        )
        .expect("other coordinator")
        .with_scopes(["read"])
        .expect("other scopes")
        .with_oauth_http_client(Arc::new(FakeOAuthHttpClient::with_responses(
            other_responses,
        )));
        let other_start = other.begin().await.expect("other begin");
        other_start
            .complete_query(&callback_query(&other_start))
            .await
            .expect("other complete");

        let rejected = first.refresh().await.expect_err("refresh rejection");
        assert_eq!(rejected.code, McpOAuthCoordinatorErrorCode::RefreshRejected);
        assert_eq!(
            first.status().await.expect("first status").state,
            AuthState::Missing
        );
        assert_eq!(
            other.status().await.expect("other status").state,
            AuthState::Ready
        );
    }

    #[tokio::test]
    async fn transient_refresh_failure_preserves_the_existing_grant() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.push(refreshable_token_response(
            "canary-access",
            Some("canary-refresh"),
            Some("read"),
        ));
        responses.extend(discovery_responses("https://auth.example.com"));
        responses.push(response(
            503,
            serde_json::json!({"error": "temporarily_unavailable"}),
        ));
        let coordinator = coordinator(vault, FakeOAuthHttpClient::with_responses(responses));
        let start = coordinator.begin().await.expect("begin");
        start
            .complete_query(&callback_query(&start))
            .await
            .expect("complete");

        let error = coordinator.refresh().await.expect_err("transient failure");
        assert_eq!(error.code, McpOAuthCoordinatorErrorCode::RefreshFailed);
        let preserved = coordinator.status().await.expect("preserved status");
        assert_eq!(preserved.state, AuthState::Ready);
        assert_eq!(preserved.granted_scopes, ["read"]);
    }

    #[tokio::test]
    async fn local_logout_reclaims_abandoned_pkce_and_pending_records() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.extend(discovery_responses("https://auth.example.com"));
        let coordinator = coordinator(vault, FakeOAuthHttpClient::with_responses(responses));
        let abandoned = coordinator.begin().await.expect("abandoned begin");
        let stable_redirect = abandoned.route().redirect_uri().to_owned();
        let abandoned_attempt = abandoned.route().attempt_id().to_owned();
        assert_eq!(
            coordinator.status().await.expect("pending").state,
            AuthState::Authenticating
        );

        let audit = coordinator.logout_local().await.expect("local logout");
        assert_eq!(audit.previous_state, AuthState::Authenticating);
        assert_eq!(audit.current_state, AuthState::Missing);
        drop(abandoned);
        let replacement = coordinator.begin().await.expect("fresh begin after logout");
        assert_eq!(replacement.route().redirect_uri(), stable_redirect);
        assert_ne!(replacement.route().attempt_id(), abandoned_attempt);
    }

    #[tokio::test]
    async fn explicit_attempt_cancellation_reclaims_only_pending_authorization_state() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.extend(discovery_responses("https://auth.example.com"));
        let coordinator = coordinator(vault, FakeOAuthHttpClient::with_responses(responses));
        let abandoned = coordinator.begin().await.expect("begin");
        let stable_redirect = abandoned.route().redirect_uri().to_owned();
        let abandoned_attempt = abandoned.route().attempt_id().to_owned();

        abandoned.cancel().await.expect("cancel exact attempt");
        assert_eq!(
            coordinator
                .status()
                .await
                .expect("status after cancel")
                .state,
            AuthState::Missing
        );

        let replacement = coordinator.begin().await.expect("begin after cancellation");
        assert_eq!(replacement.route().redirect_uri(), stable_redirect);
        assert_ne!(replacement.route().attempt_id(), abandoned_attempt);
    }

    #[tokio::test]
    async fn live_attempt_and_status_share_exact_binding_lifecycle_serialization() {
        let vault = Arc::new(MemoryVault::default());
        let coordinator = coordinator(
            vault,
            FakeOAuthHttpClient::with_responses(discovery_responses("https://auth.example.com")),
        );
        let start = coordinator.begin().await.expect("begin");
        let guard = coordinator.lifecycle.lock().await;

        assert!(
            tokio::time::timeout(Duration::from_millis(20), coordinator.status())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), start.cancel())
                .await
                .is_err()
        );

        drop(guard);
        start
            .cancel()
            .await
            .expect("cancel after lifecycle release");
        assert_eq!(
            coordinator
                .status()
                .await
                .expect("status after cancel")
                .state,
            AuthState::Missing
        );
    }

    #[tokio::test]
    async fn scope_upgrade_unions_grants_and_rejects_duplicate_or_malformed_requests() {
        let vault = Arc::new(MemoryVault::default());
        let mut responses = discovery_responses("https://auth.example.com");
        responses.push(refreshable_token_response(
            "canary-access",
            Some("canary-refresh"),
            Some("read"),
        ));
        responses.extend(discovery_responses("https://auth.example.com"));
        responses.push(refreshable_token_response(
            "canary-upgraded-access",
            Some("canary-upgraded-refresh"),
            Some("read write"),
        ));
        let coordinator = coordinator(vault, FakeOAuthHttpClient::with_responses(responses));
        let start = coordinator.begin().await.expect("begin");
        start
            .complete_query(&callback_query(&start))
            .await
            .expect("complete");

        let upgrade = coordinator
            .begin_scope_upgrade(["write"])
            .await
            .expect("scope upgrade");
        assert_eq!(upgrade.audit().previous_state, AuthState::Ready);
        assert_eq!(upgrade.audit().current_state, AuthState::Authenticating);
        let requested = Url::parse(upgrade.authorization().browser_url())
            .expect("upgrade URL")
            .query_pairs()
            .find_map(|(key, value)| (key == "scope").then(|| value.into_owned()))
            .expect("scope query");
        assert!(requested.split_whitespace().any(|scope| scope == "read"));
        assert!(requested.split_whitespace().any(|scope| scope == "write"));
        let query = callback_query(upgrade.authorization());
        upgrade
            .authorization()
            .complete_query(&query)
            .await
            .expect("upgrade complete");
        assert_eq!(
            coordinator.status().await.expect("upgraded").granted_scopes,
            ["read", "write"]
        );
        assert_eq!(
            coordinator
                .begin_scope_upgrade(["write"])
                .await
                .expect_err("no-op upgrade")
                .code,
            McpOAuthCoordinatorErrorCode::ScopeUpgradeNotRequired
        );
        assert_eq!(
            coordinator
                .begin_scope_upgrade(["bad scope"])
                .await
                .expect_err("invalid scope")
                .code,
            McpOAuthCoordinatorErrorCode::InvalidConfiguration
        );
    }
}
