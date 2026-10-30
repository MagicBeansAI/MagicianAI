//! Browser Web Push subscriptions (RFC 8030/8291/8292).
//!
//! A web session subscribes its browser with the runtime's VAPID public key
//! and registers the resulting `PushSubscription` here. The scope is the
//! request's own (the auth middleware fixes `X-Principal`/`X-Workspace`); the
//! body never names one. Delivery rides the mobile push dispatcher, so a
//! browser receives the same content-free attention pushes a phone does.
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use magician::magician_v2::api_scope::resolve_required_scope;
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::mobile_push::{MobilePushConfig, MobilePushError, MobilePushStore};
use magician::magician_v2::runtime_settings::{runtime_settings_paths, update_env_file};
use magician::magician_v2::web_push::{
    generate_vapid_key_pair, validate_subscription, WebPushKeys, WebPushVapid,
    VAPID_PRIVATE_KEY_ENV, VAPID_PUBLIC_KEY_ENV, VAPID_SUBJECT_ENV,
};

#[derive(Debug, Deserialize)]
pub struct WebPushSubscriptionBody {
    pub endpoint: String,
    pub keys: WebPushKeys,
}

#[derive(Debug, Deserialize)]
pub struct PutWebPushSubscriptionRequest {
    pub subscription: WebPushSubscriptionBody,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteWebPushSubscriptionRequest {
    pub endpoint: String,
}

#[derive(Debug, Serialize)]
struct WebPushConfigResponse {
    enabled: bool,
    vapid_public_key: Option<String>,
}

/// `GET /api/magician/v2/web-push/config`
pub async fn get_web_push_config_handler(
    req: HttpRequest,
    provider_config: web::Data<MobilePushConfig>,
) -> Result<HttpResponse> {
    if let Err(response) = resolve_required_scope(req.headers(), None) {
        return Ok(response);
    }
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(config_response(provider_config.get_ref())))
}

/// `POST /api/magician/v2/web-push/vapid` — owner only. Generates this
/// deployment's VAPID pair, stores it in the runtime env file (the private key
/// is never returned) and turns browser Web Push on for the running process.
/// Refuses when web push is already on: replacing the keys would orphan every
/// browser subscription made with the current public key.
pub async fn create_web_push_vapid_handler(
    req: HttpRequest,
    provider_config: web::Data<MobilePushConfig>,
    auth: Option<web::Data<AuthRuntime>>,
) -> Result<HttpResponse> {
    if let Err(response) = crate::programs_api::require_owner_session_or_phone(
        &req,
        auth.as_ref().map(|data| data.get_ref()),
    )
    .await
    {
        return Ok(response);
    }
    if provider_config.web_push_public_key().is_some() {
        return Ok(HttpResponse::Conflict().json(json!({
            "error": "web_push_already_configured",
            "message": "Browser notifications are already set up on this server."
        })));
    }
    let (public_key, private_key) = match generate_vapid_key_pair() {
        Ok(pair) => pair,
        Err(error) => {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "vapid_generation_failed",
                "message": error.to_string()
            })))
        },
    };
    let subject = std::env::var(VAPID_SUBJECT_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "https://magican.ai".to_string());
    let vapid = match WebPushVapid::new(&public_key, &private_key, &subject) {
        Ok(vapid) => vapid,
        Err(error) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "vapid_subject_invalid",
                "message": error.to_string()
            })))
        },
    };
    // Boot reads `.env.development` before `.env`, so the pair goes into the
    // target file and every other runtime env file that exists: a stale pair
    // left in one file would otherwise shadow the new one at the next start.
    let paths = runtime_settings_paths();
    let mut targets = vec![paths.env_target_path.clone()];
    for path in [&paths.env_development_path, &paths.env_path] {
        if path.exists() && !targets.contains(path) {
            targets.push(path.clone());
        }
    }
    let updates = [
        (VAPID_PUBLIC_KEY_ENV, Some(public_key.clone())),
        (VAPID_PRIVATE_KEY_ENV, Some(private_key.clone())),
    ];
    for target in &targets {
        if let Err(error) = update_env_file(target, &updates) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "env_write_failed",
                "message": format!("could not store the keys in {}: {error}", target.display())
            })));
        }
    }
    std::env::set_var(VAPID_PUBLIC_KEY_ENV, &public_key);
    std::env::set_var(VAPID_PRIVATE_KEY_ENV, &private_key);
    provider_config.install_web_push(vapid);
    tracing::info!(
        env_files = targets.len(),
        "[WEB-PUSH] generated and installed VAPID keys"
    );
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(config_response(provider_config.get_ref())))
}

