//! Memory API — HTTP endpoints for memory search and preference storage.
//!
//! These endpoints expose scoped memory search and preference writes to
//! runtime clients without linking directly to the memory service.
//!
//! Routes:
//! ```text
//! POST   /api/magician/v2/chat/memory/search      -> search episodes
//! POST   /api/magician/v2/chat/memory/preference   -> save/remove a preference
//! GET    /api/magician/v2/memory/effect-review    -> shadow/canary/enforced advice
//! POST   /api/magician/v2/memory/effect-review    -> stay or advance (owner)
//! ```

use actix_web::{http::header::HeaderMap, web, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};
use uuid::Uuid;

use magician::magician_v2::agents::{memory::EpisodeOutcome, AgentMemoryResolver};
use magician::magician_v2::artifact_v2::memory::V3EpisodeRecord;
use magician::magician_v2::chat::DEFAULT_AGENT_ID;

// ========================================================================
// Constants
// ========================================================================

const CHAT_GOAL_ID: &str = "chat";
const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_LIMIT: usize = 50;

// ========================================================================
// Request/Response Types
// ========================================================================

/// Request body for `POST /chat/memory/search`.
///
/// Note: `agent_id` is determined server-side (`DEFAULT_AGENT_ID`), not caller-specified.
#[derive(Debug, Deserialize)]
pub struct MemorySearchRequest {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

/// A single search result entry.
#[derive(Debug, Serialize)]
pub struct MemorySearchResult {
    pub episode_id: String,
    pub summary: String,
    pub observations: Vec<String>,
    pub timestamp: i64,
}

/// Response body for `POST /chat/memory/search`.
#[derive(Debug, Serialize)]
pub struct MemorySearchResponse {
    pub results: Vec<MemorySearchResult>,
}

/// Request body for `POST /chat/memory/preference`.
///
/// Note: `agent_id` is determined server-side (`DEFAULT_AGENT_ID`), not caller-specified.
#[derive(Debug, Deserialize)]
pub struct SavePreferenceRequest {
    pub key: String,
    pub value: String,
}

/// Response body for `POST /chat/memory/preference`.
#[derive(Debug, Serialize)]
pub struct SavePreferenceResponse {
    pub status: String,
    pub key: String,
    pub value: String,
}

fn default_search_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

// ========================================================================
// Shared MemoryApi Data
// ========================================================================

/// Shared state for memory API handlers.
pub struct MemoryApi {
    memory_resolver: AgentMemoryResolver,
    shadow_cache: tokio::sync::Mutex<
        std::collections::HashMap<
            (String, String),
            (
                Option<std::time::SystemTime>,
                Vec<magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory>,
            ),
        >,
    >,
}

impl MemoryApi {
    pub fn new(memory_resolver: AgentMemoryResolver) -> Self {
        Self {
            memory_resolver,
            shadow_cache: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub async fn shadow_memories(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<magician::magician_v2::attention::resurfacing::memory_context::ScopedMemory> {
        let Ok(service) = self.memory_resolver.resolve_for_scope(principal, workspace) else {
            return Vec::new();
        };
        let mtime = std::fs::metadata(service.storage().user_knowledge_path())
            .and_then(|meta| meta.modified())
            .ok();
        {
            let cache = self.shadow_cache.lock().await;
            if let Some((cached_mtime, memories)) =
                cache.get(&(principal.to_string(), workspace.to_string()))
            {
                if *cached_mtime == mtime {
                    return memories.clone();
                }
            }
        }
        let Ok(knowledge) = service.load_user_knowledge().await else {
            return Vec::new();
        };
        let memories =
            magician::magician_v2::attention::resurfacing::memory_context::memories_from_knowledge(
                &knowledge,
            );
        let mut cache = self.shadow_cache.lock().await;
        cache.insert(
            (principal.to_string(), workspace.to_string()),
            (mtime, memories.clone()),
        );
        memories
    }
}

fn resolve_scope_from_headers(headers: &HeaderMap) -> Result<(String, String), HttpResponse> {
    let principal = headers
        .get("X-Principal")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "missing_scope",
                "message": "A bearer with an embedded principal is required for scoped memory access"
            }))
        })?;
    let workspace = headers
        .get("X-Workspace")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "missing_scope",
                "message": "A bearer with an embedded workspace is required for scoped memory access"
            }))
        })?;
    Ok((principal, workspace))
}

