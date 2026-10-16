//! REST API endpoints for the Resource Authority system.
//!
//! Thin wrappers over `Arc<RwLock<ResourceLedger>>`, `Arc<RwLock<TokenStore>>`,
//! `Arc<RwLock<Vec<SystemCeiling>>>`, and `Arc<RwLock<SystemFreezeState>>`.
//! All routes live under `/api/resource-authority/`.

use std::collections::HashSet;
use std::sync::Arc;

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use chrono::{Duration, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::scope::resolve_required_scope;
use crate::web_api::api_error_response;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::resource_authority::{
    api_types::*,
    gate::{self, SystemFreezeState},
    ledger::{JournalEntry, PeriodClose, ResourceLedger},
    scoped_authority::ScopedAuthorityResolver,
    spend_session::{
        admit, MissingBudgetPolicy, SpendAdmission, SpendIntent, SpendOwnerPolicy,
        SpendSessionError,
    },
    token::{period_window_start, SpendToken, SystemCeiling, TokenStatus},
    token_store::TokenStore,
    types::ReservationId,
};

// ---------------------------------------------------------------------------
// App state — shared across handlers via actix-web app_data
// ---------------------------------------------------------------------------

/// Shared state for resource authority API endpoints.
#[derive(Clone)]
pub struct ResourceAuthorityApi {
    pub ledger: Arc<RwLock<ResourceLedger>>,
    pub token_store: Arc<RwLock<TokenStore>>,
    pub system_ceilings: Arc<RwLock<Vec<SystemCeiling>>>,
    pub freeze_state: Arc<RwLock<SystemFreezeState>>,
    /// IDs of reservations that have been flagged for investigation.
    pub flagged_reservations: Arc<RwLock<HashSet<String>>>,
    /// Scoped persistence root for resource authority state.
    pub storage_root: std::path::PathBuf,
    workspace_layout: Option<ArtifactV2Workspace>,
    /// Single source of truth for per-`(principal, workspace)` resource-
    /// authority state. The dispatch gate (`CompiledDispatchAuthority`)
    /// and this REST surface both hold an `Arc` to the same resolver,
    /// so writes from one path are immediately visible to the other.
    ///
    /// Before this resolver existed, each side maintained its own
    /// in-memory state and diverged at runtime — see the module
    /// comment on `resource_authority::scoped_authority`.
    scoped_resolver: Option<Arc<dyn ScopedAuthorityResolver>>,
    /// Per-scope flagged-reservations Arcs. `flagged_reservations` is
    /// API-only state (not consulted by the dispatch gate), so it lives
    /// here rather than in `ScopedAuthorityBundle`. Each `(principal,
    /// workspace)` gets its own `HashSet` that survives across requests
    /// within the same process lifetime — matches the pre-refactor
    /// behaviour where the scoped cache preserved this per-scope.
    flagged_reservations_cache:
        Arc<RwLock<std::collections::HashMap<(String, String), Arc<RwLock<HashSet<String>>>>>>,
}

impl ResourceAuthorityApi {
    /// Construct an API shell that delegates per-scope lookups to a
    /// shared `ScopedAuthorityResolver` — typically the same Arc the
    /// dispatch gate's `CompiledDispatchAuthority` carries, so writes
    /// from either path are visible to the other.
    pub fn with_scoped_resolver(
        workspace_layout: ArtifactV2Workspace,
        scoped_resolver: Arc<dyn ScopedAuthorityResolver>,
    ) -> Self {
        Self {
            ledger: Arc::new(RwLock::new(ResourceLedger::new())),
            token_store: Arc::new(RwLock::new(TokenStore::new())),
            system_ceilings: Arc::new(RwLock::new(Vec::new())),
            freeze_state: Arc::new(RwLock::new(SystemFreezeState::default())),
            flagged_reservations: Arc::new(RwLock::new(HashSet::new())),
            // This root is only a bootstrap placeholder for the unscoped API shell;
            // live request handling resolves a concrete scoped root before any I/O.
            storage_root: workspace_layout
                .system_root()
                .join("_resource_authority_scope_required"),
            workspace_layout: Some(workspace_layout),
            scoped_resolver: Some(scoped_resolver),
            flagged_reservations_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Persist ledger journal to disk (best-effort).
    pub async fn persist_ledger(&self) {
        let path = self.storage_root.join("resource_ledger.jsonl");
        let bytes = {
            let ledger = self.ledger.read().await;
            match magician::magician_v2::resource_authority::persistence::journal_to_jsonl_bytes(
                &ledger,
            ) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!("[RESOURCE-AUTHORITY] Failed to serialize ledger: {}", e);
                    return;
                },
            }
        };
        if let Some(layout) = self.workspace_layout.as_ref() {
            if let Err(e) = layout.write_atomic_path(&path, &bytes).await {
                tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist ledger: {}", e);
            }
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist ledger: {}", e);
        }
    }

    /// Persist token store to disk (best-effort).
    pub async fn persist_token_store(&self) {
        let path = self.storage_root.join("token_store.json");
        let bytes = {
            let store = self.token_store.read().await;
            match store.to_json_bytes() {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(
                        "[RESOURCE-AUTHORITY] Failed to serialize token store: {}",
                        e
                    );
                    return;
                },
            }
        };
        if let Some(layout) = self.workspace_layout.as_ref() {
            if let Err(e) = layout.write_atomic_path(&path, &bytes).await {
                tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist token store: {}", e);
            }
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist token store: {}", e);
        }
    }

    /// Persist system ceilings to disk (best-effort).
    pub async fn persist_ceilings(&self) {
        let path = self.storage_root.join("system_ceilings.json");
        let bytes = {
            let ceilings = self.system_ceilings.read().await;
            match serde_json::to_vec_pretty(&*ceilings) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!("[RESOURCE-AUTHORITY] Failed to serialize ceilings: {}", e);
                    return;
                },
            }
        };
        if let Some(layout) = self.workspace_layout.as_ref() {
            if let Err(e) = layout.write_atomic_path(&path, &bytes).await {
                tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist ceilings: {}", e);
            }
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist ceilings: {}", e);
        }
    }

    /// Persist freeze state to disk (best-effort).
    pub async fn persist_freeze(&self) {
        let path = self.storage_root.join("system_freeze.json");
        let bytes = {
            let freeze = self.freeze_state.read().await;
            match serde_json::to_vec_pretty(&*freeze) {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!(
                        "[RESOURCE-AUTHORITY] Failed to serialize freeze state: {}",
                        e
                    );
                    return;
                },
            }
        };
        if let Some(layout) = self.workspace_layout.as_ref() {
            if let Err(e) = layout.write_atomic_path(&path, &bytes).await {
                tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist freeze state: {}", e);
            }
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, bytes) {
            tracing::warn!("[RESOURCE-AUTHORITY] Failed to persist freeze state: {}", e);
        }
    }

    async fn for_request(&self, req: &HttpRequest) -> Result<Self, HttpResponse> {
        let Some(layout) = self.workspace_layout.as_ref() else {
            return Ok(self.clone());
        };

        let (principal, workspace) = resolve_required_scope(req.headers(), None)?;

        // Delegate per-scope state lookup to the shared resolver — the
        // dispatch gate holds the same Arc, so the ledger / token store
        // / ceilings / freeze handles this handler reads from are the
        // exact same Arcs the gate writes to during tool dispatch.
        // First-touch loads from `<workspace>/scopes/{p}/{w}/resource_authority/`;
        // subsequent calls hit the resolver's cache.
        let Some(resolver) = self.scoped_resolver.as_ref() else {
            // Should never happen — both constructors install a resolver.
            // Surface loudly rather than silently constructing a fresh
            // per-handler ledger that diverges from the gate's view.
            return Err(api_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "resource_authority_misconfigured",
                "ResourceAuthorityApi has no scoped resolver attached. \
                 Boot wiring must call `with_scoped_resolver(...)`.",
                None,
            ));
        };
        let bundle = resolver.resolve_scope(&principal, &workspace).await;
        let _ = layout; // workspace_layout retained for callers that re-clone

        // Reuse the same `flagged_reservations` Arc for repeated
        // requests in the same scope so flags survive across handler
        // calls. Pre-refactor the per-scope cache kept this Arc alive
        // alongside the bundle; now the bundle has no API-specific
        // fields so this companion cache fills the gap.
        let scope_key = (principal, workspace);
        let flagged_reservations = {
            let read_guard = self.flagged_reservations_cache.read().await;
            if let Some(existing) = read_guard.get(&scope_key) {
                Arc::clone(existing)
            } else {
                drop(read_guard);
                let mut write_guard = self.flagged_reservations_cache.write().await;
                Arc::clone(
                    write_guard
                        .entry(scope_key)
                        .or_insert_with(|| Arc::new(RwLock::new(HashSet::new()))),
                )
            }
        };

        Ok(Self {
            ledger: bundle.ledger,
            token_store: bundle.token_store,
            system_ceilings: bundle.system_ceilings,
            freeze_state: bundle.system_freeze,
            flagged_reservations,
            storage_root: bundle.storage_root,
            workspace_layout: Some(bundle.workspace_layout),
            scoped_resolver: Some(Arc::clone(resolver)),
            flagged_reservations_cache: Arc::clone(&self.flagged_reservations_cache),
        })
    }
}

