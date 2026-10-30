//! Lightweight activity for the visible history page, without loading messages.
use std::collections::HashSet;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;

use super::ChatApi;
use crate::scope::resolve_required_scope;

#[derive(Deserialize)]
pub struct SessionActivityQuery {
    session_ids: String,
}

fn requested_ids(value: &str) -> Result<HashSet<String>, &'static str> {
    let ids: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect();
    if ids.len() > 100 || ids.iter().any(|id| id.len() > 128) {
        return Err("session_ids must contain at most 100 identifiers of at most 128 bytes each");
    }
    Ok(ids.into_iter().map(str::to_owned).collect())
}

/// GET /chat/activity?session_ids=... — exact, authenticated visible rows only.
/// Missing, hidden and foreign sessions have the same absent result.
pub async fn get_session_activity_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<SessionActivityQuery>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let requested = match requested_ids(&query.session_ids) {
        Ok(ids) => ids,
        Err(error) => return HttpResponse::BadRequest().json(serde_json::json!({"error": error})),
    };
    match api
        .chat_service
        .working_session_ids(&principal, &workspace, &requested)
        .await
    {
        Ok(ids) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store"))
            .json(serde_json::json!({"working_session_ids": ids})),
        Err(error) => {
            tracing::warn!(%error, "history activity lookup failed");
            HttpResponse::ServiceUnavailable().finish()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_requests_are_bounded_and_deduplicated() {
        assert_eq!(
            requested_ids(" a, b,a, ").unwrap(),
            HashSet::from(["a".into(), "b".into()])
        );
        assert!(requested_ids("").unwrap().is_empty());
        assert!(requested_ids(&"x".repeat(129)).is_err());
        assert!(requested_ids(&vec!["x"; 101].join(",")).is_err());
    }
}