// ========================================================================
// Handlers
// ========================================================================

/// POST /api/magician/v2/chat/memory/search
///
/// Search recent episodes for matching content via case-insensitive substring.
pub async fn memory_search_handler(
    memory_api: web::Data<MemoryApi>,
    req: HttpRequest,
    body: web::Json<MemorySearchRequest>,
) -> impl Responder {
    debug!(
        "[MEMORY-API] POST /chat/memory/search query={:?} limit={}",
        body.query, body.limit
    );

    if body.query.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "query is required and must not be empty"
        }));
    }

    let effective_limit = body.limit.min(MAX_SEARCH_LIMIT).max(1);
    // TODO: In multi-tenant mode, derive agent_id from authenticated principal.
    // For now, ignore the caller-provided agent_id and always use the system default
    // to prevent unauthorized access to other agents' memory.
    let agent_id = DEFAULT_AGENT_ID;

    // Recall recent episodes (up to the effective limit).
    // We fetch more than requested to allow for post-filter narrowing.
    let fetch_limit = (effective_limit * 5).min(MAX_SEARCH_LIMIT);
    let (principal, workspace) = match resolve_scope_from_headers(req.headers()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let memory_service = memory_api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .map_err(|e| {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_scope",
                "message": e.to_string()
            }))
        });
    let memory_service = match memory_service {
        Ok(service) => service,
        Err(response) => return response,
    };
    let episodes = match memory_service
        .recall_native_episodes(agent_id, CHAT_GOAL_ID, None, Some(fetch_limit))
        .await
    {
        Ok(eps) => eps,
        Err(e) => {
            // AgentMemoryError::Validation when goal dir doesn't exist yet is normal
            debug!(
                "[MEMORY-API] recall_native_episodes returned error (may be empty store): {}",
                e
            );
            Vec::new()
        },
    };

    let query_lower = body.query.to_lowercase();
    let mut results = Vec::new();

    for ep in &episodes {
        if results.len() >= effective_limit {
            break;
        }

        let mut matched = false;

        // Check observations
        for obs in &ep.observations {
            if obs.to_lowercase().contains(&query_lower) {
                matched = true;
                break;
            }
        }

        // Check outcome summary
        if !matched {
            let outcome_text = Some(ep.outcome_summary.as_str());
            if let Some(text) = outcome_text {
                if text.to_lowercase().contains(&query_lower) {
                    matched = true;
                }
            }
        }

        // Check strategy summary
        if !matched {
            if let Some(ref strategy) = ep.strategy_summary {
                if strategy.to_lowercase().contains(&query_lower) {
                    matched = true;
                }
            }
        }

        // Check context at start
        if !matched {
            if let Some(ref context) = ep.context_at_start {
                if context.to_lowercase().contains(&query_lower) {
                    matched = true;
                }
            }
        }

        if matched {
            results.push(MemorySearchResult {
                episode_id: ep.episode_id.clone(),
                summary: ep.outcome_summary.clone(),
                observations: ep.observations.clone(),
                timestamp: ep
                    .started_at_dt()
                    .map(|started_at| started_at.timestamp())
                    .unwrap_or(0),
            });
        }
    }

    HttpResponse::Ok().json(MemorySearchResponse { results })
}