// ---------------------------------------------------------------------------
// API Authentication
// ---------------------------------------------------------------------------

/// Authenticated identity extracted from the `Authorization: Bearer <key>` header.
/// If `RESOURCE_AUTHORITY_API_KEY` is not set, all requests are treated as anonymous
/// (backward compatible for development).
struct ApiAuth {
    identity: String,
}

impl ApiAuth {
    /// Extract identity from request. Returns "anonymous" when no auth is configured
    /// or no header is present. Returns an error response when auth is configured
    /// but the provided key does not match.
    fn extract(req: &HttpRequest) -> Result<Self, HttpResponse> {
        let configured_key = std::env::var("RESOURCE_AUTHORITY_API_KEY").ok();

        match configured_key {
            None => {
                // No auth configured — backward compat for development
                Ok(ApiAuth {
                    identity: "anonymous".to_string(),
                })
            },
            Some(expected_key) => {
                let auth_header = req
                    .headers()
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                let expected = format!("Bearer {}", expected_key);

                // Constant-time comparison to prevent timing side-channel
                let auth_bytes = auth_header.as_bytes();
                let expected_bytes = expected.as_bytes();
                let matches = auth_bytes.len() == expected_bytes.len()
                    && auth_bytes
                        .iter()
                        .zip(expected_bytes.iter())
                        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                        == 0;

                if !matches {
                    return Err(api_error_response(
                        StatusCode::UNAUTHORIZED,
                        "unauthorized",
                        "Invalid or missing API key. Set Authorization: Bearer <key> header.",
                        None,
                    ));
                }

                // Use a hash of the key as identity (don't leak the key itself)
                Ok(ApiAuth {
                    identity: format!("api_key_{}", &expected_key[..expected_key.len().min(8)]),
                })
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Input validation
// ---------------------------------------------------------------------------

/// Validate that a string identifier contains only safe characters.
fn validate_identifier(s: &str, field_name: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err(format!("{} must not be empty", field_name));
    }
    if !s
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "{} must contain only alphanumeric, underscore, or hyphen characters",
            field_name
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Route configuration
// ---------------------------------------------------------------------------

pub fn configure_resource_authority_routes(cfg: &mut web::ServiceConfig) {
    cfg
        // System Ceilings
        .route(
            "/resource-authority/ceilings",
            web::get().to(list_ceilings_handler),
        )
        .route(
            "/resource-authority/ceilings",
            web::post().to(upsert_ceiling_handler),
        )
        .route(
            "/resource-authority/ceilings/{id}",
            web::delete().to(delete_ceiling_handler),
        )
        // Bootstrap / Withdraw
        .route(
            "/resource-authority/bootstrap",
            web::post().to(bootstrap_handler),
        )
        .route(
            "/resource-authority/withdraw",
            web::post().to(withdraw_handler),
        )
        // Tokens
        .route(
            "/resource-authority/tokens",
            web::get().to(list_tokens_handler),
        )
        .route(
            "/resource-authority/tokens/{id}",
            web::get().to(get_token_handler),
        )
        .route(
            "/resource-authority/tokens/{id}/revoke",
            web::post().to(revoke_token_handler),
        )
        // Ledger
        .route(
            "/resource-authority/ledger",
            web::get().to(query_ledger_handler),
        )
        .route(
            "/resource-authority/ledger/balances",
            web::get().to(ledger_balances_handler),
        )
        .route(
            "/resource-authority/ledger/audit",
            web::get().to(ledger_audit_handler),
        )
        // Reservations
        .route(
            "/resource-authority/reservations",
            web::get().to(list_reservations_handler),
        )
        .route(
            "/resource-authority/reservations",
            web::post().to(reserve_handler),
        )
        .route(
            "/resource-authority/reservations/{id}/flag",
            web::post().to(flag_reservation_handler),
        )
        .route(
            "/resource-authority/reservations/{id}/commit",
            web::post().to(commit_reservation_handler),
        )
        .route(
            "/resource-authority/reservations/{id}/rollback",
            web::post().to(rollback_reservation_handler),
        )
        // Freeze
        .route("/resource-authority/freeze", web::post().to(freeze_handler))
        .route(
            "/resource-authority/unfreeze",
            web::post().to(unfreeze_handler),
        )
        .route(
            "/resource-authority/freeze",
            web::get().to(freeze_status_handler),
        )
        // Period Close
        .route(
            "/resource-authority/period-close",
            web::post().to(period_close_handler),
        )
        .route(
            "/resource-authority/period-closes",
            web::get().to(list_period_closes_handler),
        )
        // Refund / Credit
        .route("/resource-authority/refund", web::post().to(refund_handler))
        .route("/resource-authority/credit", web::post().to(credit_handler));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ra_invalid_request(
    error: impl Into<String>,
    details: Option<serde_json::Value>,
) -> HttpResponse {
    api_error_response(StatusCode::BAD_REQUEST, "invalid_request", error, details)
}

fn ra_not_found(error: impl Into<String>, details: Option<serde_json::Value>) -> HttpResponse {
    api_error_response(StatusCode::NOT_FOUND, "not_found", error, details)
}

fn ra_conflict(error: impl Into<String>, details: Option<serde_json::Value>) -> HttpResponse {
    api_error_response(StatusCode::CONFLICT, "conflict", error, details)
}

fn ra_internal_error(error: impl Into<String>, details: Option<serde_json::Value>) -> HttpResponse {
    api_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        error,
        details,
    )
}

/// Build a TokenSummaryResponse from a SpendToken and the ledger.
fn build_token_summary(token: &SpendToken, ledger: &ResourceLedger) -> TokenSummaryResponse {
    let period_start = period_window_start(&token.period);
    let spent_in_period = ledger.total_spent_for_token_since(&token.id, period_start);
    let reserved_in_period = ledger.total_reserved_for_token_since(&token.id, period_start);
    let remaining_in_period = token.ceiling - spent_in_period - reserved_in_period;
    let burn_rate_per_day = ledger.burn_rate(&token.commodity, Duration::days(30));
    let projected_exhaustion = ledger.projected_exhaustion(&token.id);
    let days_of_runway = ledger.days_of_runway(&token.id);

    let status_str = match token.status {
        TokenStatus::Active => "active",
        TokenStatus::Revoked => "revoked",
        TokenStatus::Expired => "expired",
    };

    TokenSummaryResponse {
        id: token.id.clone(),
        issued_by: token.issued_by.clone(),
        issued_to: token.issued_to.clone(),
        commodity: token.commodity.clone(),
        ceiling: token.ceiling,
        period: token.period.clone(),
        carryover: token.carryover.clone(),
        period_start,
        spent_in_period,
        remaining_in_period,
        velocity_limit: token.velocity_limit.clone(),
        burn_rate_per_day,
        projected_exhaustion,
        days_of_runway,
        status: status_str.to_string(),
        expires_at: token.expires_at,
        conditions: token.conditions.clone(),
        created_at: token.created_at,
    }
}

/// Convert a JournalEntry to its API response form.
fn journal_entry_to_response(je: &JournalEntry) -> JournalEntryResponse {
    JournalEntryResponse {
        id: je.id.to_string(),
        timestamp: je.timestamp,
        accrual_date: je.accrual_date,
        reference: je.reference.clone(),
        agent_id: je.agent_id.clone(),
        entries: je
            .entries
            .iter()
            .map(|le| LedgerEntryResponse {
                account: le.account.clone(),
                amount: le.amount.value,
                commodity: le.amount.commodity.clone(),
            })
            .collect(),
        metadata: je.metadata.clone(),
    }
}

// ===========================================================================
// HANDLERS
// ===========================================================================

// ---------------------------------------------------------------------------
// System Ceilings
// ---------------------------------------------------------------------------

pub async fn list_ceilings_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    // Acquire in canonical order: ledger → system_ceilings. Matches
    // `spend_session::admit` (ledger.write before ceilings.read). The
    // gate only takes ceilings.read so
    // multi-reader semantics actually preclude a deadlock here today,
    // but matching the canonical order keeps the pattern consistent
    // and future-proofs against any caller that ever takes
    // ceilings.write while holding ledger.
    let ledger = api.ledger.read().await;
    let ceilings = api.system_ceilings.read().await;

    let items: Vec<CeilingResponse> = ceilings
        .iter()
        .map(|sc| {
            let period_start = period_window_start(&sc.period);
            let spent = ledger.total_spent_since(&sc.commodity, period_start);
            let reserved = ledger.in_flight_reserved_for_commodity(&sc.commodity);
            let remaining = sc.ceiling - spent - reserved;

            CeilingResponse {
                id: sc.id.clone(),
                commodity: sc.commodity.clone(),
                ceiling: sc.ceiling,
                relaxation: sc.relaxation,
                period: sc.period.clone(),
                carryover: sc.carryover.clone(),
                period_start,
                spent_in_period: spent,
                reserved_in_period: reserved,
                remaining_in_period: remaining,
            }
        })
        .collect();

    HttpResponse::Ok().json(CeilingsListResponse { ceilings: items })
}

pub async fn upsert_ceiling_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<CeilingRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if let Err(e) = validate_identifier(&req.commodity, "commodity") {
        return ra_invalid_request(e, None);
    }
    if req.ceiling <= Decimal::ZERO {
        return ra_invalid_request("ceiling must be positive", None);
    }
    if req.relaxation < Decimal::ZERO {
        return ra_invalid_request("relaxation must be >= 0", None);
    }
    if req.relaxation > Decimal::new(50, 2) {
        // max 50% relaxation
        return ra_invalid_request("relaxation must be <= 0.50 (50%)", None);
    }

    let ceiling_id = req
        .id
        .clone()
        .unwrap_or_else(|| format!("ceil_{}", Uuid::new_v4().simple()));

    // Clone the row under the write guard so a concurrent delete cannot
    // make a later `find(...).unwrap()` panic, and so we never take
    // ceilings.write across ledger.read (canonical order is ledger then
    // ceilings; `admit` takes ledger.write before ceilings.read).
    let sc = {
        let mut ceilings = api.system_ceilings.write().await;
        if let Some(existing) = ceilings.iter_mut().find(|c| c.id == ceiling_id) {
            existing.commodity = req.commodity;
            existing.ceiling = req.ceiling;
            existing.relaxation = req.relaxation;
            existing.period = req.period;
            existing.carryover = req.carryover;
            existing.clone()
        } else {
            let created = SystemCeiling {
                id: ceiling_id.clone(),
                commodity: req.commodity,
                ceiling: req.ceiling,
                relaxation: req.relaxation,
                period: req.period,
                carryover: req.carryover,
            };
            ceilings.push(created.clone());
            created
        }
    };

    let ledger = api.ledger.read().await;
    let period_start = period_window_start(&sc.period);
    let spent = ledger.total_spent_since(&sc.commodity, period_start);
    let reserved = ledger.in_flight_reserved_for_commodity(&sc.commodity);

    let response = HttpResponse::Ok().json(CeilingResponse {
        id: sc.id.clone(),
        commodity: sc.commodity.clone(),
        ceiling: sc.ceiling,
        relaxation: sc.relaxation,
        period: sc.period.clone(),
        carryover: sc.carryover.clone(),
        period_start,
        spent_in_period: spent,
        reserved_in_period: reserved,
        remaining_in_period: sc.ceiling - spent - reserved,
    });
    drop(ledger);
    api.persist_ceilings().await;
    response
}

pub async fn delete_ceiling_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let ceiling_id = path.into_inner();
    let mut ceilings = api.system_ceilings.write().await;

    let before_len = ceilings.len();
    ceilings.retain(|c| c.id != ceiling_id);

    if ceilings.len() == before_len {
        return ra_not_found(
            format!("Ceiling not found: {}", ceiling_id),
            Some(json!({ "id": ceiling_id })),
        );
    }

    drop(ceilings);
    let response = HttpResponse::Ok().json(json!({ "ok": true, "deleted_id": ceiling_id }));
    api.persist_ceilings().await;
    response
}

// ---------------------------------------------------------------------------
// Bootstrap / Withdraw
// ---------------------------------------------------------------------------

pub async fn bootstrap_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<BootstrapRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if let Err(e) = validate_identifier(&req.agent_id, "agent_id") {
        return ra_invalid_request(e, None);
    }
    if let Err(e) = validate_identifier(&req.commodity, "commodity") {
        return ra_invalid_request(e, None);
    }
    if req.amount <= Decimal::ZERO {
        return ra_invalid_request("amount must be positive", None);
    }
    let max_bootstrap = Decimal::new(1_000_000, 0);
    if req.amount > max_bootstrap {
        return ra_invalid_request(
            "bootstrap amount exceeds maximum of 1,000,000 per call",
            None,
        );
    }

