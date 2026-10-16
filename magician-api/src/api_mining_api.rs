//! # API Mining API
//!
//! REST endpoints for browsing learned API capabilities, auth status,
//! and replaying requests. All live state resolves from explicit V3 scope
//! headers into scoped `api_mining/` and `secrets/` roots.

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock as StdRwLock};
use std::time::{Duration, Instant};

use magician::magician_v2::api_mining::auth_capture::{
    persist_captured_auth_events, BrowserAuthCaptureRequirements,
};
use magician::magician_v2::api_mining::capability::{
    ApiCapability, CapabilitySummary, ConfidenceLevel, SideEffects,
};
use magician::magician_v2::api_mining::capability_store::CapabilityStore;
use magician::magician_v2::api_mining::maintenance::{purge_origin_artifacts, OriginPurgeReport};
use magician::magician_v2::api_mining::noise_filter::NoiseFilter;
use magician::magician_v2::api_mining::openapi_generator::generate_openapi_spec;
use magician::magician_v2::api_mining::origin_policy::{
    OriginPolicyDecision, OriginPolicyStore, OriginReplayCheck, OriginReplayMode,
};
use magician::magician_v2::api_mining::recipe::{
    RecipeMaturity, TaskInputSchema, TaskInputSource, TaskRecipe,
};
use magician::magician_v2::api_mining::recipe_packs::purge_recipe_packs;
use magician::magician_v2::api_mining::recipe_runner::{
    RecipeRunInputs, RecipeRunner, ReqwestTransport,
};
use magician::magician_v2::api_mining::recipe_store::RecipeStore;
use magician::magician_v2::api_mining::registry::{
    CapabilityRegistry, OriginEntry, RegistryHealthSnapshot, RegistryHealthWarning,
};
use magician::magician_v2::api_mining::relevance::{
    classify as classify_relevance, page_origin_for_trace, Relevance,
};
use magician::magician_v2::api_mining::replay::{build_replay_request, resolve_url_template};
use magician::magician_v2::api_mining::replay_grants::{
    is_denylisted_url_template, GrantKey, ReplayGrantStore,
};
use magician::magician_v2::api_mining::router::extract_origin;
use magician::magician_v2::api_mining::trace_storage::TraceStorage;
use magician::magician_v2::api_mining::types::SessionContext;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::browser_engine_analytics::BrowserEngineAnalyticsContext;
use magician::magician_v2::execution::primitive_dispatch::browser::{
    AgentBrowserSession, ConnectionMode,
};
use magician::magician_v2::execution::{
    CapabilityPackStore, ExecutionConfig, MagicutorClient, ScopedCapabilityResolver,
};
use magician::magician_v2::secrets::{CapturedSessionLease, SecretStore};

const NOISY_ORIGIN_TRACE_THRESHOLD: usize = 25;
const AUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(60);
const AUTH_REFRESH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const AUTH_REFRESH_STATUS_RETENTION: Duration = Duration::from_secs(10 * 60);
const TRACE_STATS_CACHE_TTL: Duration = Duration::from_secs(5);
const MAX_TRACE_STATS_CACHE_SCOPES: usize = 128;
const MAX_RECIPE_LIST_ITEMS: usize = 500;
const MAX_OVERVIEW_SITES: usize = 250;
const MAX_OVERVIEW_ORIGINS: usize = 2_000;
const MAX_OVERVIEW_CAPABILITIES: usize = 2_000;

type AuthRefreshStatusKey = (String, String, String);

#[derive(Clone, Default)]
struct OriginTraceStats {
    total_by_origin: HashMap<String, usize>,
    telemetry_by_site: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthRefreshPhase {
    Starting,
    WaitingForAuth,
    Captured,
    CapturedUnverified,
    Verifying,
    Verified,
    VerificationFailed,
    TimedOut,
    Failed,
}

impl AuthRefreshPhase {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::CapturedUnverified
                | Self::Verified
                | Self::VerificationFailed
                | Self::TimedOut
                | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuthRefreshStatus {
    pub refresh_id: String,
    pub origin_key: String,
    pub origin_url: String,
    pub phase: AuthRefreshPhase,
    pub message: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub terminal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_status: Option<u16>,
}

#[derive(Debug, Clone)]
struct StoredAuthRefreshStatus {
    status: AuthRefreshStatus,
    updated_at: Instant,
}

fn manual_replay_session_for_url(
    secret_store: &SecretStore,
    origin_url: &str,
    url_template: &str,
    parameter_overrides: &HashMap<String, String>,
) -> Result<(String, SessionContext, Option<CapturedSessionLease>), String> {
    let concrete_url = resolve_url_template(url_template, parameter_overrides)?;
    let (session, lease) = match secret_store.get_session(origin_url, &concrete_url) {
        Some((session, lease)) => (session, Some(lease)),
        None => (SessionContext::default(), None),
    };
    Ok((concrete_url, session, lease))
}

fn session_values_from_context(session: &SessionContext) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for (key, value) in &session.auth_headers {
        insert_session_value(&mut values, key, value);
        let lower = key.to_ascii_lowercase();
        if lower == "authorization" {
            let trimmed = value.trim();
            let lower_value = trimmed.to_ascii_lowercase();
            if lower_value.starts_with("bearer ") {
                insert_session_value(&mut values, "bearer", trimmed);
                insert_session_value(&mut values, "bearer_token", trimmed[7..].trim());
            } else if lower_value.starts_with("basic ") {
                insert_session_value(&mut values, "basic", trimmed);
                insert_session_value(&mut values, "basic_token", trimmed[6..].trim());
            }
        }
    }
    for (key, value) in &session.auth_query_params {
        insert_session_value(&mut values, key, value);
        insert_session_value(&mut values, &format!("query:{key}"), value);
    }
    for (key, value) in &session.cookies {
        insert_session_value(&mut values, key, value);
        insert_session_value(&mut values, &format!("cookie:{key}"), value);
    }
    for cookie in &session.cookie_header_values {
        insert_session_value(&mut values, &cookie.name, &cookie.value);
        insert_session_value(
            &mut values,
            &format!("cookie:{}", cookie.name),
            &cookie.value,
        );
    }
    for (key, value) in &session.local_storage {
        insert_session_value(&mut values, key, value);
        insert_session_value(&mut values, &format!("local_storage:{key}"), value);
    }
    for (key, value) in &session.session_storage {
        insert_session_value(&mut values, key, value);
        insert_session_value(&mut values, &format!("session_storage:{key}"), value);
    }
    values
}

fn insert_session_value(values: &mut HashMap<String, String>, key: &str, value: &str) {
    if key.trim().is_empty() || value.trim().is_empty() {
        return;
    }
    values
        .entry(key.to_string())
        .or_insert_with(|| value.to_string());
    values
        .entry(key.to_ascii_lowercase())
        .or_insert_with(|| value.to_string());
}

fn response_marks_auth_stale(status: u16) -> bool {
    status == 401
}

#[derive(Debug, Clone)]
enum DirectReplayPolicyBlock {
    RequiresHitl { reason: String },
    Denied { reason: String },
}

fn direct_replay_policy_block(
    base_path: &std::path::Path,
    origin_url: &str,
    side_effects: &SideEffects,
    confidence: &ConfidenceLevel,
) -> Option<DirectReplayPolicyBlock> {
    match OriginPolicyStore::open(base_path).check_live_replay(origin_url, side_effects, confidence)
    {
        OriginReplayCheck::Allowed => None,
        OriginReplayCheck::RequiresHitl { reason } => Some(DirectReplayPolicyBlock::RequiresHitl {
            reason: format!(
                "direct API replay for origin {origin_url} requires HITL approval: {reason}"
            ),
        }),
        OriginReplayCheck::Denied { reason } => Some(DirectReplayPolicyBlock::Denied {
            reason: format!(
                "direct API replay blocked by origin policy for {origin_url}: {reason}"
            ),
        }),
    }
}

fn workflow_direct_replay_policy_block(
    base_path: &std::path::Path,
    workflow: &magician::magician_v2::api_mining::workflow::WorkflowGraph,
) -> Option<DirectReplayPolicyBlock> {
    let Ok(registry) = CapabilityRegistry::with_base_path(base_path) else {
        return None;
    };
    let store = CapabilityStore::with_base_path(base_path);
    for step in &workflow.steps {
        let Some(capability_id) = step.capability_id.as_deref() else {
            continue;
        };
        let capability = match registry.get_capability(&workflow.origin_key, capability_id) {
            Ok(capability) => Some(capability),
            Err(_) => store.load_all_origins().ok().and_then(|origins| {
                origins
                    .into_values()
                    .flatten()
                    .find(|capability| capability.id == capability_id)
            }),
        };
        let Some(capability) = capability else {
            continue;
        };
        let side_effects = capability.effective_side_effects();
        if let Some(block) = direct_replay_policy_block(
            base_path,
            &capability.origin,
            &side_effects,
            &capability.confidence,
        ) {
            let prefix = format!(
                "workflow {} step {} capability {}",
                workflow.id, step.id, capability_id
            );
            return Some(match block {
                DirectReplayPolicyBlock::RequiresHitl { reason } => {
                    DirectReplayPolicyBlock::RequiresHitl {
                        reason: format!("{prefix}: {reason}"),
                    }
                },
                DirectReplayPolicyBlock::Denied { reason } => DirectReplayPolicyBlock::Denied {
                    reason: format!("{prefix}: {reason}"),
                },
            });
        }
    }
    None
}

/// Shared state for the API mining endpoints.
#[derive(Clone)]
pub struct ApiMiningApi {
    workspace_layout: ArtifactV2Workspace,
    secret_store_resolver: Arc<magician::magician_v2::secrets::SecretStoreResolver>,
    api_mining_config: magician::config::ApiMiningConfig,
    api_mining_switch: magician::magician_v2::api_mining::switch::ApiMiningSwitch,
    magicutor_base_url: String,
    magicutor_client: Option<Arc<MagicutorClient>>,
    http_client: reqwest::Client,
    auth_refresh_statuses: Arc<Mutex<HashMap<AuthRefreshStatusKey, StoredAuthRefreshStatus>>>,
    trace_stats_cache: Arc<Mutex<HashMap<(String, String), (Instant, OriginTraceStats)>>>,
    scoped_capability_resolver: Arc<StdRwLock<Option<Arc<ScopedCapabilityResolver>>>>,
}

impl ApiMiningApi {
    pub fn new(
        base_root: PathBuf,
        secret_store_resolver: Arc<magician::magician_v2::secrets::SecretStoreResolver>,
        api_mining_config: magician::config::ApiMiningConfig,
    ) -> Self {
        Self::with_workspace_layout(
            ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(&base_root)),
            secret_store_resolver,
            api_mining_config,
        )
    }

    pub fn with_workspace_layout(
        workspace_layout: ArtifactV2Workspace,
        secret_store_resolver: Arc<magician::magician_v2::secrets::SecretStoreResolver>,
        api_mining_config: magician::config::ApiMiningConfig,
    ) -> Self {
        let switch_workspace = workspace_layout.clone();
        let api_mining_switch = magician::magician_v2::api_mining::switch::ApiMiningSwitch::new(
            api_mining_config.enabled,
            move |principal, workspace| switch_workspace.api_mining_root(principal, workspace),
        );
        Self {
            workspace_layout,
            secret_store_resolver,
            api_mining_config: api_mining_config.validated(),
            api_mining_switch,
            magicutor_base_url: ExecutionConfig::default().base_url.to_string(),
            magicutor_client: None,
            http_client: reqwest::Client::new(),
            auth_refresh_statuses: Arc::new(Mutex::new(HashMap::new())),
            trace_stats_cache: Arc::new(Mutex::new(HashMap::new())),
            scoped_capability_resolver: Arc::new(StdRwLock::new(None)),
        }
    }

    pub fn with_magicutor_runtime(
        mut self,
        magicutor_base_url: String,
        magicutor_client: Arc<MagicutorClient>,
    ) -> Self {
        self.magicutor_base_url = magicutor_base_url;
        self.magicutor_client = Some(magicutor_client);
        self
    }

    pub fn with_switch(
        mut self,
        api_mining_switch: magician::magician_v2::api_mining::switch::ApiMiningSwitch,
    ) -> Self {
        self.api_mining_switch = api_mining_switch;
        self
    }

    /// Bind the process-shared resolver after startup has installed its
    /// compiled handlers. Purge operations can then evict the exact scope as
    /// soon as generated recipe packs disappear from the catalog.
    pub fn set_scoped_capability_resolver(&self, resolver: Arc<ScopedCapabilityResolver>) {
        *self
            .scoped_capability_resolver
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(resolver);
    }

