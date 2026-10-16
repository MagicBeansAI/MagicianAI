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
            let parsed = serde_json::from_str::<serde_json::Value>(&body).ok();
            let field = |name: &str| {
                parsed
                    .as_ref()
                    .and_then(|v| v.get(name).and_then(|v| v.as_str()).map(str::to_owned))
            };
            let message =
                field("message").unwrap_or_else(|| "Decision routing was not saved.".into());
            // The engine's stable code passes through; an engine too old to
            // send one gets the status's own code.
            let code = field("code").unwrap_or_else(|| {
                if status == 409 {
                    "decision_routing_changed_elsewhere".into()
                } else {
                    "decision_routing_invalid".into()
                }
            });
            HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
                .json(json!({"code":code,"message":message}))
        },
        Err(error) => {
            tracing::warn!(%error, "Decision routing settings request failed");
            HttpResponse::ServiceUnavailable().json(json!({"code":"decision_settings_unavailable","message":"Decision Engine is unavailable. Routing settings were not confirmed; reload before retrying."}))
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

    #[actix_web::test]
    async fn decision_routing_proxy_forwards_the_engines_code() {
        let body = |response: HttpResponse| async move {
            let bytes = actix_web::body::to_bytes(response.into_body())
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        };
        let forwarded = body(reply(Err(ClientError::Status {
            status: 400,
            body: r#"{"code":"decision_routing_invalid","message":"Unknown model: x"}"#.into(),
        })))
        .await;
        assert_eq!(forwarded["code"], "decision_routing_invalid");
        assert_eq!(forwarded["message"], "Unknown model: x");
        let older_engine = body(reply(Err(ClientError::Status {
            status: 409,
            body: r#"{"message":"Reload"}"#.into(),
        })))
        .await;
        assert_eq!(older_engine["code"], "decision_routing_changed_elsewhere");
    }
}