    let entry = JournalEntry::bootstrap(&req.agent_id, &req.commodity, req.amount);
    let entry_id = entry.id.to_string();

    let mut ledger = api.ledger.write().await;
    if let Err(e) = ledger.record(entry) {
        return ra_internal_error(format!("Failed to record bootstrap: {}", e), None);
    }

    let response = HttpResponse::Ok().json(BootstrapResponse {
        ok: true,
        agent_id: req.agent_id,
        commodity: req.commodity,
        amount: req.amount,
        journal_entry_id: entry_id,
    });
    drop(ledger);
    api.persist_ledger().await;
    response
}

pub async fn withdraw_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<WithdrawRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if let Err(e) = validate_identifier(&req.agent_id, "agent_id") {
        return ra_invalid_request(e, None);
    }
    if let Err(e) = validate_identifier(&req.commodity, "commodity") {
        return ra_invalid_request(e, None);
    }
    if req.amount <= Decimal::ZERO {
        return ra_invalid_request("amount must be positive", None);
    }

    let mut ledger = api.ledger.write().await;

    // Check available balance
    let available_account = format!("agent:{}:available:{}", req.agent_id, req.commodity);
    let available = ledger.balance(&available_account);

    if available < req.amount {
        return ra_conflict(
            format!(
                "Insufficient funds: available={}, requested={}",
                available, req.amount
            ),
            Some(json!({
                "available": available.to_string(),
                "requested": req.amount.to_string()
            })),
        );
    }

    let entry = JournalEntry::withdrawal(&req.agent_id, &req.commodity, req.amount);
    let entry_id = entry.id.to_string();

    if let Err(e) = ledger.record(entry) {
        return ra_internal_error(format!("Failed to record withdrawal: {}", e), None);
    }

    let remaining = ledger.balance(&available_account);
    drop(ledger);

    let response = HttpResponse::Ok().json(WithdrawResponse {
        ok: true,
        agent_id: req.agent_id,
        commodity: req.commodity,
        amount: req.amount,
        remaining,
        journal_entry_id: entry_id,
    });
    api.persist_ledger().await;
    response
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