/// POST /api/magician/v2/chat/memory/preference
///
/// Save or remove a user preference by recording it as an episode.
pub async fn memory_save_preference_handler(
    memory_api: web::Data<MemoryApi>,
    req: HttpRequest,
    body: web::Json<SavePreferenceRequest>,
) -> impl Responder {
    debug!(
        "[MEMORY-API] POST /chat/memory/preference key={:?} value={:?}",
        body.key, body.value
    );

    if body.key.trim().is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "key is required and must not be empty"
        }));
    }

    // TODO: In multi-tenant mode, derive agent_id from authenticated principal.
    // For now, ignore the caller-provided agent_id and always use the system default
    // to prevent unauthorized access to other agents' memory.
    let agent_id = DEFAULT_AGENT_ID;

    let is_removal = body.value.is_empty();
    let now = Utc::now();

    let observation = if is_removal {
        format!("User requested removal of preference: {}", body.key)
    } else {
        format!("User set preference: {} = {}", body.key, body.value)
    };

    let outcome = if is_removal {
        EpisodeOutcome::GoalAchieved {
            summary: format!("Removed preference: {}", body.key),
        }
    } else {
        EpisodeOutcome::GoalAchieved {
            summary: format!("Saved preference: {} = {}", body.key, body.value),
        }
    };

    let (principal, workspace) = match resolve_scope_from_headers(req.headers()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let memory_service = memory_api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .map_err(|e| {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_scope",
                "message": e.to_string()
            }))
        });
    let memory_service = match memory_service {
        Ok(service) => service,
        Err(response) => return response,
    };

    let episode = V3EpisodeRecord::new_memory_episode(
        memory_service.scoped_memory_scope(),
        agent_id,
        Uuid::new_v4().to_string(),
        CHAT_GOAL_ID,
        "preference_update",
        now.timestamp_millis() as u64,
        now,
        Some(serde_json::json!({
            "key": body.key,
            "value": body.value,
            "action": if is_removal { "remove" } else { "set" },
        })),
        now,
        now,
        &outcome,
        Vec::new(),
        vec![observation],
        Vec::new(),
        None,
        None,
        None,
    );

    // An episode is provenance, not the current preference store. Route the
    // actual write/clear through the same lifecycle as the chat memory tools.
    let saved = magician::magician_v2::chat::service::merge_user_preference(
        &memory_api.memory_resolver,
        &principal,
        &workspace,
        "preferences",
        &body.key,
        serde_json::Value::String(body.value.clone()),
    )
    .await;
    if saved.get("status").and_then(serde_json::Value::as_str) != Some("ok") {
        return HttpResponse::InternalServerError().json(saved);
    }

    match memory_service
        .append_native_episode(agent_id, &episode)
        .await
    {
        Ok(()) => HttpResponse::Ok().json(SavePreferenceResponse {
            status: "saved".to_string(),
            key: body.key.clone(),
            value: body.value.clone(),
        }),
        Err(e) => {
            error!(
                "[MEMORY-API] Failed to save preference '{}': {}",
                body.key, e
            );
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to save preference",
                "details": e.to_string()
            }))
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct MemoryEntriesQuery {
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Comma-separated tiers. Defaults to owner-confirmable tiers only.
    #[serde(default)]
    pub tiers: Option<String>,
}

const MEMORY_ENTRIES_DEFAULT_LIMIT: usize = 20;
const MEMORY_ENTRIES_MAX_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
pub struct MemoryEntryPath {
    pub tier: String,
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct MemoryEntryScopeBody {
    pub scope: Option<magician::magician_v2::agents::MemoryScope>,
}

fn resolve_memory_service(
    memory_api: &MemoryApi,
    req: &HttpRequest,
) -> Result<magician::magician_v2::agents::AgentMemoryService, HttpResponse> {
    let (principal, workspace) = resolve_scope_from_headers(req.headers())?;
    memory_api
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
        .map_err(|error| {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "invalid_scope",
                "message": error.to_string()
            }))
        })
}

