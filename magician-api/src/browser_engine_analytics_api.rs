use std::path::PathBuf;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;

use crate::scope::resolve_required_scope;
use magician::magician_v2::browser_engine_analytics::{
    list_browser_engine_usage, BrowserEngineUsageFilter,
};

#[derive(Debug, Deserialize)]
pub struct BrowserEngineUsageQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    /// `all`, `success`, or `failure`.
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

fn parse_outcome(value: Option<&str>) -> Result<Option<bool>, &'static str> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("all") => Ok(None),
        Some("success") => Ok(Some(true)),
        Some("failure") => Ok(Some(false)),
        Some(_) => Err("outcome must be one of: all, success, failure"),
    }
}

pub async fn list_browser_engine_usage_handler(
    storage_root: web::Data<PathBuf>,
    req: HttpRequest,
    query: web::Query<BrowserEngineUsageQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let success = match parse_outcome(query.outcome.as_deref()) {
        Ok(outcome) => outcome,
        Err(error) => return HttpResponse::BadRequest().json(serde_json::json!({"error": error})),
    };
    let filter = BrowserEngineUsageFilter {
        engine: query.engine.clone(),
        success,
        limit: query.limit,
        offset: query.offset,
    };
    match list_browser_engine_usage(
        storage_root.get_ref().as_path(),
        &principal,
        &workspace,
        filter,
    )
    .await
    {
        Ok(page) => HttpResponse::Ok().json(page),
        Err(error) => {
            tracing::error!(
                principal = %principal,
                workspace = %workspace,
                error = %error,
                "browser-engine analytics read failed"
            );
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "browser-engine analytics are temporarily unavailable"
            }))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::{test as actix_test, App};
    use magician::magician_v2::browser_engine_analytics::{
        BrowserEngineAnalyticsContext, BrowserEngineUsageInput,
    };

    #[test]
    fn outcome_filter_is_strict_and_typed() {
        assert_eq!(parse_outcome(None), Ok(None));
        assert_eq!(parse_outcome(Some("all")), Ok(None));
        assert_eq!(parse_outcome(Some("success")), Ok(Some(true)));
        assert_eq!(parse_outcome(Some("failure")), Ok(Some(false)));
        assert!(parse_outcome(Some("maybe")).is_err());
    }

    #[actix_web::test]
    async fn endpoint_requires_scope_and_returns_server_page_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let context = BrowserEngineAnalyticsContext::for_scope(
            temp.path(),
            "owner",
            "default",
            Some("exec-1".to_string()),
            None,
        );
        context
            .record(BrowserEngineUsageInput {
                session_id: "session-1".to_string(),
                engine: "lightpanda".to_string(),
                fallback_from: None,
                connection_mode: "headless".to_string(),
                operation: "open".to_string(),
                url: Some("https://example.com/news?private=yes".to_string()),
                success: true,
                elapsed_ms: 8,
                error_class: None,
            })
            .await;

        let app = actix_test::init_service(
            App::new()
                .app_data(web::Data::new(temp.path().to_path_buf()))
                .route(
                    "/browser/engine-usage",
                    web::get().to(list_browser_engine_usage_handler),
                ),
        )
        .await;
        let request = actix_test::TestRequest::get()
            .uri("/browser/engine-usage?limit=1&offset=0&outcome=success")
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let response: serde_json::Value = actix_test::call_and_read_body_json(&app, request).await;
        assert_eq!(response["total_count"], 1);
        assert_eq!(response["limit"], 1);
        assert_eq!(response["offset"], 0);
        assert_eq!(response["items"][0]["engine"], "lightpanda");
        assert_eq!(response["items"][0]["work_kind"], "execution");
        assert_eq!(response["items"][0]["work_id"], "exec-1");
        assert_eq!(response["items"][0]["url"], "https://example.com/news");
    }
}