    fn invalidate_capability_scope(&self, principal: &str, workspace: &str) {
        if let Some(resolver) = self
            .scoped_capability_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            resolver.invalidate_scope(principal, workspace);
        }
    }

    fn invalidate_all_capability_scopes(&self) {
        if let Some(resolver) = self
            .scoped_capability_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            resolver.clear_cache();
        }
    }

    fn clear_auth_refresh_scope(&self, principal: &str, workspace: &str) {
        self.auth_refresh_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|(stored_principal, stored_workspace, _), _| {
                stored_principal != principal || stored_workspace != workspace
            });
    }

    fn invalidate_trace_stats_scope(&self, principal: &str, workspace: &str) {
        self.trace_stats_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(principal.to_owned(), workspace.to_owned()));
    }

    async fn purge_recipe_state_for_origin(
        &self,
        principal: &str,
        workspace: &str,
        origin: &str,
        secret_store: &SecretStore,
        report: &mut OriginPurgeReport,
    ) -> Result<(), String> {
        let result: Result<(), String> = async {
            let base = self.scoped_base_path(principal, workspace);
            let store = RecipeStore::new(base.clone());
            let _scope_guard = store.scope_mutation_lock().lock_owned().await;
            // Lock every recipe file, not only successfully parsed matching
            // recipes. A file corrupted after a replay loaded it must not hide
            // that in-flight transaction from destructive origin cleanup.
            let replay_lock_ids = store.list_ids().map_err(|error| {
                format!("failed to enumerate task recipes for {origin}: {error}")
            })?;
            let _replay_guards = store
                .lock_replays(replay_lock_ids.iter().cloned())
                .await
                .map_err(|error| {
                    format!("failed to serialize task recipe purge for {origin}: {error}")
                })?;
            store.list_strict().map_err(|error| {
                format!("failed to validate task recipes before purging {origin}: {error}")
            })?;
            // Hold the scope and every affected replay lock before deleting
            // any origin artifact. Otherwise an in-flight replay can append
            // feedback or a run record after the first deletion pass and
            // briefly resurrect data the operator explicitly purged.
            let (projections_deleted, projection_rows_deleted) =
                match self.projection_pipeline_for_scope(principal, workspace) {
                    Ok(pipeline) => match pipeline.purge_origin(origin) {
                        Ok(counts) => counts,
                        Err(error) => {
                            report.projections_deleted = error.projections_deleted;
                            report.projection_rows_deleted = error.rows_deleted;
                            return Err(format!(
                                "failed to purge projected data for {origin}: {error}"
                            ));
                        },
                    },
                    Err(error) => {
                        return Err(format!(
                            "failed to open projected data for {origin}: {error}"
                        ));
                    },
                };
            report.projections_deleted = projections_deleted;
            report.projection_rows_deleted = projection_rows_deleted;
            let mut artifact_report = purge_origin_artifacts(&base, secret_store, origin)?;
            artifact_report.projections_deleted = projections_deleted;
            artifact_report.projection_rows_deleted = projection_rows_deleted;
            *report = artifact_report;
            let mut removed_recipe_ids = HashSet::new();
            let mut errors = Vec::new();
            match store.remove_origin(origin) {
                Ok(removed) => {
                    report.recipes_deleted = removed.len();
                    removed_recipe_ids.extend(removed);
                },
                Err(error) => errors.push(format!(
                    "failed to remove task recipes for {origin}: {error}"
                )),
            }
            match ReplayGrantStore::open(&base).revoke_origin(origin) {
                Ok(revoked) => report.replay_grants_revoked = revoked,
                Err(error) => errors.push(format!(
                    "failed to revoke replay grants for {origin}: {error}"
                )),
            }

            if !removed_recipe_ids.is_empty() {
                let pack_store = CapabilityPackStore::with_workspace_layout(
                    &self.workspace_layout,
                    principal,
                    workspace,
                );
                match purge_recipe_packs(
                    &pack_store,
                    &self
                        .workspace_layout
                        .scope_skills_root(principal, workspace),
                    Some(&removed_recipe_ids),
                ) {
                    Ok(removed) => report.recipe_packs_deleted = removed,
                    Err(error) => errors.push(error),
                }
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(errors.join("; "))
            }
        }
        .await;

        magician::magician_v2::api_mining::recipe_feedback::RecipeFeedbackSink::forget_scope(
            &self.workspace_layout,
            principal,
            workspace,
        );
        self.clear_auth_refresh_scope(principal, workspace);
        self.invalidate_trace_stats_scope(principal, workspace);
        self.invalidate_capability_scope(principal, workspace);
        result
    }

    fn auth_refresh_key(
        principal: &str,
        workspace: &str,
        refresh_id: &str,
    ) -> AuthRefreshStatusKey {
        (
            principal.to_string(),
            workspace.to_string(),
            refresh_id.to_string(),
        )
    }

    fn prune_auth_refresh_statuses(&self) {
        let mut statuses = self
            .auth_refresh_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        statuses.retain(|_, stored| {
            !stored.status.terminal || stored.updated_at.elapsed() < AUTH_REFRESH_STATUS_RETENTION
        });
    }

    fn active_auth_refresh_status(
        &self,
        principal: &str,
        workspace: &str,
        origin_key: &str,
    ) -> Option<AuthRefreshStatus> {
        self.prune_auth_refresh_statuses();
        let statuses = self
            .auth_refresh_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        statuses
            .iter()
            .find(|((stored_principal, stored_workspace, _), stored)| {
                stored_principal == principal
                    && stored_workspace == workspace
                    && stored.status.origin_key == origin_key
                    && !stored.status.terminal
            })
            .map(|(_, stored)| stored.status.clone())
    }

    fn insert_auth_refresh_status(
        &self,
        principal: &str,
        workspace: &str,
        status: AuthRefreshStatus,
    ) {
        let key = Self::auth_refresh_key(principal, workspace, &status.refresh_id);
        self.auth_refresh_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                key,
                StoredAuthRefreshStatus {
                    status,
                    updated_at: Instant::now(),
                },
            );
    }

    fn update_auth_refresh_status(
        &self,
        principal: &str,
        workspace: &str,
        refresh_id: &str,
        phase: AuthRefreshPhase,
        message: impl Into<String>,
        verification_status: Option<u16>,
    ) {
        let key = Self::auth_refresh_key(principal, workspace, refresh_id);
        let mut statuses = self
            .auth_refresh_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(stored) = statuses.get_mut(&key) else {
            return;
        };
        stored.status.phase = phase.clone();
        stored.status.message = message.into();
        stored.status.updated_at_ms = chrono::Utc::now().timestamp_millis();
        stored.status.terminal = phase.is_terminal();
        stored.status.verification_status = verification_status;
        stored.updated_at = Instant::now();
    }

    fn get_auth_refresh_status(
        &self,
        principal: &str,
        workspace: &str,
        origin_key: &str,
        refresh_id: &str,
    ) -> Option<AuthRefreshStatus> {
        self.prune_auth_refresh_statuses();
        let key = Self::auth_refresh_key(principal, workspace, refresh_id);
        self.auth_refresh_statuses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&key)
            .filter(|stored| stored.status.origin_key == origin_key)
            .map(|stored| stored.status.clone())
    }

    /// Resolve the per-scope `ProjectionPipelineState` through the
    /// process-wide registry in `projection_pipeline::pipeline_for_scope`.
    /// CRITICAL: the registry MUST be the only construction point so
    /// the executor's ingest hook (writes) and HTTP reads share the
    /// same pipeline handle — otherwise rows ingested via the hot
    /// path would be invisible to `query_known_resource` and vice
    /// versa.
    fn projection_pipeline_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<
        Arc<magician::magician_v2::api_mining::projection_pipeline::ProjectionPipelineState>,
        String,
    > {
        let db_path = magician::magician_v2::database_owners::database_file_path(
            &self.workspace_layout,
            principal,
            workspace,
            magician::magician_v2::database_owners::DatabaseOwner::ApiMining,
        );
        let records_dir = db_path.parent().ok_or_else(|| {
            "api mining projection database path has no parent directory".to_string()
        })?;
        magician::magician_v2::api_mining::projection_pipeline::pipeline_for_scope(
            principal,
            workspace,
            records_dir,
        )
    }

    fn resolve_required_scope(&self, req: &HttpRequest) -> Result<(String, String), HttpResponse> {
        let principal = req
            .headers()
            .get("X-Principal")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "missing principal claim in authenticated bearer"
                }))
            })?;
        let workspace = req
            .headers()
            .get("X-Workspace")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "missing workspace claim in authenticated bearer"
                }))
            })?;
        Ok((principal, workspace))
    }

    fn resolve_enabled_scope(&self, req: &HttpRequest) -> Result<(String, String), HttpResponse> {
        let (principal, workspace) = self.resolve_required_scope(req)?;
        if !self.api_mining_switch.effective(&principal, &workspace) {
            return Err(HttpResponse::Conflict().json(serde_json::json!({
                "error": "api_mining_disabled",
                "message": "API mining is disabled for this workspace",
            })));
        }
        Ok((principal, workspace))
    }

    fn scoped_base_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace_layout.api_mining_root(principal, workspace)
    }

    fn scoped_secret_store(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Arc<SecretStore>, HttpResponse> {
        self.secret_store_resolver
            .resolve_for_scope(principal, workspace)
            .map_err(|error| {
                HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("failed to resolve scoped secret store: {error}")
                }))
            })
    }

    /// Resolve a filesystem-safe origin_key back to the original origin URL
    /// using direct HashMap lookup on the registry index.
    fn resolve_origin_url(
        &self,
        principal: &str,
        workspace: &str,
        origin_key: &str,
    ) -> Option<String> {
        let registry =
            CapabilityRegistry::with_base_path(self.scoped_base_path(principal, workspace)).ok()?;
        registry
            .index()
            .origins
            .get(origin_key)
            .map(|entry| entry.origin_url.clone())
    }

    fn load_registry_index(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<magician::magician_v2::api_mining::registry::RegistryIndex, HttpResponse> {
        let registry = CapabilityRegistry::with_repaired_base_path(
            self.scoped_base_path(principal, workspace),
        )
        .map_err(|error| {
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("API mining registry could not be opened: {error}")
            }))
        })?;
        let index = registry.index().clone();
        Ok(index)
    }

    fn resolve_action_origin_url(
        &self,
        principal: &str,
        workspace: &str,
        origin_key: &str,
        requested_origin_url: Option<&str>,
    ) -> Option<String> {
        if let Some(origin_url) = self.resolve_origin_url(principal, workspace, origin_key) {
            return Some(origin_url);
        }

        requested_origin_url.and_then(|origin_url| {
            let normalized_key = CapabilityStore::origin_to_key(origin_url);
            (normalized_key == origin_key).then(|| origin_url.to_string())
        })
    }

    fn collect_origin_trace_counts_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> HashMap<String, usize> {
        self.collect_origin_trace_stats_for_scope(principal, workspace)
            .total_by_origin
    }

    fn collect_origin_trace_stats_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> OriginTraceStats {
        let cache_key = (principal.to_owned(), workspace.to_owned());
        {
            let cache = self
                .trace_stats_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((cached_at, stats)) = cache.get(&cache_key) {
                if cached_at.elapsed() < TRACE_STATS_CACHE_TTL {
                    return stats.clone();
                }
            }
        }

        let base_path = self.scoped_base_path(principal, workspace);
        let storage = TraceStorage::with_base_path(&base_path);
        let noise = NoiseFilter::load_read_only(&base_path);
        let mut stats = OriginTraceStats::default();
        let Ok(entries) = fs::read_dir(&base_path) else {
            self.cache_origin_trace_stats(cache_key, stats.clone());
            return stats;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            self.collect_trace_stats_in_dir(&storage, &noise, &path, &mut stats);
            let traces_dir = path.join("traces");
            if traces_dir.exists() {
                self.collect_trace_stats_in_dir(&storage, &noise, &traces_dir, &mut stats);
            }
        }

        self.cache_origin_trace_stats(cache_key, stats.clone());
        stats
    }

    fn cache_origin_trace_stats(&self, cache_key: (String, String), stats: OriginTraceStats) {
        let mut cache = self
            .trace_stats_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|_, (cached_at, _)| cached_at.elapsed() < TRACE_STATS_CACHE_TTL);
        if cache.len() >= MAX_TRACE_STATS_CACHE_SCOPES {
            if let Some(oldest) = cache
                .iter()
                .max_by_key(|(_, (cached_at, _))| cached_at.elapsed())
                .map(|(key, _)| key.clone())
            {
                cache.remove(&oldest);
            }
        }
        cache.insert(cache_key, (Instant::now(), stats));
    }

    fn collect_trace_stats_in_dir(
        &self,
        storage: &TraceStorage,
        noise: &NoiseFilter,
        dir: &std::path::Path,
        stats: &mut OriginTraceStats,
    ) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let is_trace_file = path.extension().and_then(|value| value.to_str()) == Some("jsonl")
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| value.starts_with("trace_"))
                    .unwrap_or(false);
            if !is_trace_file {
                continue;
            }

            if let Ok(traces) = storage.read_traces(&path) {
                for trace in traces {
                    let origin = extract_origin(&trace.url);
                    *stats.total_by_origin.entry(origin).or_insert(0) += 1;
                    let page_origin = page_origin_for_trace(&trace);
                    if classify_relevance(&trace, &page_origin, None, noise).relevance
                        == Relevance::Telemetry
                    {
                        *stats
                            .telemetry_by_site
                            .entry(site_host(&page_origin))
                            .or_insert(0) += 1;
                    }
                }
            }
        }
    }

    fn registry_health_snapshot(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<RegistryHealthSnapshot, HttpResponse> {
        let base_path = self.scoped_base_path(principal, workspace);
        let registry = CapabilityRegistry::with_base_path(&base_path).map_err(|error| {
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("API mining registry could not be opened: {error}")
            }))
        })?;
        let snapshot = registry.health_snapshot().map_err(|error| {
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("API mining registry health could not be read: {error}")
            }))
        })?;
        Ok(self.enrich_registry_health_snapshot(&base_path, snapshot))
    }

    fn enrich_registry_health_snapshot(
        &self,
        base_path: &Path,
        mut snapshot: RegistryHealthSnapshot,
    ) -> RegistryHealthSnapshot {
        let origin_policy = OriginPolicyStore::open(base_path);
        for origin in &mut snapshot.origin_readiness {
            let decision = origin_policy.decision_for_origin(&origin.origin_url);
            origin.origin_policy_decision = decision.map(origin_policy_decision_key);
            let auto_replay_allowed = origin_policy.allow_replay_for_origin(&origin.origin_url);
            origin.auto_replay_allowed = Some(auto_replay_allowed);
            let replay_mode = origin_policy.live_replay_mode_for_origin(&origin.origin_url);
            origin.replay_mode = Some(origin_replay_mode_key(replay_mode));

            if matches!(decision, Some(OriginPolicyDecision::Blocked)) {
                origin
                    .inactive_reasons
                    .push("origin_policy_blocked".to_string());
            }
            if !auto_replay_allowed {
                origin
                    .inactive_reasons
                    .push("auto_replay_not_allowed".to_string());
            }
            if matches!(
                replay_mode,
                OriginReplayMode::ObserveOnly | OriginReplayMode::ValidateOnly
            ) {
                origin
                    .inactive_reasons
                    .push("live_replay_not_allowed_by_origin_mode".to_string());
            }
            origin.inactive_reasons.sort();
            origin.inactive_reasons.dedup();
        }
        snapshot.warnings = registry_health_warnings(&snapshot, &self.api_mining_config);
        snapshot
    }
}

fn registry_health_warnings(
    snapshot: &RegistryHealthSnapshot,
    config: &magician::config::ApiMiningConfig,
) -> Vec<RegistryHealthWarning> {
    let mut warnings = Vec::new();
    if snapshot.has_index_drift() {
        warnings.push(RegistryHealthWarning {
            code: "registry_index_drift".to_string(),
            message: "Registry index drift is present; rebuild or restart the scoped router before trusting takeover readiness counts.".to_string(),
        });
    }
    if !config.enable_replay {
        warnings.push(RegistryHealthWarning {
            code: "live_replay_disabled".to_string(),
            message: "API replay is disabled in api_mining.enable_replay, so learned capabilities will not replace browser actions.".to_string(),
        });
    }
    if !config.enable_mining {
        warnings.push(RegistryHealthWarning {
            code: "mining_disabled".to_string(),
            message: "API mining is disabled in api_mining.enable_mining, so new browser traces will not promote fresh capabilities.".to_string(),
        });
    }
    if !config.auto_replay_validation.enabled {
        warnings.push(RegistryHealthWarning {
            code: "auto_replay_validation_disabled".to_string(),
            message: "Inline auto-replay validation is disabled, so candidates only validate through live routing or manual replay.".to_string(),
        });
    } else if config.auto_replay_validation.dry_run {
        warnings.push(RegistryHealthWarning {
            code: "auto_replay_dry_run".to_string(),
            message: "Inline auto-replay validation is in dry-run mode; the safety gates run, but no validation HTTP requests are emitted.".to_string(),
        });
    }

    let has_takeover_candidates = snapshot.takeover_ready_bindings_count > 0
        || snapshot.replayable_capability_count > 0
        || snapshot.action_bindings_count > 0;
    if has_takeover_candidates {
        let inactive_origins = snapshot
            .origin_readiness
            .iter()
            .filter(|origin| !origin.inactive_reasons.is_empty())
            .count();
        if inactive_origins > 0 {
            warnings.push(RegistryHealthWarning {
                code: "origins_not_active_for_takeover".to_string(),
                message: format!(
                    "{inactive_origins} origin(s) have learned capabilities but are not fully active for takeover; inspect each origin's inactive reasons."
                ),
            });
        }
    }

    warnings
}

fn origin_policy_decision_key(decision: OriginPolicyDecision) -> String {
    match decision {
        OriginPolicyDecision::Allowed => "allowed".to_string(),
        OriginPolicyDecision::Blocked => "blocked".to_string(),
    }
}

fn origin_replay_mode_key(mode: OriginReplayMode) -> String {
    match mode {
        OriginReplayMode::ObserveOnly => "observe_only",
        OriginReplayMode::ValidateOnly => "validate_only",
        OriginReplayMode::ReplayReads => "replay_reads",
        OriginReplayMode::ReplayWritesWithHitl => "replay_writes_with_hitl",
        OriginReplayMode::ReplayTrustedWrites => "replay_trusted_writes",
    }
    .to_string()
}

/// Path parameters for capability detail endpoint.
#[derive(Deserialize)]
pub struct CapabilityPath {
    origin_key: String,
    capability_id: String,
}

/// Path parameters for the sequence list endpoint.
#[derive(Deserialize)]
pub struct SequenceListPath {
    origin_key: String,
}

/// Path parameters for the sequence detail endpoint.
#[derive(Deserialize)]
pub struct SequenceDetailPath {
    origin_key: String,
    sequence_id: String,
}

/// List-view metadata for a captured sequence (no step bodies).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SequenceMetadata {
    pub id: String,
    pub task_id: String,
    pub execution_id: String,
    pub origin_key: String,
    pub step_count: usize,
    pub captured_at_ms: i64,
}

/// Path parameters for the workflow list endpoint.
#[derive(Deserialize)]
pub struct WorkflowListPath {
    origin_key: String,
}

/// Path parameters for the workflow detail endpoint.
#[derive(Deserialize)]
pub struct WorkflowDetailPath {
    origin_key: String,
    workflow_id: String,
}

/// List-view metadata for a compiled workflow (no per-step bodies).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkflowMetadata {
    pub id: String,
    pub origin_key: String,
    pub name: String,
    pub step_count: usize,
    pub maturity: magician::magician_v2::api_mining::workflow::WorkflowMaturity,
    pub last_compiled_at_ms: i64,
}

/// Path parameters for recipe detail, replay, and run-history endpoints.
#[derive(Deserialize)]
pub struct RecipePath {
    recipe_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RecipeRunsQuery {
    #[serde(default = "default_recipe_runs_limit")]
    pub limit: usize,
}

#[derive(Debug, Default, Deserialize)]
pub struct RecipeReplayQuery {
    /// Emitted recipe skills bind their publication to one exact version.
    #[serde(default)]
    pub expected_version: Option<u32>,
    /// When true, reject Draft/demoted/invalid recipes even though the
    /// operator-facing direct endpoint may explicitly exercise a Draft.
    #[serde(default)]
    pub published_only: bool,
}

fn default_recipe_runs_limit() -> usize {
    50
}

/// Path parameters for revoking a durable replay grant.
#[derive(Deserialize)]
pub struct ReplayGrantPath {
    grant_id: String,
}

/// Bounded list-view representation; response bodies and captured values stay
/// in the recipe detail endpoint only.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeMetadata {
    pub id: String,
    pub version: u32,
    pub template: String,
    pub inputs: Vec<RecipeInputMetadata>,
    pub maturity: RecipeMaturity,
    pub origins: Vec<String>,
    pub step_count: usize,
    pub replay_stats: magician::magician_v2::api_mining::workflow::ReplayStats,
    pub has_write_steps: bool,
    pub last_replayed_at_ms: Option<i64>,
}

/// Value-free task-input contract for list/overview payloads. Captured
/// examples remain in the explicit recipe-detail resource and are never
/// broadcast in the page's initial aggregate response.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeInputMetadata {
    pub name: String,
    pub schema: TaskInputSchema,
    pub source: TaskInputSource,
}

#[derive(Debug, Default, Deserialize)]
pub struct RegistryQuery {
    /// Comma-separated relevance classes. Omitted means the product default:
    /// answer-bearing, dependencies and first-party APIs.
    pub relevance: Option<String>,
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Debug, Serialize)]
pub struct OverviewResponse {
    pub recipes: Vec<RecipeMetadata>,
    pub sites: Vec<SiteGroup>,
    pub counters: OverviewCounters,
    pub auth: HashMap<String, magician::magician_v2::secrets::AuthStatusMetadata>,
    pub grants: usize,
    pub truncation: OverviewTruncation,
}

#[derive(Debug, Default, Serialize)]
pub struct OverviewTruncation {
    pub truncated: bool,
    pub omitted_recipes: usize,
    pub omitted_sites: usize,
    pub omitted_origins: usize,
    pub omitted_capabilities: usize,
}

#[derive(Debug, Serialize)]
pub struct SiteGroup {
    pub site: String,
    pub origins: Vec<OriginEntry>,
    pub capabilities: Vec<SiteCapability>,
    pub telemetry_hidden: usize,
}

/// A capability in the task-centric overview keeps its owning API origin so
/// replay and detail actions do not need a second registry request.
#[derive(Debug, Serialize)]
pub struct SiteCapability {
    pub origin_key: String,
    pub origin_url: String,
    #[serde(flatten)]
    pub capability: CapabilitySummary,
}

#[derive(Debug, Serialize)]
pub struct OverviewCounters {
    pub router: magician::magician_v2::api_mining::RouterMetricsSnapshot,
    pub passive_validation: magician::magician_v2::api_mining::PassiveValidationMetricsSnapshot,
    pub recipe: magician::magician_v2::api_mining::RecipeMetricsSnapshot,
    pub projection: magician::magician_v2::api_mining::ProjectionMetricsSnapshot,
    pub registry_health: RegistryHealthSnapshot,
}

fn metadata_for_recipe(recipe: &TaskRecipe) -> Option<RecipeMetadata> {
    let version = recipe.current()?;
    Some(RecipeMetadata {
        id: recipe.id.clone(),
        version: recipe.current_version,
        template: recipe.shape.template.clone(),
        inputs: recipe
            .shape
            .inputs
            .iter()
            .map(|input| RecipeInputMetadata {
                name: input.name.clone(),
                schema: input.schema,
                source: input.source,
            })
            .collect(),
        maturity: version.maturity,
        origins: version.origins.clone(),
        step_count: version.steps.len(),
        replay_stats: version.replay_stats.clone(),
        has_write_steps: recipe.has_write_steps(),
        last_replayed_at_ms: version.last_replayed_at_ms,
    })
}

fn relevance_filter(query: &RegistryQuery) -> Result<HashSet<Relevance>, String> {
    match query.relevance.as_deref().map(str::trim) {
        Some(value) if !value.is_empty() => value
            .split(',')
            .map(str::parse)
            .collect::<Result<HashSet<_>, _>>(),
        _ => Ok([
            Relevance::AnswerBearing,
            Relevance::Dependency,
            Relevance::FirstPartyApi,
        ]
        .into_iter()
        .collect()),
    }
}