/// GET /memory/entries
pub async fn list_user_memory_entries_handler(
    memory_api: web::Data<MemoryApi>,
    resurfacing: Option<
        web::Data<magician::magician_v2::attention::resurfacing::store::ResurfacingStore>,
    >,
    req: HttpRequest,
    query: web::Query<MemoryEntriesQuery>,
) -> impl Responder {
    let service = match resolve_memory_service(&memory_api, &req) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let offset = query.offset.unwrap_or(0);
    let limit = query
        .limit
        .unwrap_or(MEMORY_ENTRIES_DEFAULT_LIMIT)
        .clamp(1, MEMORY_ENTRIES_MAX_LIMIT);
    let requested: Vec<String> = query
        .tiers
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|tier| !tier.is_empty())
        .map(str::to_string)
        .collect();
    let default_tiers = magician::magician_v2::agents::OWNER_CONFIRMABLE_MEMORY_TIERS;
    let allowed: Vec<&str> = if requested.is_empty() {
        default_tiers.to_vec()
    } else {
        requested
            .iter()
            .map(String::as_str)
            .filter(|tier| magician::magician_v2::agents::is_owner_confirmable_memory_tier(tier))
            .collect()
    };
    match service
        .list_user_memory_entries_page(offset, limit, &allowed)
        .await
    {
        Ok((mut entries, total)) => {
            if let Some(store) = resurfacing.as_ref() {
                if let Ok((principal, workspace)) = resolve_scope_from_headers(req.headers()) {
                    if let Ok(conflicts) = store
                        .list_recent_memory_conflicts(&principal, &workspace)
                        .await
                    {
                        let by_key: std::collections::HashMap<_, _> = conflicts
                            .into_iter()
                            .map(|conflict| (conflict.memory_key.clone(), conflict))
                            .collect();
                        for entry in &mut entries {
                            let Some(object) = entry.as_object_mut() else {
                                continue;
                            };
                            let Some(tier) = object.get("tier").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            let Some(key) = object.get("key").and_then(|v| v.as_str()) else {
                                continue;
                            };
                            if let Some(conflict) = by_key.get(&format!("{tier}: {key}")) {
                                object.insert(
                                    "conflict".to_string(),
                                    serde_json::Value::String(conflict.rationale.clone()),
                                );
                                if conflict.agree_count > 0 || conflict.disagree_count > 0 {
                                    object.insert(
                                        "conflict_agree".to_string(),
                                        serde_json::json!(conflict.agree_count),
                                    );
                                    object.insert(
                                        "conflict_disagree".to_string(),
                                        serde_json::json!(conflict.disagree_count),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            HttpResponse::Ok().json(serde_json::json!({
                "entries": entries,
                "total": total,
                "offset": offset,
                "limit": limit,
            }))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "list_memory_entries_failed",
            "message": error.to_string(),
        })),
    }
}

/// POST /memory/entries/{tier}/{key}/confirm
pub async fn confirm_user_memory_entry_handler(
    memory_api: web::Data<MemoryApi>,
    req: HttpRequest,
    path: web::Path<MemoryEntryPath>,
) -> impl Responder {
    let service = match resolve_memory_service(&memory_api, &req) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service
        .confirm_user_memory_entry(&path.tier, &path.key, Utc::now().timestamp())
        .await
    {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "status": "confirmed",
            "tier": path.tier,
            "key": path.key,
        })),
        Err(error) => {
            let message = error.to_string();
            if message.contains("untrusted") {
                HttpResponse::Forbidden().json(serde_json::json!({
                    "error": "untrusted_memory",
                    "message": message,
                }))
            } else if message.contains("not found") || message.contains("missing") {
                HttpResponse::NotFound().json(serde_json::json!({
                    "error": "memory_entry_not_found",
                    "message": message,
                }))
            } else {
                HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "confirm_failed",
                    "message": message,
                }))
            }
        },
    }
}

