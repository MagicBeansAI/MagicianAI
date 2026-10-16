//! HTTP endpoints for the central `UserRequestService`.
//!
//! Routes (mounted under `/api/magician/v2`):
//! ```text
//! GET  /user-requests                     -> list pending requests (+ optional recent history)
//! POST /user-requests/{id}/respond        -> respond to a pending request
//! ```

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::scope::resolve_required_scope;
use magician::magician_v2::user_requests::{UserRequest, UserRequestRecord, UserRequestService};

// ========================================================================
// Request / Response types
// ========================================================================

/// Body for `POST /user-requests/{id}/respond`.
#[derive(Debug, Deserialize)]
pub struct RespondRequest {
    /// The chosen option id (e.g. `"allow_once"`, `"deny"`).
    pub decision: String,
    /// Optional free-text input (guidance, edited arguments, etc.).
    #[serde(default)]
    pub input: Option<String>,
    /// Which channel is responding (e.g. `"web"`, `"telegram"`).
    #[serde(default = "default_channel")]
    pub channel: String,
}

fn default_channel() -> String {
    "web".to_string()
}

/// Response for the respond endpoint.
#[derive(Debug, Serialize)]
pub struct RespondResponse {
    /// `true` if this was the first (winning) response.
    pub accepted: bool,
    /// Present when `accepted` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListUserRequestsQuery {
    #[serde(default)]
    pub include_history: bool,
    #[serde(default)]
    pub history_limit: Option<usize>,
    #[serde(default)]
    pub owner_agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListUserRequestsResponse {
    pub requests: Vec<UserRequest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<UserRequestRecord>,
}

fn missing_scope_response() -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({
        "error": "missing_scope",
        "message": "A bearer with embedded principal/workspace scope is required."
    }))
}

// ========================================================================
// Handlers
// ========================================================================

/// `GET /api/magician/v2/user-requests`
///
/// Returns all currently pending user requests.
pub async fn list_user_requests_handler(
    req: HttpRequest,
    svc: web::Data<Arc<UserRequestService>>,
    query: web::Query<ListUserRequestsQuery>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(_) => return missing_scope_response(),
    };

    let owner_agent_id = query
        .owner_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requests = svc
        .list_pending_for_scope(&principal, &workspace)
        .await
        .into_iter()
        .filter(|request| matches_owner_agent_filter(request, owner_agent_id))
        .collect::<Vec<_>>();
    let history = if query.include_history {
        filter_history_records(
            svc.list_history_for_scope(&principal, &workspace, None),
            owner_agent_id,
            query.history_limit,
        )
    } else {
        Vec::new()
    };
    HttpResponse::Ok().json(ListUserRequestsResponse { requests, history })
}

/// `POST /api/magician/v2/user-requests/{id}/respond`
///
/// Submit a response to a pending user request.  First response wins.
///
/// **Retired as of magician v0.6.502 (Phase H7.x).** This route always returns
/// `410 Gone`. Callers must POST to
/// `/api/magician/v2/hitl/{correlation_id}/respond` with
/// `{ source: "user_request", value, channel? }`; the canonical endpoint owns
/// scoped response dispatch.
pub async fn respond_user_request_handler(
    _req: HttpRequest,
    _svc: web::Data<Arc<UserRequestService>>,
    _path: web::Path<String>,
    _body: web::Json<RespondRequest>,
) -> impl Responder {
    // Phase H7.x — legacy resolve URL retired (returns 410 Gone).
    // Counter still records hits so we can identify remaining callers.
    crate::hitl_deprecation_metrics::record_hit("/api/magician/v2/user-requests/{id}/respond");
    HttpResponse::Gone().json(serde_json::json!({
        "error": "endpoint_retired",
        "message": "POST /api/magician/v2/hitl/{correlation_id}/respond with body { source: \"user_request\", value }",
        "retired_in": "magician v0.6.502 (Phase H7.x)",
        "see": "docs/plans/2026-05-11-hitl-h5-h7-dual-emit-retirement.md",
    }))
}

fn matches_owner_agent_filter(request: &UserRequest, owner_agent_id: Option<&str>) -> bool {
    let Some(owner_agent_id) = owner_agent_id else {
        return true;
    };
    request
        .context
        .as_object()
        .and_then(|context| context.get("owner_agent_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        == Some(owner_agent_id)
}

fn filter_history_records(
    records: Vec<UserRequestRecord>,
    owner_agent_id: Option<&str>,
    limit: Option<usize>,
) -> Vec<UserRequestRecord> {
    let mut filtered = records
        .into_iter()
        .filter(|record| matches_owner_agent_filter(&record.request, owner_agent_id))
        .collect::<Vec<_>>();
    if let Some(limit) = limit {
        filtered.truncate(limit);
    }
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;

    use magician::magician_v2::user_requests::{RequestOption, UserRequestStatus, UserResponse};

    fn sample_request(id: &str, owner_agent_id: Option<&str>, created_at: i64) -> UserRequest {
        let mut context = serde_json::Map::new();
        if let Some(owner_agent_id) = owner_agent_id {
            context.insert(
                "owner_agent_id".to_string(),
                serde_json::Value::String(owner_agent_id.to_string()),
            );
        }

        UserRequest {
            id: id.to_string(),
            request_type: "harness.notify_owner.briefing".to_string(),
            question: format!("Question for {id}"),
            options: vec![RequestOption {
                id: "acknowledge".to_string(),
                label: "Acknowledge".to_string(),
                requires_input: false,
            }],
            principal: "default".to_string(),
            workspace: "default".to_string(),
            context: serde_json::Value::Object(context),
            source: "harness".to_string(),
            execution_id: None,
            task_id: None,
            timeout_secs: 60,
            default_on_timeout: "acknowledge".to_string(),
            created_at,
            sensitive: None,
        }
    }

    fn resolved_record(
        id: &str,
        owner_agent_id: Option<&str>,
        created_at: i64,
    ) -> UserRequestRecord {
        UserRequestRecord {
            request: sample_request(id, owner_agent_id, created_at),
            status: UserRequestStatus::Resolved,
            response: Some(UserResponse {
                request_id: id.to_string(),
                decision: "acknowledge".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            }),
            resolved_at: Some(created_at + 1),
            request_publication_pending: None,
            resolution_publication_pending: None,
        }
    }

    #[test]
    fn filter_history_records_applies_owner_filter_before_limit() {
        let records = vec![
            resolved_record("other-newest", Some("cto"), 300),
            resolved_record("other-middle", Some("cto"), 200),
            resolved_record("owner-target", Some("ceo"), 100),
        ];

        let filtered = filter_history_records(records, Some("ceo"), Some(1));

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].request.id, "owner-target");
    }

    #[test]
    fn filter_history_records_truncates_after_owner_filter() {
        let records = vec![
            resolved_record("owner-newest", Some("ceo"), 300),
            resolved_record("other-middle", Some("cto"), 200),
            resolved_record("owner-older", Some("ceo"), 100),
        ];

        let filtered = filter_history_records(records, Some("ceo"), Some(1));

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].request.id, "owner-newest");
    }
}
