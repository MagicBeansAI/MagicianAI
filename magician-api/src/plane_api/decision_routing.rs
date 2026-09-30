//! Authenticated proxy. Routing policy and persistence stay in Decision Engine.
use actix_web::{web, HttpRequest, HttpResponse};
use decision_engine_contract::{
    client::ClientError,
    settings::{RoutingSettings, RoutingUpdate},
};
use magician::magician_v2::{auth::middleware::AuthRuntime, decision_host};
use serde_json::json;

fn reply(result: Result<RoutingSettings, ClientError>) -> HttpResponse {
    match result {
        Ok(settings) => HttpResponse::Ok().json(settings),
        Err(ClientError::Status { status, body }) if status == 400 || status == 409 => {
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("message").and_then(|v| v.as_str()).map(str::to_owned))
                .unwrap_or_else(|| "Decision routing was not saved.".into());
            HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
                .json(json!({"message":message}))
        },
        Err(error) => {
            tracing::warn!(%error, "Decision routing settings request failed");
            HttpResponse::ServiceUnavailable().json(json!({"message":"Decision Engine is unavailable. Routing settings were not confirmed; reload before retrying."}))
        },
    }
}

pub async fn plane_decision_routing_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = super::owner_identity(&req, auth.as_ref().map(|d| d.get_ref())) {
        return response;
    }
    let Some(client) = decision_host::settings_backend() else {
        return HttpResponse::ServiceUnavailable().finish();
    };
    reply(client.routing_settings().await)
}

pub async fn plane_decision_routing_put_handler(
    req: HttpRequest,
    body: web::Json<RoutingUpdate>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = super::owner_identity(&req, auth.as_ref().map(|d| d.get_ref())) {
        return response;
    }
    let Some(client) = decision_host::settings_backend() else {
        return HttpResponse::ServiceUnavailable().finish();
    };
    reply(client.update_routing(&body).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[actix_web::test]
    async fn decision_routing_settings_require_owner() {
        let req = actix_web::test::TestRequest::get().to_http_request();
        assert_eq!(
            plane_decision_routing_get_handler(req, None).await.status(),
            401
        );
        let req = actix_web::test::TestRequest::put().to_http_request();
        let update = serde_json::from_value(json!({"revision":"r", "operation":"op", "routing":{"local":{"primary":"kev"},"cloud":{"primary":"jev"}}, "allow_remote_when_local":false,"thresholds_by_model":{}})).unwrap();
        assert_eq!(
            plane_decision_routing_put_handler(req, web::Json(update), None)
                .await
                .status(),
            401
        );
    }
    #[test]
    fn decision_routing_proxy_preserves_conflict_and_never_fakes_success() {
        assert_eq!(
            reply(Err(ClientError::Status {
                status: 409,
                body: r#"{"message":"Reload"}"#.into()
            }))
            .status(),
            409
        );
        assert_eq!(reply(Err(ClientError::Timeout)).status(), 503);
    }
}
