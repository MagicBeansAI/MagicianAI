use std::{collections::HashMap, time::Duration};

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client,
};
use tokio::time::sleep;

use crate::magician_v2::progress_channel_seam::{
    channel::ProgressChannel,
    surface_routing::webhook_surface_renders_agent_event,
    types::{ProgressMessage, Subscription},
};

const REQUEST_TIMEOUT_SECS: u64 = 10;
const DELIVERY_ATTEMPTS: usize = 3;
const INITIAL_BACKOFF_MS: u64 = 250;

#[derive(Clone)]
pub struct WebhookChannel {
    client: Client,
}

impl WebhookChannel {
    pub fn new() -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("failed to construct webhook progress client")?;
        Ok(Self { client })
    }

    fn parse_headers(metadata: &HashMap<String, String>) -> anyhow::Result<HeaderMap> {
        let raw = metadata
            .get("headers")
            .map(String::as_str)
            .unwrap_or("")
            .trim();
        if raw.is_empty() {
            return Ok(HeaderMap::new());
        }

        let parsed: HashMap<String, String> =
            serde_json::from_str(raw).context("webhook metadata.headers must be valid JSON")?;
        let mut headers = HeaderMap::new();
        for (name, value) in parsed {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid webhook header name `{name}`"))?;
            let header_value = HeaderValue::from_str(&value)
                .with_context(|| format!("invalid webhook header value for `{name}`"))?;
            headers.insert(header_name, header_value);
        }
        Ok(headers)
    }
}

#[async_trait]
impl ProgressChannel for WebhookChannel {
    fn id(&self) -> &str {
        "webhook"
    }

    async fn deliver(
        &self,
        subscription: &Subscription,
        message: &ProgressMessage,
    ) -> anyhow::Result<()> {
        // Taxonomy-driven visibility gate. Webhooks fire for lifecycle
        // milestones and curated user-facing categories — never for the
        // raw agentic-stream / planning / LLM internals. The decision
        // is sourced from `realtime_events::GAUI_EVENT_TAXONOMY` via
        // the shared `webhook_surface_renders_agent_event` predicate,
        // so adding a new event_type with the right category routes it
        // correctly here without touching this file.
        if let Some(event_type) = message.event_type.as_deref() {
            if !webhook_surface_renders_agent_event(event_type) {
                return Ok(());
            }
        }

        let url = subscription
            .metadata
            .get("url")
            .map(String::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("webhook subscription missing url metadata"))?;
        let headers = Self::parse_headers(&subscription.metadata)?;

        let mut backoff = Duration::from_millis(INITIAL_BACKOFF_MS);
        let mut last_error = None;
        for attempt in 0..DELIVERY_ATTEMPTS {
            let request = self.client.post(url).headers(headers.clone()).json(message);
            match request.send().await {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    last_error = Some(anyhow!(
                        "webhook returned HTTP {}{}",
                        status,
                        if body.trim().is_empty() {
                            String::new()
                        } else {
                            format!(": {}", body.trim())
                        }
                    ));
                },
                Err(error) => {
                    last_error = Some(error.into());
                },
            }

            if attempt + 1 < DELIVERY_ATTEMPTS {
                sleep(backoff).await;
                backoff *= 2;
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("webhook delivery failed")))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use wiremock::{
        matchers::{body_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    use crate::magician_v2::progress_channel_seam::types::{
        ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource, Subscription,
        SubscriptionFilter, SubscriptionSource,
    };

    use crate::magician_v2::progress_channel_seam::*;

    fn sample_message() -> ProgressMessage {
        ProgressMessage {
            id: "msg-1".to_string(),
            seq: 1,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-1".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            parent_execution_id: None,
            agent_id: Some("agent-a".to_string()),
            ui_thread_id: None,
            step_id: None,
            routing_keys: vec!["task/task-1".to_string()],
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "completed".to_string(),
                summary: Some("done".to_string()),
            },
            timestamp: 1_700_000_000_000,
        }
    }

    fn sample_subscription(url: &str) -> Subscription {
        let mut metadata = HashMap::new();
        metadata.insert("url".to_string(), url.to_string());
        metadata.insert(
            "headers".to_string(),
            serde_json::json!({ "x-progress-token": "secret" }).to_string(),
        );
        Subscription {
            id: "sub-1".to_string(),
            channel_id: "webhook".to_string(),
            filter: SubscriptionFilter::TaskId("task-1".to_string()),
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            min_severity: ProgressSeverity::Info,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 86400,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        }
    }

    #[tokio::test]
    async fn webhook_channel_posts_progress_message_json() {
        let server = MockServer::start().await;
        let message = sample_message();
        Mock::given(method("POST"))
            .and(path("/progress"))
            .and(header("x-progress-token", "secret"))
            .and(body_json(&message))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let channel = WebhookChannel::new().expect("channel");
        channel
            .deliver(
                &sample_subscription(&format!("{}/progress", server.uri())),
                &message,
            )
            .await
            .expect("delivery should succeed");
    }
}
