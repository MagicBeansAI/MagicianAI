//! HTTP diagnostics for the shared attention-routing funnel.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;

use crate::scope::resolve_required_scope;
use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;

const DEFAULT_LOOKBACK_HOURS: i64 = 24 * 7;
const MAX_LOOKBACK_HOURS: i64 = 24 * 90;
const DEFAULT_RECENT_LIMIT: usize = 20;
const MAX_RECENT_LIMIT: usize = 100;

#[derive(Debug, Deserialize)]
pub struct AttentionFunnelObservabilityQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub since_ms: Option<i64>,
    #[serde(default)]
    pub lookback_hours: Option<i64>,
    #[serde(default)]
    pub recent_limit: Option<usize>,
}

fn err_json(status: actix_web::http::StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

fn effective_since_ms(query: &AttentionFunnelObservabilityQuery) -> Option<i64> {
    if let Some(since_ms) = query.since_ms {
        return Some(since_ms);
    }
    let lookback_hours = query
        .lookback_hours
        .unwrap_or(DEFAULT_LOOKBACK_HOURS)
        .clamp(0, MAX_LOOKBACK_HOURS);
    if lookback_hours == 0 {
        return None;
    }
    Some(
        chrono::Utc::now()
            .timestamp_millis()
            .saturating_sub(lookback_hours.saturating_mul(3_600_000)),
    )
}

/// `GET /attention-funnel/observability`.
///
/// Scope-aware diagnostic snapshot for route events grouped by stage, source,
/// source family, lane, route reason, and drop reason. This endpoint is
/// adapter-neutral; current channel/resurfacing APIs remain the card stores.
pub async fn get_attention_funnel_observability_handler(
    store: web::Data<AttentionFunnelStore>,
    req: HttpRequest,
    query: web::Query<AttentionFunnelObservabilityQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let recent_limit = query
        .recent_limit
        .unwrap_or(DEFAULT_RECENT_LIMIT)
        .clamp(0, MAX_RECENT_LIMIT);
    match store
        .observability(
            &principal,
            &workspace,
            effective_since_ms(&query),
            recent_limit,
        )
        .await
    {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(err) => err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}