fn site_host(origin: &str) -> String {
    url::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| origin.to_owned())
}

/// Overview capabilities are emitted once in `SiteGroup::capabilities` with
/// their owning origin. Keeping the registry's embedded summaries here would
/// duplicate records, bypass overview filtering, and inflate the initial UI
/// payload with hidden capability metadata.
fn overview_origin_entry(entry: &OriginEntry) -> OriginEntry {
    let mut entry = entry.clone();
    entry.capabilities.clear();
    entry
}

fn sort_overview_sites(sites: &mut [SiteGroup]) {
    sites.sort_by(|left, right| {
        left.capabilities
            .is_empty()
            .cmp(&right.capabilities.is_empty())
            .then_with(|| right.capabilities.len().cmp(&left.capabilities.len()))
            .then_with(|| left.site.cmp(&right.site))
    });
}

/// Path parameters for auth-status endpoint.
#[derive(Deserialize)]
pub struct AuthStatusPath {
    origin_key: String,
}

/// Path parameters for the OpenAPI spec endpoint.
#[derive(Deserialize)]
pub struct OpenApiPath {
    origin_key: String,
}

/// Path parameters for origin-level actions.
#[derive(Deserialize)]
pub struct OriginPath {
    origin_key: String,
}

/// Path parameters for polling a deterministic CDP auth refresh.
#[derive(Deserialize)]
pub struct AuthRefreshStatusPath {
    origin_key: String,
    refresh_id: String,
}

#[derive(Deserialize)]
pub struct OriginActionRequest {
    pub origin_url: String,
}

// ───────────────────────── Replay Types ──────────────────────────

/// Optional request body for the replay endpoint.
#[derive(Deserialize, Default)]
pub struct ReplayRequestBody {
    #[serde(default)]
    pub parameter_overrides: HashMap<String, String>,
    #[serde(default)]
    pub headers_overrides: HashMap<String, String>,
    #[serde(default)]
    pub body_override: Option<String>,
}

/// Response from the replay endpoint.
#[derive(Serialize)]
pub struct ReplayResponse {
    pub status: u16,
    pub headers: Option<HashMap<String, String>>,
    pub body: Option<String>,
    pub elapsed_ms: u64,
    pub auth_was_stale: bool,
    pub confidence_after: String,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct NoisyOriginsResponse {
    pub threshold: usize,
    pub origins: Vec<NoisyOriginEntry>,
}

#[derive(Serialize)]
pub struct NoisyOriginEntry {
    pub origin_key: String,
    pub origin_url: String,
    pub trace_count: usize,
    pub capability_count: usize,
    pub decision: Option<OriginPolicyDecision>,
}

#[derive(Serialize)]
pub struct OriginPolicyActionResponse {
    pub origin_key: String,
    pub origin_url: String,
    pub decision: OriginPolicyDecision,
    #[serde(default)]
    pub allow_replay: bool,
    #[serde(default)]
    pub replay_mode: OriginReplayMode,
}

/// Body for `POST /api-mining/origins/{origin_key}/allow-replay`.
#[derive(Deserialize)]
pub struct AllowReplayRequest {
    pub origin_url: Option<String>,
    pub allow_replay: bool,
}

/// Body for `POST /api-mining/origins/{origin_key}/replay-mode`.
#[derive(Deserialize)]
pub struct SetReplayModeRequest {
    pub origin_url: Option<String>,
    pub replay_mode: OriginReplayMode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiMiningSettingsLayer {
    Scope,
    Process,
}

#[derive(Debug, Deserialize)]
pub struct PutApiMiningSettingsRequest {
    pub enabled: bool,
    pub layer: ApiMiningSettingsLayer,
}

#[derive(Debug, Deserialize)]
pub struct DisableAndPurgeRequest {
    pub confirm: String,
}

#[derive(Serialize)]
pub struct PurgeOriginResponse {
    pub origin_key: String,
    pub origin_url: String,
    pub report: OriginPurgeReport,
}

// ───────────────────────── Handlers ──────────────────────────────

/// `GET /api-mining/projections` — list every projection in the
/// scope with lifecycle state + row count.
pub async fn list_projections(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.projection_pipeline_for_scope(&principal, &workspace) {
        Ok(pipeline) => HttpResponse::Ok().json(serde_json::json!({
            "projections": pipeline.list(),
        })),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err,
        })),
    }
}

/// `POST /api-mining/projections/{id}/approve` — operator-driven
/// approval flips a Pending projection to Approved, unlocking row
/// ingest on subsequent replays.
pub async fn approve_projection(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let projection_id = path.into_inner();
    let pipeline = match api.projection_pipeline_for_scope(&principal, &workspace) {
        Ok(p) => p,
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": err,
            }));
        },
    };
    match pipeline.approve_projection(&projection_id) {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "projection_id": projection_id,
            "status": "approved",
        })),
        Err(err) => HttpResponse::NotFound().json(serde_json::json!({
            "error": err,
        })),
    }
}

/// `POST /api-mining/projections/{id}/purge-rows` — drop all rows so
/// the schema can be re-shaped on the next ingest. Used after a
/// non-additive migration was rejected.
pub async fn purge_projection_rows(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let projection_id = path.into_inner();
    let pipeline = match api.projection_pipeline_for_scope(&principal, &workspace) {
        Ok(p) => p,
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": err,
            }));
        },
    };
    match pipeline.purge_rows(&projection_id) {
        Ok(purged) => HttpResponse::Ok().json(serde_json::json!({
            "projection_id": projection_id,
            "purged_rows": purged,
        })),
        Err(err) => HttpResponse::NotFound().json(serde_json::json!({
            "error": err,
        })),
    }
}

/// Request body for `POST /api-mining/projections/query`.
#[derive(Debug, Deserialize)]
pub struct QueryKnownResourceRequest {
    /// Origin URL (scheme + host).
    pub origin: String,
    /// Resource label (matches `ResourceProjection::resource_label`).
    pub resource: String,
    /// Optional SQL WHERE fragment with `?` placeholders.
    #[serde(default)]
    pub where_clause: Option<String>,
    /// Bound parameters for the WHERE fragment.
    #[serde(default)]
    pub params: Vec<serde_json::Value>,
}

/// `POST /api-mining/projections/query` — PL Task 18 surface.
///
/// Surfaces `ProjectionPipelineState::query_known_resource` as the operator
/// HTTP endpoint used by the native UI and available to governed tool adapters.
/// Returns typed errors so a caller can react (`no_projection` → replay;
/// `pending_approval` → notify operator; `store_error` → log).
///
/// The handler opens the same durable, per-scope projection pipeline used by
/// replay ingest, so reads remain available across process restarts and do not
/// depend on an orchestrator-owned in-memory handle.
pub async fn query_known_resource_handler(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    body: web::Json<QueryKnownResourceRequest>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let pipeline = match api.projection_pipeline_for_scope(&principal, &workspace) {
        Ok(p) => p,
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("projection pipeline open failed: {err}"),
            }));
        },
    };
    match pipeline.query_known_resource(
        &body.origin,
        &body.resource,
        body.where_clause.as_deref(),
        &body.params,
    ) {
        Ok((rows, served_from)) => HttpResponse::Ok().json(serde_json::json!({
            "served_from": served_from,
            "rows": rows,
        })),
        Err(err) => HttpResponse::Ok().json(serde_json::json!({ "error": err })),
    }
}

/// `GET /capability-evolution/summary` — return per-scope counts of
/// catalog state (number of packs by lifecycle status). Operator-
/// facing surface for the Capability Evolution pipeline. Reads the
/// catalog file directly; counts reflect on-disk state, not the
/// in-memory runtime cache (which can lag slightly during a mining
/// cycle).
pub async fn get_capability_evolution_summary(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = magician::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
        &api.workspace_layout,
        &principal,
        &workspace,
    );
    let catalog = match store.load_catalog() {
        Ok(cat) => cat,
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("failed to load capability catalog: {err}"),
            }));
        },
    };
    let mut by_status: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for record in &catalog.packs {
        let key = format!("{:?}", record.metadata.status);
        *by_status.entry(key).or_insert(0) += 1;
    }
    HttpResponse::Ok().json(serde_json::json!({
        "principal": principal,
        "workspace": workspace,
        "total_packs": catalog.packs.len(),
        "by_status": by_status,
    }))
}

/// `GET /api-mining/router-metrics` — return the per-scope router
/// outcome counter snapshot. Counters reset to zero on process
/// restart; the response is JSON-serializable for both the Forge
/// "Routing activity" tile and ad-hoc inspection.
pub async fn get_router_metrics(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let snapshot =
        magician::magician_v2::api_mining::router_snapshot_for_scope(&principal, &workspace);
    HttpResponse::Ok().json(snapshot)
}

/// `GET /api-mining/projection-metrics` — return the per-scope
/// projection pipeline counter snapshot.
pub async fn get_projection_metrics(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let snapshot =
        magician::magician_v2::api_mining::projection_snapshot_for_scope(&principal, &workspace);
    HttpResponse::Ok().json(snapshot)
}

/// `GET /api-mining/passive-validation-metrics` — return the per-scope
/// passive XHR/Fetch validation counter snapshot.
pub async fn get_passive_validation_metrics(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let snapshot = magician::magician_v2::api_mining::passive_validation_snapshot_for_scope(
        &principal, &workspace,
    );
    HttpResponse::Ok().json(snapshot)
}

/// `GET /api-mining/registry-health` — return registry integrity and
/// takeover-readiness counters for the active scope.
pub async fn get_registry_health(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.registry_health_snapshot(&principal, &workspace) {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(response) => response,
    }
}

/// `GET /api-mining/sequence-metrics` — return the per-scope sequence
/// capture counter snapshot.
pub async fn get_sequence_metrics(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let snapshot =
        magician::magician_v2::api_mining::sequence_snapshot_for_scope(&principal, &workspace);
    HttpResponse::Ok().json(snapshot)
}

/// `GET /api-mining/workflow-metrics` — return the per-scope workflow
/// compilation counter snapshot.
pub async fn get_workflow_metrics(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let snapshot =
        magician::magician_v2::api_mining::workflow_snapshot_for_scope(&principal, &workspace);
    HttpResponse::Ok().json(snapshot)
}

/// `GET /api-mining/recipe-metrics` — task-level recipe lookup, replay,
/// recovery, and approval counters for the active scope.
pub async fn get_recipe_metrics(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok()
        .json(magician::magician_v2::api_mining::recipe_snapshot_for_scope(&principal, &workspace))
}

/// `GET /api-mining/settings` — effective live state and its deciding layer.
pub async fn get_api_mining_settings(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok().json(api.api_mining_switch.state(&principal, &workspace))
}

/// `PUT /api-mining/settings` — durable scope override or owner-only process
/// ceiling. Both update the shared live handle before returning.
pub async fn put_api_mining_settings(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    body: web::Json<PutApiMiningSettingsRequest>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    let process_layer = matches!(&body.layer, ApiMiningSettingsLayer::Process);
    if process_layer && principal != "owner" {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "owner_required",
            "message": "only the host owner may change the process API-mining ceiling",
        }));
    }
    // Settings and compiler publication share the same scope mutation locks.
    // Once this endpoint returns `off`, no detached compile that began under
    // the old state can publish afterward. Stable ordering avoids deadlocks
    // when the process ceiling covers every known scope.
    let mut lock_scopes = if process_layer {
        api.workspace_layout.list_scopes()
    } else {
        vec![(principal.clone(), workspace.clone())]
    };
    lock_scopes.push((principal.clone(), workspace.clone()));
    lock_scopes.sort_unstable();
    lock_scopes.dedup();
    let mut _mutation_guards = Vec::with_capacity(lock_scopes.len());
    for (lock_principal, lock_workspace) in lock_scopes {
        let store = RecipeStore::new(api.scoped_base_path(&lock_principal, &lock_workspace));
        _mutation_guards.push(store.scope_mutation_lock().lock_owned().await);
    }
    let write_result = match body.layer {
        ApiMiningSettingsLayer::Scope => {
            api.api_mining_switch
                .set_scope(&principal, &workspace, Some(body.enabled))
        },
        ApiMiningSettingsLayer::Process => {
            let path = magician::config::magician_config_path();
            let mut config = match magician::config::load_magician_config_from_path(&path) {
                Ok(config) => config,
                Err(error) => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": "api_mining_settings_io_error",
                        "message": error.to_string(),
                    }));
                },
            };
            config.api_mining.enabled = body.enabled;
            let block = match serde_yaml::to_string(&config.api_mining) {
                Ok(block) => block.trim().trim_start_matches("---").trim().to_owned(),
                Err(error) => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": "api_mining_settings_encode_error",
                        "message": error.to_string(),
                    }));
                },
            };
            magician::magician_v2::runtime_settings::write_top_level_yaml_block(
                &path,
                "api_mining",
                &block,
            )
            .map(|_| api.api_mining_switch.set_process_enabled(body.enabled))
        },
    };
    match write_result {
        Ok(()) => {
            if process_layer {
                api.invalidate_all_capability_scopes();
            } else {
                api.invalidate_capability_scope(&principal, &workspace);
            }
            HttpResponse::Ok().json(api.api_mining_switch.state(&principal, &workspace))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "api_mining_settings_io_error",
            "message": error.to_string(),
        })),
    }
}

/// `POST /api-mining/settings/disable-and-purge` — explicit destructive
/// cleanup. The typed phrase is the authority boundary; the scope is disabled
/// before any learned artifact is touched.
pub async fn disable_and_purge_api_mining(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    body: web::Json<DisableAndPurgeRequest>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if body.confirm != "delete learned data" {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "confirmation_required",
            "message": "confirm must exactly equal `delete learned data`",
        }));
    }
    if let Err(error) = api
        .api_mining_switch
        .set_scope(&principal, &workspace, Some(false))
    {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "api_mining_settings_io_error",
            "message": error.to_string(),
        }));
    }
    let base = api.scoped_base_path(&principal, &workspace);
    let recipe_store = RecipeStore::new(base.clone());
    let _recipe_scope_guard = recipe_store.scope_mutation_lock().lock_owned().await;
    let recipe_ids = match recipe_store.list_lockable_ids() {
        Ok(recipe_ids) => recipe_ids,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "api_mining_purge_recipe_scan_failed",
                "message": error.to_string(),
            }));
        },
    };
    let _recipe_replay_guards = match recipe_store.lock_replays(recipe_ids).await {
        Ok(guards) => guards,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "api_mining_purge_lock_failed",
                "message": error.to_string(),
            }));
        },
    };
    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let origins = CapabilityRegistry::with_base_path(&base)
        .map(|registry| {
            registry
                .index()
                .origins
                .values()
                .map(|entry| entry.origin_url.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut errors = Vec::new();
    for origin in &origins {
        if let Err(error) = purge_origin_artifacts(&base, &secret_store, origin) {
            errors.push(format!("{origin}: {error}"));
        }
    }
    if let Err(error) = secret_store.clear_all_captured() {
        errors.push(format!("captured auth: {error}"));
    }
    if let Ok(pipeline) = api.projection_pipeline_for_scope(&principal, &workspace) {
        for projection in pipeline.list() {
            if let Err(error) = pipeline.purge_rows(&projection.id) {
                errors.push(format!("projection {}: {error}", projection.id));
            }
        }
    }
    let pack_store =
        CapabilityPackStore::with_workspace_layout(&api.workspace_layout, &principal, &workspace);
    let recipe_packs_removed = match purge_recipe_packs(
        &pack_store,
        &api.workspace_layout
            .scope_skills_root(&principal, &workspace),
        None,
    ) {
        Ok(removed) => removed,
        Err(error) => {
            errors.push(format!("published recipe packs: {error}"));
            0
        },
    };
    magician::magician_v2::api_mining::recipe_feedback::RecipeFeedbackSink::forget_scope(
        &api.workspace_layout,
        &principal,
        &workspace,
    );
    magician::magician_v2::api_mining::projection_pipeline::forget_pipeline_for_scope(
        &principal, &workspace,
    );
    let layout = ArtifactV2Workspace::with_local_file_provider(&base);
    let mut artifacts_removed = 0usize;
    match fs::read_dir(&base) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry.file_name() == "settings.json" {
                    continue;
                }
                let path = entry.path();
                let removal = match entry.file_type() {
                    Ok(kind) if kind.is_dir() && !kind.is_symlink() => {
                        layout.remove_dir_all_path_sync(&path)
                    },
                    Ok(_) => layout.remove_file_path_sync(&path),
                    Err(error) => {
                        errors.push(format!("{}: {error}", path.display()));
                        continue;
                    },
                };
                match removal {
                    Ok(()) => artifacts_removed = artifacts_removed.saturating_add(1),
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => errors.push(format!("{}: {error}", base.display())),
    }
    ReplayGrantStore::forget_shared_state(&base);
    OriginPolicyStore::forget_shared_state(&base);
    api.clear_auth_refresh_scope(&principal, &workspace);
    api.invalidate_trace_stats_scope(&principal, &workspace);
    api.invalidate_capability_scope(&principal, &workspace);
    if errors.is_empty() {
        HttpResponse::Ok().json(serde_json::json!({
            "status": "disabled_and_purged",
            "origins_purged": origins.len(),
            "artifact_entries_removed": artifacts_removed,
            "recipe_packs_removed": recipe_packs_removed,
        }))
    } else {
        HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "api_mining_purge_incomplete",
            "origins_attempted": origins.len(),
            "errors": errors,
        }))
    }
}

/// `GET /api-mining/recipes` — list compact recipe metadata for dashboards and
/// operators without loading response samples into the wire payload.
pub async fn list_recipes(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = RecipeStore::new(api.scoped_base_path(&principal, &workspace));
    match store.list_for_scope(&principal, &workspace) {
        Ok(mut recipes) => {
            recipes.sort_by(|left, right| {
                right
                    .current()
                    .and_then(|version| version.last_replayed_at_ms)
                    .cmp(
                        &left
                            .current()
                            .and_then(|version| version.last_replayed_at_ms),
                    )
                    .then_with(|| left.id.cmp(&right.id))
            });
            let rows: Vec<RecipeMetadata> = recipes
                .into_iter()
                .take(MAX_RECIPE_LIST_ITEMS)
                .filter_map(|recipe| metadata_for_recipe(&recipe))
                .collect();
            HttpResponse::Ok().json(rows)
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to list task recipes: {error}"),
        })),
    }
}

