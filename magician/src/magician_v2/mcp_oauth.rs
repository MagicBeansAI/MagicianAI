//! Product-owned MCP OAuth callback, browser-launch, and pending-UX boundary.
//!
//! OAuth protocol mechanics remain in the official SDK through
//! `magician-mcp-client`. This module owns only product concerns: a bounded registry of
//! live browser flows, exact callback routing, safe system-browser launch, restart
//! lookup, and canonical pending/resolved projections. It is wired into the HTTP server
//! but remains dormant until a governed MCP strategy registers a coordinator and begins
//! a flow.

use std::{
    collections::{HashMap, HashSet},
    fmt, mem,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use actix_web::{http::header, web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use magician_mcp_client::{
    McpOAuthAuthorizationStart, McpOAuthCoordinator, McpOAuthCoordinatorError,
    McpOAuthCoordinatorErrorCode, McpOAuthCredentialStatus, McpOAuthLifecycleAudit,
};
use parking_lot::Mutex;
use serde_json::json;
use tokio::{process::Command, sync::OwnedSemaphorePermit};
use tool_runtime_core::credential_profiles::CredentialScope;
use url::{Host, Url};
use zeroize::Zeroizing;

use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

const MAX_LIVE_FLOWS: usize = 256;
const MAX_REGISTERED_COORDINATORS: usize = 1_024;
const MAX_CONSUMED_CALLBACKS: usize = MAX_REGISTERED_COORDINATORS;
const LIVE_FLOW_TTL: Duration = Duration::from_secs(10 * 60);
const BROWSER_LAUNCH_TIMEOUT: Duration = Duration::from_secs(5);
const CALLBACK_BINDING_ID_BYTES: usize = 64;
const CALLBACK_FLOW_ID_BYTES: usize = 32;
const MAX_CALLBACK_QUERY_BYTES: usize = 32 * 1024;
const CALLBACK_SOURCE: &str = "mcp_oauth";

static SHARED_MCP_OAUTH_API: OnceLock<Arc<McpOAuthApi>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpOAuthProductErrorCode {
    InvalidRoute,
    CapacityExceeded,
    PendingAuthorizationConflict,
    AuthorizationNotFound,
    AuthorizationNotLive,
    ScopeMismatch,
    AuthorizationRequired,
    RefreshRejected,
    ScopeUpgradeNotRequired,
    BrowserUnavailable,
    CallbackRejected,
    ProviderUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct McpOAuthProductError {
    pub code: McpOAuthProductErrorCode,
    pub message: &'static str,
}

impl McpOAuthProductError {
    const fn new(code: McpOAuthProductErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }
}

/// Opaque product handle. It deliberately contains no authorization URL, code, token,
/// provider error, or profile alias.
#[derive(Clone, PartialEq, Eq)]
pub struct McpOAuthPendingHandle {
    binding_id: String,
    flow_id: String,
    attempt_id: String,
    browser_opened: bool,
}

impl McpOAuthPendingHandle {
    pub fn browser_opened(&self) -> bool {
        self.browser_opened
    }
}

impl fmt::Debug for McpOAuthPendingHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthPendingHandle")
            .field("binding_id", &self.binding_id)
            .field("flow_id", &self.flow_id)
            .field("attempt_id", &self.attempt_id)
            .field("browser_opened", &self.browser_opened)
            .finish()
    }
}

/// Authorization launch material for a trusted native setup client.
///
/// The URL is deliberately neither serializable nor printable. A setup-token
/// gated API may hand it to Magican Desktop, whose Rust process opens it in the
/// user's browser; it must never enter model, audit, or generic event payloads.
pub struct McpOAuthNativeSetupLaunch {
    pending: McpOAuthPendingHandle,
    authorization_url: Zeroizing<String>,
}

impl McpOAuthNativeSetupLaunch {
    pub fn pending(&self) -> &McpOAuthPendingHandle {
        &self.pending
    }

    pub fn authorization_url(&self) -> &str {
        self.authorization_url.as_str()
    }
}

impl fmt::Debug for McpOAuthNativeSetupLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthNativeSetupLaunch")
            .field("pending", &self.pending)
            .field("authorization_url", &"[REDACTED]")
            .finish()
    }
}

#[async_trait]
trait ProductOAuthLiveFlow: Send + Sync {
    fn binding_id(&self) -> &str;
    fn flow_id(&self) -> &str;
    fn attempt_id(&self) -> &str;
    fn browser_url(&self) -> &str;
    async fn complete_query(&self, query: &str) -> Result<String, CoordinatorCompletionFailure>;
    async fn cancel(&self) -> Result<(), CoordinatorCompletionFailure>;
}