/// PATCH /memory/entries/{tier}/{key}/scope
pub async fn patch_user_memory_entry_scope_handler(
    memory_api: web::Data<MemoryApi>,
    req: HttpRequest,
    path: web::Path<MemoryEntryPath>,
    body: web::Json<MemoryEntryScopeBody>,
) -> impl Responder {
    let service = match resolve_memory_service(&memory_api, &req) {
        Ok(service) => service,
        Err(response) => return response,
    };
    match service
        .set_user_memory_entry_scope(&path.tier, &path.key, body.scope.clone())
        .await
    {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({
            "status": "updated",
            "tier": path.tier,
            "key": path.key,
        })),
        Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "scope_update_failed",
            "message": error.to_string(),
        })),
    }
}

/// POST /memory/entries/{tier}/{key}/keep-conflict
///
/// Owner acknowledgement that the written memory stands. Does not change
/// trust — `/confirm` is the promotion path. No-op persist: 204, no writes.
pub async fn keep_user_memory_entry_conflict_handler(
    memory_api: web::Data<MemoryApi>,
    req: HttpRequest,
    _path: web::Path<MemoryEntryPath>,
) -> impl Responder {
    if let Err(response) = resolve_memory_service(&memory_api, &req) {
        return response;
    }
    HttpResponse::NoContent().finish()
}

#[derive(Debug, Deserialize)]
pub struct MemoryEffectReviewBody {
    pub decision: String,
}

fn memory_effect_review_json(
    review: magician::magician_v2::attention::resurfacing::memory_effect_review::MemoryEffectReview,
    pending_hitl: bool,
) -> serde_json::Value {
    serde_json::json!({
        "compiled_mode": magician::magician_v2::attention::resurfacing::memory_effects::MEMORY_EFFECT_MODE,
        "effective_mode": magician::magician_v2::attention::resurfacing::memory_effects::effective_memory_effect_mode(),
        "pending_hitl": pending_hitl,
        "observation": review.observation,
        "advice": review.advice,
        "reason": review.reason,
        "next_step": review.next_step,
    })
}

async fn review_for_request(
    store: &magician::magician_v2::attention::resurfacing::store::ResurfacingStore,
    user_requests: &magician::magician_v2::user_requests::UserRequestService,
    principal: &str,
    workspace: &str,
) -> (
    magician::magician_v2::attention::resurfacing::memory_effect_review::MemoryEffectReview,
    bool,
) {
    let judgements = store
        .list_recent_memory_judgements_sync(principal, workspace, 200)
        .unwrap_or_default();
    let review =
        magician::magician_v2::attention::resurfacing::memory_effect_review::review_live_mode(
            &judgements,
        );
    let pending_hitl = user_requests
        .list_pending_for_scope(principal, workspace)
        .await
        .iter()
        .any(|request| {
            request.request_type
                == magician::magician_v2::attention::resurfacing::memory_effect_review::REQUEST_TYPE
        });
    (review, pending_hitl)
}