/// `GET /api-mining/recipes/{recipe_id}` — return a complete task recipe.
pub async fn get_recipe(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<RecipePath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = RecipeStore::new(api.scoped_base_path(&principal, &workspace));
    match store.load_for_scope(&path.recipe_id, &principal, &workspace) {
        Ok(Some(recipe)) => HttpResponse::Ok().json(recipe),
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "unknown recipe",
            "recipe_id": path.recipe_id,
        })),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": error.to_string(),
                "recipe_id": path.recipe_id,
            }))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to load task recipe: {error}"),
            "recipe_id": path.recipe_id,
        })),
    }
}

/// `GET /api-mining/recipes/{recipe_id}/runs` — newest-first bounded ledger.
pub async fn list_recipe_runs(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<RecipePath>,
    query: web::Query<RecipeRunsQuery>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let base = api.scoped_base_path(&principal, &workspace);
    match RecipeStore::new(base.clone()).load_for_scope(&path.recipe_id, &principal, &workspace) {
        Ok(Some(_)) => {},
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "unknown recipe",
                "recipe_id": path.recipe_id,
            }));
        },
        Err(error) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": error.to_string(),
                "recipe_id": path.recipe_id,
            }));
        },
    }
    match magician::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(base)
        .list(&path.recipe_id, query.limit)
        .await
    {
        Ok(runs) => HttpResponse::Ok().json(serde_json::json!({ "runs": runs })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("could not read recipe run ledger: {error}"),
            "recipe_id": path.recipe_id,
        })),
    }
}

/// `POST /api-mining/recipes/{recipe_id}/replay` — direct, non-interactive
/// replay. Writes without an existing durable grant return 409; this endpoint
/// never invokes HITL itself.
pub async fn replay_recipe(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<RecipePath>,
    query: web::Query<RecipeReplayQuery>,
    body: web::Json<RecipeRunInputs>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if !api.api_mining_config.recipes.enabled
        || !api
            .api_mining_config
            .recipes
            .transport_ladder
            .iter()
            .any(|transport| transport.eq_ignore_ascii_case("reqwest"))
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "recipe_reqwest_transport_disabled",
            "message": "direct recipe replay requires reqwest in api_mining.recipes.transport_ladder",
        }));
    }
    let base = api.scoped_base_path(&principal, &workspace);
    let store = RecipeStore::new(base.clone());
    let replay_lock = match store.replay_lock(&path.recipe_id) {
        Ok(lock) => lock,
        Err(error) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": error.to_string(),
                "recipe_id": path.recipe_id,
            }));
        },
    };
    // A direct request must not survive the caller's timeout in a queue
    // behind another run's potentially long HITL approval. Reject before any
    // dispatch instead of executing an abandoned manual write later.
    let _replay_guard = match replay_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "recipe_busy",
                "message": "This recipe is already running or being updated. No request was sent; wait for it to finish before starting another run.",
                "recipe_id": path.recipe_id,
            }));
        },
    };
    if !api.api_mining_switch.effective(&principal, &workspace) {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "api_mining_disabled",
            "message": "API mining was disabled before this replay acquired its execution lock",
        }));
    }
    let mut recipe = match store.load_for_scope(&path.recipe_id, &principal, &workspace) {
        Ok(Some(recipe)) => recipe,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "unknown recipe",
                "recipe_id": path.recipe_id,
            }));
        },
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": error.to_string(),
                "recipe_id": path.recipe_id,
            }));
        },
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("failed to load task recipe: {error}"),
                "recipe_id": path.recipe_id,
            }));
        },
    };
    if query
        .expected_version
        .is_some_and(|expected| expected != recipe.current_version)
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "stale_recipe_skill",
            "message": "the emitted recipe skill targets an older recipe version",
            "expected_version": query.expected_version,
            "current_version": recipe.current_version,
        }));
    }
    if query.published_only
        && magician::magician_v2::api_mining::recipe_packs::recipe_to_pack_definition(&recipe)
            .is_none()
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "recipe_not_published",
            "message": "the recipe is no longer Candidate+ or structurally publishable",
            "current_version": recipe.current_version,
        }));
    }

    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let grants = ReplayGrantStore::open(&base);
    let policy = OriginPolicyStore::open(&base);
    let metrics =
        magician::magician_v2::api_mining::recipe_metrics_for_scope(&principal, &workspace);
    if let Some(version) = recipe.current() {
        for step in version
            .steps
            .iter()
            .filter(|step| step.side_effects == SideEffects::Write)
        {
            let grant_key = GrantKey {
                recipe_id: Some(recipe.id.clone()),
                step_id: Some(step.id.clone()),
                capability_id: step.capability_id.clone(),
                request_shape_fingerprint: step.effective_request_shape_fingerprint(),
            };
            if !is_denylisted_url_template(&step.url_template)
                && magician::magician_v2::api_mining::recipe::request_body_shape_is_grantable(
                    step.body_template.as_deref(),
                )
                && grants.lookup(&grant_key).is_some()
            {
                metrics.record_grant_used();
            }
        }
    }
    let session_lookup = |origin: &str, url: &str| {
        secret_store
            .get_session(origin, url)
            .map(|(session, _lease)| session)
    };
    let feedback_sink =
        magician::magician_v2::api_mining::recipe_feedback::RecipeFeedbackSink::for_scope(
            &api.workspace_layout,
            &principal,
            &workspace,
        )
        .map_err(|error| {
            tracing::warn!(%error, "recipe feedback sink unavailable for direct replay");
            error
        })
        .ok();
    let step_feedback = |origin: &str,
                         capability_id: &str,
                         url_template: &str,
                         success: bool,
                         auth_stale: bool,
                         status: u16,
                         response_body: &str| {
        if api.api_mining_switch.effective(&principal, &workspace) {
            if let Some(sink) = &feedback_sink {
                sink.record(
                    origin,
                    capability_id,
                    url_template,
                    success,
                    auth_stale,
                    status,
                    response_body,
                );
            }
        }
    };
    let can_continue = || api.api_mining_switch.effective(&principal, &workspace);
    let runner = RecipeRunner {
        can_continue: Some(&can_continue),
        transports: vec![Box::new(ReqwestTransport::default())],
        grants: &grants,
        origin_policy: &policy,
        session_lookup: &session_lookup,
        auth_healer: None,
        max_auth_heals: 0,
        step_feedback: Some(&step_feedback),
        observer: None,
    };
    let replay_started_at = Instant::now();
    metrics.record_replay_started();
    // One-run write approvals are an in-process capability minted only after
    // HITL. Never let a JSON caller assert them, even if the wire type changes
    // in the future (the field is also serde-skipped in RecipeRunInputs).
    let mut run_inputs = body.into_inner();
    run_inputs.approved_write_steps.clear();
    let result = runner.run(&mut recipe, &run_inputs).await;
    let write_outcome_uncertain = result.write_outcome_uncertain(&recipe);
    if !api.api_mining_switch.effective(&principal, &workspace) {
        return direct_recipe_response(
            result,
            write_outcome_uncertain,
            Some("API mining was disabled during replay; execution outcome is preserved, but no replay state was persisted"),
        );
    }
    for _ in 0..result.auth_heals {
        metrics.record_auth_heal();
    }
    if result.success {
        metrics.record_replay_succeeded();
    } else {
        let failure_class = result
            .failure
            .as_ref()
            .map(|failure| failure.class)
            .or_else(|| result.fallback.as_ref().map(|fallback| fallback.class))
            .or_else(|| {
                result.pending_approval.as_ref().map(|_| {
                    magician::magician_v2::api_mining::recipe_runner::FailureClass::PolicyBlocked
                })
            });
        metrics.record_replay_failed(failure_class);
        if result.fallback.is_some() {
            metrics.record_fallback_handoff();
        }
    }
    let task_id = format!("recipe_{}", recipe.id);
    let execution_id = format!("direct_{}", uuid::Uuid::new_v4().simple());
    match magician::magician_v2::api_mining::recipe_runs::persist_capability_sequence(
        &base,
        &recipe,
        &result,
        &task_id,
        &execution_id,
    ) {
        Ok(Some(_)) => {
            let sequence_metrics = magician::magician_v2::api_mining::sequence_metrics_for_scope(
                &principal, &workspace,
            );
            sequence_metrics.record_started();
            sequence_metrics.record_finalized();
        },
        Ok(None) => {},
        Err(error) => tracing::warn!(
            recipe_id = %recipe.id,
            %error,
            "direct replay sequence persistence failed"
        ),
    }
    let run_record = magician::magician_v2::api_mining::recipe_runs::RecipeRunRecord::from_result(
        &recipe,
        &result,
        task_id,
        execution_id,
        "api",
        replay_started_at.elapsed().as_millis() as u64,
        Vec::new(),
    );
    if let Err(error) = magician::magician_v2::api_mining::recipe_runs::RecipeRunLedger::new(&base)
        .append(&run_record)
        .await
    {
        tracing::warn!(recipe_id = %recipe.id, %error, "direct replay run ledger append failed");
    }
    if let Err(error) = store.save(&recipe) {
        tracing::warn!(recipe_id = %recipe.id, %error, "replay completed but statistics persistence failed");
        return direct_recipe_response(
            result,
            write_outcome_uncertain,
            Some("Recipe statistics could not be saved; do not repeat completed writes"),
        );
    }
    let pack_store = magician::magician_v2::execution::CapabilityPackStore::with_workspace_layout(
        &api.workspace_layout,
        &principal,
        &workspace,
    );
    match magician::magician_v2::api_mining::recipe_packs::publish_recipe_pack(
        &pack_store,
        &api.workspace_layout
            .scope_skills_root(&principal, &workspace),
        &recipe,
    ) {
        Ok(true) => api.invalidate_capability_scope(&principal, &workspace),
        Ok(false) => {},
        Err(error) => {
            tracing::warn!(recipe_id = %recipe.id, %error, "recipe pack upsert failed after direct replay");
        },
    }
    direct_recipe_response(result, write_outcome_uncertain, None)
}

fn direct_recipe_response(
    result: magician::magician_v2::api_mining::recipe_runner::RecipeRunResult,
    write_outcome_uncertain: bool,
    state_warning: Option<&str>,
) -> HttpResponse {
    let conflict = result.pending_approval.is_some() || write_outcome_uncertain;
    let mut body = serde_json::json!(result);
    if let Some(warning) = state_warning {
        body["state_persistence_warning"] = serde_json::json!(warning);
    }
    if write_outcome_uncertain {
        body["effect_uncertain"] = serde_json::json!(true);
        body["retryable"] = serde_json::json!(false);
        body["browser_retry_allowed"] = serde_json::json!(false);
    }
    if conflict {
        HttpResponse::Conflict().json(body)
    } else {
        HttpResponse::Ok().json(body)
    }
}

/// `GET /api-mining/replay-grants` — list durable grants, including revoked
/// records so the audit trail remains visible.
pub async fn list_replay_grants(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    HttpResponse::Ok()
        .json(ReplayGrantStore::open(api.scoped_base_path(&principal, &workspace)).list())
}

/// `DELETE /api-mining/replay-grants/{grant_id}` — revoke one active grant.
pub async fn revoke_replay_grant(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<ReplayGrantPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = ReplayGrantStore::open(api.scoped_base_path(&principal, &workspace));
    match store.revoke(&path.grant_id) {
        Ok(true) => HttpResponse::Ok().json(serde_json::json!({
            "grant_id": path.grant_id,
            "revoked": true,
        })),
        Ok(false) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "unknown or already revoked replay grant",
            "grant_id": path.grant_id,
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to revoke replay grant: {error}"),
            "grant_id": path.grant_id,
        })),
    }
}

/// `GET /api-mining/registry` — filtered registry index. The default excludes
/// hidden records plus relevance classes that have not contributed to a task.
pub async fn get_registry(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    query: web::Query<RegistryQuery>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let relevance = match relevance_filter(&query) {
        Ok(relevance) => relevance,
        Err(error) => {
            return HttpResponse::BadRequest().json(serde_json::json!({ "error": error }));
        },
    };
    match api.load_registry_index(&principal, &workspace) {
        Ok(mut index) => {
            for entry in index.origins.values_mut() {
                entry.capabilities.retain(|capability| {
                    (query.include_hidden || !capability.hidden)
                        && relevance.contains(&capability.relevance)
                });
                entry.capability_count = entry.capabilities.len();
            }
            index
                .origins
                .retain(|_, entry| !entry.capabilities.is_empty());
            HttpResponse::Ok().json(index)
        },
        Err(response) => response,
    }
}

/// `GET /api-mining/overview` — one bounded dashboard payload for recipes,
/// site-grouped APIs, activity counters, auth status and active grants.
pub async fn get_overview(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    query: web::Query<RegistryQuery>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let relevance = match relevance_filter(&query) {
        Ok(relevance) => relevance,
        Err(error) => {
            return HttpResponse::BadRequest().json(serde_json::json!({ "error": error }));
        },
    };
    let base = api.scoped_base_path(&principal, &workspace);
    let registry = match CapabilityRegistry::with_repaired_base_path(&base) {
        Ok(registry) => registry,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("API mining registry could not be opened: {error}")
            }));
        },
    };
    let index = registry.index().clone();
    let trace_stats = api.collect_origin_trace_stats_for_scope(&principal, &workspace);
    let mut sites = BTreeMap::<String, SiteGroup>::new();
    for entry in index.origins.values() {
        if entry.capabilities.is_empty() {
            let site = site_host(&entry.origin_url);
            sites.entry(site.clone()).or_insert_with(|| SiteGroup {
                site,
                origins: vec![overview_origin_entry(entry)],
                capabilities: Vec::new(),
                telemetry_hidden: 0,
            });
            continue;
        }
        for capability in &entry.capabilities {
            let site = site_host(
                capability
                    .parent_origin
                    .as_deref()
                    .unwrap_or(entry.origin_url.as_str()),
            );
            let group = sites.entry(site.clone()).or_insert_with(|| SiteGroup {
                site,
                origins: Vec::new(),
                capabilities: Vec::new(),
                telemetry_hidden: 0,
            });
            if !group
                .origins
                .iter()
                .any(|origin| origin.origin_key == entry.origin_key)
            {
                group.origins.push(overview_origin_entry(entry));
            }
            if (query.include_hidden || !capability.hidden)
                && relevance.contains(&capability.relevance)
            {
                group.capabilities.push(SiteCapability {
                    origin_key: entry.origin_key.clone(),
                    origin_url: entry.origin_url.clone(),
                    capability: capability.clone(),
                });
            }
        }
    }
    for (site, count) in trace_stats.telemetry_by_site {
        let group = sites.entry(site.clone()).or_insert_with(|| SiteGroup {
            site,
            origins: Vec::new(),
            capabilities: Vec::new(),
            telemetry_hidden: 0,
        });
        group.telemetry_hidden = group.telemetry_hidden.saturating_add(count);
    }
    let mut sites: Vec<_> = sites.into_values().collect();
    for site in &mut sites {
        site.origins
            .sort_by(|left, right| left.origin_url.cmp(&right.origin_url));
        site.capabilities.sort_by(|left, right| {
            right
                .capability
                .relevance
                .cmp(&left.capability.relevance)
                .then_with(|| left.capability.name.cmp(&right.capability.name))
                .then_with(|| left.capability.id.cmp(&right.capability.id))
        });
    }
    sort_overview_sites(&mut sites);

    // Keep the aggregate endpoint safe for long-lived workspaces. Ordering is
    // deterministic and origins that own retained capabilities are admitted
    // before observation-only origins, so truncation never makes the visible
    // capability rows unusable while capacity remains.
    let total_sites = sites.len();
    let total_origins: usize = sites.iter().map(|site| site.origins.len()).sum();
    let total_capabilities: usize = sites.iter().map(|site| site.capabilities.len()).sum();
    let omitted_sites = total_sites.saturating_sub(MAX_OVERVIEW_SITES);
    sites.truncate(MAX_OVERVIEW_SITES);
    let mut remaining_capabilities = MAX_OVERVIEW_CAPABILITIES;
    for site in &mut sites {
        let retained = site.capabilities.len().min(remaining_capabilities);
        site.capabilities.truncate(retained);
        remaining_capabilities = remaining_capabilities.saturating_sub(retained);
    }
    let capability_origins: HashSet<_> = sites
        .iter()
        .flat_map(|site| site.capabilities.iter())
        .map(|capability| capability.origin_key.clone())
        .collect();
    let mut remaining_observation_origins =
        MAX_OVERVIEW_ORIGINS.saturating_sub(capability_origins.len());
    for site in &mut sites {
        site.origins.sort_by(|left, right| {
            capability_origins
                .contains(&right.origin_key)
                .cmp(&capability_origins.contains(&left.origin_key))
                .then_with(|| left.origin_url.cmp(&right.origin_url))
        });
        site.origins.retain(|origin| {
            if capability_origins.contains(&origin.origin_key) {
                return true;
            }
            if remaining_observation_origins == 0 {
                return false;
            }
            remaining_observation_origins -= 1;
            true
        });
    }
    let retained_origins: usize = sites.iter().map(|site| site.origins.len()).sum();
    let retained_capabilities: usize = sites.iter().map(|site| site.capabilities.len()).sum();
    let omitted_origins = total_origins.saturating_sub(retained_origins);
    let omitted_capabilities = total_capabilities.saturating_sub(retained_capabilities);

    let recipe_store = RecipeStore::new(base.clone());
    let mut recipes = match recipe_store.list_for_scope(&principal, &workspace) {
        Ok(recipes) => recipes,
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("Task Recipe overview could not be read: {error}")
            }));
        },
    };
    recipes.sort_by(|left, right| {
        right
            .current()
            .and_then(|version| version.last_replayed_at_ms)
            .cmp(
                &left
                    .current()
                    .and_then(|version| version.last_replayed_at_ms),
            )
            .then_with(|| left.id.cmp(&right.id))
    });
    let omitted_recipes = recipes.len().saturating_sub(MAX_RECIPE_LIST_ITEMS);
    let recipes = recipes
        .iter()
        .take(MAX_RECIPE_LIST_ITEMS)
        .filter_map(metadata_for_recipe)
        .collect();
    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let visible_origins: HashMap<_, _> = sites
        .iter()
        .flat_map(|site| site.origins.iter())
        .map(|origin| (origin.origin_key.as_str(), origin.origin_url.as_str()))
        .collect();
    let auth = visible_origins
        .into_iter()
        .map(|(origin_key, origin_url)| {
            (
                origin_key.to_owned(),
                secret_store.captured_status(origin_url),
            )
        })
        .collect();
    let grants = ReplayGrantStore::open(&base).active_count();
    let registry_health = match registry.health_snapshot() {
        Ok(snapshot) => api.enrich_registry_health_snapshot(&base, snapshot),
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("API mining registry health could not be read: {error}")
            }));
        },
    };
    HttpResponse::Ok().json(OverviewResponse {
        recipes,
        sites,
        counters: OverviewCounters {
            router: magician::magician_v2::api_mining::router_snapshot_for_scope(
                &principal, &workspace,
            ),
            passive_validation:
                magician::magician_v2::api_mining::passive_validation_snapshot_for_scope(
                    &principal, &workspace,
                ),
            recipe: magician::magician_v2::api_mining::recipe_snapshot_for_scope(
                &principal, &workspace,
            ),
            projection: magician::magician_v2::api_mining::projection_snapshot_for_scope(
                &principal, &workspace,
            ),
            registry_health,
        },
        auth,
        grants,
        truncation: OverviewTruncation {
            truncated: omitted_recipes > 0
                || omitted_sites > 0
                || omitted_origins > 0
                || omitted_capabilities > 0,
            omitted_recipes,
            omitted_sites,
            omitted_origins,
            omitted_capabilities,
        },
    })
}