#[async_trait]
trait ProductOAuthCoordinator: Send + Sync {
    fn binding_id(&self) -> String;
    fn scope(&self) -> CredentialScope;
    async fn begin(&self) -> Result<Arc<dyn ProductOAuthLiveFlow>, CoordinatorBeginFailure>;
    async fn begin_scope_upgrade(
        &self,
        required_scopes: Vec<String>,
    ) -> Result<(Arc<dyn ProductOAuthLiveFlow>, McpOAuthLifecycleAudit), CoordinatorLifecycleFailure>;
    async fn status(&self) -> Result<McpOAuthCredentialStatus, CoordinatorLifecycleFailure>;
    async fn refresh(&self) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure>;
    async fn invalidate_definitive_rejection(
        &self,
    ) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure>;
    async fn logout_local(&self) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure>;
    async fn complete_after_restart_query(
        &self,
        binding_id: &str,
        flow_id: &str,
        query: &str,
    ) -> Result<String, CoordinatorCompletionFailure>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinatorBeginFailure {
    Conflict,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinatorCompletionFailure {
    Rejected,
    NotFound,
    Terminal,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinatorLifecycleFailure {
    AuthorizationRequired,
    RefreshRejected,
    NotRequired,
    Unavailable,
}

struct SdkOAuthLiveFlow {
    start: McpOAuthAuthorizationStart,
}

#[async_trait]
impl ProductOAuthLiveFlow for SdkOAuthLiveFlow {
    fn binding_id(&self) -> &str {
        self.start.route().binding_id()
    }

    fn flow_id(&self) -> &str {
        self.start.route().flow_id()
    }

    fn attempt_id(&self) -> &str {
        self.start.route().attempt_id()
    }

    fn browser_url(&self) -> &str {
        self.start.browser_url()
    }

    async fn complete_query(&self, query: &str) -> Result<String, CoordinatorCompletionFailure> {
        self.start
            .complete_query(query)
            .await
            .map(|outcome| outcome.attempt_id)
            .map_err(map_completion_failure)
    }

    async fn cancel(&self) -> Result<(), CoordinatorCompletionFailure> {
        self.start.cancel().await.map_err(map_completion_failure)
    }
}

struct SdkOAuthCoordinator {
    coordinator: Arc<McpOAuthCoordinator>,
    scope: CredentialScope,
}

impl SdkOAuthCoordinator {
    fn new(coordinator: Arc<McpOAuthCoordinator>) -> Self {
        Self {
            scope: coordinator.profile_key().scope.clone(),
            coordinator,
        }
    }
}

#[async_trait]
impl ProductOAuthCoordinator for SdkOAuthCoordinator {
    fn binding_id(&self) -> String {
        self.coordinator.callback_binding_id()
    }

    fn scope(&self) -> CredentialScope {
        self.scope.clone()
    }

    async fn begin(&self) -> Result<Arc<dyn ProductOAuthLiveFlow>, CoordinatorBeginFailure> {
        self.coordinator
            .begin()
            .await
            .map(|start| Arc::new(SdkOAuthLiveFlow { start }) as Arc<dyn ProductOAuthLiveFlow>)
            .map_err(map_begin_failure)
    }

    async fn begin_scope_upgrade(
        &self,
        required_scopes: Vec<String>,
    ) -> Result<(Arc<dyn ProductOAuthLiveFlow>, McpOAuthLifecycleAudit), CoordinatorLifecycleFailure>
    {
        self.coordinator
            .begin_scope_upgrade(required_scopes)
            .await
            .map(|start| {
                let audit = start.audit().clone();
                (
                    Arc::new(SdkOAuthLiveFlow {
                        start: start.into_authorization(),
                    }) as Arc<dyn ProductOAuthLiveFlow>,
                    audit,
                )
            })
            .map_err(map_lifecycle_failure)
    }

    async fn status(&self) -> Result<McpOAuthCredentialStatus, CoordinatorLifecycleFailure> {
        self.coordinator
            .status()
            .await
            .map_err(map_lifecycle_failure)
    }

    async fn refresh(&self) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure> {
        self.coordinator
            .refresh()
            .await
            .map_err(map_lifecycle_failure)
    }

    async fn invalidate_definitive_rejection(
        &self,
    ) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure> {
        self.coordinator
            .invalidate_definitive_rejection()
            .await
            .map_err(map_lifecycle_failure)
    }

    async fn logout_local(&self) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure> {
        self.coordinator
            .logout_local()
            .await
            .map_err(map_lifecycle_failure)
    }

    async fn complete_after_restart_query(
        &self,
        binding_id: &str,
        flow_id: &str,
        query: &str,
    ) -> Result<String, CoordinatorCompletionFailure> {
        self.coordinator
            .complete_after_restart_query(binding_id, flow_id, query)
            .await
            .map(|outcome| outcome.attempt_id)
            .map_err(map_completion_failure)
    }
}

#[async_trait]
trait McpOAuthBrowserLauncher: Send + Sync {
    async fn open(&self, authorization_url: &str) -> Result<(), McpOAuthProductError>;
}

#[derive(Debug, Default)]
struct SystemBrowserLauncher;

#[async_trait]
impl McpOAuthBrowserLauncher for SystemBrowserLauncher {
    async fn open(&self, authorization_url: &str) -> Result<(), McpOAuthProductError> {
        let mut command = {
            #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
            {
                system_browser_command(authorization_url)
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
            {
                let _ = authorization_url;
                return Err(browser_unavailable());
            }
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let status = tokio::time::timeout(BROWSER_LAUNCH_TIMEOUT, command.status())
            .await
            .map_err(|_| browser_unavailable())?
            .map_err(|_| browser_unavailable())?;
        if status.success() {
            Ok(())
        } else {
            Err(browser_unavailable())
        }
    }
}

#[cfg(target_os = "macos")]
fn system_browser_command(authorization_url: &str) -> Command {
    let mut command = Command::new("open");
    command.arg(authorization_url);
    command
}

#[cfg(target_os = "linux")]
fn system_browser_command(authorization_url: &str) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(authorization_url);
    command
}

#[cfg(target_os = "windows")]
fn system_browser_command(authorization_url: &str) -> Command {
    let mut command = Command::new("rundll32.exe");
    command
        .arg("url.dll,FileProtocolHandler")
        .arg(authorization_url);
    command
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RouteKey {
    binding_id: String,
    flow_id: String,
}

struct CorrelationKey {
    route: RouteKey,
    attempt_id: String,
}

struct RegisteredCoordinator {
    coordinator: Arc<dyn ProductOAuthCoordinator>,
    scope: CredentialScope,
}

struct LiveFlow {
    flow: Arc<dyn ProductOAuthLiveFlow>,
    scope: CredentialScope,
    expires_at: Instant,
    _capacity: OwnedSemaphorePermit,
}

#[derive(Default)]
struct RegistryState {
    coordinators: HashMap<String, RegisteredCoordinator>,
    starting_bindings: HashSet<String>,
    lifecycle_mutations: HashSet<String>,
    callback_inflight: HashSet<RouteKey>,
    consumed_callbacks: HashMap<RouteKey, Instant>,
    live_flows: HashMap<RouteKey, Arc<LiveFlow>>,
}

struct BeginReservation {
    registry: Arc<Mutex<RegistryState>>,
    binding_id: String,
    registration_rollback: RegistrationRollback,
    active: bool,
}

enum RegistrationRollback {
    None,
    Remove,
    Restore(RegisteredCoordinator),
}

impl BeginReservation {
    fn commit(mut self, key: RouteKey, flow: Arc<LiveFlow>) -> Result<(), McpOAuthProductError> {
        let mut state = self.registry.lock();
        if !state.starting_bindings.remove(&self.binding_id)
            || key.binding_id != self.binding_id
            || state.live_flows.contains_key(&key)
        {
            return Err(provider_unavailable());
        }
        // Callback route ids are stable for an exact binding. A newly admitted flow
        // therefore supersedes the bounded replay tombstone left by its predecessor.
        state.consumed_callbacks.remove(&key);
        state.live_flows.insert(key, flow);
        self.registration_rollback = RegistrationRollback::None;
        self.active = false;
        Ok(())
    }
}

impl Drop for BeginReservation {
    fn drop(&mut self) {
        if self.active {
            let mut state = self.registry.lock();
            state.starting_bindings.remove(&self.binding_id);
            match mem::replace(&mut self.registration_rollback, RegistrationRollback::None) {
                RegistrationRollback::None => {},
                RegistrationRollback::Remove => {
                    state.coordinators.remove(&self.binding_id);
                },
                RegistrationRollback::Restore(previous) => {
                    state.coordinators.insert(self.binding_id.clone(), previous);
                },
            }
        }
    }
}

struct CallbackReservation {
    registry: Arc<Mutex<RegistryState>>,
    key: RouteKey,
}

struct LifecycleReservation {
    registry: Arc<Mutex<RegistryState>>,
    binding_id: String,
}

impl Drop for LifecycleReservation {
    fn drop(&mut self) {
        self.registry
            .lock()
            .lifecycle_mutations
            .remove(&self.binding_id);
    }
}

impl Drop for CallbackReservation {
    fn drop(&mut self) {
        self.registry.lock().callback_inflight.remove(&self.key);
    }
}

enum CallbackTarget {
    Live(Arc<LiveFlow>),
    Restart(Arc<dyn ProductOAuthCoordinator>, CredentialScope),
}

/// Shared product callback broker. One instance is installed as Actix app data so every
/// worker sees the same bounded registry and callback replay state.
pub struct McpOAuthApi {
    registry: Arc<Mutex<RegistryState>>,
    capacity: Arc<tokio::sync::Semaphore>,
    launcher: Arc<dyn McpOAuthBrowserLauncher>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    live_flow_ttl: Duration,
    callback_base_url: String,
}

impl McpOAuthApi {
    pub fn new(broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        Self::with_dependencies(
            broadcaster,
            Arc::new(SystemBrowserLauncher),
            MAX_LIVE_FLOWS,
            LIVE_FLOW_TTL,
        )
    }

    /// Construct the callback broker for the HTTP port selected by the running
    /// Magician process, using its loopback callback origin.
    pub fn new_with_callback_port(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        port: u16,
    ) -> Self {
        Self::new_with_callback_origin(broadcaster, port, None)
            .expect("the loopback MCP OAuth callback origin is valid")
    }

    /// Construct the callback broker for this process and, when configured,
    /// use the server's reviewed public origin. Native Desktop setup can then
    /// open the provider on a different machine while the callback and token
    /// remain owned by the remote Magician engine.
    pub fn new_with_callback_origin(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        port: u16,
        public_origin: Option<&str>,
    ) -> Result<Self, McpOAuthProductError> {
        let origin = match public_origin
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(origin) => validate_callback_origin(origin)?,
            None => format!("http://127.0.0.1:{port}"),
        };
        let mut api = Self::new(broadcaster);
        api.callback_base_url = format!("{origin}/api/magician/v2/auth/mcp/oauth/callback");
        Ok(api)
    }

    /// Exact callback base passed to official-SDK OAuth coordinators.
    pub fn callback_base_url(&self) -> &str {
        &self.callback_base_url
    }

    /// Install the process callback broker used by governed MCP dispatch.
    /// Installation is single-assignment so a later caller cannot replace live
    /// OAuth flow authority with a different registry.
    pub fn install_shared(api: Arc<Self>) -> Result<(), Arc<Self>> {
        SHARED_MCP_OAUTH_API.set(api)
    }

    /// Obtain the already-installed process callback broker. Runtime dispatch
    /// fails closed when boot has not installed it.
    pub fn shared() -> Option<Arc<Self>> {
        SHARED_MCP_OAUTH_API.get().cloned()
    }

    fn with_dependencies(
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        launcher: Arc<dyn McpOAuthBrowserLauncher>,
        max_live_flows: usize,
        live_flow_ttl: Duration,
    ) -> Self {
        Self {
            registry: Arc::new(Mutex::new(RegistryState::default())),
            capacity: Arc::new(tokio::sync::Semaphore::new(max_live_flows)),
            launcher,
            broadcaster,
            live_flow_ttl,
            callback_base_url: "http://127.0.0.1:3002/api/magician/v2/auth/mcp/oauth/callback"
                .to_owned(),
        }
    }

    /// Register the exact SDK coordinator and begin an authorization flow. Product
    /// integration must call this only after governed strategy/profile resolution; no
    /// request or model field may construct the coordinator.
    pub async fn register_and_begin(
        &self,
        coordinator: Arc<McpOAuthCoordinator>,
    ) -> Result<McpOAuthPendingHandle, McpOAuthProductError> {
        self.begin_with(Arc::new(SdkOAuthCoordinator::new(coordinator)))
            .await
    }

    /// Begin an authorization flow without opening a browser on the engine.
    /// The returned URL is restricted to trusted native setup code so a remote
    /// or containerized Magician can keep OAuth custody while Desktop opens the
    /// interactive page on the operator's machine.
    pub async fn register_and_begin_for_native_setup(
        &self,
        coordinator: Arc<McpOAuthCoordinator>,
    ) -> Result<McpOAuthNativeSetupLaunch, McpOAuthProductError> {
        let (pending, authorization_url) = self
            .begin_with_mode(Arc::new(SdkOAuthCoordinator::new(coordinator)), false)
            .await?;
        Ok(McpOAuthNativeSetupLaunch {
            pending,
            authorization_url: authorization_url.ok_or_else(provider_unavailable)?,
        })
    }

    /// Register an exact coordinator without starting a new flow. Profile-catalog
    /// startup uses this path so a callback can resume durable SDK state after a
    /// process restart. Registration is rejected while the same binding is starting or
    /// live, preventing a configuration replacement from changing an active exchange.
    pub fn register_coordinator(
        &self,
        coordinator: Arc<McpOAuthCoordinator>,
    ) -> Result<(), McpOAuthProductError> {
        self.register_with(Arc::new(SdkOAuthCoordinator::new(coordinator)))
    }

    fn register_with(
        &self,
        coordinator: Arc<dyn ProductOAuthCoordinator>,
    ) -> Result<(), McpOAuthProductError> {
        self.sweep_expired();
        let binding_id = coordinator.binding_id();
        validate_route_component(&binding_id, CALLBACK_BINDING_ID_BYTES)?;
        let scope = coordinator.scope();
        let mut state = self.registry.lock();
        if state.starting_bindings.contains(&binding_id)
            || state.lifecycle_mutations.contains(&binding_id)
            || state
                .live_flows
                .keys()
                .any(|key| key.binding_id == binding_id)
            || state
                .callback_inflight
                .iter()
                .any(|key| key.binding_id == binding_id)
        {
            return Err(pending_conflict());
        }
        if !state.coordinators.contains_key(&binding_id)
            && state.coordinators.len() >= MAX_REGISTERED_COORDINATORS
        {
            return Err(capacity_exceeded());
        }
        if state
            .coordinators
            .get(&binding_id)
            .is_some_and(|existing| existing.scope != scope)
        {
            return Err(scope_mismatch());
        }
        state
            .coordinators
            .insert(binding_id, RegisteredCoordinator { coordinator, scope });
        Ok(())
    }

    /// Remove one exact coordinator when its governed catalog/profile binding is
    /// removed. The operation is idempotent, but fails closed while any start,
    /// callback, live flow, or lifecycle mutation still owns that authority.
    pub fn unregister_coordinator(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<bool, McpOAuthProductError> {
        validate_route_component(binding_id, CALLBACK_BINDING_ID_BYTES)?;
        self.sweep_expired();
        let mut state = self.registry.lock();
        let Some(existing) = state.coordinators.get(binding_id) else {
            return Ok(false);
        };
        if existing.scope != *scope {
            return Err(scope_mismatch());
        }
        if state.starting_bindings.contains(binding_id)
            || state.lifecycle_mutations.contains(binding_id)
            || state
                .live_flows
                .keys()
                .any(|key| key.binding_id == binding_id)
            || state
                .callback_inflight
                .iter()
                .any(|key| key.binding_id == binding_id)
        {
            return Err(pending_conflict());
        }
        Ok(state.coordinators.remove(binding_id).is_some())
    }

    /// Observe a registered exact binding. This is a secret-free, non-authorizing
    /// snapshot; dispatch must still obtain a fresh access token from the coordinator.
    pub async fn credential_status(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<McpOAuthCredentialStatus, McpOAuthProductError> {
        let coordinator = self.registered_for_scope(binding_id, scope)?;
        coordinator
            .status()
            .await
            .map_err(map_product_lifecycle_failure)
    }

    /// Single-flight explicit refresh for one exact binding.
    pub async fn refresh_binding(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<McpOAuthLifecycleAudit, McpOAuthProductError> {
        let (coordinator, reservation) = self.reserve_lifecycle(binding_id, scope)?;
        let binding_id = binding_id.to_owned();
        let scope = scope.clone();
        tokio::spawn(async move {
            let _reservation = reservation;
            match coordinator.refresh().await {
                Ok(audit) => {
                    record_lifecycle_audit(&binding_id, &scope, &audit);
                    Ok(audit)
                },
                Err(error) => {
                    record_lifecycle_failure(&binding_id, &scope, "refresh", error);
                    Err(map_product_lifecycle_failure(error))
                },
            }
        })
        .await
        .map_err(|_| provider_unavailable())?
    }

    /// Clear a credential only after a trusted MCP response classifier has established
    /// definitive rejection. Transient transport/provider failures must not call this.
    pub async fn invalidate_definitive_rejection(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<McpOAuthLifecycleAudit, McpOAuthProductError> {
        let (coordinator, reservation) = self.reserve_lifecycle(binding_id, scope)?;
        let binding_id = binding_id.to_owned();
        let scope = scope.clone();
        tokio::spawn(async move {
            let _reservation = reservation;
            match coordinator.invalidate_definitive_rejection().await {
                Ok(audit) => {
                    record_lifecycle_audit(&binding_id, &scope, &audit);
                    Ok(audit)
                },
                Err(error) => {
                    record_lifecycle_failure(
                        &binding_id,
                        &scope,
                        "definitive_rejection_invalidation",
                        error,
                    );
                    Err(map_product_lifecycle_failure(error))
                },
            }
        })
        .await
        .map_err(|_| provider_unavailable())?
    }

    /// Idempotent local logout. This removes exact-binding credentials and abandoned
    /// callback state; it does not claim remote provider revocation.
    pub async fn logout_local(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<McpOAuthLifecycleAudit, McpOAuthProductError> {
        let (coordinator, reservation) = self.reserve_lifecycle(binding_id, scope)?;
        let binding_id = binding_id.to_owned();
        let scope = scope.clone();
        tokio::spawn(async move {
            let _reservation = reservation;
            match coordinator.logout_local().await {
                Ok(audit) => {
                    record_lifecycle_audit(&binding_id, &scope, &audit);
                    Ok(audit)
                },
                Err(error) => {
                    record_lifecycle_failure(&binding_id, &scope, "local_logout", error);
                    Err(map_product_lifecycle_failure(error))
                },
            }
        })
        .await
        .map_err(|_| provider_unavailable())?
    }

    /// Begin an explicit SDK-owned scope upgrade for a registered exact binding.
    pub async fn begin_scope_upgrade(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
        required_scopes: Vec<String>,
    ) -> Result<(McpOAuthPendingHandle, McpOAuthLifecycleAudit), McpOAuthProductError> {
        self.sweep_expired();
        let capacity = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| capacity_exceeded())?;
        let (coordinator, reservation) = self.reserve_registered_begin(binding_id, scope)?;
        let (flow, audit) = match coordinator.begin_scope_upgrade(required_scopes).await {
            Ok(start) => start,
            Err(error) => {
                record_lifecycle_failure(binding_id, scope, "scope_upgrade", error);
                return Err(map_product_lifecycle_failure(error));
            },
        };
        if flow.binding_id() != binding_id {
            return Err(invalid_route());
        }
        validate_route_component(flow.flow_id(), CALLBACK_FLOW_ID_BYTES)?;
        let flow_id = flow.flow_id().to_owned();
        validate_route_component(flow.attempt_id(), CALLBACK_FLOW_ID_BYTES)?;
        let attempt_id = flow.attempt_id().to_owned();
        let key = RouteKey {
            binding_id: binding_id.to_owned(),
            flow_id: flow_id.clone(),
        };
        let live = Arc::new(LiveFlow {
            flow,
            scope: scope.clone(),
            expires_at: Instant::now() + self.live_flow_ttl,
            _capacity: capacity,
        });
        reservation.commit(key, Arc::clone(&live))?;
        self.emit_pending(binding_id, &flow_id, &attempt_id, scope, false);
        record_lifecycle_audit(binding_id, scope, &audit);
        let browser_opened = match self.launcher.open(live.flow.browser_url()).await {
            Ok(()) => true,
            Err(_) => {
                self.emit_pending(binding_id, &flow_id, &attempt_id, scope, true);
                false
            },
        };
        Ok((
            McpOAuthPendingHandle {
                binding_id: binding_id.to_owned(),
                flow_id,
                attempt_id,
                browser_opened,
            },
            audit,
        ))
    }

    async fn begin_with(
        &self,
        coordinator: Arc<dyn ProductOAuthCoordinator>,
    ) -> Result<McpOAuthPendingHandle, McpOAuthProductError> {
        self.begin_with_mode(coordinator, true)
            .await
            .map(|(pending, _)| pending)
    }

    async fn begin_with_mode(
        &self,
        coordinator: Arc<dyn ProductOAuthCoordinator>,
        launch_browser: bool,
    ) -> Result<(McpOAuthPendingHandle, Option<Zeroizing<String>>), McpOAuthProductError> {
        self.sweep_expired();
        let binding_id = coordinator.binding_id();
        validate_route_component(&binding_id, CALLBACK_BINDING_ID_BYTES)?;
        let scope = coordinator.scope();
        let capacity = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| capacity_exceeded())?;
        let reservation = {
            let mut state = self.registry.lock();
            if state.starting_bindings.contains(&binding_id)
                || state.lifecycle_mutations.contains(&binding_id)
                || state
                    .live_flows
                    .keys()
                    .any(|key| key.binding_id == binding_id)
                || state
                    .callback_inflight
                    .iter()
                    .any(|key| key.binding_id == binding_id)
            {
                return Err(pending_conflict());
            }
            if !state.coordinators.contains_key(&binding_id)
                && state.coordinators.len() >= MAX_REGISTERED_COORDINATORS
            {
                return Err(capacity_exceeded());
            }
            let previous = state.coordinators.insert(
                binding_id.clone(),
                RegisteredCoordinator {
                    coordinator: Arc::clone(&coordinator),
                    scope: scope.clone(),
                },
            );
            state.starting_bindings.insert(binding_id.clone());
            BeginReservation {
                registry: Arc::clone(&self.registry),
                binding_id: binding_id.clone(),
                registration_rollback: previous
                    .map(RegistrationRollback::Restore)
                    .unwrap_or(RegistrationRollback::Remove),
                active: true,
            }
        };

        let flow = coordinator
            .begin()
            .await
            .map_err(map_product_begin_failure)?;
        if flow.binding_id() != binding_id {
            return Err(invalid_route());
        }
        validate_route_component(flow.flow_id(), CALLBACK_FLOW_ID_BYTES)?;
        let flow_id = flow.flow_id().to_owned();
        validate_route_component(flow.attempt_id(), CALLBACK_FLOW_ID_BYTES)?;
        let attempt_id = flow.attempt_id().to_owned();
        let key = RouteKey {
            binding_id: binding_id.clone(),
            flow_id: flow_id.clone(),
        };
        let live = Arc::new(LiveFlow {
            flow,
            scope: scope.clone(),
            expires_at: Instant::now() + self.live_flow_ttl,
            _capacity: capacity,
        });
        let authorization_url =
            (!launch_browser).then(|| Zeroizing::new(live.flow.browser_url().to_owned()));
        reservation.commit(key, Arc::clone(&live))?;
        // The scoped HITL lifecycle admits one request per correlation id and
        // refuses any later request carrying different content for the same
        // key, so a second publication describing the launch failure would be
        // discarded before it reached a subscriber. Settle the launch outcome
        // first and let the single admitted request state it.
        let browser_opened = if launch_browser {
            self.launcher.open(live.flow.browser_url()).await.is_ok()
        } else {
            false
        };
        self.emit_pending(
            &binding_id,
            &flow_id,
            &attempt_id,
            &scope,
            launch_browser && !browser_opened,
        );
        Ok((
            McpOAuthPendingHandle {
                binding_id,
                flow_id,
                attempt_id,
                browser_opened,
            },
            authorization_url,
        ))
    }

    fn registered_for_scope(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<Arc<dyn ProductOAuthCoordinator>, McpOAuthProductError> {
        validate_route_component(binding_id, CALLBACK_BINDING_ID_BYTES)?;
        let state = self.registry.lock();
        let entry = state
            .coordinators
            .get(binding_id)
            .ok_or_else(authorization_not_found)?;
        if entry.scope != *scope {
            return Err(scope_mismatch());
        }
        Ok(Arc::clone(&entry.coordinator))
    }

    fn reserve_registered_begin(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<(Arc<dyn ProductOAuthCoordinator>, BeginReservation), McpOAuthProductError> {
        validate_route_component(binding_id, CALLBACK_BINDING_ID_BYTES)?;
        let mut state = self.registry.lock();
        let entry = state
            .coordinators
            .get(binding_id)
            .ok_or_else(authorization_not_found)?;
        if entry.scope != *scope {
            return Err(scope_mismatch());
        }
        let coordinator = Arc::clone(&entry.coordinator);
        if state.starting_bindings.contains(binding_id)
            || state.lifecycle_mutations.contains(binding_id)
            || state
                .live_flows
                .keys()
                .any(|key| key.binding_id == binding_id)
            || state
                .callback_inflight
                .iter()
                .any(|key| key.binding_id == binding_id)
        {
            return Err(pending_conflict());
        }
        state.starting_bindings.insert(binding_id.to_owned());
        Ok((
            coordinator,
            BeginReservation {
                registry: Arc::clone(&self.registry),
                binding_id: binding_id.to_owned(),
                registration_rollback: RegistrationRollback::None,
                active: true,
            },
        ))
    }

    fn reserve_lifecycle(
        &self,
        binding_id: &str,
        scope: &CredentialScope,
    ) -> Result<(Arc<dyn ProductOAuthCoordinator>, LifecycleReservation), McpOAuthProductError>
    {
        validate_route_component(binding_id, CALLBACK_BINDING_ID_BYTES)?;
        self.sweep_expired();
        let mut state = self.registry.lock();
        let entry = state
            .coordinators
            .get(binding_id)
            .ok_or_else(authorization_not_found)?;
        if entry.scope != *scope {
            return Err(scope_mismatch());
        }
        let coordinator = Arc::clone(&entry.coordinator);
        if state.starting_bindings.contains(binding_id)
            || state.lifecycle_mutations.contains(binding_id)
            || state
                .live_flows
                .keys()
                .any(|key| key.binding_id == binding_id)
            || state
                .callback_inflight
                .iter()
                .any(|key| key.binding_id == binding_id)
        {
            return Err(pending_conflict());
        }
        state.lifecycle_mutations.insert(binding_id.to_owned());
        Ok((
            coordinator,
            LifecycleReservation {
                registry: Arc::clone(&self.registry),
                binding_id: binding_id.to_owned(),
            },
        ))
    }

    async fn complete_callback(
        &self,
        binding_id: &str,
        flow_id: &str,
        query: &str,
    ) -> Result<(), McpOAuthProductError> {
        validate_route_component(binding_id, CALLBACK_BINDING_ID_BYTES)?;
        validate_route_component(flow_id, CALLBACK_FLOW_ID_BYTES)?;
        validate_callback_query(query)?;
        let key = RouteKey {
            binding_id: binding_id.to_owned(),
            flow_id: flow_id.to_owned(),
        };
        if self.is_callback_consumed(&key) {
            return Err(authorization_not_found());
        }
        self.sweep_expired();
        let target = {
            let mut state = self.registry.lock();
            if state.callback_inflight.contains(&key) {
                return Err(callback_rejected());
            }
            if state.lifecycle_mutations.contains(binding_id) {
                return Err(pending_conflict());
            }
            let target = if let Some(live) = state.live_flows.get(&key).cloned() {
                CallbackTarget::Live(live)
            } else if let Some(entry) = state.coordinators.get(binding_id) {
                CallbackTarget::Restart(Arc::clone(&entry.coordinator), entry.scope.clone())
            } else {
                return Err(authorization_not_found());
            };
            if state.callback_inflight.len() >= MAX_LIVE_FLOWS {
                return Err(capacity_exceeded());
            }
            state.callback_inflight.insert(key.clone());
            target
        };
        let _reservation = CallbackReservation {
            registry: Arc::clone(&self.registry),
            key: key.clone(),
        };
        match target {
            CallbackTarget::Live(live) => {
                let expected_attempt = live.flow.attempt_id().to_owned();
                let result = live
                    .flow
                    .complete_query(query)
                    .await
                    .and_then(|attempt_id| {
                        if attempt_id == expected_attempt {
                            Ok(attempt_id)
                        } else {
                            Err(CoordinatorCompletionFailure::Terminal)
                        }
                    });
                self.finish_completion(
                    key,
                    live.scope.clone(),
                    Some(&expected_attempt),
                    result,
                    true,
                )
            },
            CallbackTarget::Restart(coordinator, scope) => {
                let result = coordinator
                    .complete_after_restart_query(binding_id, flow_id, query)
                    .await;
                self.finish_completion(key, scope, None, result, false)
            },
        }
    }

    pub async fn retry_browser_for_correlation(
        &self,
        correlation_id: &str,
        scope: &CredentialScope,
    ) -> Result<(), McpOAuthProductError> {
        let correlation = parse_correlation_id(correlation_id)?;
        let key = &correlation.route;
        self.sweep_expired();
        let live = {
            let state = self.registry.lock();
            if state.callback_inflight.contains(key) {
                return Err(pending_conflict());
            }
            state
                .live_flows
                .get(key)
                .cloned()
                .ok_or_else(authorization_not_live)?
        };
        if live.scope != *scope {
            return Err(scope_mismatch());
        }
        if live.flow.attempt_id() != correlation.attempt_id {
            return Err(authorization_not_live());
        }
        self.launcher.open(live.flow.browser_url()).await
    }

    pub async fn dismiss_correlation(
        &self,
        correlation_id: &str,
        scope: &CredentialScope,
    ) -> Result<(), McpOAuthProductError> {
        let correlation = parse_correlation_id(correlation_id)?;
        let key = correlation.route;
        self.sweep_expired();
        let live = {
            let mut state = self.registry.lock();
            if state.callback_inflight.contains(&key) {
                return Err(pending_conflict());
            }
            let live = state
                .live_flows
                .get(&key)
                .cloned()
                .ok_or_else(authorization_not_found)?;
            if live.scope != *scope {
                return Err(scope_mismatch());
            }
            if live.flow.attempt_id() != correlation.attempt_id {
                return Err(authorization_not_live());
            }
            state.callback_inflight.insert(key.clone());
            live
        };
        let _reservation = CallbackReservation {
            registry: Arc::clone(&self.registry),
            key: key.clone(),
        };
        match live.flow.cancel().await {
            Ok(()) | Err(CoordinatorCompletionFailure::NotFound) => {},
            Err(CoordinatorCompletionFailure::Rejected)
            | Err(CoordinatorCompletionFailure::Terminal) => return Err(callback_rejected()),
            Err(CoordinatorCompletionFailure::Unavailable) => {
                return Err(provider_unavailable());
            },
        }
        let removed = self.registry.lock().live_flows.remove(&key);
        drop(removed);
        self.emit_resolved(
            &key,
            &correlation.attempt_id,
            scope,
            "dismissed",
            Some("dismissed"),
        );
        Ok(())
    }

    fn finish_completion(
        &self,
        key: RouteKey,
        scope: CredentialScope,
        known_attempt_id: Option<&str>,
        result: Result<String, CoordinatorCompletionFailure>,
        was_live: bool,
    ) -> Result<(), McpOAuthProductError> {
        match result {
            Ok(attempt_id) => {
                validate_route_component(&attempt_id, CALLBACK_FLOW_ID_BYTES)?;
                let removed = {
                    let mut state = self.registry.lock();
                    let removed = state.live_flows.remove(&key);
                    remember_consumed_callback(
                        &mut state.consumed_callbacks,
                        key.clone(),
                        Instant::now() + self.live_flow_ttl,
                    );
                    removed
                };
                drop(removed);
                if !was_live {
                    // A restart-completed flow published its request from the
                    // process that began it, so this process holds no pending
                    // lifecycle record and the resolution below would be
                    // refused as unrequested. Restore the request first.
                    self.emit_pending(&key.binding_id, &key.flow_id, &attempt_id, &scope, false);
                }
                self.emit_resolved(
                    &key,
                    &attempt_id,
                    &scope,
                    "responded",
                    Some("authenticated"),
                );
                Ok(())
            },
            Err(CoordinatorCompletionFailure::Rejected) => Err(callback_rejected()),
            Err(CoordinatorCompletionFailure::Unavailable) => Err(provider_unavailable()),
            Err(CoordinatorCompletionFailure::NotFound) => {
                if was_live {
                    let removed = self.registry.lock().live_flows.remove(&key);
                    drop(removed);
                    if let Some(attempt_id) = known_attempt_id {
                        self.emit_resolved(&key, attempt_id, &scope, "expired", None);
                    }
                }
                Err(authorization_not_found())
            },
            Err(CoordinatorCompletionFailure::Terminal) => {
                let removed = self.registry.lock().live_flows.remove(&key);
                drop(removed);
                if let Some(attempt_id) = known_attempt_id {
                    self.emit_resolved(&key, attempt_id, &scope, "cancelled", Some("failed"));
                }
                Err(callback_rejected())
            },
        }
    }

    fn is_callback_consumed(&self, key: &RouteKey) -> bool {
        self.registry.lock().consumed_callbacks.contains_key(key)
    }

    fn sweep_expired(&self) {
        let expired = {
            let now = Instant::now();
            let mut state = self.registry.lock();
            state
                .consumed_callbacks
                .retain(|_, expires_at| *expires_at > now);
            let keys = state
                .live_flows
                .iter()
                .filter(|(key, flow)| {
                    flow.expires_at <= now && !state.callback_inflight.contains(*key)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| state.live_flows.remove(&key).map(|flow| (key, flow)))
                .collect::<Vec<_>>()
        };
        for (key, flow) in expired {
            self.emit_resolved(&key, flow.flow.attempt_id(), &flow.scope, "expired", None);
            drop(flow);
        }
    }

    fn emit_pending(
        &self,
        binding_id: &str,
        flow_id: &str,
        attempt_id: &str,
        scope: &CredentialScope,
        browser_launch_failed: bool,
    ) {
        let hint = if browser_launch_failed {
            "The browser could not be opened automatically. Open this request to try again."
        } else {
            "This request closes automatically when sign-in succeeds."
        };
        self.broadcaster.emit(RuntimeTransportEvent::HitlRequested {
            correlation_id: correlation_id(binding_id, flow_id, attempt_id),
            source: CALLBACK_SOURCE.to_owned(),
            input_type: "external_action".to_owned(),
            prompt: "Finish connecting this service in your browser.".to_owned(),
            hint: Some(hint.to_owned()),
            input_schema: Some(json!({
                "instructions": "Complete sign-in in the browser window. If it did not open, use the button below to try again.",
                "done_label": "Open sign-in again"
            })),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(scope.principal.as_str().to_owned()),
            workspace: Some(scope.workspace.as_str().to_owned()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }

    fn emit_resolved(
        &self,
        key: &RouteKey,
        attempt_id: &str,
        scope: &CredentialScope,
        outcome: &str,
        decision: Option<&str>,
    ) {
        self.broadcaster.emit(RuntimeTransportEvent::HitlResolved {
            correlation_id: correlation_id(&key.binding_id, &key.flow_id, attempt_id),
            source: CALLBACK_SOURCE.to_owned(),
            outcome: outcome.to_owned(),
            decision: decision.map(str::to_owned),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(scope.principal.as_str().to_owned()),
            workspace: Some(scope.workspace.as_str().to_owned()),
            timestamp: chrono::Utc::now().timestamp_millis(),
        });
    }
}

fn record_lifecycle_audit(
    binding_id: &str,
    scope: &CredentialScope,
    audit: &McpOAuthLifecycleAudit,
) {
    tracing::info!(
        target: "mcp_oauth_lifecycle",
        binding_id,
        principal = scope.principal.as_str(),
        workspace = scope.workspace.as_str(),
        operation = ?audit.operation,
        previous_state = ?audit.previous_state,
        current_state = ?audit.current_state,
        granted_scope_count = audit.granted_scope_count,
        refresh_available = audit.refresh_available,
        "MCP OAuth lifecycle mutation completed"
    );
}

fn record_lifecycle_failure(
    binding_id: &str,
    scope: &CredentialScope,
    operation: &'static str,
    error: CoordinatorLifecycleFailure,
) {
    tracing::warn!(
        target: "mcp_oauth_lifecycle",
        binding_id,
        principal = scope.principal.as_str(),
        workspace = scope.workspace.as_str(),
        operation,
        outcome = ?error,
        "MCP OAuth lifecycle mutation did not complete"
    );
}

pub fn configure_routes(config: &mut web::ServiceConfig) {
    config.route(
        "/auth/mcp/oauth/callback/{binding_id}/{flow_id}",
        web::get().to(mcp_oauth_callback_handler),
    );
}

async fn mcp_oauth_callback_handler(
    api: web::Data<McpOAuthApi>,
    request: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let (binding_id, flow_id) = path.into_inner();
    let result = api
        .complete_callback(&binding_id, &flow_id, request.query_string())
        .await;
    match result {
        Ok(()) => callback_page(
            actix_web::http::StatusCode::OK,
            "Sign-in complete",
            "You can close this window and return to Magican.",
        ),
        Err(error) if error.code == McpOAuthProductErrorCode::AuthorizationNotFound => {
            callback_page(
                actix_web::http::StatusCode::NOT_FOUND,
                "Sign-in link expired",
                "Return to Magican and start sign-in again.",
            )
        },
        Err(error) if error.code == McpOAuthProductErrorCode::ProviderUnavailable => callback_page(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "Sign-in could not finish",
            "Return to Magican and try again in a moment.",
        ),
        Err(_) => callback_page(
            actix_web::http::StatusCode::BAD_REQUEST,
            "Sign-in could not finish",
            "Return to Magican and start sign-in again.",
        ),
    }
}

fn remember_consumed_callback(
    callbacks: &mut HashMap<RouteKey, Instant>,
    key: RouteKey,
    expires_at: Instant,
) {
    if !callbacks.contains_key(&key) && callbacks.len() >= MAX_CONSUMED_CALLBACKS {
        let oldest = callbacks
            .iter()
            .min_by_key(|(_, expires_at)| **expires_at)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            callbacks.remove(&oldest);
        }
    }
    callbacks.insert(key, expires_at);
}

fn callback_page(
    status: actix_web::http::StatusCode,
    title: &'static str,
    message: &'static str,
) -> HttpResponse {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>html{{color-scheme:light dark}}body{{font:16px system-ui;display:grid;min-height:100vh;place-items:center;margin:0}}main{{max-width:34rem;padding:2rem;text-align:center}}h1{{font-size:1.5rem}}</style></head><body><main><h1>{title}</h1><p>{message}</p></main></body></html>"
    );
    HttpResponse::build(status)
        .insert_header((header::CONTENT_TYPE, "text/html; charset=utf-8"))
        .insert_header((header::CACHE_CONTROL, "no-store, max-age=0"))
        .insert_header((header::REFERRER_POLICY, "no-referrer"))
        .insert_header((header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .insert_header((
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
        ))
        .body(body)
}

fn correlation_id(binding_id: &str, flow_id: &str, attempt_id: &str) -> String {
    format!("{CALLBACK_SOURCE}:{binding_id}:{flow_id}:{attempt_id}")
}

fn parse_correlation_id(value: &str) -> Result<CorrelationKey, McpOAuthProductError> {
    let Some(route) = value.strip_prefix("mcp_oauth:") else {
        return Err(invalid_route());
    };
    let mut components = route.split(':');
    let (Some(binding_id), Some(flow_id), Some(attempt_id), None) = (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) else {
        return Err(invalid_route());
    };
    validate_route_component(binding_id, CALLBACK_BINDING_ID_BYTES)?;
    validate_route_component(flow_id, CALLBACK_FLOW_ID_BYTES)?;
    validate_route_component(attempt_id, CALLBACK_FLOW_ID_BYTES)?;
    Ok(CorrelationKey {
        route: RouteKey {
            binding_id: binding_id.to_owned(),
            flow_id: flow_id.to_owned(),
        },
        attempt_id: attempt_id.to_owned(),
    })
}

fn validate_route_component(value: &str, expected_len: usize) -> Result<(), McpOAuthProductError> {
    if value.len() != expected_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_route());
    }
    Ok(())
}

fn validate_callback_query(value: &str) -> Result<(), McpOAuthProductError> {
    if value.is_empty()
        || value.len() > MAX_CALLBACK_QUERY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(callback_rejected());
    }
    Ok(())
}

fn map_begin_failure(error: McpOAuthCoordinatorError) -> CoordinatorBeginFailure {
    if error.code == McpOAuthCoordinatorErrorCode::PendingAuthorizationConflict {
        CoordinatorBeginFailure::Conflict
    } else {
        CoordinatorBeginFailure::Unavailable
    }
}

fn map_lifecycle_failure(error: McpOAuthCoordinatorError) -> CoordinatorLifecycleFailure {
    match error.code {
        McpOAuthCoordinatorErrorCode::AuthorizationRequired => {
            CoordinatorLifecycleFailure::AuthorizationRequired
        },
        McpOAuthCoordinatorErrorCode::RefreshRejected => {
            CoordinatorLifecycleFailure::RefreshRejected
        },
        McpOAuthCoordinatorErrorCode::ScopeUpgradeNotRequired => {
            CoordinatorLifecycleFailure::NotRequired
        },
        _ => CoordinatorLifecycleFailure::Unavailable,
    }
}

fn map_completion_failure(error: McpOAuthCoordinatorError) -> CoordinatorCompletionFailure {
    match error.code {
        McpOAuthCoordinatorErrorCode::PendingAuthorizationNotFound => {
            CoordinatorCompletionFailure::NotFound
        },
        McpOAuthCoordinatorErrorCode::CallbackRejected => CoordinatorCompletionFailure::Rejected,
        McpOAuthCoordinatorErrorCode::IssuerMismatch
        | McpOAuthCoordinatorErrorCode::TokenExchangeFailed => {
            CoordinatorCompletionFailure::Terminal
        },
        _ => CoordinatorCompletionFailure::Unavailable,
    }
}

fn map_product_begin_failure(error: CoordinatorBeginFailure) -> McpOAuthProductError {
    match error {
        CoordinatorBeginFailure::Conflict => pending_conflict(),
        CoordinatorBeginFailure::Unavailable => provider_unavailable(),
    }
}

fn map_product_lifecycle_failure(error: CoordinatorLifecycleFailure) -> McpOAuthProductError {
    match error {
        CoordinatorLifecycleFailure::AuthorizationRequired => authorization_required_product(),
        CoordinatorLifecycleFailure::RefreshRejected => refresh_rejected_product(),
        CoordinatorLifecycleFailure::NotRequired => scope_upgrade_not_required_product(),
        CoordinatorLifecycleFailure::Unavailable => provider_unavailable(),
    }
}

const fn invalid_route() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::InvalidRoute,
        "the MCP OAuth callback route is invalid",
    )
}

fn validate_callback_origin(origin: &str) -> Result<String, McpOAuthProductError> {
    let parsed = Url::parse(origin).map_err(|_| invalid_route())?;
    let loopback_http = parsed.scheme() == "http"
        && match parsed.host() {
            Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(address)) => address.is_loopback(),
            Some(Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
    if !(parsed.scheme() == "https" || loopback_http)
        || parsed.cannot_be_a_base()
        || parsed.host().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(invalid_route());
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

const fn capacity_exceeded() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::CapacityExceeded,
        "the MCP OAuth callback registry is at capacity",
    )
}

const fn pending_conflict() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::PendingAuthorizationConflict,
        "an MCP OAuth authorization is already pending",
    )
}

const fn authorization_not_found() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::AuthorizationNotFound,
        "the MCP OAuth authorization is missing or expired",
    )
}

const fn authorization_not_live() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::AuthorizationNotLive,
        "the MCP OAuth browser flow is no longer live",
    )
}

const fn scope_mismatch() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::ScopeMismatch,
        "the MCP OAuth authorization belongs to another scope",
    )
}