/// GET /memory/effect-review
pub async fn get_memory_effect_review_handler(
    store: web::Data<magician::magician_v2::attention::resurfacing::store::ResurfacingStore>,
    user_requests: web::Data<
        std::sync::Arc<magician::magician_v2::user_requests::UserRequestService>,
    >,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match resolve_scope_from_headers(req.headers()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (review, pending_hitl) = review_for_request(
        store.get_ref(),
        user_requests.get_ref(),
        &principal,
        &workspace,
    )
    .await;
    HttpResponse::Ok().json(memory_effect_review_json(review, pending_hitl))
}

/// POST /memory/effect-review
///
/// Owner stay/advance for the shadow → canary → enforced rollout. Stay is
/// always accepted. Advance is refused unless the current advice is an
/// advance step.
pub async fn post_memory_effect_review_handler(
    store: web::Data<magician::magician_v2::attention::resurfacing::store::ResurfacingStore>,
    user_requests: web::Data<
        std::sync::Arc<magician::magician_v2::user_requests::UserRequestService>,
    >,
    req: HttpRequest,
    body: web::Json<MemoryEffectReviewBody>,
) -> impl Responder {
    let (principal, workspace) = match resolve_scope_from_headers(req.headers()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (review, pending_hitl) = review_for_request(
        store.get_ref(),
        user_requests.get_ref(),
        &principal,
        &workspace,
    )
    .await;
    match magician::magician_v2::attention::resurfacing::memory_effect_review::apply_review_decision(
        &review,
        body.decision.trim(),
    ) {
        Ok(_) => {
            let (review, pending_hitl) = review_for_request(
                store.get_ref(),
                user_requests.get_ref(),
                &principal,
                &workspace,
            )
            .await;
            HttpResponse::Ok().json(memory_effect_review_json(review, pending_hitl))
        },
        Err(message) if body.decision.trim() == "advance" => {
            HttpResponse::Conflict().json(serde_json::json!({
                "error": "not_ready",
                "message": message,
                "review": memory_effect_review_json(review, pending_hitl),
            }))
        },
        Err(message) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": "invalid_decision",
            "message": message,
        })),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use actix_web::{test, web, App};

    async fn seeded_api() -> (tempfile::TempDir, web::Data<MemoryApi>) {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let service = resolver.resolve_for_scope("owner", "home").unwrap();
        service
            .save_user_knowledge(&serde_json::json!({
                "accounts": [{"key": "acme", "value": "acct", "source_type": "entity"}],
                "preferences": [{
                    "key": "vendor",
                    "value": "Avoid vendor calls",
                    "source_type": "insight"
                }]
            }))
            .await
            .unwrap();
        (tmp, web::Data::new(MemoryApi::new(resolver)))
    }

    #[actix_web::test]
    async fn list_defaults_to_confirmable_tiers_and_skips_accounts() {
        let (_tmp, api) = seeded_api().await;
        let app = test::init_service(App::new().app_data(api).route(
            "/memory/entries",
            web::get().to(list_user_memory_entries_handler),
        ))
        .await;
        let req = test::TestRequest::get()
            .uri("/memory/entries")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, req).await;
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(body["total"], 1);
        assert_eq!(entries[0]["key"], "vendor");
        assert_eq!(entries[0]["tier"], "preferences");
    }

    #[actix_web::test]
    async fn confirm_and_scope_round_trip_on_preferences() {
        let (_tmp, api) = seeded_api().await;
        let app = test::init_service(
            App::new()
                .app_data(api)
                .route(
                    "/memory/entries/{tier}/{key}/confirm",
                    web::post().to(confirm_user_memory_entry_handler),
                )
                .route(
                    "/memory/entries/{tier}/{key}/scope",
                    web::patch().to(patch_user_memory_entry_scope_handler),
                ),
        )
        .await;
        let confirm = test::TestRequest::post()
            .uri("/memory/entries/preferences/vendor/confirm")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .to_request();
        let confirm_body: serde_json::Value = test::call_and_read_body_json(&app, confirm).await;
        assert_eq!(confirm_body["status"], "confirmed");

        let scope = test::TestRequest::patch()
            .uri("/memory/entries/preferences/vendor/scope")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .set_json(serde_json::json!({
                "scope": {"topics":["vendor"],"entities":[],"applies_to":[]}
            }))
            .to_request();
        let scope_body: serde_json::Value = test::call_and_read_body_json(&app, scope).await;
        assert_eq!(scope_body["status"], "updated");
    }

    #[actix_web::test]
    async fn confirm_of_a_workflow_is_forbidden_by_the_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        resolver
            .resolve_for_scope("owner", "home")
            .unwrap()
            .save_user_knowledge(&serde_json::json!({
                "workflows": [{"key": "w1", "value": "do it", "source_type": "insight"}]
            }))
            .await
            .unwrap();
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(MemoryApi::new(resolver)))
                .route(
                    "/memory/entries/{tier}/{key}/confirm",
                    web::post().to(confirm_user_memory_entry_handler),
                ),
        )
        .await;
        let req = test::TestRequest::post()
            .uri("/memory/entries/workflows/w1/confirm")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .to_request();
        let response = test::call_service(&app, req).await;
        assert!(response.status().is_client_error());
    }

    #[actix_web::test]
    async fn keep_conflict_returns_204_and_does_not_change_trust() {
        let (_tmp, api) = seeded_api().await;
        let app = test::init_service(
            App::new()
                .app_data(api)
                .route(
                    "/memory/entries",
                    web::get().to(list_user_memory_entries_handler),
                )
                .route(
                    "/memory/entries/{tier}/{key}/keep-conflict",
                    web::post().to(keep_user_memory_entry_conflict_handler),
                ),
        )
        .await;
        let keep = test::TestRequest::post()
            .uri("/memory/entries/preferences/vendor/keep-conflict")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .to_request();
        let keep_response = test::call_service(&app, keep).await;
        assert_eq!(
            keep_response.status(),
            actix_web::http::StatusCode::NO_CONTENT
        );

        let list = test::TestRequest::get()
            .uri("/memory/entries")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, list).await;
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries[0]["key"], "vendor");
        assert_eq!(entries[0]["trust"], "inferred");
    }

    #[actix_web::test]
    async fn effect_review_get_on_empty_store_stays_collecting() {
        let _guard =
            magician::magician_v2::attention::resurfacing::memory_effects::MEMORY_EFFECT_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        magician::magician_v2::attention::resurfacing::memory_effects::reset_memory_effect_runtime_for_test();
        let store =
            magician::magician_v2::attention::resurfacing::store::ResurfacingStore::open_in_temp();
        let user_requests = std::sync::Arc::new(
            magician::magician_v2::user_requests::UserRequestService::new(std::sync::Arc::new(
                magician::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(8),
            )),
        );
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(store))
                .app_data(web::Data::new(user_requests))
                .route(
                    "/memory/effect-review",
                    web::get().to(get_memory_effect_review_handler),
                )
                .route(
                    "/memory/effect-review",
                    web::post().to(post_memory_effect_review_handler),
                ),
        )
        .await;
        let get = test::TestRequest::get()
            .uri("/memory/effect-review")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .to_request();
        let body: serde_json::Value = test::call_and_read_body_json(&app, get).await;
        assert_eq!(body["advice"], "collect_shadow_evidence");
        assert_eq!(body["compiled_mode"], "shadow");
        assert_eq!(body["effective_mode"], "shadow");
        assert_eq!(body["pending_hitl"], false);

        let advance = test::TestRequest::post()
            .uri("/memory/effect-review")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .set_json(serde_json::json!({ "decision": "advance" }))
            .to_request();
        let advance_response = test::call_service(&app, advance).await;
        assert_eq!(
            advance_response.status(),
            actix_web::http::StatusCode::CONFLICT
        );

        let stay = test::TestRequest::post()
            .uri("/memory/effect-review")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "home"))
            .set_json(serde_json::json!({ "decision": "stay" }))
            .to_request();
        let stay_body: serde_json::Value = test::call_and_read_body_json(&app, stay).await;
        assert_eq!(stay_body["effective_mode"], "shadow");
        magician::magician_v2::attention::resurfacing::memory_effects::reset_memory_effect_runtime_for_test();
    }

    #[tokio::test]
    async fn shadow_memories_caches_until_the_knowledge_file_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let service = resolver.resolve_for_scope("owner", "home").unwrap();
        service
            .save_user_knowledge(&serde_json::json!({
                "preferences": [{
                    "key": "vendor",
                    "value": "Avoid vendor calls",
                    "source_type": "owner_confirmed",
                    "scope": {"topics":["vendor"],"entities":[],"applies_to":[]}
                }]
            }))
            .await
            .unwrap();
        let api = MemoryApi::new(resolver);
        let first = api.shadow_memories("owner", "home").await;
        assert_eq!(first.len(), 1);
        let second = api.shadow_memories("owner", "home").await;
        assert_eq!(second.len(), 1);
        service
            .save_user_knowledge(&serde_json::json!({"preferences": []}))
            .await
            .unwrap();
        let third = api.shadow_memories("owner", "home").await;
        assert!(third.is_empty());
    }
}
