//! Scope comes from auth middleware, never from query/body filters.
use actix_web::{web, HttpRequest, HttpResponse};
use magician::magician_v2::{
    analytics::decision_metrics::{DecisionMetrics, HealthQuery, MetricsQuery, ReviewUpdate},
    auth::middleware::AuthRuntime,
};
use magicllm::LlmScope;
use serde_json::json;
fn scope(req: &HttpRequest, auth: Option<&AuthRuntime>) -> Result<LlmScope, HttpResponse> {
    super::owner_identity(req, auth)?;
    let (p, w) = crate::scope::resolve_required_scope(req.headers(), None)?;
    let scope = LlmScope::new(p, w);
    if !scope.is_valid() {
        return Err(HttpResponse::BadRequest().json(json!({"message":"Invalid scope"})));
    }
    Ok(scope)
}
pub async fn decision_metrics_get(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
    service: web::Data<DecisionMetrics>,
    query: web::Query<MetricsQuery>,
) -> HttpResponse {
    let scope = match scope(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(message) = query.validate() {
        return HttpResponse::BadRequest().json(json!({"message":message}));
    }
    match service.summary(scope, query.into_inner()).await {
        Ok(result) => HttpResponse::Ok().json(result),
        Err(_) => HttpResponse::ServiceUnavailable()
            .json(json!({"message":"Decision metrics unavailable; retry shortly"})),
    }
}
pub async fn decision_metrics_cases(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
    service: web::Data<DecisionMetrics>,
    query: web::Query<MetricsQuery>,
) -> HttpResponse {
    let scope = match scope(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(message) = query.validate() {
        return HttpResponse::BadRequest().json(json!({"message":message}));
    }
    match service.cases(scope, query.into_inner()).await {
        Ok(result) => HttpResponse::Ok().json(result),
        Err(_) => HttpResponse::ServiceUnavailable()
            .json(json!({"message":"Recent cases unavailable; retry shortly"})),
    }
}
/// Per-operation Decision Model health (`?hours=`, default 24, max 168).
pub async fn decision_metrics_health(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
    service: web::Data<DecisionMetrics>,
    query: web::Query<HealthQuery>,
) -> HttpResponse {
    let scope = match scope(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if let Err(message) = query.hours() {
        return HttpResponse::BadRequest().json(json!({"message":message}));
    }
    match service.health(scope, query.into_inner()).await {
        Ok(result) => HttpResponse::Ok().json(result),
        Err(_) => HttpResponse::ServiceUnavailable()
            .json(json!({"message":"Decision model health unavailable; retry shortly"})),
    }
}
pub async fn decision_metrics_review(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
    service: web::Data<DecisionMetrics>,
    body: web::Json<ReviewUpdate>,
) -> HttpResponse {
    let scope = match scope(&req, auth.as_ref().map(|data| data.get_ref())) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match service.review(scope, body.into_inner()).await {
        Ok(revision) => HttpResponse::Ok().json(json!({"revision":revision})),
        Err(message) => HttpResponse::build(if message.contains("changed elsewhere") {
            actix_web::http::StatusCode::CONFLICT
        } else if message.contains("busy")
            || message.contains("timed out")
            || message.contains("unavailable")
        {
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        } else {
            actix_web::http::StatusCode::BAD_REQUEST
        })
        .json(json!({"message":message})),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dashboard_rejects_header_only_and_grant_auth() {
        let req = actix_web::test::TestRequest::get()
            .insert_header(("X-Principal", "other"))
            .insert_header(("X-Workspace", "default"))
            .to_http_request();
        assert_eq!(scope(&req, None).unwrap_err().status(), 401);
        let req = actix_web::test::TestRequest::get()
            .insert_header(("Authorization", "Bearer plt_fake"))
            .to_http_request();
        assert_eq!(scope(&req, None).unwrap_err().status(), 401);
    }
}