const fn authorization_required_product() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::AuthorizationRequired,
        "MCP OAuth authorization is required",
    )
}

const fn refresh_rejected_product() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::RefreshRejected,
        "the MCP OAuth refresh was rejected and local credentials were invalidated",
    )
}

const fn scope_upgrade_not_required_product() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::ScopeUpgradeNotRequired,
        "the requested MCP OAuth scopes are already granted",
    )
}

const fn browser_unavailable() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::BrowserUnavailable,
        "the system browser could not be opened",
    )
}

const fn callback_rejected() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::CallbackRejected,
        "the MCP OAuth callback was rejected",
    )
}

const fn provider_unavailable() -> McpOAuthProductError {
    McpOAuthProductError::new(
        McpOAuthProductErrorCode::ProviderUnavailable,
        "the MCP OAuth provider is unavailable",
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use parking_lot::Mutex;
    use static_assertions::{assert_impl_all, assert_not_impl_any};

    use super::*;

    #[derive(Default)]
    struct FakeBrowserLauncher {
        fail: AtomicBool,
        opened: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl McpOAuthBrowserLauncher for FakeBrowserLauncher {
        async fn open(&self, authorization_url: &str) -> Result<(), McpOAuthProductError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(browser_unavailable());
            }
            self.opened.lock().push(authorization_url.to_owned());
            Ok(())
        }
    }

    struct FakeLiveFlow {
        binding_id: String,
        flow_id: String,
        attempt_id: String,
        browser_url: String,
        completion: Arc<AtomicU8>,
        cancel_count: Arc<AtomicUsize>,
        callbacks: Arc<Mutex<Vec<String>>>,
        callback_started: Option<Arc<tokio::sync::Notify>>,
        callback_release: Option<Arc<tokio::sync::Notify>>,
    }

    #[async_trait]
    impl ProductOAuthLiveFlow for FakeLiveFlow {
        fn binding_id(&self) -> &str {
            &self.binding_id
        }

        fn flow_id(&self) -> &str {
            &self.flow_id
        }

        fn attempt_id(&self) -> &str {
            &self.attempt_id
        }

        fn browser_url(&self) -> &str {
            &self.browser_url
        }

        async fn complete_query(
            &self,
            query: &str,
        ) -> Result<String, CoordinatorCompletionFailure> {
            self.callbacks.lock().push(query.to_owned());
            if let Some(started) = &self.callback_started {
                started.notify_one();
            }
            if let Some(release) = &self.callback_release {
                release.notified().await;
            }
            completion_result(self.completion.load(Ordering::SeqCst))?;
            Ok(self.attempt_id.clone())
        }

        async fn cancel(&self) -> Result<(), CoordinatorCompletionFailure> {
            self.cancel_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeCoordinator {
        binding_id: String,
        flow_id: String,
        attempt_id: String,
        scope: CredentialScope,
        begin_count: AtomicUsize,
        begin_failure: bool,
        refresh_count: AtomicUsize,
        completion: Arc<AtomicU8>,
        cancel_count: Arc<AtomicUsize>,
        callbacks: Arc<Mutex<Vec<String>>>,
        callback_started: Option<Arc<tokio::sync::Notify>>,
        callback_release: Option<Arc<tokio::sync::Notify>>,
        begin_started: Option<Arc<tokio::sync::Notify>>,
        begin_release: Option<Arc<tokio::sync::Notify>>,
        lifecycle_started: Option<Arc<tokio::sync::Notify>>,
        lifecycle_release: Option<Arc<tokio::sync::Notify>>,
    }

    impl FakeCoordinator {
        fn new(binding_byte: char, flow_byte: char, principal: &str) -> Self {
            Self {
                binding_id: binding_byte.to_string().repeat(CALLBACK_BINDING_ID_BYTES),
                flow_id: flow_byte.to_string().repeat(CALLBACK_FLOW_ID_BYTES),
                attempt_id: "e".repeat(CALLBACK_FLOW_ID_BYTES),
                scope: CredentialScope::new(principal, "default").expect("scope"),
                begin_count: AtomicUsize::new(0),
                begin_failure: false,
                refresh_count: AtomicUsize::new(0),
                completion: Arc::new(AtomicU8::new(0)),
                cancel_count: Arc::new(AtomicUsize::new(0)),
                callbacks: Arc::new(Mutex::new(Vec::new())),
                callback_started: None,
                callback_release: None,
                begin_started: None,
                begin_release: None,
                lifecycle_started: None,
                lifecycle_release: None,
            }
        }
    }

    #[async_trait]
    impl ProductOAuthCoordinator for FakeCoordinator {
        fn binding_id(&self) -> String {
            self.binding_id.clone()
        }

        fn scope(&self) -> CredentialScope {
            self.scope.clone()
        }

        async fn begin(&self) -> Result<Arc<dyn ProductOAuthLiveFlow>, CoordinatorBeginFailure> {
            self.begin_count.fetch_add(1, Ordering::SeqCst);
            if self.begin_failure {
                return Err(CoordinatorBeginFailure::Unavailable);
            }
            if let Some(started) = &self.begin_started {
                started.notify_one();
            }
            if let Some(release) = &self.begin_release {
                release.notified().await;
            }
            Ok(Arc::new(FakeLiveFlow {
                binding_id: self.binding_id.clone(),
                flow_id: self.flow_id.clone(),
                attempt_id: self.attempt_id.clone(),
                browser_url: "https://auth.example/authorize?state=canary-state".to_owned(),
                completion: Arc::clone(&self.completion),
                cancel_count: Arc::clone(&self.cancel_count),
                callbacks: Arc::clone(&self.callbacks),
                callback_started: self.callback_started.clone(),
                callback_release: self.callback_release.clone(),
            }))
        }

        async fn begin_scope_upgrade(
            &self,
            _required_scopes: Vec<String>,
        ) -> Result<
            (Arc<dyn ProductOAuthLiveFlow>, McpOAuthLifecycleAudit),
            CoordinatorLifecycleFailure,
        > {
            let flow = self
                .begin()
                .await
                .map_err(|_| CoordinatorLifecycleFailure::Unavailable)?;
            Ok((
                flow,
                lifecycle_audit(tool_runtime_core::manifest::AuthState::Authenticating),
            ))
        }

        async fn status(&self) -> Result<McpOAuthCredentialStatus, CoordinatorLifecycleFailure> {
            Ok(McpOAuthCredentialStatus {
                state: tool_runtime_core::manifest::AuthState::Ready,
                granted_scopes: vec!["read".to_owned()],
                refresh_available: true,
            })
        }

        async fn refresh(&self) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure> {
            self.refresh_count.fetch_add(1, Ordering::SeqCst);
            if let Some(started) = &self.lifecycle_started {
                started.notify_one();
            }
            if let Some(release) = &self.lifecycle_release {
                release.notified().await;
            }
            Ok(lifecycle_audit(
                tool_runtime_core::manifest::AuthState::Ready,
            ))
        }

        async fn invalidate_definitive_rejection(
            &self,
        ) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure> {
            Ok(lifecycle_audit(
                tool_runtime_core::manifest::AuthState::Missing,
            ))
        }

        async fn logout_local(
            &self,
        ) -> Result<McpOAuthLifecycleAudit, CoordinatorLifecycleFailure> {
            Ok(lifecycle_audit(
                tool_runtime_core::manifest::AuthState::Missing,
            ))
        }

        async fn complete_after_restart_query(
            &self,
            _binding_id: &str,
            _flow_id: &str,
            query: &str,
        ) -> Result<String, CoordinatorCompletionFailure> {
            self.callbacks.lock().push(query.to_owned());
            completion_result(self.completion.load(Ordering::SeqCst))?;
            Ok(self.attempt_id.clone())
        }
    }

    fn completion_result(mode: u8) -> Result<(), CoordinatorCompletionFailure> {
        match mode {
            0 => Ok(()),
            1 => Err(CoordinatorCompletionFailure::Rejected),
            2 => Err(CoordinatorCompletionFailure::NotFound),
            3 => Err(CoordinatorCompletionFailure::Terminal),
            _ => Err(CoordinatorCompletionFailure::Unavailable),
        }
    }

    fn lifecycle_audit(
        current_state: tool_runtime_core::manifest::AuthState,
    ) -> McpOAuthLifecycleAudit {
        McpOAuthLifecycleAudit {
            operation: magician_mcp_client::McpOAuthLifecycleOperation::Refresh,
            previous_state: tool_runtime_core::manifest::AuthState::Ready,
            current_state,
            granted_scope_count: if current_state == tool_runtime_core::manifest::AuthState::Ready {
                1
            } else {
                0
            },
            refresh_available: current_state == tool_runtime_core::manifest::AuthState::Ready,
        }
    }

    fn api(
        launcher: Arc<FakeBrowserLauncher>,
        max_live_flows: usize,
        ttl: Duration,
    ) -> (
        McpOAuthApi,
        tokio::sync::broadcast::Receiver<RuntimeTransportEvent>,
    ) {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        let receiver = broadcaster.subscribe();
        (
            McpOAuthApi::with_dependencies(broadcaster, launcher, max_live_flows, ttl),
            receiver,
        )
    }

    #[test]
    fn callback_base_tracks_the_selected_server_port_and_api_route() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(4));
        let api = McpOAuthApi::new_with_callback_port(broadcaster, 4317);
        assert_eq!(
            api.callback_base_url(),
            "http://127.0.0.1:4317/api/magician/v2/auth/mcp/oauth/callback"
        );
    }

    #[test]
    fn callback_base_accepts_reviewed_https_origin_and_rejects_unsafe_origins() {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(4));
        let api = McpOAuthApi::new_with_callback_origin(
            Arc::clone(&broadcaster),
            4317,
            Some("https://connect.magican.ai/"),
        )
        .expect("public callback origin");
        assert_eq!(
            api.callback_base_url(),
            "https://connect.magican.ai/api/magician/v2/auth/mcp/oauth/callback"
        );
        assert!(McpOAuthApi::new_with_callback_origin(
            Arc::clone(&broadcaster),
            4317,
            Some("http://connect.magican.ai"),
        )
        .is_err());
        assert!(McpOAuthApi::new_with_callback_origin(
            broadcaster,
            4317,
            Some("https://connect.magican.ai/untrusted"),
        )
        .is_err());
    }

    #[tokio::test]
    async fn native_setup_returns_a_redacted_url_without_opening_the_engine_browser() {
        assert_not_impl_any!(McpOAuthNativeSetupLaunch: serde::Serialize);
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(Arc::clone(&launcher), 4, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let (pending, authorization_url) = api
            .begin_with_mode(coordinator, false)
            .await
            .expect("native authorization begin");
        let launch = McpOAuthNativeSetupLaunch {
            pending,
            authorization_url: authorization_url.expect("authorization URL"),
        };
        assert!(!launch.pending().browser_opened());
        assert!(launcher.opened.lock().is_empty());
        assert_eq!(
            launch.authorization_url(),
            "https://auth.example/authorize?state=canary-state"
        );
        let debug = format!("{launch:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("auth.example"));
        assert!(!debug.contains("canary-state"));
    }

    #[tokio::test]
    async fn begin_opens_the_exact_capability_and_emits_secret_free_pending_state() {
        assert_impl_all!(McpOAuthApi: Send, Sync);
        assert_not_impl_any!(McpOAuthPendingHandle: serde::Serialize);
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, mut events) = api(Arc::clone(&launcher), 4, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let handle = api
            .begin_with(coordinator)
            .await
            .expect("authorization begin");
        assert!(handle.browser_opened());
        assert_eq!(
            launcher.opened.lock().as_slice(),
            ["https://auth.example/authorize?state=canary-state"]
        );
        let event = events.try_recv().expect("pending event");
        let encoded = serde_json::to_string(&event).expect("event JSON");
        assert!(encoded.contains("mcp_oauth"));
        assert!(encoded.contains("owner"));
        assert!(!encoded.contains("auth.example"));
        assert!(!encoded.contains("canary-state"));
    }

    #[tokio::test]
    async fn failed_begin_rolls_back_its_coordinator_registration() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        let mut coordinator = FakeCoordinator::new('a', 'b', "owner");
        coordinator.begin_failure = true;
        api.begin_with(Arc::new(coordinator))
            .await
            .expect_err("begin failure");
        let state = api.registry.lock();
        assert!(state.coordinators.is_empty());
        assert!(state.starting_bindings.is_empty());
    }

    #[tokio::test]
    async fn callback_consumes_the_live_flow_once_and_emits_resolution() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, mut events) = api(launcher, 4, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        let callbacks = Arc::clone(&coordinator.callbacks);
        api.begin_with(coordinator).await.expect("begin");
        let _ = events.try_recv().expect("pending");

        api.complete_callback(&binding, &flow, "code=secret&state=opaque")
            .await
            .expect("callback");
        let resolved =
            serde_json::to_string(&events.try_recv().expect("resolved")).expect("resolved JSON");
        assert!(resolved.contains("authenticated"));
        assert!(!resolved.contains("code=secret"));

        let replay = api
            .complete_callback(&binding, &flow, "code=secret&state=opaque")
            .await
            .expect_err("replay");
        assert_eq!(replay.code, McpOAuthProductErrorCode::AuthorizationNotFound);
        assert_eq!(callbacks.lock().len(), 1);
    }

    #[tokio::test]
    async fn newly_admitted_flow_reclaims_its_stable_callback_route() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 4, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        let callbacks = Arc::clone(&coordinator.callbacks);

        api.begin_with(coordinator.clone())
            .await
            .expect("first begin");
        api.complete_callback(&binding, &flow, "code=first&state=opaque")
            .await
            .expect("first callback");
        api.begin_with(coordinator).await.expect("second begin");
        api.complete_callback(&binding, &flow, "code=second&state=opaque")
            .await
            .expect("second callback");

        assert_eq!(callbacks.lock().len(), 2);
    }

    #[test]
    fn consumed_callback_replay_registry_is_bounded() {
        let now = Instant::now();
        let mut callbacks = HashMap::new();
        for index in 0..=MAX_CONSUMED_CALLBACKS {
            remember_consumed_callback(
                &mut callbacks,
                RouteKey {
                    binding_id: format!("binding-{index}"),
                    flow_id: format!("flow-{index}"),
                },
                now + Duration::from_secs(index as u64),
            );
        }

        assert_eq!(callbacks.len(), MAX_CONSUMED_CALLBACKS);
        assert!(!callbacks.contains_key(&RouteKey {
            binding_id: "binding-0".to_owned(),
            flow_id: "flow-0".to_owned(),
        }));
        assert!(callbacks.contains_key(&RouteKey {
            binding_id: format!("binding-{MAX_CONSUMED_CALLBACKS}"),
            flow_id: format!("flow-{MAX_CONSUMED_CALLBACKS}"),
        }));
    }

    #[tokio::test]
    async fn concurrent_callback_for_the_same_route_is_rejected_without_a_second_exchange() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 4, LIVE_FLOW_TTL);
        let api = Arc::new(api);
        let mut coordinator = FakeCoordinator::new('a', 'b', "owner");
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        coordinator.callback_started = Some(Arc::clone(&started));
        coordinator.callback_release = Some(Arc::clone(&release));
        let coordinator = Arc::new(coordinator);
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        let callbacks = Arc::clone(&coordinator.callbacks);
        api.begin_with(coordinator).await.expect("begin");

        let first_api = Arc::clone(&api);
        let first_binding = binding.clone();
        let first_flow = flow.clone();
        let first = tokio::spawn(async move {
            first_api
                .complete_callback(&first_binding, &first_flow, "code=first&state=opaque")
                .await
        });
        started.notified().await;
        let duplicate = api
            .complete_callback(&binding, &flow, "code=second&state=opaque")
            .await
            .expect_err("duplicate callback");
        assert_eq!(duplicate.code, McpOAuthProductErrorCode::CallbackRejected);
        assert_eq!(callbacks.lock().len(), 1);
        release.notify_one();
        first.await.expect("callback task").expect("first callback");
    }

    #[tokio::test]
    async fn rejected_callback_keeps_the_live_browser_flow_retriable() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(Arc::clone(&launcher), 4, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        coordinator.completion.store(1, Ordering::SeqCst);
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        let scope = coordinator.scope.clone();
        api.begin_with(coordinator).await.expect("begin");

        let rejected = api
            .complete_callback(&binding, &flow, "state=wrong")
            .await
            .expect_err("rejected");
        assert_eq!(rejected.code, McpOAuthProductErrorCode::CallbackRejected);
        api.retry_browser_for_correlation(
            &correlation_id(&binding, &flow, &"e".repeat(CALLBACK_FLOW_ID_BYTES)),
            &scope,
        )
        .await
        .expect("retry browser");
        assert_eq!(launcher.opened.lock().len(), 2);
    }

    #[tokio::test]
    async fn capacity_and_scope_checks_fail_before_cross_flow_effects() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 1, LIVE_FLOW_TTL);
        let first = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let second = Arc::new(FakeCoordinator::new('c', 'd', "owner"));
        let binding = first.binding_id.clone();
        let flow = first.flow_id.clone();
        api.begin_with(first).await.expect("first");
        let full = api.begin_with(second.clone()).await.expect_err("capacity");
        assert_eq!(full.code, McpOAuthProductErrorCode::CapacityExceeded);
        assert_eq!(second.begin_count.load(Ordering::SeqCst), 0);

        let other_scope = CredentialScope::new("other", "default").expect("scope");
        let mismatch = api
            .retry_browser_for_correlation(
                &correlation_id(&binding, &flow, &"e".repeat(CALLBACK_FLOW_ID_BYTES)),
                &other_scope,
            )
            .await
            .expect_err("scope mismatch");
        assert_eq!(mismatch.code, McpOAuthProductErrorCode::ScopeMismatch);
    }

    #[tokio::test]
    async fn concurrent_begin_for_one_binding_has_one_network_owner() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        let api = Arc::new(api);
        let mut coordinator = FakeCoordinator::new('a', 'b', "owner");
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        coordinator.begin_started = Some(Arc::clone(&started));
        coordinator.begin_release = Some(Arc::clone(&release));
        let coordinator = Arc::new(coordinator);

        let first_api = Arc::clone(&api);
        let first_coordinator = coordinator.clone();
        let first = tokio::spawn(async move { first_api.begin_with(first_coordinator).await });
        started.notified().await;
        let duplicate = api
            .begin_with(coordinator.clone())
            .await
            .expect_err("duplicate start");
        assert_eq!(
            duplicate.code,
            McpOAuthProductErrorCode::PendingAuthorizationConflict
        );
        assert_eq!(coordinator.begin_count.load(Ordering::SeqCst), 1);
        release.notify_one();
        first.await.expect("begin task").expect("first begin");
    }

    #[tokio::test]
    async fn cancelled_begin_releases_binding_and_capacity_reservations() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 1, LIVE_FLOW_TTL);
        let api = Arc::new(api);
        let mut blocked = FakeCoordinator::new('a', 'b', "owner");
        let started = Arc::new(tokio::sync::Notify::new());
        blocked.begin_started = Some(Arc::clone(&started));
        blocked.begin_release = Some(Arc::new(tokio::sync::Notify::new()));
        let blocked = Arc::new(blocked);
        let blocked_api = Arc::clone(&api);
        let task = tokio::spawn(async move { blocked_api.begin_with(blocked).await });
        started.notified().await;
        task.abort();
        assert!(task.await.expect_err("cancelled begin").is_cancelled());

        let replacement = Arc::new(FakeCoordinator::new('a', 'c', "owner"));
        api.begin_with(replacement)
            .await
            .expect("reservation released");
    }

    #[tokio::test]
    async fn cancelled_callback_releases_exact_route_singleflight_reservation() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        let api = Arc::new(api);
        let mut coordinator = FakeCoordinator::new('a', 'b', "owner");
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        coordinator.callback_started = Some(Arc::clone(&started));
        coordinator.callback_release = Some(Arc::clone(&release));
        let coordinator = Arc::new(coordinator);
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        api.begin_with(coordinator).await.expect("begin");

        let blocked_api = Arc::clone(&api);
        let blocked_binding = binding.clone();
        let blocked_flow = flow.clone();
        let task = tokio::spawn(async move {
            blocked_api
                .complete_callback(&blocked_binding, &blocked_flow, "code=first&state=opaque")
                .await
        });
        started.notified().await;
        task.abort();
        assert!(task.await.expect_err("cancelled callback").is_cancelled());

        release.notify_one();
        api.complete_callback(&binding, &flow, "code=second&state=opaque")
            .await
            .expect("callback reservation released");
    }

    #[tokio::test]
    async fn browser_failure_keeps_pending_flow_for_a_safe_retry() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        launcher.fail.store(true, Ordering::SeqCst);
        let (api, mut events) = api(Arc::clone(&launcher), 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let correlation = correlation_id(
            &coordinator.binding_id,
            &coordinator.flow_id,
            &coordinator.attempt_id,
        );
        let scope = coordinator.scope.clone();
        let handle = api.begin_with(coordinator).await.expect("pending begin");
        assert!(!handle.browser_opened());
        // One admitted request per correlation id, and it names the failure so
        // the pending card offers the retry instead of promising a sign-in
        // window that never opened.
        let pending =
            serde_json::to_string(&events.try_recv().expect("pending")).expect("pending JSON");
        assert!(pending.contains("The browser could not be opened automatically"));
        assert!(events.try_recv().is_err());

        launcher.fail.store(false, Ordering::SeqCst);
        api.retry_browser_for_correlation(&correlation, &scope)
            .await
            .expect("retry");
        assert_eq!(launcher.opened.lock().len(), 1);
    }

    #[tokio::test]
    async fn stale_attempt_actions_cannot_retry_or_dismiss_the_current_flow() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(Arc::clone(&launcher), 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let stale_correlation = correlation_id(
            &coordinator.binding_id,
            &coordinator.flow_id,
            &"f".repeat(CALLBACK_FLOW_ID_BYTES),
        );
        let current_correlation = correlation_id(
            &coordinator.binding_id,
            &coordinator.flow_id,
            &coordinator.attempt_id,
        );
        let scope = coordinator.scope.clone();
        let cancel_count = Arc::clone(&coordinator.cancel_count);
        api.begin_with(coordinator).await.expect("begin");
        assert_eq!(launcher.opened.lock().len(), 1);

        let stale_retry = api
            .retry_browser_for_correlation(&stale_correlation, &scope)
            .await
            .expect_err("stale retry");
        assert_eq!(
            stale_retry.code,
            McpOAuthProductErrorCode::AuthorizationNotLive
        );
        let stale_dismiss = api
            .dismiss_correlation(&stale_correlation, &scope)
            .await
            .expect_err("stale dismiss");
        assert_eq!(
            stale_dismiss.code,
            McpOAuthProductErrorCode::AuthorizationNotLive
        );
        assert_eq!(launcher.opened.lock().len(), 1);
        assert_eq!(cancel_count.load(Ordering::SeqCst), 0);

        api.dismiss_correlation(&current_correlation, &scope)
            .await
            .expect("current dismissal");
        assert_eq!(cancel_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn registered_coordinator_completes_when_the_live_session_is_gone() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, mut events) = api(launcher, 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        let callbacks = Arc::clone(&coordinator.callbacks);
        api.begin_with(coordinator).await.expect("begin");
        let _ = events.try_recv().expect("pending");
        let removed = api.registry.lock().live_flows.remove(&RouteKey {
            binding_id: binding.clone(),
            flow_id: flow.clone(),
        });
        drop(removed);

        api.complete_callback(&binding, &flow, "code=secret&state=opaque")
            .await
            .expect("restart callback");
        assert_eq!(callbacks.lock().as_slice(), ["code=secret&state=opaque"]);
        let resolved =
            serde_json::to_string(&events.try_recv().expect("resolved")).expect("resolved JSON");
        assert!(resolved.contains("authenticated"));
        assert!(!resolved.contains("code=secret"));

        let replay = api
            .complete_callback(&binding, &flow, "code=secret&state=opaque")
            .await
            .expect_err("restart replay");
        assert_eq!(replay.code, McpOAuthProductErrorCode::AuthorizationNotFound);
        assert_eq!(callbacks.lock().len(), 1);
    }

    #[tokio::test]
    async fn restart_registration_resolves_without_starting_a_second_flow() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, mut events) = api(launcher, 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let flow = coordinator.flow_id.clone();
        let callbacks = Arc::clone(&coordinator.callbacks);
        api.register_with(coordinator.clone())
            .expect("restart registration");
        assert_eq!(coordinator.begin_count.load(Ordering::SeqCst), 0);

        api.complete_callback(&binding, &flow, "code=secret&state=opaque")
            .await
            .expect("restart callback");
        assert_eq!(coordinator.begin_count.load(Ordering::SeqCst), 0);
        assert_eq!(callbacks.lock().len(), 1);
        // This process never began the flow, so it restores the request it
        // inherited before resolving it. A resolution the local lifecycle has
        // no pending record for is refused, not published.
        assert!(matches!(
            events.try_recv().expect("restored request"),
            RuntimeTransportEvent::HitlRequested { .. }
        ));
        let resolved =
            serde_json::to_string(&events.try_recv().expect("resolved")).expect("resolved JSON");
        assert!(resolved.contains("authenticated"));
        assert!(!resolved.contains("code=secret"));
    }

    #[test]
    fn coordinator_unregister_is_idempotent_scope_bound_and_releases_registry_capacity() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        for index in 0..(MAX_REGISTERED_COORDINATORS + 16) {
            let mut coordinator = FakeCoordinator::new('a', 'b', "owner");
            coordinator.binding_id = format!("{index:064x}");
            let coordinator = Arc::new(coordinator);
            let binding = coordinator.binding_id.clone();
            let scope = coordinator.scope.clone();
            api.register_with(coordinator).expect("register");
            assert!(api
                .unregister_coordinator(&binding, &scope)
                .expect("unregister"));
            assert!(!api
                .unregister_coordinator(&binding, &scope)
                .expect("idempotent unregister"));
        }
        assert!(api.registry.lock().coordinators.is_empty());
    }

    #[tokio::test]
    async fn coordinator_unregister_rejects_live_authority_until_dismissed() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let scope = coordinator.scope.clone();
        let correlation = correlation_id(&binding, &coordinator.flow_id, &coordinator.attempt_id);
        api.begin_with(coordinator).await.expect("begin");
        assert_eq!(
            api.unregister_coordinator(&binding, &scope)
                .expect_err("live flow owns authority")
                .code,
            McpOAuthProductErrorCode::PendingAuthorizationConflict
        );
        api.dismiss_correlation(&correlation, &scope)
            .await
            .expect("dismiss");
        assert!(api
            .unregister_coordinator(&binding, &scope)
            .expect("unregister after dismiss"));
    }

    #[tokio::test]
    async fn expiry_and_dismissal_release_capacity_and_emit_terminal_events() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (mut api, mut events) = api(launcher, 1, Duration::ZERO);
        let first = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let first_correlation =
            correlation_id(&first.binding_id, &first.flow_id, &first.attempt_id);
        let scope = first.scope.clone();
        api.begin_with(first).await.expect("first");
        let _ = events.try_recv().expect("pending");
        let expired = api
            .retry_browser_for_correlation(&first_correlation, &scope)
            .await
            .expect_err("expired");
        assert_eq!(expired.code, McpOAuthProductErrorCode::AuthorizationNotLive);
        let expired_event = serde_json::to_string(&events.try_recv().expect("expired event"))
            .expect("expired JSON");
        assert!(expired_event.contains("expired"));

        let second = Arc::new(FakeCoordinator::new('c', 'd', "owner"));
        let second_correlation =
            correlation_id(&second.binding_id, &second.flow_id, &second.attempt_id);
        let second_scope = second.scope.clone();
        api.live_flow_ttl = LIVE_FLOW_TTL;
        api.begin_with(second).await.expect("capacity released");
        let _ = events.try_recv().expect("pending");
        api.dismiss_correlation(&second_correlation, &second_scope)
            .await
            .expect("dismiss");
        let dismissed = serde_json::to_string(&events.try_recv().expect("dismissed event"))
            .expect("dismissed JSON");
        assert!(dismissed.contains("dismissed"));
    }

    #[tokio::test]
    async fn exact_binding_refresh_survives_caller_cancellation_and_remains_single_flight() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        let api = Arc::new(api);
        let mut coordinator = FakeCoordinator::new('a', 'b', "owner");
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        coordinator.lifecycle_started = Some(Arc::clone(&started));
        coordinator.lifecycle_release = Some(Arc::clone(&release));
        let coordinator = Arc::new(coordinator);
        let binding = coordinator.binding_id.clone();
        let scope = coordinator.scope.clone();
        api.register_with(coordinator.clone()).expect("register");

        let first_api = Arc::clone(&api);
        let first_binding = binding.clone();
        let first_scope = scope.clone();
        let first = tokio::spawn(async move {
            first_api
                .refresh_binding(&first_binding, &first_scope)
                .await
        });
        started.notified().await;
        let duplicate = api
            .refresh_binding(&binding, &scope)
            .await
            .expect_err("duplicate refresh");
        assert_eq!(
            duplicate.code,
            McpOAuthProductErrorCode::PendingAuthorizationConflict
        );
        assert_eq!(coordinator.refresh_count.load(Ordering::SeqCst), 1);
        first.abort();
        assert!(first.await.expect_err("cancelled refresh").is_cancelled());

        let mut replacement = FakeCoordinator::new('a', 'c', "owner");
        replacement.lifecycle_release = None;
        let replacement = Arc::new(replacement);
        let still_owned = api
            .register_with(replacement.clone())
            .expect_err("detached refresh retains reservation");
        assert_eq!(
            still_owned.code,
            McpOAuthProductErrorCode::PendingAuthorizationConflict
        );
        release.notify_one();
        let mut registered = false;
        for _ in 0..32 {
            tokio::task::yield_now().await;
            if api.register_with(replacement.clone()).is_ok() {
                registered = true;
                break;
            }
        }
        assert!(registered, "completed refresh must release its reservation");
        api.refresh_binding(&binding, &scope)
            .await
            .expect("refresh after cancellation");
    }

    #[tokio::test]
    async fn lifecycle_status_and_mutations_enforce_exact_scope() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(launcher, 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let scope = coordinator.scope.clone();
        api.register_with(coordinator).expect("register");

        let status = api
            .credential_status(&binding, &scope)
            .await
            .expect("status");
        assert_eq!(status.state, tool_runtime_core::manifest::AuthState::Ready);
        let other_scope = CredentialScope::new("other", "default").expect("scope");
        let mismatch = api
            .logout_local(&binding, &other_scope)
            .await
            .expect_err("scope mismatch");
        assert_eq!(mismatch.code, McpOAuthProductErrorCode::ScopeMismatch);
        let logout = api.logout_local(&binding, &scope).await.expect("logout");
        assert_eq!(
            logout.current_state,
            tool_runtime_core::manifest::AuthState::Missing
        );
    }

    #[tokio::test]
    async fn scope_upgrade_uses_the_existing_live_flow_and_blocks_other_mutations() {
        let launcher = Arc::new(FakeBrowserLauncher::default());
        let (api, _events) = api(Arc::clone(&launcher), 2, LIVE_FLOW_TTL);
        let coordinator = Arc::new(FakeCoordinator::new('a', 'b', "owner"));
        let binding = coordinator.binding_id.clone();
        let scope = coordinator.scope.clone();
        api.register_with(coordinator).expect("register");

        let (pending, audit) = api
            .begin_scope_upgrade(&binding, &scope, vec!["write".to_owned()])
            .await
            .expect("scope upgrade");
        assert!(pending.browser_opened());
        assert_eq!(
            audit.current_state,
            tool_runtime_core::manifest::AuthState::Authenticating
        );
        let blocked = api
            .invalidate_definitive_rejection(&binding, &scope)
            .await
            .expect_err("live flow blocks invalidation");
        assert_eq!(
            blocked.code,
            McpOAuthProductErrorCode::PendingAuthorizationConflict
        );
        assert_eq!(launcher.opened.lock().len(), 1);
    }

    #[test]
    fn route_and_callback_pages_are_bounded_static_surfaces() {
        let valid = correlation_id(&"a".repeat(64), &"b".repeat(32), &"c".repeat(32));
        assert!(parse_correlation_id(&valid).is_ok());
        assert!(parse_correlation_id("mcp_oauth:a:b").is_err());
        assert!(parse_correlation_id(&format!("{valid}:extra")).is_err());
        assert!(validate_callback_query("").is_err());
        assert!(validate_callback_query("code=x\n&state=y").is_err());
        assert!(validate_callback_query(&"x".repeat(MAX_CALLBACK_QUERY_BYTES + 1)).is_err());

        let response = callback_page(
            actix_web::http::StatusCode::BAD_REQUEST,
            "Sign-in could not finish",
            "Return to Magican and start sign-in again.",
        );
        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&header::HeaderValue::from_static("no-store, max-age=0"))
        );
        assert!(response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY));
    }
}