/// `GET /api-mining/noisy-origins` — return high-frequency origins for review.
pub async fn get_noisy_origins(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let index = match api.load_registry_index(&principal, &workspace) {
        Ok(index) => index,
        Err(response) => return response,
    };

    let trace_counts = api.collect_origin_trace_counts_for_scope(&principal, &workspace);
    let origin_policy = OriginPolicyStore::open(api.scoped_base_path(&principal, &workspace));
    let capability_counts: HashMap<String, usize> = index
        .origins
        .values()
        .map(|entry| (entry.origin_url.clone(), entry.capability_count))
        .collect();

    let mut origins: Vec<NoisyOriginEntry> = trace_counts
        .into_iter()
        .filter(|(_, trace_count)| *trace_count >= NOISY_ORIGIN_TRACE_THRESHOLD)
        .map(|(origin_url, trace_count)| NoisyOriginEntry {
            origin_key: CapabilityStore::origin_to_key(&origin_url),
            capability_count: capability_counts.get(&origin_url).copied().unwrap_or(0),
            origin_url: origin_url.clone(),
            trace_count,
            decision: origin_policy.decision_for_origin(&origin_url),
        })
        .collect();

    origins.sort_by(|left, right| {
        right
            .trace_count
            .cmp(&left.trace_count)
            .then_with(|| left.origin_url.cmp(&right.origin_url))
    });

    HttpResponse::Ok().json(NoisyOriginsResponse {
        threshold: NOISY_ORIGIN_TRACE_THRESHOLD,
        origins,
    })
}

/// `GET /api-mining/capabilities/{origin_key}/{capability_id}` — return full
/// `ApiCapability` as JSON.
pub async fn get_capability(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<CapabilityPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let origin_url = match api.resolve_origin_url(&principal, &workspace, &path.origin_key) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };

    match CapabilityRegistry::with_base_path(api.scoped_base_path(&principal, &workspace))
        .and_then(|registry| registry.get_capability(&origin_url, &path.capability_id))
    {
        Ok(capability) => HttpResponse::Ok().json(capability),
        Err(err) => HttpResponse::NotFound().json(serde_json::json!({
            "error": err,
            "origin_key": path.origin_key,
            "capability_id": path.capability_id
        })),
    }
}

/// `POST /api-mining/capabilities/{origin_key}/{capability_id}/bless` —
/// operator-driven promotion of a capability to `Validated` confidence.
///
/// Bypasses the organic `replay_success_count` ladder so an operator
/// can authorize a mined capability for replay-driven flows without
/// waiting for the agent to invoke it N times. Useful for seeding the
/// capability evolution pipeline against well-known read-only
/// endpoints (e.g. a Metabase card the operator confirms is safe to
/// memoize) or for unblocking a candidate the agent never picks up
/// because the LLM hasn't surfaced the right tool yet.
///
/// Idempotent: calling on an already-Validated or Trusted capability
/// is a no-op. Cannot demote — operators must use the existing demote
/// path for that.
pub async fn bless_capability(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<CapabilityPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let origin_url = match api.resolve_origin_url(&principal, &workspace, &path.origin_key) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };

    let store = CapabilityStore::with_base_path(api.scoped_base_path(&principal, &workspace));
    let mut capability = match store.load(&origin_url, &path.capability_id) {
        Ok(cap) => cap,
        Err(err) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": err,
                "origin_key": path.origin_key,
                "capability_id": path.capability_id
            }));
        },
    };

    let previous_confidence = format!("{:?}", capability.confidence);
    let already_validated = matches!(
        capability.confidence,
        magician::magician_v2::api_mining::capability::ConfidenceLevel::Validated
            | magician::magician_v2::api_mining::capability::ConfidenceLevel::Trusted
    );
    if !already_validated {
        capability.confidence =
            magician::magician_v2::api_mining::capability::ConfidenceLevel::Validated;
        capability.updated_at = chrono::Utc::now().timestamp();
    }

    if let Err(err) = store.save(&capability) {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to persist blessed capability: {err}"),
            "origin_key": path.origin_key,
            "capability_id": path.capability_id
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({
        "origin_key": path.origin_key,
        "capability_id": path.capability_id,
        "previous_confidence": previous_confidence,
        "new_confidence": format!("{:?}", capability.confidence),
        "no_op": already_validated,
    }))
}

/// `GET /api-mining/sequences/{origin_key}` — list captured sequences for
/// an origin as `SequenceMetadata` (step bodies excluded to keep response
/// size bounded across long-running scopes). Empty array when no sequences
/// have been captured yet.
pub async fn list_sequences(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<SequenceListPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = magician::magician_v2::api_mining::sequence_store::SequenceStore::new(
        api.scoped_base_path(&principal, &workspace),
    );
    match store.list(&path.origin_key) {
        Ok(sequences) => {
            let metadata: Vec<SequenceMetadata> = sequences
                .into_iter()
                .map(|s| SequenceMetadata {
                    id: s.id,
                    task_id: s.task_id,
                    execution_id: s.execution_id,
                    origin_key: s.origin_key,
                    step_count: s.steps.len(),
                    captured_at_ms: s.captured_at_ms,
                })
                .collect();
            HttpResponse::Ok().json(metadata)
        },
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to list sequences: {}", err),
            "origin_key": path.origin_key,
        })),
    }
}

/// `GET /api-mining/sequences/{origin_key}/{sequence_id}` — full
/// `CapabilitySequence` JSON (steps + response bodies). 404 when the
/// sequence id is not found in the origin's directory.
pub async fn get_sequence(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<SequenceDetailPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = magician::magician_v2::api_mining::sequence_store::SequenceStore::new(
        api.scoped_base_path(&principal, &workspace),
    );
    match store.load(&path.origin_key, &path.sequence_id) {
        Ok(Some(sequence)) => HttpResponse::Ok().json(sequence),
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "sequence not found",
            "origin_key": path.origin_key,
            "sequence_id": path.sequence_id,
        })),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to load sequence: {}", err),
            "origin_key": path.origin_key,
            "sequence_id": path.sequence_id,
        })),
    }
}

/// `GET /api-mining/workflows/{origin_key}` — list compiled workflows for
/// an origin as `WorkflowMetadata` (step bodies excluded). Empty array when
/// no workflows have been compiled yet (auto-trigger fires only when 2+
/// sequences exist per origin).
pub async fn list_workflows(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<WorkflowListPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = magician::magician_v2::api_mining::workflow_store::WorkflowStore::new(
        api.scoped_base_path(&principal, &workspace),
    );
    match store.list(&path.origin_key) {
        Ok(workflows) => {
            let metadata: Vec<WorkflowMetadata> = workflows
                .into_iter()
                .map(|w| WorkflowMetadata {
                    id: w.id,
                    origin_key: w.origin_key,
                    name: w.name,
                    step_count: w.steps.len(),
                    maturity: w.confidence.workflow_level,
                    last_compiled_at_ms: w.last_compiled_at_ms,
                })
                .collect();
            HttpResponse::Ok().json(metadata)
        },
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to list workflows: {}", err),
            "origin_key": path.origin_key,
        })),
    }
}

/// Process-wide replay lock registry. Serializes concurrent replays of
/// the same (principal, workspace, origin_key, workflow_id) tuple so the
/// load-mutate-save sequence for `replay_stats` + `maturity` can't race
/// and lose updates. Different workflows still run in parallel.
///
/// Per-tuple `tokio::sync::Mutex` lives in this static map for the
/// process lifetime; restart resets. Cross-process replays would need
/// file locks via `fs2` — out of scope for v1 since the only replay
/// surface is HTTP and HTTP runs in a single magician process.
fn replay_lock_for(
    principal: &str,
    workspace: &str,
    origin_key: &str,
    workflow_id: &str,
) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::sync::Mutex as TokioMutex;

    static REPLAY_LOCKS: OnceLock<
        Mutex<HashMap<(String, String, String, String), Arc<TokioMutex<()>>>>,
    > = OnceLock::new();
    let registry = REPLAY_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (
        principal.to_string(),
        workspace.to_string(),
        origin_key.to_string(),
        workflow_id.to_string(),
    );
    let mut guard = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        guard
            .entry(key)
            .or_insert_with(|| Arc::new(TokioMutex::new(()))),
    )
}

/// `POST /api-mining/workflows/{origin_key}/{workflow_id}/replay` —
/// operator-driven workflow replay. Executes the compiled WorkflowGraph
/// step-by-step via direct HTTP, persists the updated workflow back to
/// disk (replay_stats + maturity), and returns the ReplayResult.
///
/// Concurrent replays of the same workflow are serialized via a
/// process-wide per-tuple Mutex so the load-mutate-save sequence can't
/// race and lose `replay_stats` increments. Different workflows still
/// run in parallel.
pub async fn replay_workflow(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<WorkflowDetailPath>,
    body: web::Json<magician::magician_v2::api_mining::workflow_replay::ReplayInputs>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // Acquire the per-workflow lock BEFORE loading so the load/mutate/save
    // sequence is serialized for this (principal, workspace, origin, wf).
    let lock = replay_lock_for(&principal, &workspace, &path.origin_key, &path.workflow_id);
    let _guard = lock.lock().await;
    let base = api.scoped_base_path(&principal, &workspace);
    let store = magician::magician_v2::api_mining::workflow_store::WorkflowStore::new(base.clone());
    let mut workflow = match store.load(&path.origin_key, &path.workflow_id) {
        Ok(Some(w)) => w,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "workflow not found",
                "origin_key": path.origin_key,
                "workflow_id": path.workflow_id,
            }));
        },
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("failed to load workflow: {}", err),
            }));
        },
    };

    if let Some(block) = workflow_direct_replay_policy_block(&base, &workflow) {
        let (status, reason) = match block {
            DirectReplayPolicyBlock::RequiresHitl { reason } => {
                (actix_web::http::StatusCode::CONFLICT, reason)
            },
            DirectReplayPolicyBlock::Denied { reason } => {
                (actix_web::http::StatusCode::FORBIDDEN, reason)
            },
        };
        return HttpResponse::build(status).json(serde_json::json!({
            "error": reason,
            "origin_key": path.origin_key,
            "workflow_id": path.workflow_id,
        }));
    }

    let runner = match magician::magician_v2::api_mining::replay::ApiRunner::with_base_path(&base) {
        Ok(r) => std::sync::Arc::new(tokio::sync::Mutex::new(r)),
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("failed to construct ApiRunner: {}", err),
            }));
        },
    };
    let executor = std::sync::Arc::new(
        magician::magician_v2::api_mining::workflow_replay::step_executor::StepExecutor::new(
            runner,
        ),
    );
    let replay_metrics =
        magician::magician_v2::api_mining::replay_metrics_for_scope(&principal, &workspace);
    let engine =
        magician::magician_v2::api_mining::workflow_replay::WorkflowReplayEngine::new(executor)
            .with_metrics(replay_metrics);

    let (session_ctx, session_values) = match api.scoped_secret_store(&principal, &workspace) {
        Ok(secret_store) => {
            let origin_url = api
                .resolve_origin_url(&principal, &workspace, &workflow.origin_key)
                .unwrap_or_else(|| workflow.origin_key.clone());
            let session_ctx = secret_store
                .get_session(&origin_url, &origin_url)
                .map(|(session, _lease)| session)
                .unwrap_or_default();
            let session_values = session_values_from_context(&session_ctx);
            (session_ctx, session_values)
        },
        Err(response) => return response,
    };

    let result = engine
        .replay(
            &mut workflow,
            &body.into_inner(),
            session_values,
            &session_ctx,
        )
        .await;

    // Persist the updated workflow regardless of result — stats + maturity
    // are mutated on both success and failure paths.
    if let Err(err) = store.save(&workflow) {
        tracing::warn!(
            "workflow_replay: failed to persist updated workflow {}: {}",
            workflow.id,
            err
        );
    }

    HttpResponse::Ok().json(result)
}

/// `GET /api-mining/workflow-metrics` — see the workflow compilation
/// counters; replay metrics are exposed at `/api-mining/replay-metrics`.
pub async fn get_replay_metrics(api: web::Data<ApiMiningApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let snapshot =
        magician::magician_v2::api_mining::replay_snapshot_for_scope(&principal, &workspace);
    HttpResponse::Ok().json(snapshot)
}

/// `GET /api-mining/workflows/{origin_key}/{workflow_id}` — full
/// `WorkflowGraph` JSON. 404 when the workflow id is not found.
pub async fn get_workflow(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<WorkflowDetailPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store = magician::magician_v2::api_mining::workflow_store::WorkflowStore::new(
        api.scoped_base_path(&principal, &workspace),
    );
    match store.load(&path.origin_key, &path.workflow_id) {
        Ok(Some(workflow)) => HttpResponse::Ok().json(workflow),
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "workflow not found",
            "origin_key": path.origin_key,
            "workflow_id": path.workflow_id,
        })),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to load workflow: {}", err),
            "origin_key": path.origin_key,
            "workflow_id": path.workflow_id,
        })),
    }
}

/// `GET /api-mining/auth-status/{origin_key}` — return non-secret auth
/// metadata for an origin.
pub async fn get_auth_status(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<AuthStatusPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let origin_url = match api.resolve_origin_url(&principal, &workspace, &path.origin_key) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };

    let store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let status = store.captured_status(&origin_url);

    HttpResponse::Ok().json(status)
}

/// `GET /api-mining/auth-statuses` — return auth status for all origins in a single response.
/// Keys in the response are origin_keys (filesystem-safe) for frontend compatibility.
pub async fn get_all_auth_statuses(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let registry =
        match CapabilityRegistry::with_base_path(api.scoped_base_path(&principal, &workspace)) {
            Ok(registry) => registry,
            Err(error) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": error
                }));
            },
        };
    let origins = registry.index().origins.clone();
    let store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };

    let mut statuses = HashMap::new();
    for (origin_key, entry) in &origins {
        statuses.insert(origin_key.clone(), store.captured_status(&entry.origin_url));
    }

    HttpResponse::Ok().json(statuses)
}

/// `POST /api-mining/origins/{origin_key}/refresh-auth` — start a deterministic
/// CDP session that captures fresh auth from the signed-in browser profile.
pub async fn refresh_origin_auth(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let origin_url = match api.resolve_origin_url(&principal, &workspace, &path.origin_key) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let parsed = match url::Url::parse(&origin_url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => parsed,
        _ => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "origin URL must be http or https",
                "origin_key": path.origin_key,
                "origin_url": origin_url,
            }));
        },
    };

    if let Some(status) = api.active_auth_refresh_status(&principal, &workspace, &path.origin_key) {
        return HttpResponse::Ok().json(status);
    }

    let origin_url = extract_origin(parsed.as_str());
    let now = chrono::Utc::now().timestamp_millis();
    let refresh_id = uuid::Uuid::new_v4().to_string();
    let status = AuthRefreshStatus {
        refresh_id: refresh_id.clone(),
        origin_key: path.origin_key.clone(),
        origin_url: origin_url.clone(),
        phase: AuthRefreshPhase::Starting,
        message: "Starting a secure browser capture session.".to_string(),
        started_at_ms: now,
        updated_at_ms: now,
        terminal: false,
        verification_status: None,
    };
    api.insert_auth_refresh_status(&principal, &workspace, status.clone());

    let api_for_refresh = api.get_ref().clone();
    let origin_key = path.origin_key.clone();
    actix_web::rt::spawn(async move {
        run_origin_auth_refresh(
            api_for_refresh,
            principal,
            workspace,
            origin_key,
            origin_url,
            refresh_id,
        )
        .await;
    });

    HttpResponse::Accepted().json(status)
}

/// `GET /api-mining/origins/{origin_key}/refresh-auth/{refresh_id}` — poll a
/// scoped CDP auth-refresh session without exposing captured values.
pub async fn get_origin_auth_refresh_status(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<AuthRefreshStatusPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.get_auth_refresh_status(&principal, &workspace, &path.origin_key, &path.refresh_id) {
        Some(status) => HttpResponse::Ok().json(status),
        None => HttpResponse::NotFound().json(serde_json::json!({
            "error": "auth refresh session not found",
            "origin_key": path.origin_key,
            "refresh_id": path.refresh_id,
        })),
    }
}