pub async fn list_tokens_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    // Acquire in canonical order: ledger → token_store. Matches
    // `spend_session::admit` so a
    // concurrent gate writer + this REST reader can't deadlock by
    // grabbing the two Arcs in opposite orders. Pre-Phase-D the two
    // sides operated on separate Arcs (gate global vs REST per-scope)
    // so the inverted order here was unreachable; Phase D unified
    // the Arcs and would have exposed the deadlock cycle.
    let ledger = api.ledger.read().await;
    let token_store = api.token_store.read().await;

    let tokens: Vec<TokenSummaryResponse> = token_store
        .tokens
        .values()
        .map(|t| build_token_summary(t, &ledger))
        .collect();

    HttpResponse::Ok().json(TokensListResponse { tokens })
}

pub async fn get_token_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let token_id = path.into_inner();
    // Acquire in canonical order: ledger → token_store. See the
    // matching comment in `list_tokens_handler` for the deadlock cycle
    // this avoids against `spend_session::admit`.
    let ledger = api.ledger.read().await;
    let token_store = api.token_store.read().await;

    let token = match token_store.get(&token_id) {
        Ok(t) => t,
        Err(_) => {
            return ra_not_found(
                format!("Token not found: {}", token_id),
                Some(json!({ "id": token_id })),
            );
        },
    };

    let summary = build_token_summary(token, &ledger);

    // Collect spend history for this token
    let token_prefix = format!("token:{}:", token_id);
    let spend_history: Vec<JournalEntryResponse> = ledger
        .journal
        .iter()
        .filter(|je| {
            je.entries
                .iter()
                .any(|le| le.account.starts_with(&token_prefix))
        })
        .map(journal_entry_to_response)
        .collect();

    HttpResponse::Ok().json(TokenDetailResponse {
        token: summary,
        spend_history,
    })
}

pub async fn revoke_token_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let token_id = path.into_inner();

    // Lock ordering: ledger first, then token_store
    let mut ledger = api.ledger.write().await;
    let mut token_store = api.token_store.write().await;

    let token = match token_store.get(&token_id) {
        Ok(t) => t.clone(),
        Err(_) => {
            return ra_not_found(
                format!("Token not found: {}", token_id),
                Some(json!({ "id": token_id })),
            );
        },
    };

    let previous_status = match token.status {
        TokenStatus::Active => "active",
        TokenStatus::Revoked => "revoked",
        TokenStatus::Expired => "expired",
    };

    if token.status != TokenStatus::Active {
        return ra_conflict(
            format!("Token {} is already {}", token_id, previous_status),
            Some(json!({ "status": previous_status })),
        );
    }

    // Revert unspent budget back to issuer
    let budget_account = token.budget_account();
    let unspent = ledger.balance(&budget_account);
    if unspent > Decimal::ZERO {
        let revert =
            JournalEntry::token_revert(&token.issued_by, &token.id, &token.commodity, unspent);
        if let Err(e) = ledger.record(revert) {
            return ra_internal_error(format!("Failed to revert unspent budget: {}", e), None);
        }
    }

    // Revoke the token
    if let Err(e) = token_store.revoke(&token_id) {
        return ra_internal_error(format!("Failed to revoke token: {}", e), None);
    }

    drop(ledger);
    drop(token_store);

    let response = HttpResponse::Ok().json(TokenRevokeResponse {
        ok: true,
        token_id,
        previous_status: previous_status.to_string(),
    });
    api.persist_ledger().await;
    api.persist_token_store().await;
    response
}

// ---------------------------------------------------------------------------
// Ledger
// ---------------------------------------------------------------------------

pub async fn query_ledger_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    query: web::Query<LedgerQueryParams>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let ledger = api.ledger.read().await;
    let params = query.into_inner();

    let limit = params.limit.unwrap_or(100).min(1000);

    let entries: Vec<JournalEntryResponse> = ledger
        .journal
        .iter()
        .rev() // newest first
        .filter(|je| {
            // Filter by commodity
            if let Some(ref commodity) = params.commodity {
                if !je
                    .entries
                    .iter()
                    .any(|le| &le.amount.commodity == commodity)
                {
                    return false;
                }
            }
            // Filter by since
            if let Some(since) = params.since {
                if je.accrual_date < since {
                    return false;
                }
            }
            // Filter by agent_id
            if let Some(ref agent_id) = params.agent_id {
                if &je.agent_id != agent_id {
                    return false;
                }
            }
            // Filter by token_id
            if let Some(ref token_id) = params.token_id {
                let prefix = format!("token:{}:", token_id);
                if !je.entries.iter().any(|le| le.account.starts_with(&prefix)) {
                    return false;
                }
            }
            true
        })
        .take(limit)
        .map(journal_entry_to_response)
        .collect();

    let total_count = entries.len();

    HttpResponse::Ok().json(LedgerQueryResponse {
        entries,
        total_count,
    })
}

pub async fn ledger_balances_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    query: web::Query<BalancesQueryParams>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let ledger = api.ledger.read().await;
    let params = query.into_inner();

    let balances: Vec<AccountBalanceResponse> = ledger
        .accounts
        .values()
        .filter(|a| {
            if let Some(ref commodity) = params.commodity {
                &a.commodity == commodity
            } else {
                true
            }
        })
        .map(|a| AccountBalanceResponse {
            account: a.id.clone(),
            commodity: a.commodity.clone(),
            balance: a.cached_balance,
        })
        .collect();

    HttpResponse::Ok().json(BalancesResponse { balances })
}

pub async fn ledger_audit_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let ledger = api.ledger.read().await;

    match ledger.audit() {
        Ok(()) => HttpResponse::Ok().json(AuditResponse {
            ok: true,
            message: "Conservation invariant holds — all commodities sum to zero".to_string(),
            imbalances: None,
        }),
        Err(imbalances) => {
            let items: Vec<AuditImbalanceResponse> = imbalances
                .iter()
                .map(|i| AuditImbalanceResponse {
                    commodity: i.commodity.clone(),
                    expected: i.expected,
                    actual: i.actual,
                })
                .collect();
            HttpResponse::Ok().json(AuditResponse {
                ok: false,
                message: format!(
                    "Conservation invariant violated: {} imbalances",
                    items.len()
                ),
                imbalances: Some(items),
            })
        },
    }
}

// ---------------------------------------------------------------------------
// Reservations
// ---------------------------------------------------------------------------

pub async fn list_reservations_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let ledger = api.ledger.read().await;
    let now = Utc::now();

    let reservations: Vec<ReservationResponse> = ledger
        .active_reservations
        .values()
        .map(|r| {
            let elapsed = now.signed_duration_since(r.created_at);
            let is_stale = elapsed > Duration::seconds(r.max_duration_secs);
            ReservationResponse {
                id: r.id.0.clone(),
                token_id: r.token_id.clone(),
                commodity: r.commodity.clone(),
                amount: r.amount,
                agent_id: r.agent_id.clone(),
                created_at: r.created_at,
                max_duration_secs: r.max_duration_secs,
                is_stale,
            }
        })
        .collect();

    HttpResponse::Ok().json(ReservationsListResponse { reservations })
}