fn config_response(config: &MobilePushConfig) -> WebPushConfigResponse {
    let key = config.web_push_public_key();
    WebPushConfigResponse {
        enabled: key.is_some(),
        vapid_public_key: key,
    }
}

/// `PUT /api/magician/v2/web-push/subscription` — idempotent per endpoint.
pub async fn put_web_push_subscription_handler(
    req: HttpRequest,
    body: web::Json<PutWebPushSubscriptionRequest>,
    store: web::Data<Arc<MobilePushStore>>,
    provider_config: web::Data<MobilePushConfig>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    if provider_config.web_push_public_key().is_none() {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "web_push_not_configured",
            "detail": "this deployment has no VAPID keys"
        })));
    }
    let body = body.into_inner();
    let endpoint = body.subscription.endpoint.trim().to_string();
    if let Err(error) = validate_subscription(&endpoint, &body.subscription.keys) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "invalid_web_push_subscription",
            "detail": error.to_string()
        })));
    }
    match store
        .upsert_web(
            &principal,
            &workspace,
            &endpoint,
            &body.subscription.keys,
            body.label.as_deref(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
    {
        Ok(row) => Ok(HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(json!({ "id": row.id }))),
        Err(error) => Ok(store_error_response(error)),
    }
}

/// `DELETE /api/magician/v2/web-push/subscription` — 204 whether or not the
/// endpoint was registered.
pub async fn delete_web_push_subscription_handler(
    req: HttpRequest,
    body: web::Json<DeleteWebPushSubscriptionRequest>,
    store: web::Data<Arc<MobilePushStore>>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let endpoint = body.endpoint.trim();
    if endpoint.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "invalid_web_push_subscription",
            "detail": "endpoint is required"
        })));
    }
    match store.remove_web(&principal, &workspace, endpoint).await {
        Ok(_) => Ok(HttpResponse::NoContent().finish()),
        Err(error) => Ok(store_error_response(error)),
    }
}