async fn run_origin_auth_refresh(
    api: ApiMiningApi,
    principal: String,
    workspace: String,
    origin_key: String,
    origin_url: String,
    refresh_id: String,
) {
    if !api.api_mining_switch.effective(&principal, &workspace) {
        api.update_auth_refresh_status(
            &principal,
            &workspace,
            &refresh_id,
            AuthRefreshPhase::Failed,
            "API mining was turned off before authentication capture started.",
            None,
        );
        return;
    }
    let magicutor_base_url = match url::Url::parse(&api.magicutor_base_url) {
        Ok(mut url) if matches!(url.scheme(), "http" | "https") => {
            if !url.path().ends_with('/') {
                let path = format!("{}/", url.path());
                url.set_path(&path);
            }
            url.set_query(None);
            url.set_fragment(None);
            url
        },
        _ => {
            api.update_auth_refresh_status(
                &principal,
                &workspace,
                &refresh_id,
                AuthRefreshPhase::Failed,
                "The configured Magicutor base URL is not a valid HTTP or HTTPS URL.",
                None,
            );
            return;
        },
    };
    let cli_path = match AgentBrowserSession::resolve_cli_path_for_scope(
        Some(api.workspace_layout.base_root()),
        Some(&principal),
        Some(&workspace),
    ) {
        Ok(path) => path,
        Err(error) => {
            api.update_auth_refresh_status(
                &principal,
                &workspace,
                &refresh_id,
                AuthRefreshPhase::Failed,
                format!("Could not start the secure browser: {error}"),
                None,
            );
            return;
        },
    };
    let session_id = format!("magician-auth-refresh-{}", refresh_id.replace('-', ""));
    let connection_mode = match magicutor_cdp_connection_mode(&magicutor_base_url, &session_id) {
        Ok(mode) => mode,
        Err(error) => {
            api.update_auth_refresh_status(
                &principal,
                &workspace,
                &refresh_id,
                AuthRefreshPhase::Failed,
                format!("Could not resolve the configured browser capture service: {error}"),
                None,
            );
            return;
        },
    };
    let session =
        match AgentBrowserSession::new_with_session_id(session_id, connection_mode, cli_path) {
            Ok(session) => Arc::new(
                session
                    .with_analytics_context(
                        BrowserEngineAnalyticsContext::for_scope(
                            api.workspace_layout.base_root(),
                            &principal,
                            &workspace,
                            None,
                            None,
                        )
                        .with_work("api_auth_refresh", refresh_id.clone()),
                    )
                    .with_initial_url(Some(origin_url.clone())),
            ),
            Err(error) => {
                api.update_auth_refresh_status(
                    &principal,
                    &workspace,
                    &refresh_id,
                    AuthRefreshPhase::Failed,
                    format!("Could not create the secure browser session: {error}"),
                    None,
                );
                return;
            },
        };
    let Some(magicutor) = api.magicutor_client.clone() else {
        api.update_auth_refresh_status(
            &principal,
            &workspace,
            &refresh_id,
            AuthRefreshPhase::Failed,
            "The browser capture service is not configured for API Mining.",
            None,
        );
        return;
    };

    if let Err(error) = session.ensure_connected().await {
        api.update_auth_refresh_status(
            &principal,
            &workspace,
            &refresh_id,
            AuthRefreshPhase::Failed,
            format!("Could not open the signed-in browser profile: {error}"),
            None,
        );
        cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
        return;
    }

    api.update_auth_refresh_status(
        &principal,
        &workspace,
        &refresh_id,
        AuthRefreshPhase::WaitingForAuth,
        "Waiting for an authenticated request from this origin. Navigate in the opened window if the page does not load protected data automatically.",
        None,
    );

    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(_) => {
            api.update_auth_refresh_status(
                &principal,
                &workspace,
                &refresh_id,
                AuthRefreshPhase::Failed,
                "Could not open the encrypted captured-auth store.",
                None,
            );
            cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
            return;
        },
    };
    let browser_auth_requirements = browser_auth_capture_requirements(
        &api.scoped_base_path(&principal, &workspace),
        &origin_url,
    );

    let started = Instant::now();
    let mut last_drain_error = None;
    let mut last_snapshot_at = None;
    while started.elapsed() < AUTH_REFRESH_TIMEOUT {
        if !api.api_mining_switch.effective(&principal, &workspace) {
            let _ = secret_store.clear_captured(&origin_url);
            api.update_auth_refresh_status(
                &principal,
                &workspace,
                &refresh_id,
                AuthRefreshPhase::Failed,
                "Authentication capture stopped because API mining was turned off.",
                None,
            );
            cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
            return;
        }
        let mut captured = false;
        match magicutor.drain_captured_auth(session.session_id()).await {
            Ok(events) => {
                last_drain_error = None;
                if !api.api_mining_switch.effective(&principal, &workspace) {
                    let _ = secret_store.clear_captured(&origin_url);
                    api.update_auth_refresh_status(
                        &principal,
                        &workspace,
                        &refresh_id,
                        AuthRefreshPhase::Failed,
                        "Authentication capture stopped because API mining was turned off.",
                        None,
                    );
                    cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
                    return;
                }
                match persist_captured_auth_events(
                    &events,
                    secret_store.as_ref(),
                    Some(&origin_url),
                ) {
                    Ok(origins) if origins.contains(&origin_url) => captured = true,
                    Ok(_) => {},
                    Err(error) => {
                        api.update_auth_refresh_status(
                            &principal,
                            &workspace,
                            &refresh_id,
                            AuthRefreshPhase::Failed,
                            format!("Could not save the captured authentication: {error}"),
                            None,
                        );
                        cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
                        return;
                    },
                }
            },
            Err(error) => last_drain_error = Some(error.to_string()),
        }

        if last_snapshot_at
            .map(|last: Instant| last.elapsed() >= Duration::from_secs(2))
            .unwrap_or(true)
        {
            last_snapshot_at = Some(Instant::now());
            match capture_browser_auth_snapshot(
                &session,
                secret_store.as_ref(),
                &origin_url,
                &browser_auth_requirements,
            )
            .await
            {
                Ok(snapshot_captured) => captured |= snapshot_captured,
                Err(error) => last_drain_error = Some(error),
            }
            if !api.api_mining_switch.effective(&principal, &workspace) {
                let _ = secret_store.clear_captured(&origin_url);
                api.update_auth_refresh_status(
                    &principal,
                    &workspace,
                    &refresh_id,
                    AuthRefreshPhase::Failed,
                    "Authentication capture stopped because API mining was turned off.",
                    None,
                );
                cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
                return;
            }
        }

        if captured {
            api.update_auth_refresh_status(
                &principal,
                &workspace,
                &refresh_id,
                AuthRefreshPhase::Captured,
                "Captured fresh authentication securely.",
                None,
            );
            verify_refreshed_auth(
                &api,
                &principal,
                &workspace,
                &origin_key,
                &origin_url,
                &refresh_id,
                secret_store.as_ref(),
            )
            .await;
            cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
            return;
        }
        tokio::time::sleep(AUTH_REFRESH_POLL_INTERVAL).await;
    }

    if !api.api_mining_switch.effective(&principal, &workspace) {
        let _ = secret_store.clear_captured(&origin_url);
        api.update_auth_refresh_status(
            &principal,
            &workspace,
            &refresh_id,
            AuthRefreshPhase::Failed,
            "Authentication capture stopped because API mining was turned off.",
            None,
        );
        cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
        return;
    }

    let message = last_drain_error.map_or_else(
        || {
            "No authenticated request was observed within 60 seconds. Use the opened browser window to load a protected page, then retry."
                .to_string()
        },
        |error| format!("The browser capture service did not provide auth material: {error}"),
    );
    api.update_auth_refresh_status(
        &principal,
        &workspace,
        &refresh_id,
        AuthRefreshPhase::TimedOut,
        message,
        None,
    );
    cleanup_auth_refresh_session(&session, magicutor.as_ref()).await;
}

fn magicutor_cdp_connection_mode(
    base_url: &url::Url,
    session_id: &str,
) -> Result<ConnectionMode, String> {
    magician::magician_v2::api_mining::auth_refresh::magicutor_cdp_connection_mode(
        base_url, session_id,
    )
}

async fn cleanup_auth_refresh_session(
    session: &Arc<AgentBrowserSession>,
    magicutor: &MagicutorClient,
) {
    let _ = session.shutdown().await;
    let _ = magicutor.delete_session(session.session_id()).await;
}

async fn capture_browser_auth_snapshot(
    session: &AgentBrowserSession,
    secret_store: &SecretStore,
    origin_url: &str,
    requirements: &BrowserAuthCaptureRequirements,
) -> Result<bool, String> {
    magician::magician_v2::api_mining::auth_refresh::capture_browser_auth_snapshot(
        session,
        secret_store,
        origin_url,
        requirements,
    )
    .await
}

fn browser_auth_capture_requirements(
    base_path: &std::path::Path,
    origin_url: &str,
) -> BrowserAuthCaptureRequirements {
    let Ok(registry) = CapabilityRegistry::with_base_path(base_path) else {
        return BrowserAuthCaptureRequirements::default();
    };
    let Ok(capabilities) = registry.get_all_for_origin(origin_url) else {
        return BrowserAuthCaptureRequirements::default();
    };

    let mut requirements = BrowserAuthCaptureRequirements::default();
    for capability in capabilities {
        requirements
            .cookies
            .extend(capability.auth_requirements.cookies);
        requirements
            .local_storage_keys
            .extend(capability.auth_requirements.local_storage_keys);
        requirements
            .session_storage_keys
            .extend(capability.auth_requirements.session_storage_keys);
    }
    requirements
}

async fn verify_refreshed_auth(
    api: &ApiMiningApi,
    principal: &str,
    workspace: &str,
    origin_key: &str,
    origin_url: &str,
    refresh_id: &str,
    secret_store: &SecretStore,
) {
    if !api.api_mining_switch.effective(principal, workspace) {
        let _ = secret_store.clear_captured(origin_url);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::Failed,
            "Authentication verification stopped because API mining was turned off.",
            None,
        );
        return;
    }
    let base_path = api.scoped_base_path(principal, workspace);
    let mut registry = match CapabilityRegistry::with_base_path(&base_path) {
        Ok(registry) => registry,
        Err(error) => {
            api.update_auth_refresh_status(
                principal,
                workspace,
                refresh_id,
                AuthRefreshPhase::CapturedUnverified,
                format!("Captured fresh authentication; verification was skipped because the API registry could not be opened: {error}"),
                None,
            );
            return;
        },
    };
    let capabilities = match registry.get_all_for_origin(origin_url) {
        Ok(capabilities) => capabilities,
        Err(error) => {
            api.update_auth_refresh_status(
                principal,
                workspace,
                refresh_id,
                AuthRefreshPhase::CapturedUnverified,
                format!("Captured fresh authentication; verification was skipped because capabilities could not be loaded: {error}"),
                None,
            );
            return;
        },
    };
    let Some(mut capability) = select_auth_verification_capability(capabilities, secret_store)
    else {
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::CapturedUnverified,
            "Captured fresh authentication. No concrete read-only capability was available for automatic verification.",
            None,
        );
        return;
    };
    if direct_replay_policy_block(
        &base_path,
        origin_url,
        &capability.effective_side_effects(),
        &capability.confidence,
    )
    .is_some()
    {
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::CapturedUnverified,
            "Captured fresh authentication. Automatic verification was skipped by the origin replay policy.",
            None,
        );
        return;
    }

    let Some((session, lease)) = secret_store.get_session(origin_url, &capability.url_template)
    else {
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::VerificationFailed,
            "Authentication was captured, but it could not be assembled for the verification request.",
            None,
        );
        return;
    };
    let replay_request = match build_replay_request(&capability, &HashMap::new(), &session) {
        Ok(request) => request,
        Err(error) => {
            api.update_auth_refresh_status(
                principal,
                workspace,
                refresh_id,
                AuthRefreshPhase::CapturedUnverified,
                format!("Captured fresh authentication; the verification request was not concrete: {error}"),
                None,
            );
            return;
        },
    };
    api.update_auth_refresh_status(
        principal,
        workspace,
        refresh_id,
        AuthRefreshPhase::Verifying,
        "Verifying the captured authentication with a safe GET request.",
        None,
    );

    let mut request = api
        .http_client
        .get(&replay_request.url)
        .timeout(Duration::from_millis(replay_request.timeout_ms));
    for (name, value) in &replay_request.headers {
        request = request.header(name, value);
    }
    if !api.api_mining_switch.effective(principal, workspace) {
        let _ = secret_store.clear_captured(origin_url);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::Failed,
            "Authentication verification stopped because API mining was turned off.",
            None,
        );
        return;
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            if !api.api_mining_switch.effective(principal, workspace) {
                let _ = secret_store.clear_captured(origin_url);
                api.update_auth_refresh_status(
                    principal,
                    workspace,
                    refresh_id,
                    AuthRefreshPhase::Failed,
                    "Authentication verification stopped because API mining was turned off.",
                    None,
                );
                return;
            }
            capability.record_replay_failure();
            let _ = registry.register(&capability);
            api.update_auth_refresh_status(
                principal,
                workspace,
                refresh_id,
                AuthRefreshPhase::VerificationFailed,
                format!("Authentication was captured, but verification could not reach the API: {error}"),
                None,
            );
            return;
        },
    };
    if !api.api_mining_switch.effective(principal, workspace) {
        let _ = secret_store.clear_captured(origin_url);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::Failed,
            "Authentication verification stopped because API mining was turned off.",
            None,
        );
        return;
    }
    let status = response.status().as_u16();
    if response.status().is_success() {
        capability.record_replay_success();
        let _ = secret_store.mark_fresh_lease(&lease);
        let _ = registry.register(&capability);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::Verified,
            "Fresh authentication captured and verified.",
            Some(status),
        );
    } else if response_marks_auth_stale(status) {
        capability.record_auth_failure();
        let _ = secret_store.mark_stale_lease(&lease);
        let _ = registry.register(&capability);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::VerificationFailed,
            "The API rejected the newly captured authentication with HTTP 401.",
            Some(status),
        );
    } else if status == 403 {
        capability.record_auth_failure();
        let _ = registry.register(&capability);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::VerificationFailed,
            "Authentication was captured, but the verification request returned HTTP 403. This may be a permission, tenant, CSRF, or required-header problem rather than an expired login.",
            Some(status),
        );
    } else {
        capability.record_replay_failure();
        let _ = registry.register(&capability);
        api.update_auth_refresh_status(
            principal,
            workspace,
            refresh_id,
            AuthRefreshPhase::VerificationFailed,
            format!("Authentication was captured, but verification returned HTTP {status}."),
            Some(status),
        );
    }
    tracing::info!(
        origin_key,
        status,
        phase = ?api.get_auth_refresh_status(principal, workspace, origin_key, refresh_id).map(|value| value.phase),
        "api_mining.auth_refresh.completed"
    );
}

fn select_auth_verification_capability(
    mut capabilities: Vec<ApiCapability>,
    secret_store: &SecretStore,
) -> Option<ApiCapability> {
    capabilities.sort_by(|left, right| {
        right
            .sample_count
            .cmp(&left.sample_count)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    capabilities.into_iter().find(|capability| {
        capability.method.eq_ignore_ascii_case("GET")
            && capability.is_replayable()
            && capability.effective_side_effects() == SideEffects::ReadOnly
            && (!capability.auth_requirements.headers.is_empty()
                || !capability.auth_requirements.cookies.is_empty()
                || !capability.auth_requirements.query_params.is_empty()
                || !capability.auth_requirements.local_storage_keys.is_empty()
                || !capability.auth_requirements.session_storage_keys.is_empty())
            && secret_store
                .get_session(&capability.origin, &capability.url_template)
                .is_some_and(|(session, _)| {
                    build_replay_request(capability, &HashMap::new(), &session).is_ok()
                })
    })
}

/// `POST /api-mining/origins/{origin_key}/allow` — mark a noisy origin as allowed.
pub async fn allow_origin(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
    body: web::Json<Option<OriginActionRequest>>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let requested_origin_url = body.into_inner().map(|value| value.origin_url);
    let origin_url = match api.resolve_action_origin_url(
        &principal,
        &workspace,
        &path.origin_key,
        requested_origin_url.as_deref(),
    ) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let origin_policy = OriginPolicyStore::open(api.scoped_base_path(&principal, &workspace));

    match origin_policy.set_decision(&origin_url, OriginPolicyDecision::Allowed) {
        Ok(entry) => HttpResponse::Ok().json(OriginPolicyActionResponse {
            origin_key: entry.origin_key,
            origin_url: entry.origin_url,
            decision: entry.decision,
            allow_replay: entry.allow_replay,
            replay_mode: entry.replay_mode,
        }),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err,
            "origin_key": path.origin_key
        })),
    }
}

/// `POST /api-mining/origins/{origin_key}/block` — block future capture/mining for an origin.
pub async fn block_origin(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
    body: web::Json<Option<OriginActionRequest>>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let requested_origin_url = body.into_inner().map(|value| value.origin_url);
    let origin_url = match api.resolve_action_origin_url(
        &principal,
        &workspace,
        &path.origin_key,
        requested_origin_url.as_deref(),
    ) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let origin_policy = OriginPolicyStore::open(api.scoped_base_path(&principal, &workspace));

    match origin_policy.set_decision(&origin_url, OriginPolicyDecision::Blocked) {
        Ok(entry) => HttpResponse::Ok().json(OriginPolicyActionResponse {
            origin_key: entry.origin_key,
            origin_url: entry.origin_url,
            decision: entry.decision,
            allow_replay: entry.allow_replay,
            replay_mode: entry.replay_mode,
        }),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err,
            "origin_key": path.origin_key
        })),
    }
}

/// `POST /api-mining/origins/{origin_key}/allow-replay` — opt this
/// origin in (or out) of inline auto-replay during mining. Mining
/// fires safety-gated GET/HEAD replays against Candidate capabilities
/// only when this flag is `true`. Defaults to `false` for every origin
/// until the operator explicitly flips it.
pub async fn set_origin_allow_replay(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
    body: web::Json<AllowReplayRequest>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let payload = body.into_inner();
    let origin_url = match api.resolve_action_origin_url(
        &principal,
        &workspace,
        &path.origin_key,
        payload.origin_url.as_deref(),
    ) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let origin_policy = OriginPolicyStore::open(api.scoped_base_path(&principal, &workspace));

    match origin_policy.set_allow_replay(&origin_url, payload.allow_replay) {
        Ok(entry) => HttpResponse::Ok().json(OriginPolicyActionResponse {
            origin_key: entry.origin_key,
            origin_url: entry.origin_url,
            decision: entry.decision,
            allow_replay: entry.allow_replay,
            replay_mode: entry.replay_mode,
        }),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err,
            "origin_key": path.origin_key
        })),
    }
}

/// `POST /api-mining/origins/{origin_key}/replay-mode` — set the
/// precise replay policy for live API takeover. This supersedes the
/// older boolean `allow-replay` control while keeping that endpoint as
/// a compatibility shim (`true` => `replay_reads`, `false` =>
/// `validate_only`).
pub async fn set_origin_replay_mode(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
    body: web::Json<SetReplayModeRequest>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let payload = body.into_inner();
    let origin_url = match api.resolve_action_origin_url(
        &principal,
        &workspace,
        &path.origin_key,
        payload.origin_url.as_deref(),
    ) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let origin_policy = OriginPolicyStore::open(api.scoped_base_path(&principal, &workspace));

    match origin_policy.set_replay_mode(&origin_url, payload.replay_mode) {
        Ok(entry) => HttpResponse::Ok().json(OriginPolicyActionResponse {
            origin_key: entry.origin_key,
            origin_url: entry.origin_url,
            decision: entry.decision,
            allow_replay: entry.allow_replay,
            replay_mode: entry.replay_mode,
        }),
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err,
            "origin_key": path.origin_key
        })),
    }
}