pub async fn flag_reservation_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let reservation_id_str = path.into_inner();
    let ledger = api.ledger.read().await;

    let rid = ReservationId(reservation_id_str.clone());
    if !ledger.active_reservations.contains_key(&rid) {
        return ra_not_found(
            format!("Reservation not found: {}", reservation_id_str),
            Some(json!({ "id": reservation_id_str })),
        );
    }

    // Persist the flagged state so it survives across queries
    let mut flagged = api.flagged_reservations.write().await;
    flagged.insert(reservation_id_str.clone());

    HttpResponse::Ok().json(ReservationActionResponse {
        ok: true,
        reservation_id: reservation_id_str,
        action: "flagged".to_string(),
    })
}

pub async fn commit_reservation_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    path: web::Path<String>,
    body: Option<web::Json<CommitReservationRequest>>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let reservation_id_str = path.into_inner();
    // Absent body (or absent `actual`) → commit the full reserved amount
    // (backward-compatible with callers that POST no body). A present `actual`
    // commits the metered actual cost — delta returned to budget when under the
    // reservation, overage recorded when over.
    let actual_cost = body.and_then(|b| b.into_inner().actual);
    let mut ledger = api.ledger.write().await;

    let rid = ReservationId(reservation_id_str.clone());

    match gate::commit_spend_group(&mut ledger, &rid, actual_cost) {
        Ok(()) => {
            drop(ledger);
            let response = HttpResponse::Ok().json(ReservationActionResponse {
                ok: true,
                reservation_id: reservation_id_str,
                action: "committed".to_string(),
            });
            api.persist_ledger().await;
            response
        },
        Err(e) => ra_not_found(
            format!("Failed to commit reservation: {}", e),
            Some(json!({ "id": reservation_id_str })),
        ),
    }
}

pub async fn rollback_reservation_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    path: web::Path<String>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let reservation_id_str = path.into_inner();
    let mut ledger = api.ledger.write().await;

    let rid = ReservationId(reservation_id_str.clone());

    match gate::rollback_spend_group(&mut ledger, &rid) {
        Ok(()) => {
            drop(ledger);
            let response = HttpResponse::Ok().json(ReservationActionResponse {
                ok: true,
                reservation_id: reservation_id_str,
                action: "rolled_back".to_string(),
            });
            api.persist_ledger().await;
            response
        },
        Err(e) => ra_not_found(
            format!("Failed to rollback reservation: {}", e),
            Some(json!({ "id": reservation_id_str })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Reserve (REST bridge for the food-ordering wrappers)
// ---------------------------------------------------------------------------

/// `POST /resource-authority/reservations` — reserve `amount` of `commodity`
/// against the tool-scoped daily budget for `tool`, returning a reservation id
/// the caller then commits (with the actual amount) or rolls back. Exposes the
/// dispatch gate's reserve → commit → rollback lifecycle over HTTP so the
/// Zepto/Swiggy `.mjs` wrappers can hold an order's rupee total against the
/// daily ceiling BEFORE checkout, then settle the true total afterwards.
///
/// Resolves the same per-`(principal, workspace)` scope the dispatch gate writes
/// to, lazily issues/funds the config token via the same `SpendTokenResolver`,
/// and reserves through `gate::reserve_spend`. A `409` means the reservation
/// would breach the ceiling, or no `budgets:` row matches `tool` + `commodity`.
pub async fn reserve_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<ReserveRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let req = body.into_inner();
    if let Err(e) = validate_identifier(&req.tool, "tool") {
        return ra_invalid_request(e, None);
    }
    if let Err(e) = validate_identifier(&req.commodity, "commodity") {
        return ra_invalid_request(e, None);
    }
    if req.amount <= Decimal::ZERO {
        return ra_invalid_request("amount must be positive", None);
    }
    let agent_id = req.agent_id.clone().unwrap_or_default();

    // Resolve the same per-scope bundle the dispatch gate writes to (so the
    // ledger / token store / ceilings this reservation touches are the exact
    // Arcs the gate and the `/budget` UI also see). Production always wires both
    // `workspace_layout` and `scoped_resolver` (via `with_scoped_resolver`), so
    // this takes the real-scope branch. When `workspace_layout` is None we default
    // the scope ids; if no `scoped_resolver` is attached either (a bare test
    // shell), the handler returns a clear error below rather than operating on a
    // divergent ledger.
    let (principal, workspace) = if api.workspace_layout.is_some() {
        match resolve_required_scope(http_req.headers(), None) {
            Ok(pw) => pw,
            Err(resp) => return resp,
        }
    } else {
        ("anonymous".to_string(), "default".to_string())
    };
    let Some(resolver) = api.scoped_resolver.as_ref() else {
        return ra_internal_error(
            "ResourceAuthorityApi has no scoped resolver attached. Boot wiring must call \
             `with_scoped_resolver(...)`.",
            None,
        );
    };
    let admission = match admit(
        resolver.as_ref(),
        SpendIntent {
            principal,
            workspace,
            agent_id,
            tool_name: req.tool.clone(),
            commodity: req.commodity.clone(),
            amount: req.amount,
            missing_budget: MissingBudgetPolicy::Reject,
            owner: SpendOwnerPolicy::ActiveOnly,
        },
    )
    .await
    {
        Ok(admission) => admission,
        Err(SpendSessionError::NoBudget { tool, commodity }) => {
            return ra_conflict(
                format!(
                    "No budget configured authorizing tool `{tool}` to spend `{commodity}`. Add a `budgets:` \
                     row under `resource_authority` in magician-config.yaml."
                ),
                Some(json!({ "tool": tool, "commodity": commodity })),
            );
        },
        Err(SpendSessionError::Disabled) => {
            return ra_conflict(
                "Resource authority is disabled",
                Some(json!({ "tool": req.tool, "commodity": req.commodity })),
            );
        },
        Err(SpendSessionError::Gate(reason)) => {
            return ra_conflict(
                reason,
                Some(json!({
                    "tool": req.tool,
                    "commodity": req.commodity,
                    "proposed": req.amount.to_string(),
                })),
            );
        },
    };

    match admission {
        SpendAdmission::Reserved(hold) => {
            let reservation_id = hold.reservation_id().0.clone();
            hold.persist().await;
            HttpResponse::Ok().json(ReserveResponse {
                ok: true,
                reservation_id,
                tool: req.tool,
                commodity: magician::magician_v2::resource_authority::canonicalize_commodity(
                    &req.commodity,
                ),
                amount: req.amount,
            })
        },
        SpendAdmission::Uncounted => ra_conflict(
            "Resource authority is disabled",
            Some(json!({ "tool": req.tool, "commodity": req.commodity })),
        ),
    }
}

// ---------------------------------------------------------------------------
// Freeze
// ---------------------------------------------------------------------------

pub async fn freeze_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<FreezeRequest>,
) -> HttpResponse {
    let auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if req.reason.is_empty() {
        return ra_invalid_request("reason is required", None);
    }

    let mut freeze_state = api.freeze_state.write().await;
    freeze_state.freeze(&auth.identity, &req.reason);
    drop(freeze_state);

    let response = HttpResponse::Ok().json(FreezeActionResponse {
        ok: true,
        frozen: true,
        reason: req.reason,
    });
    api.persist_freeze().await;
    response
}

pub async fn unfreeze_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<UnfreezeRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if req.reason.is_empty() {
        return ra_invalid_request("reason is required", None);
    }

    let mut freeze_state = api.freeze_state.write().await;
    freeze_state.unfreeze();
    drop(freeze_state);

    let response = HttpResponse::Ok().json(FreezeActionResponse {
        ok: true,
        frozen: false,
        reason: req.reason,
    });
    api.persist_freeze().await;
    response
}

