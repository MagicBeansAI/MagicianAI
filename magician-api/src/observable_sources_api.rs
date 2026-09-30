//! Scoped HTTP contract for declarative Observe sources and subscriptions.

use std::sync::Arc;

use actix_web::{http::StatusCode, web, HttpRequest, HttpResponse};
use serde::Deserialize;

use crate::scope::resolve_required_scope;
use magician::magician_v2::content_sources::{
    ObservableSourceReadiness, ObservableSourceRuntime, ObservationSubscriptionState,
    ObservationSubscriptionView, PutObservationSubscription, SubscriptionMutationError,
};

#[derive(Debug, Deserialize)]
pub struct SourceListQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    required_action: Option<String>,
    #[serde(default)]
    readiness: Option<ObservableSourceReadiness>,
    #[serde(default)]
    subscribed: Option<bool>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct SubscriptionListQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    state: Option<ObservationSubscriptionState>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ObservabilityQuery {
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

pub async fn list_observable_sources_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    query: web::Query<SourceListQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match runtime
        .list_offers(
            &principal,
            &workspace,
            query.required_action.as_deref(),
            query.readiness.clone(),
            query.subscribed,
            query.cursor.as_deref(),
            query.limit,
        )
        .await
    {
        Ok(page) => HttpResponse::Ok().json(page),
        Err(error) => mutation_error(error),
    }
}

pub async fn list_observation_subscriptions_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    query: web::Query<SubscriptionListQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match runtime
        .list_subscriptions(
            &principal,
            &workspace,
            query.state,
            query.enabled,
            query.cursor.as_deref(),
            query.limit,
        )
        .await
    {
        Ok(page) => HttpResponse::Ok().json(serde_json::json!({
            "items": page
                .items
                .into_iter()
                .map(ObservationSubscriptionView::from)
                .collect::<Vec<_>>(),
            "total": page.total,
            "next_cursor": page.next_cursor,
        })),
        Err(error) => mutation_error(error),
    }
}

pub async fn put_observation_subscription_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PutObservationSubscription>,
) -> HttpResponse {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match runtime
        .put_subscription(
            &principal,
            &workspace,
            &path.into_inner(),
            body.into_inner(),
        )
        .await
    {
        Ok(subscription) => {
            HttpResponse::Ok().json(ObservationSubscriptionView::from(subscription))
        },
        Err(error) => mutation_error(error),
    }
}

pub async fn delete_observation_subscription_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match runtime
        .delete_subscription(&principal, &workspace, &path.into_inner())
        .await
    {
        Ok(_) => HttpResponse::NoContent().finish(),
        Err(error) => mutation_error(error),
    }
}

pub async fn run_observation_subscription_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match runtime
        .get_ref()
        .run_now(&principal, &workspace, &path.into_inner())
        .await
    {
        Ok(outcome) => HttpResponse::Ok().json(outcome),
        Err(error) => mutation_error(error),
    }
}

pub async fn observable_source_observability_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    query: web::Query<ObservabilityQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match runtime
        .list_observability(&principal, &workspace, query.cursor.as_deref(), query.limit)
        .await
    {
        Ok(page) => HttpResponse::Ok().json(page),
        Err(error) => mutation_error(error),
    }
}

pub async fn observation_run_history_handler(
    runtime: web::Data<Arc<ObservableSourceRuntime>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ObservabilityQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match runtime
        .list_run_history(
            &principal,
            &workspace,
            &path.into_inner(),
            query.cursor.as_deref(),
            query.limit,
        )
        .await
    {
        Ok(page) => HttpResponse::Ok().json(page),
        Err(error) => mutation_error(error),
    }
}

fn mutation_error(error: SubscriptionMutationError) -> HttpResponse {
    let message = if matches!(&error, SubscriptionMutationError::Internal(_)) {
        "observable source operation failed".to_string()
    } else {
        error.to_string()
    };
    let (status, code) = match error {
        SubscriptionMutationError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        SubscriptionMutationError::Unavailable => (StatusCode::CONFLICT, "source_unavailable"),
        SubscriptionMutationError::StaleSource => (StatusCode::CONFLICT, "stale_source"),
        SubscriptionMutationError::RevisionConflict => (StatusCode::CONFLICT, "revision_conflict"),
        SubscriptionMutationError::StaleCursor => (StatusCode::CONFLICT, "stale_cursor"),
        SubscriptionMutationError::Invalid(_) => (StatusCode::BAD_REQUEST, "invalid_request"),
        SubscriptionMutationError::Busy => (StatusCode::TOO_MANY_REQUESTS, "run_busy"),
        SubscriptionMutationError::Internal(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        },
    };
    HttpResponse::build(status).json(serde_json::json!({
        "error": code,
        "message": message,
    }))
}

fn default_limit() -> usize {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::body::to_bytes;

    #[actix_web::test]
    async fn stale_and_busy_mutations_have_stable_http_contracts() {
        let stale = mutation_error(SubscriptionMutationError::StaleSource);
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let body = to_bytes(stale.into_body()).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
            "stale_source"
        );

        let busy = mutation_error(SubscriptionMutationError::Busy);
        assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);

        let cursor = mutation_error(SubscriptionMutationError::StaleCursor);
        assert_eq!(cursor.status(), StatusCode::CONFLICT);
        let body = to_bytes(cursor.into_body()).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
            "stale_cursor"
        );

        let internal = mutation_error(SubscriptionMutationError::Internal(anyhow::anyhow!(
            "secret provider detail"
        )));
        let body = to_bytes(internal.into_body()).await.unwrap();
        let body = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        assert_eq!(body["error"], "internal_error");
        assert_eq!(body["message"], "observable source operation failed");
        assert!(!body.to_string().contains("secret provider detail"));
    }
}