/// `POST /api-mining/origins/{origin_key}/purge` — delete existing mined artifacts for an origin.
pub async fn purge_origin(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
    body: web::Json<Option<OriginActionRequest>>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let requested_origin_url = body.into_inner().map(|value| value.origin_url);
    let origin_url = match api.resolve_action_origin_url(
        &principal,
        &workspace,
        &path.origin_key,
        requested_origin_url.as_deref(),
    ) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let base_path = api.scoped_base_path(&principal, &workspace);
    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };

    let mut report = OriginPurgeReport::default();
    if let Err(error) = api
        .purge_recipe_state_for_origin(
            &principal,
            &workspace,
            &origin_url,
            &secret_store,
            &mut report,
        )
        .await
    {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error,
            "origin_key": path.origin_key,
            "partial_report": report,
        }));
    }

    if let Ok(mut registry) = CapabilityRegistry::with_base_path(&base_path) {
        let _ = registry.remove_origin(&origin_url);
        let _ = registry.rebuild();
    }

    HttpResponse::Ok().json(PurgeOriginResponse {
        origin_key: path.origin_key.clone(),
        origin_url,
        report,
    })
}

/// `POST /api-mining/origins/{origin_key}/block-and-purge` — block future capture and purge current data.
pub async fn block_and_purge_origin(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OriginPath>,
    body: web::Json<Option<OriginActionRequest>>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let requested_origin_url = body.into_inner().map(|value| value.origin_url);
    let origin_url = match api.resolve_action_origin_url(
        &principal,
        &workspace,
        &path.origin_key,
        requested_origin_url.as_deref(),
    ) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };
    let base_path = api.scoped_base_path(&principal, &workspace);
    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let origin_policy = OriginPolicyStore::open(&base_path);

    if let Err(err) = origin_policy.set_decision(&origin_url, OriginPolicyDecision::Blocked) {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": err,
            "origin_key": path.origin_key
        }));
    }

    let mut report = OriginPurgeReport::default();
    if let Err(error) = api
        .purge_recipe_state_for_origin(
            &principal,
            &workspace,
            &origin_url,
            &secret_store,
            &mut report,
        )
        .await
    {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error,
            "origin_key": path.origin_key,
            "partial_report": report,
        }));
    }

    if let Ok(mut registry) = CapabilityRegistry::with_base_path(&base_path) {
        let _ = registry.remove_origin(&origin_url);
        let _ = registry.rebuild();
    }

    HttpResponse::Ok().json(PurgeOriginResponse {
        origin_key: path.origin_key.clone(),
        origin_url,
        report,
    })
}

/// `POST /api-mining/replay/{origin_key}/{capability_id}` — replay a learned
/// API capability with optional parameter/header/body overrides.
pub async fn replay_capability(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<CapabilityPath>,
    body: web::Json<Option<ReplayRequestBody>>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_enabled_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let overrides = body.into_inner().unwrap_or_default();
    let start = std::time::Instant::now();

    // 1. Resolve origin_key → origin_url
    let origin_url = match api.resolve_origin_url(&principal, &workspace, &path.origin_key) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(ReplayResponse {
                status: 0,
                headers: None,
                body: None,
                elapsed_ms: 0,
                auth_was_stale: false,
                confidence_after: String::new(),
                error: Some(format!("Unknown origin key: {}", path.origin_key)),
            });
        },
    };

    // 2. Load full ApiCapability from the registry
    let scoped_base_path = api.scoped_base_path(&principal, &workspace);
    let secret_store = match api.scoped_secret_store(&principal, &workspace) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let mut registry = match CapabilityRegistry::with_base_path(&scoped_base_path) {
        Ok(registry) => registry,
        Err(err) => {
            return HttpResponse::InternalServerError().json(ReplayResponse {
                status: 0,
                headers: None,
                body: None,
                elapsed_ms: 0,
                auth_was_stale: false,
                confidence_after: String::new(),
                error: Some(err),
            });
        },
    };

    let mut capability = match registry.get_capability(&origin_url, &path.capability_id) {
        Ok(cap) => cap,
        Err(err) => {
            return HttpResponse::NotFound().json(ReplayResponse {
                status: 0,
                headers: None,
                body: None,
                elapsed_ms: 0,
                auth_was_stale: false,
                confidence_after: String::new(),
                error: Some(err),
            });
        },
    };

    // 2b. Enforce confidence-based replay guard (SSRF protection)
    if !capability.is_replayable() {
        return HttpResponse::Forbidden().json(ReplayResponse {
            status: 0,
            headers: None,
            body: None,
            elapsed_ms: 0,
            auth_was_stale: false,
            confidence_after: serde_json::to_string(&capability.confidence).unwrap_or_default(),
            error: Some(format!(
                "Capability not replayable at confidence {:?} (requires {:?})",
                capability.confidence,
                capability.min_replay_confidence()
            )),
        });
    }

    let side_effects = capability.effective_side_effects();
    if let Some(block) = direct_replay_policy_block(
        &scoped_base_path,
        &origin_url,
        &side_effects,
        &capability.confidence,
    ) {
        let (status, reason) = match block {
            DirectReplayPolicyBlock::RequiresHitl { reason } => {
                (actix_web::http::StatusCode::CONFLICT, reason)
            },
            DirectReplayPolicyBlock::Denied { reason } => {
                (actix_web::http::StatusCode::FORBIDDEN, reason)
            },
        };
        return HttpResponse::build(status).json(ReplayResponse {
            status: 0,
            headers: None,
            body: None,
            elapsed_ms: start.elapsed().as_millis() as u64,
            auth_was_stale: false,
            confidence_after: serde_json::to_string(&capability.confidence).unwrap_or_default(),
            error: Some(reason),
        });
    }

    // 3. Resolve the concrete URL first, then load captured auth for that exact replay URL.
    let (concrete_url, session, captured_session_lease) = match manual_replay_session_for_url(
        &secret_store,
        &origin_url,
        &capability.url_template,
        &overrides.parameter_overrides,
    ) {
        Ok(result) => result,
        Err(err) => {
            return HttpResponse::BadRequest().json(ReplayResponse {
                status: 0,
                headers: None,
                body: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                auth_was_stale: false,
                confidence_after: serde_json::to_string(&capability.confidence).unwrap_or_default(),
                error: Some(format!("Failed to resolve replay URL: {}", err)),
            });
        },
    };

    // 4. Build a ReplayRequest
    let replay_req =
        match build_replay_request(&capability, &overrides.parameter_overrides, &session) {
            Ok(r) => r,
            Err(err) => {
                return HttpResponse::BadRequest().json(ReplayResponse {
                    status: 0,
                    headers: None,
                    body: None,
                    elapsed_ms: start.elapsed().as_millis() as u64,
                    auth_was_stale: false,
                    confidence_after: serde_json::to_string(&capability.confidence)
                        .unwrap_or_default(),
                    error: Some(format!("Failed to build replay request: {}", err)),
                });
            },
        };
    debug_assert_eq!(replay_req.url, concrete_url);

    // 5. Apply optional header/body overrides
    let mut final_headers = replay_req.headers.clone();
    for (key, value) in &overrides.headers_overrides {
        final_headers.insert(key.clone(), value.clone());
    }
    let final_body = overrides.body_override.or(replay_req.body);

    // 6. Execute via reqwest
    let method = match replay_req.method.to_uppercase().as_str() {
        "GET" => reqwest::Method::GET,
        "HEAD" => reqwest::Method::HEAD,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        "OPTIONS" => reqwest::Method::OPTIONS,
        other => {
            return HttpResponse::BadRequest().json(ReplayResponse {
                status: 0,
                headers: None,
                body: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                auth_was_stale: false,
                confidence_after: serde_json::to_string(&capability.confidence).unwrap_or_default(),
                error: Some(format!("Unsupported HTTP method: {}", other)),
            });
        },
    };

    let mut req_builder = api
        .http_client
        .request(method, &replay_req.url)
        .timeout(std::time::Duration::from_millis(replay_req.timeout_ms));

    for (key, value) in &final_headers {
        req_builder = req_builder.header(key, value);
    }
    if let Some(ref body_str) = final_body {
        req_builder = req_builder.body(body_str.clone());
    }

    let response = match req_builder.send().await {
        Ok(resp) => resp,
        Err(err) => {
            // Record failure in registry
            capability.record_replay_failure();
            let confidence_after =
                serde_json::to_string(&capability.confidence).unwrap_or_default();
            let _ = registry.register(&capability);

            return HttpResponse::Ok().json(ReplayResponse {
                status: 0,
                headers: None,
                body: None,
                elapsed_ms: start.elapsed().as_millis() as u64,
                auth_was_stale: false,
                confidence_after,
                error: Some(format!("Request failed: {}", err)),
            });
        },
    };

    let elapsed_ms = start.elapsed().as_millis() as u64;
    let resp_status = response.status().as_u16();

    // Collect response headers
    let resp_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect();

    let resp_body = response.text().await.ok();

    // A 401 is an authentication failure. A 403 can instead represent valid
    // auth with insufficient permission, tenant context, CSRF state, or another
    // required application header, so it must not poison the captured lease.
    let auth_was_stale = response_marks_auth_stale(resp_status);
    if auth_was_stale {
        capability.record_auth_failure();
        if let Some(ref lease) = captured_session_lease {
            let _ = secret_store.mark_stale_lease(lease);
        }
    } else if resp_status == 403 {
        capability.record_auth_failure();
    } else if (200..300).contains(&resp_status) {
        capability.record_replay_success();
        if let Some(ref lease) = captured_session_lease {
            let _ = secret_store.mark_fresh_lease(lease);
        }
    } else {
        capability.record_replay_failure();
    }

    let confidence_after = serde_json::to_string(&capability.confidence).unwrap_or_default();

    // Persist updated capability to registry
    let _ = registry.register(&capability);

    // 9. Return the response
    HttpResponse::Ok().json(ReplayResponse {
        status: resp_status,
        headers: Some(resp_headers),
        body: resp_body,
        elapsed_ms,
        auth_was_stale,
        confidence_after,
        error: None,
    })
}

/// `GET /api-mining/openapi/{origin_key}` — generate and return an OpenAPI 3.0
/// spec (JSON) for all capabilities belonging to the given origin.
pub async fn get_openapi(
    api: web::Data<ApiMiningApi>,
    req: HttpRequest,
    path: web::Path<OpenApiPath>,
) -> impl Responder {
    let (principal, workspace) = match api.resolve_required_scope(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let origin_url = match api.resolve_origin_url(&principal, &workspace, &path.origin_key) {
        Some(url) => url,
        None => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Unknown origin key",
                "origin_key": path.origin_key
            }));
        },
    };

    let capabilities =
        match CapabilityRegistry::with_base_path(api.scoped_base_path(&principal, &workspace))
            .and_then(|registry| registry.get_all_for_origin(&origin_url))
        {
            Ok(caps) => caps,
            Err(err) => {
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": err,
                    "origin_key": path.origin_key
                }));
            },
        };

    let spec = generate_openapi_spec(&origin_url, &capabilities);
    HttpResponse::Ok().json(spec)
}

#[cfg(test)]
mod tests {

    use super::{
        direct_replay_policy_block, manual_replay_session_for_url, overview_origin_entry,
        response_marks_auth_stale, select_auth_verification_capability,
        session_values_from_context, sort_overview_sites, workflow_direct_replay_policy_block,
        AuthRefreshPhase, DirectReplayPolicyBlock, SequenceMetadata, SiteCapability, SiteGroup,
    };

    use actix_web::{test as actix_test, web, App};
    use magician::magician_v2::api_mining::capability::{
        ApiCapability, ConfidenceLevel, SideEffects,
    };
    use magician::magician_v2::api_mining::origin_policy::{OriginPolicyStore, OriginReplayMode};
    use magician::magician_v2::api_mining::recipe::{
        request_shape_fingerprint, AnswerField, CompiledFrom, Extractor, RecipeAuth,
        RecipeParamSource, RecipeShape, RecipeStep, RecipeVersion, TaskInput, TaskInputSchema,
        TaskInputSource, TaskRecipe,
    };
    use magician::magician_v2::api_mining::recipe_store::RecipeStore;
    use magician::magician_v2::api_mining::registry::CapabilityRegistry;
    use magician::magician_v2::api_mining::sequence::{
        CapabilitySequence, ExecutionPath, SequenceStep,
    };
    use magician::magician_v2::api_mining::sequence_store::SequenceStore;
    use magician::magician_v2::api_mining::types::SessionContext;
    use magician::magician_v2::api_mining::workflow::{
        AuthRequirements, ReplayStats, WorkflowConfidence, WorkflowGraph, WorkflowMaturity,
        WorkflowStep,
    };
    use magician::magician_v2::secrets::{
        CookieWithMetadata, InMemoryKeyProvider, SameSite, SecretRuntimeCapabilities, SecretStore,
        SecretStoreResolver,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn sample_sequence(id: &str, origin: &str, n_steps: usize) -> CapabilitySequence {
        let steps: Vec<SequenceStep> = (0..n_steps)
            .map(|i| SequenceStep {
                step_index: i,
                capability_id: Some("cap_a".to_string()),
                origin: format!("https://{origin}"),
                concrete_url: format!("https://{origin}/api/x/{i}"),
                method: "GET".to_string(),
                request_params: HashMap::new(),
                request_body: None,
                response_status: Some(200),
                response_body: Some("{}".to_string()),
                action_binding_id: None,
                browser_action_desc: None,
                browser_action: None,
                browser_arguments: None,
                executed_via: ExecutionPath::ApiReplay,
                timestamp_ms: 1_780_000_000_000,
                duration_ms: 1,
            })
            .collect();
        CapabilitySequence {
            id: id.to_string(),
            task_id: "task_x".to_string(),
            execution_id: "exec_x".to_string(),
            origin_key: origin.to_string(),
            steps,
            captured_at_ms: 1_780_000_000_000,
            finalized: true,
        }
    }

    #[test]
    fn overview_origin_metadata_does_not_duplicate_capability_summaries() {
        let capability = ApiCapability::new(
            "items".into(),
            "https://api.example.test".into(),
            "GET".into(),
            "https://api.example.test/items".into(),
        )
        .to_summary();
        let entry = magician::magician_v2::api_mining::registry::OriginEntry {
            origin_key: "api_example_test".into(),
            origin_url: "https://api.example.test".into(),
            capability_count: 1,
            trace_count: 3,
            capabilities: vec![capability],
            updated_at: 42,
        };

        let overview = overview_origin_entry(&entry);

        assert!(overview.capabilities.is_empty());
        assert_eq!(overview.capability_count, 1);
        assert_eq!(overview.trace_count, 3);
        assert_eq!(entry.capabilities.len(), 1);
    }

    #[test]
    fn overview_bounds_prioritize_sites_with_retained_capabilities() {
        let capability = ApiCapability::new(
            "items".into(),
            "https://z-useful.test".into(),
            "GET".into(),
            "https://z-useful.test/items".into(),
        )
        .to_summary();
        let mut sites = vec![
            SiteGroup {
                site: "a-observation-only.test".into(),
                origins: Vec::new(),
                capabilities: Vec::new(),
                telemetry_hidden: 4,
            },
            SiteGroup {
                site: "z-useful.test".into(),
                origins: Vec::new(),
                capabilities: vec![SiteCapability {
                    origin_key: "z-useful.test".into(),
                    origin_url: "https://z-useful.test".into(),
                    capability,
                }],
                telemetry_hidden: 0,
            },
        ];

        sort_overview_sites(&mut sites);

        assert_eq!(sites[0].site, "z-useful.test");
    }

    #[actix_web::test]
    async fn direct_recipe_replay_returns_conflict_before_an_ungranted_write() {
        let dir = tempdir().expect("temp runtime root");
        let root = dir.path().to_path_buf();
        let resolver = Arc::new(SecretStoreResolver::new_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            root.clone(),
            SecretRuntimeCapabilities::fully_available("in_memory"),
        ));
        let api =
            super::ApiMiningApi::new(root, resolver, magician::config::ApiMiningConfig::default());
        let origin = "https://write.example";
        let url = format!("{origin}/api/cart/items");
        let recipe = TaskRecipe {
            id: "recipe_write_contract".to_string(),
            scope_principal: "test".to_string(),
            scope_workspace: "test".to_string(),
            agent_id: "personal-assistant".to_string(),
            shape: RecipeShape {
                description_template: None,
                template: "add item to cart".to_string(),
                fingerprint: "shape".to_string(),
                inputs: vec![TaskInput {
                    name: "item".into(),
                    schema: TaskInputSchema::String,
                    example_value: "private-example-value".into(),
                    source: TaskInputSource::BrowserTyped,
                }],
            },
            current_version: 1,
            versions: vec![RecipeVersion {
                version: 1,
                origins: vec![origin.to_string()],
                steps: vec![
                    RecipeStep {
                        id: "write".to_string(),
                        origin: origin.to_string(),
                        method: "POST".to_string(),
                        url_template: url.clone(),
                        headers_template: HashMap::new(),
                        body_template: Some(r#"{"item":"{item}"}"#.to_string()),
                        capability_id: None,
                        param_sources: HashMap::from([(
                            "item".to_string(),
                            RecipeParamSource::TaskInput {
                                name: "item".to_string(),
                            },
                        )]),
                        body_param_types: HashMap::from([(
                            "item".to_string(),
                            TaskInputSchema::String,
                        )]),
                        side_effects: SideEffects::Write,
                        request_shape_fingerprint: request_shape_fingerprint(
                            "POST",
                            &url,
                            Some(r#"{"item":"{item}"}"#),
                        ),
                        verify_with: Some("verify".to_string()),
                        browser_fallback: None,
                        transport_hint: None,
                    },
                    RecipeStep {
                        id: "verify".to_string(),
                        origin: origin.to_string(),
                        method: "GET".to_string(),
                        url_template: format!("{url}?item={{item}}"),
                        headers_template: HashMap::new(),
                        body_template: None,
                        capability_id: None,
                        param_sources: HashMap::from([(
                            "item".to_string(),
                            RecipeParamSource::TaskInput {
                                name: "item".to_string(),
                            },
                        )]),
                        body_param_types: HashMap::new(),
                        side_effects: SideEffects::ReadOnly,
                        request_shape_fingerprint: request_shape_fingerprint(
                            "GET",
                            &format!("{url}?item={{item}}"),
                            None,
                        ),
                        verify_with: None,
                        browser_fallback: None,
                        transport_hint: None,
                    },
                ],
                data_flows: Vec::new(),
                answer_spec: vec![AnswerField {
                    field: "item".to_string(),
                    step_id: "verify".to_string(),
                    extractor: Extractor::JsonPath {
                        path: "$.item".to_string(),
                    },
                }],
                auth: RecipeAuth::default(),
                maturity: WorkflowMaturity::Draft,
                replay_stats: ReplayStats::default(),
                compiled_from: CompiledFrom {
                    task_id: "task".to_string(),
                    execution_id: "execution".to_string(),
                    task_text_fingerprint: None,
                    monitor_revision: None,
                    sequence_ids: Vec::new(),
                    trace_files: Vec::new(),
                },
                compiled_at_ms: 1,
                last_replayed_at_ms: None,
            }],
        };
        let metadata =
            serde_json::to_string(&super::metadata_for_recipe(&recipe).unwrap()).unwrap();
        assert!(metadata.contains("item"));
        assert!(!metadata.contains("private-example-value"));
        let recipe_store = RecipeStore::new(api.scoped_base_path("test", "test"));
        recipe_store.save(&recipe).expect("save write recipe");

        let app = actix_test::init_service(App::new().app_data(web::Data::new(api)).route(
            "/api/magician/v2/api-mining/recipes/{recipe_id}/replay",
            web::post().to(super::replay_recipe),
        ))
        .await;
        let lock = recipe_store.replay_lock(&recipe.id).unwrap();
        let held = lock.lock().await;
        let busy_request = actix_test::TestRequest::post()
            .uri("/api/magician/v2/api-mining/recipes/recipe_write_contract/replay")
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .set_json(serde_json::json!({"inputs": {"item": "public-runtime-value"}}))
            .to_request();
        let busy = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            actix_test::call_service(&app, busy_request),
        )
        .await
        .expect("busy direct replay must not wait for the execution lock");
        assert_eq!(busy.status(), actix_web::http::StatusCode::CONFLICT);
        let busy_body: serde_json::Value = actix_test::read_body_json(busy).await;
        assert_eq!(busy_body["error"], "recipe_busy");
        assert!(busy_body.get("pending_approval").is_none());
        drop(held);
        let request = actix_test::TestRequest::post()
            .uri("/api/magician/v2/api-mining/recipes/recipe_write_contract/replay")
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .set_json(serde_json::json!({
                "inputs": {"item": "public-runtime-value"},
                "approved_write_steps": ["write"]
            }))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::CONFLICT);
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["pending_approval"]["step_id"], "write");

