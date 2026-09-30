use std::{collections::HashMap, sync::Arc};

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};

use crate::scope::resolve_required_scope;
use magician::magician_v2::progress_channel_seam::{
    ExecutionProgressRouter, ProgressSeverity, Subscription, SubscriptionFilter, SubscriptionSource,
};

fn default_webhook_retention_secs() -> i64 {
    24 * 60 * 60
}

fn default_min_severity() -> ProgressSeverity {
    ProgressSeverity::Info
}

#[derive(Debug, Deserialize)]
pub struct WebhookSubscriptionsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWebhookSubscriptionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub filter: SubscriptionFilter,
    pub url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default = "default_min_severity")]
    pub min_severity: ProgressSeverity,
    #[serde(default = "default_webhook_retention_secs")]
    pub retention_secs: i64,
}

#[derive(Debug, Serialize)]
pub struct WebhookSubscriptionRecord {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub filter: SubscriptionFilter,
    pub min_severity: ProgressSeverity,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub retention_secs: i64,
    pub source: SubscriptionSource,
    pub watermark: u64,
    pub pending_retry_count: usize,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct WebhookSubscriptionResponse {
    pub subscription: WebhookSubscriptionRecord,
}

#[derive(Debug, Serialize)]
pub struct WebhookSubscriptionListResponse {
    pub subscriptions: Vec<WebhookSubscriptionRecord>,
}

#[derive(Clone)]
pub struct ProgressChannelApi {
    router: ExecutionProgressRouter,
}

impl ProgressChannelApi {
    pub fn new(router: ExecutionProgressRouter) -> Self {
        Self { router }
    }

    pub async fn list_webhook_subscriptions(
        &self,
        req: &HttpRequest,
        query: web::Query<WebhookSubscriptionsQuery>,
    ) -> Result<HttpResponse> {
        let (principal, workspace) =
            match resolve_required_scope(req.headers(), query.workspace.clone()) {
                Ok(scope) => scope,
                Err(response) => return Ok(response),
            };
        let subscriptions = self
            .router
            .list_subscriptions(&principal, &workspace, Some("webhook"))
            .await;
        Ok(HttpResponse::Ok().json(WebhookSubscriptionListResponse {
            subscriptions: subscriptions
                .into_iter()
                .filter_map(subscription_to_record)
                .collect(),
        }))
    }

    pub async fn create_webhook_subscription(
        &self,
        req: &HttpRequest,
        request: web::Json<CreateWebhookSubscriptionRequest>,
    ) -> Result<HttpResponse> {
        let request = request.into_inner();
        let (principal, workspace) =
            match resolve_required_scope(req.headers(), request.workspace.clone()) {
                Ok(scope) => scope,
                Err(response) => return Ok(response),
            };
        let url = request.url.trim();
        if url.is_empty() {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "url is required"
            })));
        }
        if !matches!(
            url::Url::parse(url).ok().map(|parsed| parsed.scheme().to_string()),
            Some(ref scheme) if scheme == "http" || scheme == "https"
        ) {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "url must be a valid http or https URL"
            })));
        }
        if request.retention_secs < -1 || request.retention_secs == i64::MIN {
            return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                "error": "retention_secs must be -1, 0, or a positive number of seconds"
            })));
        }

        let mut metadata = HashMap::new();
        metadata.insert("url".to_string(), url.to_string());
        if !request.headers.is_empty() {
            metadata.insert(
                "headers".to_string(),
                serde_json::to_string(&request.headers).unwrap_or_else(|_| "{}".to_string()),
            );
        }

        let id = match self
            .router
            .subscribe(Subscription {
                id: String::new(),
                channel_id: "webhook".to_string(),
                filter: request.filter,
                principal: principal.clone(),
                workspace,
                min_severity: request.min_severity,
                metadata,
                source: SubscriptionSource::Dynamic,
                retention_secs: request.retention_secs,
                message_template: None,
                output_severity: None,
                watermark: 0,
                pending_retry: Default::default(),
                created_at: 0,
            })
            .await
        {
            Ok(id) => id,
            Err(error) => {
                return Ok(HttpResponse::BadRequest().json(serde_json::json!({
                    "error": format!("{error}")
                })));
            },
        };

        let Some(subscription) = self.router.get_subscription(&id).await else {
            return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "subscription was created but could not be reloaded"
            })));
        };
        Ok(HttpResponse::Created().json(WebhookSubscriptionResponse {
            subscription: subscription_to_record(subscription).expect("webhook subscription"),
        }))
    }

    pub async fn delete_progress_subscription(
        &self,
        req: &HttpRequest,
        path: web::Path<String>,
        query: web::Query<WebhookSubscriptionsQuery>,
    ) -> Result<HttpResponse> {
        let (principal, workspace) =
            match resolve_required_scope(req.headers(), query.workspace.clone()) {
                Ok(scope) => scope,
                Err(response) => return Ok(response),
            };
        let subscription_id = path.into_inner();
        let Some(subscription) = self.router.get_subscription(&subscription_id).await else {
            return Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "subscription not found"
            })));
        };
        if subscription.channel_id != "webhook"
            || subscription.principal != principal
            || subscription.workspace != workspace
        {
            return Ok(HttpResponse::NotFound().json(serde_json::json!({
                "error": "subscription not found"
            })));
        }

        if let Err(error) = self.router.unsubscribe(&subscription_id).await {
            return Ok(HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("{error}")
            })));
        }
        Ok(HttpResponse::NoContent().finish())
    }
}

fn subscription_to_record(subscription: Subscription) -> Option<WebhookSubscriptionRecord> {
    let url = subscription.metadata.get("url")?.to_string();
    let headers = subscription
        .metadata
        .get("headers")
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    Some(WebhookSubscriptionRecord {
        id: subscription.id,
        principal: subscription.principal,
        workspace: subscription.workspace,
        filter: subscription.filter,
        min_severity: subscription.min_severity,
        url,
        headers,
        retention_secs: subscription.retention_secs,
        source: subscription.source,
        watermark: subscription.watermark,
        pending_retry_count: subscription.pending_retry.len(),
        created_at: subscription.created_at,
    })
}

pub async fn list_webhook_subscriptions_handler(
    api: web::Data<Arc<ProgressChannelApi>>,
    req: HttpRequest,
    query: web::Query<WebhookSubscriptionsQuery>,
) -> Result<HttpResponse> {
    api.list_webhook_subscriptions(&req, query).await
}

pub async fn create_webhook_subscription_handler(
    api: web::Data<Arc<ProgressChannelApi>>,
    req: HttpRequest,
    request: web::Json<CreateWebhookSubscriptionRequest>,
) -> Result<HttpResponse> {
    api.create_webhook_subscription(&req, request).await
}

pub async fn delete_progress_subscription_handler(
    api: web::Data<Arc<ProgressChannelApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<WebhookSubscriptionsQuery>,
) -> Result<HttpResponse> {
    api.delete_progress_subscription(&req, path, query).await
}