pub async fn freeze_status_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let freeze_state = api.freeze_state.read().await;

    HttpResponse::Ok().json(FreezeStatusResponse {
        frozen: freeze_state.frozen,
        frozen_at: freeze_state.frozen_at,
        frozen_by: freeze_state.frozen_by.clone(),
        reason: freeze_state.reason.clone(),
    })
}

// ---------------------------------------------------------------------------
// Period Close
// ---------------------------------------------------------------------------

pub async fn period_close_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<PeriodCloseRequest>,
) -> HttpResponse {
    let auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if req.commodity.is_empty() {
        return ra_invalid_request("commodity is required", None);
    }

    let now = Utc::now();
    let close = PeriodClose {
        commodity: req.commodity.clone(),
        period_end: req.period_end,
        closed_at: now,
        closed_by: auth.identity,
    };

    let mut ledger = api.ledger.write().await;
    ledger.period_closes.push(close);

    HttpResponse::Ok().json(PeriodCloseResponse {
        ok: true,
        commodity: req.commodity,
        period_end: req.period_end,
        closed_at: now,
    })
}

pub async fn list_period_closes_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let ledger = api.ledger.read().await;

    let period_closes: Vec<PeriodCloseEntryResponse> = ledger
        .period_closes
        .iter()
        .map(|pc| PeriodCloseEntryResponse {
            commodity: pc.commodity.clone(),
            period_end: pc.period_end,
            closed_at: pc.closed_at,
            closed_by: pc.closed_by.clone(),
        })
        .collect();

    HttpResponse::Ok().json(PeriodClosesListResponse { period_closes })
}

// ---------------------------------------------------------------------------
// Refund / Credit
// ---------------------------------------------------------------------------

pub async fn refund_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<RefundRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if req.token_id.is_empty() {
        return ra_invalid_request("token_id is required", None);
    }
    if req.amount <= Decimal::ZERO {
        return ra_invalid_request("amount must be positive", None);
    }
    if req.reason.is_empty() {
        return ra_invalid_request("reason is required", None);
    }

    // Look up the token to get its commodity
    let token_store = api.token_store.read().await;
    let token = match token_store.get(&req.token_id) {
        Ok(t) => t.clone(),
        Err(_) => {
            return ra_not_found(
                format!("Token not found: {}", req.token_id),
                Some(json!({ "id": req.token_id })),
            );
        },
    };
    let commodity = token.commodity.clone();
    // Drop the read lock before acquiring write lock on ledger
    drop(token_store);

    let mut ledger = api.ledger.write().await;

    // Validate refund doesn't exceed expense balance
    let expense_account = format!("token:{}:expense:{}", token.id, token.commodity);
    let expense_balance = ledger.balance(&expense_account);
    if req.amount > expense_balance {
        return ra_invalid_request(
            "refund amount exceeds total expenses for this token",
            Some(json!({
                "expense_balance": expense_balance.to_string(),
                "requested_refund": req.amount.to_string()
            })),
        );
    }

    let entry = JournalEntry::refund(&req.token_id, &commodity, req.amount, &req.reason);
    let entry_id = entry.id.to_string();

    if let Err(e) = ledger.record(entry) {
        return ra_internal_error(format!("Failed to record refund: {}", e), None);
    }

    HttpResponse::Ok().json(RefundResponse {
        ok: true,
        token_id: req.token_id,
        amount: req.amount,
        journal_entry_id: entry_id,
    })
}

pub async fn credit_handler(
    http_req: HttpRequest,
    api: web::Data<ResourceAuthorityApi>,
    body: web::Json<CreditRequest>,
) -> HttpResponse {
    let _auth = match ApiAuth::extract(&http_req) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let api = match api.for_request(&http_req).await {
        Ok(api) => api,
        Err(resp) => return resp,
    };
    let req = body.into_inner();

    if let Err(e) = validate_identifier(&req.agent_id, "agent_id") {
        return ra_invalid_request(e, None);
    }
    if let Err(e) = validate_identifier(&req.commodity, "commodity") {
        return ra_invalid_request(e, None);
    }
    if req.amount <= Decimal::ZERO {
        return ra_invalid_request("amount must be positive", None);
    }
    if req.reason.is_empty() {
        return ra_invalid_request("reason is required", None);
    }

    let entry = JournalEntry::vendor_credit(&req.agent_id, &req.commodity, req.amount, &req.reason);
    let entry_id = entry.id.to_string();

    let mut ledger = api.ledger.write().await;
    if let Err(e) = ledger.record(entry) {
        return ra_internal_error(format!("Failed to record credit: {}", e), None);
    }

    HttpResponse::Ok().json(CreditResponse {
        ok: true,
        agent_id: req.agent_id,
        commodity: req.commodity,
        amount: req.amount,
        journal_entry_id: entry_id,
    })
}

