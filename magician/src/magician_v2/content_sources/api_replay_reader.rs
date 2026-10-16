use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;

use super::{
    public_http::{resolve_public_browser_url, validate_public_http_url},
    retrieval::{
        RetrievalTransportFailure, RetrievalTransportTrace, META_ACTUAL_TRANSPORT, META_EXTRACT_MS,
        META_SESSION_OUTCOME,
    },
    AdapterAuth, AdapterExecution, ContentDocument, ContentPrivacy, ContentProvenance,
    ContentSourceCapabilities, ContentSourceClass, ContentSourceDescriptor, ReadRequest,
    RetrievalActionMetadata, RetrievalAuthority, RetrievalOutputKind, RetrievalRung,
    SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION, MAX_DOCUMENT_TEXT_CHARS,
};
use crate::{
    config::ApiMiningConfig,
    magician_v2::api_mining::{
        replay::{ApiRunner, ReplayDnsPin, ReplayRequest},
        router::{ApiRouter, RouteDecision},
        types::SessionContext,
    },
};

pub const API_REPLAY_READER_ID: &str = "verified-api-replay-reader";
pub const API_REPLAY_READ_ACTION: &str = "api_replay.read";

#[derive(Debug, Clone)]
pub struct VerifiedApiReplayReader {
    descriptor: ContentSourceDescriptor,
    config: ApiMiningConfig,
    base_path: PathBuf,
    max_document_chars: usize,
}

impl VerifiedApiReplayReader {
    pub fn new(config: ApiMiningConfig, base_path: PathBuf, max_document_chars: usize) -> Self {
        Self {
            descriptor: ContentSourceDescriptor {
                adapter_id: API_REPLAY_READER_ID.into(),
                display_name: "Verified read-only API replay".into(),
                class: ContentSourceClass::WebPage,
                capabilities: ContentSourceCapabilities {
                    discovery: false,
                    full_content: true,
                    cursor: false,
                    conditional_fetch: false,
                    execution: AdapterExecution::LocalProcess,
                    auth: AdapterAuth::Optional,
                    sends_user_intent: false,
                    metered: false,
                },
                retrieval: RetrievalActionMetadata::reader(
                    API_REPLAY_READ_ACTION,
                    RetrievalRung::VerifiedReplay,
                    RetrievalAuthority::PublicRemoteRead,
                    vec![RetrievalOutputKind::Gist, RetrievalOutputKind::FullText],
                ),
            },
            config,
            base_path,
            max_document_chars: max_document_chars.min(MAX_DOCUMENT_TEXT_CHARS),
        }
    }
}

#[async_trait]
impl super::ContentReader for VerifiedApiReplayReader {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn read(&self, request: &ReadRequest) -> Result<ContentDocument> {
        if request.candidate.privacy != ContentPrivacy::Public {
            bail!("verified anonymous API replay cannot read private content");
        }
        let raw_url = request
            .candidate
            .canonical_url
            .as_deref()
            .ok_or_else(|| anyhow!("verified API replay requires a canonical URL"))?;
        let url = validate_public_http_url(raw_url)?.to_string();
        let router = ApiRouter::with_base_path(&self.config, &self.base_path);
        let decision = router.route_navigate(&url, &SessionContext::default(), &Default::default());
        let (replay_request, request_params, capability_id, origin, read_only_hint) = match decision
        {
            RouteDecision::Replay {
                request,
                request_params,
                capability_id,
                origin,
                read_only_hint,
                ..
            } => (
                request,
                request_params,
                capability_id,
                origin,
                read_only_hint,
            ),
            RouteDecision::ReplayRequiresHitl { .. } => {
                bail!("authentication required: API replay requires approval")
            },
            RouteDecision::PassThrough { reason, .. } => {
                bail!("verified API replay unavailable: {reason}")
            },
        };
        if !read_only_hint {
            bail!("verified API replay rejected a non-read-only capability");
        }
        let dns_pin = validate_anonymous_replay_request(&replay_request, &url).await?;

        let mut runner = ApiRunner::with_base_path(&self.base_path)
            .map_err(|error| anyhow!("initializing verified API replay: {error}"))?;
        let replay = runner
            .replay_with_reqwest_pinned(
                &origin,
                &capability_id,
                &request_params,
                &SessionContext::default(),
                Some(&url),
                &dns_pin,
                None,
            )
            .await
            .map_err(|error| {
                replay_failure(format!("running verified API replay: {error}"), None)
            })?;
        if replay.status == 401 || replay.status == 403 || replay.auth_failure {
            return Err(replay_failure(
                format!(
                    "authentication required: verified API replay returned status {}",
                    replay.status
                ),
                Some(replay.timing_ms),
            ));
        }
        if !replay.success {
            return Err(replay_failure(
                format!(
                    "verified API replay unavailable: {}",
                    replay
                        .error
                        .or(replay.fallback_reason)
                        .unwrap_or_else(|| format!("status {}", replay.status))
                ),
                Some(replay.timing_ms),
            ));
        }
        let body = replay
            .response_body
            .filter(|body| !body.trim().is_empty())
            .ok_or_else(|| {
                replay_failure(
                    "verified API replay returned an empty response".into(),
                    Some(replay.timing_ms),
                )
            })?;
        let text = body
            .chars()
            .take(self.max_document_chars)
            .collect::<String>();
        let mut metadata = BTreeMap::new();
        metadata.insert(META_ACTUAL_TRANSPORT.into(), json!("api_replay"));
        metadata.insert(META_SESSION_OUTCOME.into(), json!("not_applicable"));
        metadata.insert(META_EXTRACT_MS.into(), json!(replay.timing_ms));
        metadata.insert("api_replay_status".into(), json!(replay.status));
        metadata.insert(
            "api_replay_capability_fingerprint".into(),
            json!(blake3::hash(capability_id.as_bytes()).to_hex().to_string()),
        );

