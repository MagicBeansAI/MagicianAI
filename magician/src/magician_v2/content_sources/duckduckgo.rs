use std::collections::BTreeMap;

use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;

use super::{
    AdapterAuth, AdapterExecution, ContentCandidate, ContentPrivacy, ContentProvenance,
    ContentSourceCapabilities, ContentSourceClass, ContentSourceDescriptor, DiscoveryAdapter,
    DiscoveryPage, DiscoveryRequest, RetrievalActionMetadata, RetrievalAuthority, RetrievalRung,
    SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION,
};
use crate::magician_v2::execution::compiled_handlers::web_search::search_public_web;

pub struct DuckDuckGoDiscoveryAdapter {
    descriptor: ContentSourceDescriptor,
}

impl DuckDuckGoDiscoveryAdapter {
    pub fn new() -> Self {
        Self {
            descriptor: ContentSourceDescriptor {
                adapter_id: "web-search".into(),
                display_name: "DuckDuckGo public web search".into(),
                class: ContentSourceClass::WebSearch,
                capabilities: ContentSourceCapabilities {
                    discovery: true,
                    full_content: false,
                    cursor: false,
                    conditional_fetch: false,
                    execution: AdapterExecution::RemoteEndpoint,
                    auth: AdapterAuth::None,
                    sends_user_intent: true,
                    metered: false,
                },
                retrieval: {
                    let mut metadata = RetrievalActionMetadata::discovery(
                        "web_search.discover",
                        RetrievalRung::PublicSearch,
                        RetrievalAuthority::PublicRemoteRead,
                        true,
                    );
                    metadata.accepted_options =
                        vec!["allowed_domains".into(), "blocked_domains".into()];
                    metadata
                },
            },
        }
    }
}

#[async_trait]
impl DiscoveryAdapter for DuckDuckGoDiscoveryAdapter {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage> {
        let query = request
            .query
            .as_deref()
            .filter(|query| !query.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("DuckDuckGo discovery requires a query"))?;
        if request.cursor.is_some() {
            bail!("DuckDuckGo discovery does not support cursors");
        }
        let allowed_domains = string_list_option(&request.options, "allowed_domains")?;
        let blocked_domains = string_list_option(&request.options, "blocked_domains")?;
        let observed_at_ms = Utc::now().timestamp_millis();
        let results =
            search_public_web(query, &allowed_domains, &blocked_domains, request.limit).await?;
        let items = normalize_results(&self.descriptor, results, observed_at_ms)?;
        Ok(DiscoveryPage {
            items,
            next_cursor: None,
            validators: BTreeMap::new(),
            cost: None,
            transport: Default::default(),
        })
    }
}

fn normalize_results(
    descriptor: &ContentSourceDescriptor,
    results: Vec<
        crate::magician_v2::execution::compiled_handlers::web_search::PublicWebSearchResult,
    >,
    observed_at_ms: i64,
) -> Result<Vec<ContentCandidate>> {
    results
        .into_iter()
        .map(|result| {
            let canonical_url = super::canonicalize_http_url(&result.url)?;
            Ok(ContentCandidate {
                schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
                identity: SourceIdentity::new(
                    &descriptor.adapter_id,
                    blake3::hash(canonical_url.as_bytes()).to_hex().to_string(),
                )?,
                title: result.title,
                cheap_text: if result.snippet.trim().is_empty() {
                    canonical_url.clone()
                } else {
                    result.snippet
                },
                canonical_url: Some(canonical_url.clone()),
                published_at_ms: None,
                observed_at_ms,
                privacy: ContentPrivacy::Public,
                content_hash: None,
                provenance: ContentProvenance {
                    source_label: "DuckDuckGo".into(),
                    source_url: Some(canonical_url),
                    retrieved_by: descriptor.adapter_id.clone(),
                },
                metadata: BTreeMap::new(),
            })
        })
        .collect()
}

fn string_list_option(options: &BTreeMap<String, Value>, name: &str) -> Result<Vec<String>> {
    let Some(value) = options.get(name) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{name} must be an array of strings"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("{name} must contain only strings"))
        })
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::magician_v2::execution::compiled_handlers::web_search::PublicWebSearchResult;

    #[test]
    fn compiled_web_search_results_use_the_provider_neutral_candidate_contract() {
        let adapter = DuckDuckGoDiscoveryAdapter::new();
        let items = normalize_results(
            adapter.descriptor(),
            vec![PublicWebSearchResult {
                title: "Rust runtime".into(),
                url: "https://example.com/runtime?utm_source=ddg#details".into(),
                snippet: "Async cancellation and scheduling".into(),
            }],
            42,
        )
        .unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].identity.adapter_id, "web-search");
        assert_eq!(items[0].observed_at_ms, 42);
        assert_eq!(
            items[0].canonical_url.as_deref(),
            Some("https://example.com/runtime")
        );
        assert_eq!(items[0].provenance.source_label, "DuckDuckGo");
        assert_eq!(items[0].provenance.retrieved_by, "web-search");
        assert_eq!(items[0].privacy, ContentPrivacy::Public);
    }

    #[test]
    fn compiled_web_search_option_contract_rejects_non_string_lists() {
        let options = BTreeMap::from([("allowed_domains".into(), json!(["example.com", 7]))]);
        assert!(string_list_option(&options, "allowed_domains").is_err());
    }
}