        let unpublished_request = actix_test::TestRequest::post()
			.uri(
				"/api/magician/v2/api-mining/recipes/recipe_write_contract/replay?published_only=true&expected_version=1",
			)
			.insert_header(("X-Principal", "test"))
			.insert_header(("X-Workspace", "test"))
			.set_json(serde_json::json!({
				"inputs": {"item": "public-runtime-value"}
			}))
			.to_request();
        let unpublished_response = actix_test::call_service(&app, unpublished_request).await;
        assert_eq!(
            unpublished_response.status(),
            actix_web::http::StatusCode::CONFLICT
        );
        let unpublished_body: serde_json::Value =
            actix_test::read_body_json(unpublished_response).await;
        assert_eq!(unpublished_body["error"], "recipe_not_published");

        let stale_request = actix_test::TestRequest::post()
			.uri(
				"/api/magician/v2/api-mining/recipes/recipe_write_contract/replay?published_only=true&expected_version=2",
			)
			.insert_header(("X-Principal", "test"))
			.insert_header(("X-Workspace", "test"))
			.set_json(serde_json::json!({
				"inputs": {"item": "public-runtime-value"}
			}))
			.to_request();
        let stale_response = actix_test::call_service(&app, stale_request).await;
        assert_eq!(
            stale_response.status(),
            actix_web::http::StatusCode::CONFLICT
        );
        let stale_body: serde_json::Value = actix_test::read_body_json(stale_response).await;
        assert_eq!(stale_body["error"], "stale_recipe_skill");

        let mut misplaced = recipe;
        misplaced.scope_principal = "another-principal".into();
        recipe_store.save(&misplaced).unwrap();
        let request = actix_test::TestRequest::post()
            .uri("/api/magician/v2/api-mining/recipes/recipe_write_contract/replay")
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .set_json(serde_json::json!({"inputs": {"item": "oranges"}}))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert!(body["error"].as_str().unwrap().contains("durable scope"));
        assert!(body.get("pending_approval").is_none());
    }

    #[actix_web::test]
    async fn replay_persistence_warning_never_masks_success_or_uncertain_write() {
        use magician::magician_v2::api_mining::recipe_runner::{
            FailureClass, RecipeRunFailure, RecipeRunResult,
        };
        for uncertain in [false, true] {
            let result = RecipeRunResult {
                success: !uncertain,
                auth_heals: 0,
                answer: serde_json::Map::from_iter([("item".into(), serde_json::json!("saved"))]),
                steps: vec![],
                fallback: None,
                pending_approval: None,
                failure: uncertain.then(|| RecipeRunFailure {
                    step_id: "write".into(),
                    class: FailureClass::Network,
                    detail: "timeout".into(),
                }),
            };
            let response =
                super::direct_recipe_response(result, uncertain, Some("statistics unavailable"));
            assert_eq!(
                response.status(),
                if uncertain {
                    actix_web::http::StatusCode::CONFLICT
                } else {
                    actix_web::http::StatusCode::OK
                }
            );
            let bytes = actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["success"], !uncertain);
            assert_eq!(body["state_persistence_warning"], "statistics unavailable");
            if uncertain {
                assert_eq!(body["retryable"], false);
                assert_eq!(body["browser_retry_allowed"], false);
                assert_eq!(body["failure"]["step_id"], "write");
            } else {
                assert_eq!(body["answer"]["item"], "saved");
            }
        }
    }

    #[actix_web::test]
    async fn api_mining_off_keeps_reads_available_and_blocks_direct_recipe_replay() {
        let dir = tempdir().expect("temp runtime root");
        let root = dir.path().to_path_buf();
        let resolver = Arc::new(SecretStoreResolver::new_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            root.clone(),
            SecretRuntimeCapabilities::fully_available("in_memory"),
        ));
        let api =
            super::ApiMiningApi::new(root, resolver, magician::config::ApiMiningConfig::default());
        api.api_mining_switch
            .set_scope("test", "test", Some(false))
            .expect("disable API mining for test scope");

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .route(
                    "/api/magician/v2/api-mining/recipes",
                    web::get().to(super::list_recipes),
                )
                .route(
                    "/api/magician/v2/api-mining/recipes/{recipe_id}/replay",
                    web::post().to(super::replay_recipe),
                ),
        )
        .await;

        let read_request = actix_test::TestRequest::get()
            .uri("/api/magician/v2/api-mining/recipes")
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .to_request();
        let read_response = actix_test::call_service(&app, read_request).await;
        assert_eq!(read_response.status(), actix_web::http::StatusCode::OK);

        let replay_request = actix_test::TestRequest::post()
            .uri("/api/magician/v2/api-mining/recipes/missing/replay")
            .insert_header(("X-Principal", "test"))
            .insert_header(("X-Workspace", "test"))
            .set_json(serde_json::json!({"inputs": {}}))
            .to_request();
        let replay_response = actix_test::call_service(&app, replay_request).await;
        assert_eq!(
            replay_response.status(),
            actix_web::http::StatusCode::CONFLICT
        );
        let body: serde_json::Value = actix_test::read_body_json(replay_response).await;
        assert_eq!(body["error"], "api_mining_disabled");
    }

    #[actix_web::test]
    async fn non_owner_cannot_mutate_the_process_switch() {
        let dir = tempdir().expect("temp runtime root");
        let root = dir.path().to_path_buf();
        let resolver = Arc::new(SecretStoreResolver::new_with_capabilities(
            Box::new(InMemoryKeyProvider::new()),
            root.clone(),
            SecretRuntimeCapabilities::fully_available("in_memory"),
        ));
        let api =
            super::ApiMiningApi::new(root, resolver, magician::config::ApiMiningConfig::default());
        let app = actix_test::init_service(App::new().app_data(web::Data::new(api)).route(
            "/api/magician/v2/api-mining/settings",
            web::put().to(super::put_api_mining_settings),
        ))
        .await;

        let request = actix_test::TestRequest::put()
            .uri("/api/magician/v2/api-mining/settings")
            .insert_header(("X-Principal", "not-owner"))
            .insert_header(("X-Workspace", "default"))
            .set_json(serde_json::json!({"enabled": false, "layer": "process"}))
            .to_request();
        let response = actix_test::call_service(&app, request).await;
        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
        let body: serde_json::Value = actix_test::read_body_json(response).await;
        assert_eq!(body["error"], "owner_required");
    }

    #[test]
    fn session_values_from_context_exposes_auth_aliases_for_workflow_replay() {
        let mut session = SessionContext::default();
        session
            .auth_headers
            .insert("Authorization".to_string(), "Bearer token-abc".to_string());
        session
            .auth_headers
            .insert("X-CSRF-Token".to_string(), "csrf-123".to_string());
        session
            .auth_query_params
            .insert("api_key".to_string(), "query-secret".to_string());
        session
            .cookies
            .insert("sid".to_string(), "cookie-secret".to_string());
        session
            .local_storage
            .insert("authToken".to_string(), "local-secret".to_string());

        let values = session_values_from_context(&session);

        assert_eq!(
            values.get("authorization").map(String::as_str),
            Some("Bearer token-abc")
        );
        assert_eq!(
            values.get("bearer").map(String::as_str),
            Some("Bearer token-abc")
        );
        assert_eq!(
            values.get("bearer_token").map(String::as_str),
            Some("token-abc")
        );
        assert_eq!(
            values.get("x-csrf-token").map(String::as_str),
            Some("csrf-123")
        );
        assert_eq!(
            values.get("query:api_key").map(String::as_str),
            Some("query-secret")
        );
        assert_eq!(
            values.get("cookie:sid").map(String::as_str),
            Some("cookie-secret")
        );
        assert_eq!(
            values.get("local_storage:authToken").map(String::as_str),
            Some("local-secret")
        );
    }

    #[test]
    fn direct_replay_policy_blocks_write_replay_that_requires_hitl() {
        let temp = tempdir().unwrap();
        let origin = "https://api.example.com";
        OriginPolicyStore::open(temp.path())
            .set_replay_mode(origin, OriginReplayMode::ReplayWritesWithHitl)
            .unwrap();

        let block = direct_replay_policy_block(
            temp.path(),
            origin,
            &SideEffects::Write,
            &ConfidenceLevel::Validated,
        );

        match block {
            Some(DirectReplayPolicyBlock::RequiresHitl { reason }) => {
                assert!(reason.contains("requires HITL approval"));
            },
            other => panic!("expected HITL-required direct replay block, got {other:?}"),
        }
    }

    #[test]
    fn workflow_direct_replay_policy_blocks_write_step_that_requires_hitl() {
        let temp = tempdir().unwrap();
        let origin = "https://api.example.com";
        OriginPolicyStore::open(temp.path())
            .set_replay_mode(origin, OriginReplayMode::ReplayWritesWithHitl)
            .unwrap();

        let mut capability = ApiCapability::new(
            "place_order".to_string(),
            origin.to_string(),
            "POST".to_string(),
            format!("{origin}/orders"),
        );
        capability.add_sample("req-2".to_string());
        capability.add_sample("req-3".to_string());
        let capability_id = capability.id.clone();
        CapabilityRegistry::with_base_path(temp.path())
            .unwrap()
            .register(&capability)
            .unwrap();

        let workflow = WorkflowGraph {
            id: "workflow-order".to_string(),
            origin_key: "api.example.com".to_string(),
            name: "Order workflow".to_string(),
            steps: vec![WorkflowStep {
                id: "step-place-order".to_string(),
                step_index: 0,
                capability_id: Some(capability_id.clone()),
                param_sources: HashMap::new(),
                skip_if: None,
                browser_only: false,
                browser_fallback: None,
            }],
            data_flows: Vec::new(),
            auth_requirements: AuthRequirements::default(),
            confidence: WorkflowConfidence {
                workflow_level: WorkflowMaturity::Candidate,
                step_confidences: HashMap::new(),
            },
            compiled_from_sequence_ids: vec!["seq-1".to_string()],
            last_compiled_at_ms: 1,
            last_replayed_at_ms: None,
            replay_stats: ReplayStats::default(),
        };

        match workflow_direct_replay_policy_block(temp.path(), &workflow) {
            Some(DirectReplayPolicyBlock::RequiresHitl { reason }) => {
                assert!(reason.contains("step-place-order"));
                assert!(reason.contains(&capability_id));
            },
            other => panic!("expected workflow HITL-required block, got {other:?}"),
        }
    }

    #[test]
    fn list_metadata_omits_step_bodies() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        store
            .save(&sample_sequence("seq_1", "example.com", 3))
            .unwrap();

        let listed = store.list("example.com").unwrap();
        let metadata: Vec<SequenceMetadata> = listed
            .into_iter()
            .map(|s| SequenceMetadata {
                id: s.id,
                task_id: s.task_id,
                execution_id: s.execution_id,
                origin_key: s.origin_key,
                step_count: s.steps.len(),
                captured_at_ms: s.captured_at_ms,
            })
            .collect();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].step_count, 3);
        let json = serde_json::to_value(&metadata[0]).unwrap();
        assert!(json.get("steps").is_none()); // not a field on SequenceMetadata
    }

    #[test]
    fn detail_lookup_returns_full_sequence() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        store
            .save(&sample_sequence("seq_1", "example.com", 2))
            .unwrap();
        let loaded = store.load("example.com", "seq_1").unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().steps.len(), 2);
    }

    #[test]
    fn detail_lookup_missing_returns_none() {
        let dir = tempdir().unwrap();
        let store = SequenceStore::new(dir.path().to_path_buf());
        let loaded = store.load("example.com", "seq_nope").unwrap();
        assert!(loaded.is_none());
    }

    fn test_secret_store() -> SecretStore {
        let dir = tempfile::tempdir().expect("tempdir should exist");
        let path = dir.keep();
        SecretStore::new_empty(Box::new(InMemoryKeyProvider::new()), path)
    }

    #[test]
    fn manual_replay_session_uses_concrete_url_for_cookie_selection() {
        let store = test_secret_store();
        store
            .store_captured(
                "https://api.example.com",
                HashMap::new(),
                vec![CookieWithMetadata {
                    name: "session".to_string(),
                    value: "order-cookie".to_string(),
                    domain: "api.example.com".to_string(),
                    path: "/orders/123".to_string(),
                    secure: true,
                    http_only: true,
                    same_site: SameSite::Lax,
                    expires: None,
                }],
                HashMap::new(),
                HashMap::new(),
            )
            .expect("captured cookies should store");

        let (concrete_url, session, lease) = manual_replay_session_for_url(
            &store,
            "https://api.example.com",
            "https://api.example.com/orders/{id}",
            &HashMap::from([("id".to_string(), "123".to_string())]),
        )
        .expect("manual replay session should resolve");

        assert_eq!(concrete_url, "https://api.example.com/orders/123");
        assert_eq!(
            session.cookie_header_string().as_deref(),
            Some("session=order-cookie")
        );
        assert_eq!(
            lease
                .expect("exact-origin session should carry a stale-mark lease")
                .stale_mark_targets()
                .len(),
            1
        );
    }

    #[test]
    fn only_unauthorized_marks_captured_auth_stale() {
        assert!(response_marks_auth_stale(401));
        assert!(!response_marks_auth_stale(403));
        assert!(!response_marks_auth_stale(500));
    }

    #[test]
    fn auth_refresh_terminal_phases_do_not_stop_during_verification() {
        assert!(!AuthRefreshPhase::Starting.is_terminal());
        assert!(!AuthRefreshPhase::WaitingForAuth.is_terminal());
        assert!(!AuthRefreshPhase::Captured.is_terminal());
        assert!(!AuthRefreshPhase::Verifying.is_terminal());
        assert!(AuthRefreshPhase::CapturedUnverified.is_terminal());
        assert!(AuthRefreshPhase::Verified.is_terminal());
        assert!(AuthRefreshPhase::VerificationFailed.is_terminal());
        assert!(AuthRefreshPhase::TimedOut.is_terminal());
        assert!(AuthRefreshPhase::Failed.is_terminal());
    }

    #[test]
    fn auth_refresh_cdp_url_uses_configured_magicutor_origin_and_session() {
        let base_url = url::Url::parse("https://magicutor.internal:3443/proxy/")
            .expect("configured Magicutor URL should parse");
        let mode = super::magicutor_cdp_connection_mode(&base_url, "refresh-123")
            .expect("CDP URL should resolve");
        match mode {
            super::ConnectionMode::Cdp { url } => assert_eq!(
                url,
                "wss://magicutor.internal:3443/proxy/devtools/browser/refresh-123"
            ),
            _ => panic!("auth refresh must use CDP mode"),
        }
    }

    #[test]
    fn auth_refresh_selects_concrete_authenticated_get() {
        let origin = "https://app.example.com";
        let store = test_secret_store();
        store
            .store_captured(
                origin,
                HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer captured-token".to_string(),
                )]),
                Vec::new(),
                HashMap::new(),
                HashMap::new(),
            )
            .expect("captured auth should store");

        let mut concrete = ApiCapability::new(
            "current user".to_string(),
            origin.to_string(),
            "GET".to_string(),
            format!("{origin}/api/me"),
        );
        concrete.auth_requirements.headers = vec!["Authorization".to_string()];
        concrete.add_sample("trace-2".to_string());
        concrete.add_sample("trace-3".to_string());

        let mut unresolved = ApiCapability::new(
            "employee".to_string(),
            origin.to_string(),
            "GET".to_string(),
            format!("{origin}/api/employees/{{id}}"),
        );
        unresolved.auth_requirements.headers = vec!["Authorization".to_string()];
        unresolved.add_sample("trace-4".to_string());
        unresolved.add_sample("trace-5".to_string());
        unresolved.add_sample("trace-6".to_string());

        let selected = select_auth_verification_capability(vec![unresolved, concrete], &store)
            .expect("concrete authenticated GET should be selected");
        assert_eq!(selected.url_template, format!("{origin}/api/me"));
    }
}