        Ok(ContentDocument {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new(
                API_REPLAY_READER_ID,
                blake3::hash(url.as_bytes()).to_hex().to_string(),
            )?,
            title: request.candidate.title.clone(),
            text: text.clone(),
            canonical_url: Some(url.clone()),
            media_type: Some(
                if serde_json::from_str::<serde_json::Value>(&text).is_ok() {
                    "application/json; source=verified-api-replay".into()
                } else {
                    "text/plain; source=verified-api-replay".into()
                },
            ),
            fetched_at_ms: Utc::now().timestamp_millis(),
            privacy: ContentPrivacy::Public,
            content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            provenance: ContentProvenance {
                source_label: url::Url::parse(&url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_string))
                    .unwrap_or_else(|| "verified API replay".into()),
                source_url: Some(url),
                retrieved_by: API_REPLAY_READ_ACTION.into(),
            },
            metadata,
        })
    }
}

async fn validate_anonymous_replay_request(
    request: &ReplayRequest,
    requested_url: &str,
) -> Result<ReplayDnsPin> {
    let replay_url = validate_public_http_url(&request.url)?;
    let requested = validate_public_http_url(requested_url)?;
    if replay_url.host_str().map(str::to_ascii_lowercase)
        != requested.host_str().map(str::to_ascii_lowercase)
    {
        bail!("verified API replay target escaped the requested public domain");
    }
    if request.headers.keys().any(|name| {
        matches!(
            name.trim().to_ascii_lowercase().as_str(),
            "authorization"
                | "proxy-authorization"
                | "cookie"
                | "set-cookie"
                | "x-api-key"
                | "x-auth-token"
        )
    }) {
        bail!("verified anonymous API replay cannot send credential-bearing headers");
    }
    if replay_url
        .query_pairs()
        .any(|(name, _)| sensitive_credential_name(&name))
        || request
            .body
            .as_deref()
            .is_some_and(body_contains_credential_fields)
    {
        bail!("verified anonymous API replay cannot send credential-bearing query/body fields");
    }
    let (replay_url, addresses) = resolve_public_browser_url(replay_url.as_str()).await?;
    Ok(ReplayDnsPin {
        host: replay_url
            .host_str()
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| anyhow!("verified API replay URL requires a host"))?,
        addresses,
    })
}

fn sensitive_credential_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "access_token"
            | "api_key"
            | "apikey"
            | "auth"
            | "authorization"
            | "cookie"
            | "key"
            | "passwd"
            | "password"
            | "session"
            | "sig"
            | "signature"
            | "token"
    )
}

fn body_contains_credential_fields(body: &str) -> bool {
    fn json_contains(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(object) => object
                .iter()
                .any(|(name, value)| sensitive_credential_name(name) || json_contains(value)),
            serde_json::Value::Array(values) => values.iter().any(json_contains),
            _ => false,
        }
    }
    serde_json::from_str::<serde_json::Value>(body)
        .map(|value| json_contains(&value))
        .unwrap_or_else(|_| {
            url::form_urlencoded::parse(body.as_bytes())
                .any(|(name, _)| sensitive_credential_name(&name))
        })
}

fn replay_failure(message: String, timing_ms: Option<u64>) -> anyhow::Error {
    RetrievalTransportFailure {
        message,
        trace: RetrievalTransportTrace {
            actual_transport: Some("api_replay".into()),
            session_outcome: Some("not_applicable".into()),
            extract_ms: timing_ms,
            ..RetrievalTransportTrace::default()
        },
    }
    .into()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn replay_request(url: &str) -> ReplayRequest {
        ReplayRequest {
            method: "GET".into(),
            url: url.into(),
            headers: HashMap::new(),
            timeout_ms: 1_000,
            body: None,
        }
    }

    #[tokio::test]
    async fn anonymous_replay_rejects_cross_domain_and_credentials_before_execution() {
        let cross_domain = validate_anonymous_replay_request(
            &replay_request("https://example.net/api"),
            "https://example.com/page",
        )
        .await
        .unwrap_err();
        assert!(cross_domain.to_string().contains("escaped"));

        let mut credentialed = replay_request("https://example.com/api");
        credentialed
            .headers
            .insert("Authorization".into(), "Bearer secret".into());
        let credentialed =
            validate_anonymous_replay_request(&credentialed, "https://example.com/page")
                .await
                .unwrap_err();
        assert!(credentialed.to_string().contains("credential-bearing"));

        let query_secret = validate_anonymous_replay_request(
            &replay_request("https://example.com/api?api_key=secret"),
            "https://example.com/page",
        )
        .await
        .unwrap_err();
        assert!(query_secret.to_string().contains("query/body"));
    }
}