fn store_error_response(error: MobilePushError) -> HttpResponse {
    let status = match error {
        MobilePushError::InvalidToken
        | MobilePushError::InvalidBinding
        | MobilePushError::TaskRequired
        | MobilePushError::UnexpectedTask => actix_web::http::StatusCode::BAD_REQUEST,
        MobilePushError::Capacity => actix_web::http::StatusCode::TOO_MANY_REQUESTS,
        MobilePushError::Io(_) | MobilePushError::Corrupt(_) => {
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
        },
    };
    HttpResponse::build(status).json(json!({
        "error": "web_push_registration_failed",
        "detail": error.to_string()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};
    use magician::magician_v2::web_push::WebPushVapid;

    const PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const ENDPOINT: &str = "https://fcm.googleapis.com/fcm/send/browser-1";

    fn enabled() -> MobilePushConfig {
        MobilePushConfig::default().with_web_push(
            WebPushVapid::new(
                PUBLIC,
                "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw",
                "mailto:owner@example.test",
            )
            .unwrap(),
        )
    }

    fn subscription(endpoint: &str) -> serde_json::Value {
        json!({
            "subscription": {
                "endpoint": endpoint,
                "keys": {
                    "p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
                    "auth": "BTBZMqHH6r4Tts7J_aSIgg"
                }
            },
            "label": "Chrome on Mac"
        })
    }

    macro_rules! app {
        ($store:expr, $config:expr) => {
            test::init_service(
                App::new()
                    .app_data(web::Data::new($store))
                    .app_data(web::Data::new($config))
                    .route(
                        "/web-push/config",
                        web::get().to(get_web_push_config_handler),
                    )
                    .route(
                        "/web-push/subscription",
                        web::put().to(put_web_push_subscription_handler),
                    )
                    .route(
                        "/web-push/subscription",
                        web::delete().to(delete_web_push_subscription_handler),
                    ),
            )
            .await
        };
    }

    fn scoped(request: test::TestRequest) -> test::TestRequest {
        request
            .insert_header(("X-Principal", "owner"))
            .insert_header(("X-Workspace", "default"))
    }

    #[actix_web::test]
    async fn config_reports_the_vapid_key_only_when_web_push_is_on() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(MobilePushStore::open(temp.path()).await.unwrap());
        let app = app!(Arc::clone(&store), enabled());
        let response: serde_json::Value = test::call_and_read_body_json(
            &app,
            scoped(test::TestRequest::get().uri("/web-push/config")).to_request(),
        )
        .await;
        assert_eq!(
            response,
            json!({"enabled": true, "vapid_public_key": PUBLIC})
        );

        let off = app!(store, MobilePushConfig::default());
        let response: serde_json::Value = test::call_and_read_body_json(
            &off,
            scoped(test::TestRequest::get().uri("/web-push/config")).to_request(),
        )
        .await;
        assert_eq!(
            response,
            json!({"enabled": false, "vapid_public_key": null})
        );
        // No scope, no answer.
        let response = test::call_service(
            &off,
            test::TestRequest::get()
                .uri("/web-push/config")
                .to_request(),
        )
        .await;
        assert!(response.status().is_client_error());
    }

    #[actix_web::test]
    async fn a_browser_subscribes_idempotently_and_unsubscribes() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(MobilePushStore::open(temp.path()).await.unwrap());
        let app = app!(Arc::clone(&store), enabled());
        let put = || {
            scoped(test::TestRequest::put().uri("/web-push/subscription"))
                .set_json(subscription(ENDPOINT))
                .to_request()
        };
        let first: serde_json::Value = test::call_and_read_body_json(&app, put()).await;
        let second: serde_json::Value = test::call_and_read_body_json(&app, put()).await;
        assert!(first["id"].as_str().unwrap().starts_with("push_"));
        assert_eq!(first, second, "idempotent per endpoint");
        assert_eq!(
            first.as_object().unwrap().len(),
            1,
            "the id only, never keys"
        );
        let rows = store.application_registrations("owner", "default").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label.as_deref(), Some("Chrome on Mac"));

        let delete = || {
            scoped(test::TestRequest::delete().uri("/web-push/subscription"))
                .set_json(json!({"endpoint": ENDPOINT}))
                .to_request()
        };
        assert_eq!(test::call_service(&app, delete()).await.status(), 204);
        assert_eq!(test::call_service(&app, delete()).await.status(), 204);
        assert!(store
            .application_registrations("owner", "default")
            .await
            .is_empty());
    }

    #[actix_web::test]
    async fn subscriptions_are_validated_scoped_and_refused_when_web_push_is_off() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(MobilePushStore::open(temp.path()).await.unwrap());
        let app = app!(Arc::clone(&store), enabled());
        // A URL that is not a browser push service is never stored (or fetched).
        let response = test::call_service(
            &app,
            scoped(test::TestRequest::put().uri("/web-push/subscription"))
                .set_json(subscription("https://169.254.169.254/latest/meta-data"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), 400);
        let mut bad_keys = subscription(ENDPOINT);
        bad_keys["subscription"]["keys"]["auth"] = json!("AAAA");
        let response = test::call_service(
            &app,
            scoped(test::TestRequest::put().uri("/web-push/subscription"))
                .set_json(bad_keys)
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), 400);
        // No scope headers: refused before anything is stored.
        let response = test::call_service(
            &app,
            test::TestRequest::put()
                .uri("/web-push/subscription")
                .set_json(subscription(ENDPOINT))
                .to_request(),
        )
        .await;
        assert!(response.status().is_client_error());
        assert!(store.registered_scopes().await.is_empty());

        let off = app!(Arc::clone(&store), MobilePushConfig::default());
        let response = test::call_service(
            &off,
            scoped(test::TestRequest::put().uri("/web-push/subscription"))
                .set_json(subscription(ENDPOINT))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), 503);
        assert!(store.registered_scopes().await.is_empty());
    }
}