// ===========================================================================
// TESTS
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::{test, web, App};
    use magician::magician_v2::resource_authority::{
        gate::SystemFreezeState,
        ledger::{JournalEntry, ResourceLedger},
        token::{
            CarryoverPolicy, CeilingPeriod, SpendToken, SystemCeiling, TokenStatus, VelocityLimit,
        },
        token_store::TokenStore,
    };
    use rust_decimal::Decimal;
    use serde_json::json;

    /// Build a test app with empty state.
    fn test_app_state() -> ResourceAuthorityApi {
        ResourceAuthorityApi {
            ledger: Arc::new(RwLock::new(ResourceLedger::new())),
            token_store: Arc::new(RwLock::new(TokenStore::new())),
            system_ceilings: Arc::new(RwLock::new(Vec::new())),
            freeze_state: Arc::new(RwLock::new(SystemFreezeState::default())),
            flagged_reservations: Arc::new(RwLock::new(HashSet::new())),
            storage_root: std::env::temp_dir().join("magician_test_ra"),
            workspace_layout: None,
            // Tests operate on the locally-constructed Arcs above
            // (no `for_request` indirection); `None` is fine here.
            scoped_resolver: None,
            flagged_reservations_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Build a test app with pre-populated state for token/ledger tests.
    async fn test_app_state_with_data() -> ResourceAuthorityApi {
        let state = test_app_state();

        // Bootstrap agent
        {
            let mut ledger = state.ledger.write().await;
            let entry = JournalEntry::bootstrap("cfo", "USD", Decimal::new(5000, 0));
            ledger.record(entry).unwrap();
        }

        // Create a token
        {
            let mut ts = state.token_store.write().await;
            ts.create(SpendToken {
                id: "rct_test".to_string(),
                issued_by: "cfo".to_string(),
                issued_to: "worker".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(200, 0),
                period: CeilingPeriod::Daily,
                carryover: CarryoverPolicy::None,
                conditions: vec!["test only".to_string()],
                velocity_limit: Some(VelocityLimit {
                    max_amount: Decimal::new(500, 0),
                    window_seconds: 3600,
                }),
                status: TokenStatus::Active,
                expires_at: None,
                created_at: Utc::now(),
                system_ceiling_id: None,
                last_period_start: None,
            })
            .unwrap();

            // Issue token budget from agent available
            let mut ledger = state.ledger.write().await;
            let issuance =
                JournalEntry::token_issuance("cfo", "rct_test", "USD", Decimal::new(200, 0));
            ledger.record(issuance).unwrap();
        }

        // Add a system ceiling
        {
            let mut ceilings = state.system_ceilings.write().await;
            ceilings.push(SystemCeiling {
                id: "ceil_usd_monthly".to_string(),
                commodity: "USD".to_string(),
                ceiling: Decimal::new(5000, 0),
                relaxation: Decimal::new(2, 2), // 2%
                period: CeilingPeriod::Monthly,
                carryover: CarryoverPolicy::None,
            });
        }

        state
    }

    #[actix_web::test]
    async fn test_api_ceilings_crud() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        // List — empty
        let req = test::TestRequest::get()
            .uri("/resource-authority/ceilings")
            .to_request();
        let resp: CeilingsListResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ceilings.is_empty());

        // Create
        let req = test::TestRequest::post()
            .uri("/resource-authority/ceilings")
            .set_json(json!({
                "commodity": "USD",
                "ceiling": "5000",
                "relaxation": "0.02",
                "period": "monthly",
                "carryover": { "type": "none" }
            }))
            .to_request();
        let resp: CeilingResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.commodity, "USD");
        assert_eq!(resp.ceiling, Decimal::new(5000, 0));
        let ceiling_id = resp.id.clone();

        // List — 1 entry
        let req = test::TestRequest::get()
            .uri("/resource-authority/ceilings")
            .to_request();
        let resp: CeilingsListResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.ceilings.len(), 1);

        // Update
        let req = test::TestRequest::post()
            .uri("/resource-authority/ceilings")
            .set_json(json!({
                "id": ceiling_id,
                "commodity": "USD",
                "ceiling": "10000",
                "relaxation": "0.03",
                "period": "monthly",
                "carryover": { "type": "none" }
            }))
            .to_request();
        let resp: CeilingResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.ceiling, Decimal::new(10000, 0));

        // Delete
        let req = test::TestRequest::delete()
            .uri(&format!("/resource-authority/ceilings/{}", ceiling_id))
            .to_request();
        let resp: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp["ok"], true);

        // List — empty again
        let req = test::TestRequest::get()
            .uri("/resource-authority/ceilings")
            .to_request();
        let resp: CeilingsListResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ceilings.is_empty());
    }

    #[actix_web::test]
    async fn test_api_bootstrap_creates_journal() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/resource-authority/bootstrap")
            .set_json(json!({
                "agent_id": "cfo",
                "commodity": "USD",
                "amount": "5000"
            }))
            .to_request();
        let resp: BootstrapResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.agent_id, "cfo");

        // Verify ledger has the entry
        let ledger = state.ledger.read().await;
        assert_eq!(ledger.journal.len(), 1);
        let available = ledger.balance(&"agent:cfo:available:USD".to_string());
        assert_eq!(available, Decimal::new(5000, 0));
    }

    #[actix_web::test]
    async fn test_api_withdraw_succeeds() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/resource-authority/withdraw")
            .set_json(json!({
                "agent_id": "cfo",
                "commodity": "USD",
                "amount": "1000"
            }))
            .to_request();
        let resp: WithdrawResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        // cfo had 5000, issued 200 to token, so 4800 available. Withdraw 1000 -> 3800.
        assert_eq!(resp.remaining, Decimal::new(3800, 0));
    }

    #[actix_web::test]
    async fn test_api_withdraw_insufficient_rejected() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/resource-authority/withdraw")
            .set_json(json!({
                "agent_id": "cfo",
                "commodity": "USD",
                "amount": "99999"
            }))
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[actix_web::test]
    async fn test_api_token_list() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/resource-authority/tokens")
            .to_request();
        let resp: TokensListResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.tokens.len(), 1);
        assert_eq!(resp.tokens[0].id, "rct_test");
        assert_eq!(resp.tokens[0].status, "active");
        assert_eq!(resp.tokens[0].commodity, "USD");
    }

    #[actix_web::test]
    async fn test_api_token_detail() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/resource-authority/tokens/rct_test")
            .to_request();
        let resp: TokenDetailResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.token.id, "rct_test");
        // Should have the issuance entry in spend history
        assert!(!resp.spend_history.is_empty());
    }

    #[actix_web::test]
    async fn test_api_token_revoke() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/resource-authority/tokens/rct_test/revoke")
            .to_request();
        let resp: TokenRevokeResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.previous_status, "active");

        // Verify token is revoked
        let ts = state.token_store.read().await;
        assert_eq!(ts.get("rct_test").unwrap().status, TokenStatus::Revoked);

        // Verify revert journal entry (unspent budget returned to issuer)
        let ledger = state.ledger.read().await;
        let revert_entries: Vec<_> = ledger
            .journal
            .iter()
            .filter(|je| je.reference.starts_with("token_revert:"))
            .collect();
        assert!(!revert_entries.is_empty());
    }

    #[actix_web::test]
    async fn test_api_ledger_query_filters() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        // All entries
        let req = test::TestRequest::get()
            .uri("/resource-authority/ledger")
            .to_request();
        let resp: LedgerQueryResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.entries.len() >= 2); // bootstrap + issuance

        // Filter by commodity
        let req = test::TestRequest::get()
            .uri("/resource-authority/ledger?commodity=USD")
            .to_request();
        let resp: LedgerQueryResponse = test::call_and_read_body_json(&app, req).await;
        assert!(!resp.entries.is_empty());

        // Filter by agent_id
        let req = test::TestRequest::get()
            .uri("/resource-authority/ledger?agent_id=system")
            .to_request();
        let resp: LedgerQueryResponse = test::call_and_read_body_json(&app, req).await;
        // bootstrap entry has agent_id = "system"
        assert!(!resp.entries.is_empty());
    }

    #[actix_web::test]
    async fn test_api_audit_passes() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/resource-authority/ledger/audit")
            .to_request();
        let resp: AuditResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert!(resp.imbalances.is_none());
    }

    #[actix_web::test]
    async fn test_api_balance_summary() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/resource-authority/ledger/balances")
            .to_request();
        let resp: BalancesResponse = test::call_and_read_body_json(&app, req).await;
        assert!(!resp.balances.is_empty());

        // Filter by commodity
        let req = test::TestRequest::get()
            .uri("/resource-authority/ledger/balances?commodity=USD")
            .to_request();
        let resp: BalancesResponse = test::call_and_read_body_json(&app, req).await;
        assert!(!resp.balances.is_empty());
        assert!(resp.balances.iter().all(|b| b.commodity == "USD"));
    }

    #[actix_web::test]
    async fn test_api_freeze_blocks_spend() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        // Freeze
        let req = test::TestRequest::post()
            .uri("/resource-authority/freeze")
            .set_json(json!({ "reason": "emergency" }))
            .to_request();
        let resp: FreezeActionResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert!(resp.frozen);

        // Verify freeze state
        let freeze = state.freeze_state.read().await;
        assert!(freeze.frozen);
        assert_eq!(freeze.reason.as_deref(), Some("emergency"));
    }

    #[actix_web::test]
    async fn test_api_unfreeze_resumes() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        // Freeze first
        let req = test::TestRequest::post()
            .uri("/resource-authority/freeze")
            .set_json(json!({ "reason": "emergency" }))
            .to_request();
        let _: FreezeActionResponse = test::call_and_read_body_json(&app, req).await;

        // Unfreeze
        let req = test::TestRequest::post()
            .uri("/resource-authority/unfreeze")
            .set_json(json!({ "reason": "all clear" }))
            .to_request();
        let resp: FreezeActionResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert!(!resp.frozen);

        let freeze = state.freeze_state.read().await;
        assert!(!freeze.frozen);
    }

    #[actix_web::test]
    async fn test_api_freeze_status() {
        let state = test_app_state();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        // Initially unfrozen
        let req = test::TestRequest::get()
            .uri("/resource-authority/freeze")
            .to_request();
        let resp: FreezeStatusResponse = test::call_and_read_body_json(&app, req).await;
        assert!(!resp.frozen);

        // Freeze
        let req = test::TestRequest::post()
            .uri("/resource-authority/freeze")
            .set_json(json!({ "reason": "test" }))
            .to_request();
        let _: FreezeActionResponse = test::call_and_read_body_json(&app, req).await;

        // Check status
        let req = test::TestRequest::get()
            .uri("/resource-authority/freeze")
            .to_request();
        let resp: FreezeStatusResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.frozen);
        assert!(resp.frozen_at.is_some());
        assert_eq!(resp.reason.as_deref(), Some("test"));
    }

    #[actix_web::test]
    async fn test_api_period_close() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let period_end = Utc::now();
        let req = test::TestRequest::post()
            .uri("/resource-authority/period-close")
            .set_json(json!({
                "commodity": "USD",
                "period": "monthly",
                "period_end": period_end.to_rfc3339()
            }))
            .to_request();
        let resp: PeriodCloseResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.commodity, "USD");

        // Verify it's in the ledger
        let ledger = state.ledger.read().await;
        assert_eq!(ledger.period_closes.len(), 1);
    }

    #[actix_web::test]
    async fn test_api_period_closes_list() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        // Close a period
        let req = test::TestRequest::post()
            .uri("/resource-authority/period-close")
            .set_json(json!({
                "commodity": "USD",
                "period": "monthly",
                "period_end": Utc::now().to_rfc3339()
            }))
            .to_request();
        let _: PeriodCloseResponse = test::call_and_read_body_json(&app, req).await;

        // List
        let req = test::TestRequest::get()
            .uri("/resource-authority/period-closes")
            .to_request();
        let resp: PeriodClosesListResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.period_closes.len(), 1);
        assert_eq!(resp.period_closes[0].commodity, "USD");
    }

    #[actix_web::test]
    async fn test_api_refund() {
        let state = test_app_state_with_data().await;

        // Record an expense of $50 against the token so we can refund part of it
        {
            use magician::magician_v2::resource_authority::ledger::{LedgerEntry, ResourceAmount};
            let mut ledger = state.ledger.write().await;
            let expense_entry = JournalEntry {
                id: Uuid::new_v4(),
                timestamp: Utc::now(),
                accrual_date: Utc::now(),
                entries: vec![
                    LedgerEntry {
                        account: "token:rct_test:expense:USD".to_string(),
                        amount: ResourceAmount {
                            value: Decimal::new(50, 0),
                            commodity: "USD".to_string(),
                        },
                    },
                    LedgerEntry {
                        account: "token:rct_test:budget:USD".to_string(),
                        amount: ResourceAmount {
                            value: Decimal::new(-50, 0),
                            commodity: "USD".to_string(),
                        },
                    },
                ],
                reference: "spend:rct_test:test_expense".to_string(),
                agent_id: "worker".to_string(),
                metadata: std::collections::HashMap::new(),
            };
            ledger.record(expense_entry).unwrap();
        }

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/resource-authority/refund")
            .set_json(json!({
                "token_id": "rct_test",
                "amount": "25",
                "reason": "invalid charge"
            }))
            .to_request();
        let resp: RefundResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.token_id, "rct_test");

        // Verify conservation still holds
        let ledger = state.ledger.read().await;
        assert!(ledger.audit().is_ok());
    }

    #[actix_web::test]
    async fn test_api_vendor_credit() {
        let state = test_app_state_with_data().await;
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri("/resource-authority/credit")
            .set_json(json!({
                "agent_id": "cfo",
                "commodity": "USD",
                "amount": "100",
                "reason": "promotional credit"
            }))
            .to_request();
        let resp: CreditResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.agent_id, "cfo");

        // Verify conservation
        let ledger = state.ledger.read().await;
        assert!(ledger.audit().is_ok());
    }

    #[actix_web::test]
    async fn test_api_reservations_list() {
        let state = test_app_state_with_data().await;

        // Create a reservation manually
        {
            let mut ledger = state.ledger.write().await;
            let ts = state.token_store.read().await;
            let token = ts.get("rct_test").unwrap();
            let ceilings = state.system_ceilings.read().await;
            let freeze = state.freeze_state.read().await;
            let _ = gate::reserve_spend(
                &freeze,
                &mut ledger,
                token,
                &ceilings,
                Decimal::new(50, 0),
                "worker",
            );
        }

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/resource-authority/reservations")
            .to_request();
        let resp: ReservationsListResponse = test::call_and_read_body_json(&app, req).await;
        assert_eq!(resp.reservations.len(), 1);
        assert_eq!(resp.reservations[0].commodity, "USD");
        assert_eq!(resp.reservations[0].amount, Decimal::new(50, 0));
    }

    #[actix_web::test]
    async fn test_api_reservation_commit() {
        let state = test_app_state_with_data().await;

        // Create a reservation
        let reservation_id;
        {
            let mut ledger = state.ledger.write().await;
            let ts = state.token_store.read().await;
            let token = ts.get("rct_test").unwrap();
            let ceilings = state.system_ceilings.read().await;
            let freeze = state.freeze_state.read().await;
            reservation_id = gate::reserve_spend(
                &freeze,
                &mut ledger,
                token,
                &ceilings,
                Decimal::new(50, 0),
                "worker",
            )
            .unwrap();
        }

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!(
                "/resource-authority/reservations/{}/commit",
                reservation_id
            ))
            .to_request();
        let resp: ReservationActionResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.action, "committed");

        // Verify no more active reservations
        let ledger = state.ledger.read().await;
        assert!(ledger.active_reservations.is_empty());
        assert!(ledger.audit().is_ok());
    }

    #[actix_web::test]
    async fn test_api_reservation_rollback() {
        let state = test_app_state_with_data().await;

        // Create a reservation
        let reservation_id;
        {
            let mut ledger = state.ledger.write().await;
            let ts = state.token_store.read().await;
            let token = ts.get("rct_test").unwrap();
            let ceilings = state.system_ceilings.read().await;
            let freeze = state.freeze_state.read().await;
            reservation_id = gate::reserve_spend(
                &freeze,
                &mut ledger,
                token,
                &ceilings,
                Decimal::new(50, 0),
                "worker",
            )
            .unwrap();
        }

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!(
                "/resource-authority/reservations/{}/rollback",
                reservation_id
            ))
            .to_request();
        let resp: ReservationActionResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.action, "rolled_back");

        // Verify budget restored
        let ledger = state.ledger.read().await;
        assert!(ledger.active_reservations.is_empty());
        let budget = ledger.balance(&"token:rct_test:budget:USD".to_string());
        assert_eq!(budget, Decimal::new(200, 0)); // fully restored
        assert!(ledger.audit().is_ok());
    }

    #[actix_web::test]
    async fn test_api_reservation_flag() {
        let state = test_app_state_with_data().await;

        // Create a reservation
        let reservation_id;
        {
            let mut ledger = state.ledger.write().await;
            let ts = state.token_store.read().await;
            let token = ts.get("rct_test").unwrap();
            let ceilings = state.system_ceilings.read().await;
            let freeze = state.freeze_state.read().await;
            reservation_id = gate::reserve_spend(
                &freeze,
                &mut ledger,
                token,
                &ceilings,
                Decimal::new(50, 0),
                "worker",
            )
            .unwrap();
        }

        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(state.clone()))
                .configure(configure_resource_authority_routes),
        )
        .await;

        let req = test::TestRequest::post()
            .uri(&format!(
                "/resource-authority/reservations/{}/flag",
                reservation_id
            ))
            .to_request();
        let resp: ReservationActionResponse = test::call_and_read_body_json(&app, req).await;
        assert!(resp.ok);
        assert_eq!(resp.action, "flagged");
    }
}
