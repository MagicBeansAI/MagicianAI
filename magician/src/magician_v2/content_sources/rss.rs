use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use feed_rs::model::Entry;
use serde_json::json;

use super::{
    public_http::{
        validate_public_http_url, ConditionalHttpRequest, PublicHttpFetch, PublicHttpFetchPolicy,
        PublicHttpFetcher,
    },
    traits::DiscoveryAdapter,
    types::{
        canonicalize_http_url, AdapterAuth, AdapterExecution, ContentCandidate, ContentPrivacy,
        ContentProvenance, ContentSourceCapabilities, ContentSourceClass, ContentSourceDescriptor,
        DiscoveryPage, DiscoveryRequest, DiscoveryTransportStats, DiscoveryValidator,
        SourceIdentity, CONTENT_SOURCE_SCHEMA_VERSION, MAX_CANDIDATE_CHEAP_TEXT_CHARS,
        MAX_CANDIDATE_TITLE_CHARS,
    },
};

const RSS_ADAPTER_ID: &str = "rss";
const MAX_FEED_BYTES: usize = 2 * 1024 * 1024;

#[async_trait]
trait FeedTransport: Send + Sync {
    async fn fetch(
        &self,
        url: &str,
        conditional: &ConditionalHttpRequest,
    ) -> Result<PublicHttpFetch>;
}

struct ReqwestFeedTransport {
    fetcher: PublicHttpFetcher,
}

impl ReqwestFeedTransport {
    fn new() -> Result<Self> {
        Ok(Self {
            fetcher: PublicHttpFetcher::new(PublicHttpFetchPolicy {
                max_response_bytes: MAX_FEED_BYTES,
                max_redirects: 5,
                timeout: Duration::from_secs(20),
                user_agent: "MagicianFeedReader/1".into(),
                // Feed servers use many inconsistent content types. The
                // bounded parser below remains the payload validator.
                accepted_media_types: Default::default(),
                allow_missing_media_type: true,
            })?,
        })
    }
}

#[async_trait]
impl FeedTransport for ReqwestFeedTransport {
    async fn fetch(
        &self,
        url: &str,
        conditional: &ConditionalHttpRequest,
    ) -> Result<PublicHttpFetch> {
        self.fetcher.fetch(url, conditional).await
    }
}

pub struct RssDiscoveryAdapter {
    descriptor: ContentSourceDescriptor,
    transport: Arc<dyn FeedTransport>,
}

impl RssDiscoveryAdapter {
    pub fn new() -> Result<Self> {
        Ok(Self::with_transport(Arc::new(ReqwestFeedTransport::new()?)))
    }

    fn with_transport(transport: Arc<dyn FeedTransport>) -> Self {
        Self {
            descriptor: ContentSourceDescriptor {
                adapter_id: RSS_ADAPTER_ID.into(),
                display_name: "RSS / Atom".into(),
                class: ContentSourceClass::Syndication,
                capabilities: ContentSourceCapabilities {
                    discovery: true,
                    full_content: false,
                    cursor: false,
                    conditional_fetch: true,
                    execution: AdapterExecution::RemoteEndpoint,
                    auth: AdapterAuth::None,
                    // The configured URL reaches its origin, but the user's
                    // feed intent and generated search query do not.
                    sends_user_intent: false,
                    metered: false,
                },
                retrieval: {
                    let mut metadata = super::RetrievalActionMetadata::discovery(
                        "rss.discover",
                        super::RetrievalRung::SourceNative,
                        super::RetrievalAuthority::PublicRemoteRead,
                        true,
                    );
                    metadata.accepts_targets = true;
                    metadata.requires_targets = true;
                    metadata.accepted_options = Vec::new();
                    metadata
                },
            },
            transport,
        }
    }
}

#[async_trait]
impl DiscoveryAdapter for RssDiscoveryAdapter {
    fn descriptor(&self) -> &ContentSourceDescriptor {
        &self.descriptor
    }

    async fn discover(&self, request: &DiscoveryRequest) -> Result<DiscoveryPage> {
        if request.targets.is_empty() {
            bail!("RSS discovery requires at least one feed URL");
        }

        let mut candidate_groups = Vec::with_capacity(request.targets.len());
        let mut validators = BTreeMap::new();
        let mut transport = DiscoveryTransportStats::default();
        for target in &request.targets {
            validate_public_http_url(target)?;
            let conditional = request
                .validators
                .get(target)
                .map(|validator| ConditionalHttpRequest {
                    etag: validator.etag.clone(),
                    last_modified: validator.last_modified.clone(),
                })
                .unwrap_or_default();
            match self.transport.fetch(target, &conditional).await? {
                PublicHttpFetch::Modified {
                    etag,
                    last_modified,
                    body,
                    ..
                } => {
                    transport.modified_targets = transport.modified_targets.saturating_add(1);
                    transport.response_bytes = transport
                        .response_bytes
                        .saturating_add(body.len().min(u64::MAX as usize) as u64);
                    candidate_groups.push(VecDeque::from(parse_feed_candidates(
                        target,
                        &body,
                        request.limit,
                    )?));
                    validators.insert(
                        target.clone(),
                        DiscoveryValidator {
                            etag,
                            last_modified,
                        },
                    );
                },
                PublicHttpFetch::NotModified {
                    etag,
                    last_modified,
                    ..
                } => {
                    transport.not_modified_targets =
                        transport.not_modified_targets.saturating_add(1);
                    candidate_groups.push(VecDeque::new());
                    validators.insert(
                        target.clone(),
                        DiscoveryValidator {
                            etag: etag.or(conditional.etag),
                            last_modified: last_modified.or(conditional.last_modified),
                        },
                    );
                },
            }
        }

        let mut items = Vec::with_capacity(request.limit);
        while items.len() < request.limit {
            let mut advanced = false;
            for candidates in &mut candidate_groups {
                if let Some(candidate) = candidates.pop_front() {
                    items.push(candidate);
                    advanced = true;
                    if items.len() == request.limit {
                        break;
                    }
                }
            }
            if !advanced {
                break;
            }
        }

        Ok(DiscoveryPage {
            items,
            next_cursor: None,
            validators,
            cost: None,
            transport,
        })
    }
}

fn parse_feed_candidates(
    feed_url: &str,
    body: &[u8],
    limit: usize,
) -> Result<Vec<ContentCandidate>> {
    let feed = feed_rs::parser::parse(body).context("parsing RSS, Atom, or JSON Feed")?;
    let source_label = feed
        .title
        .as_ref()
        .map(|title| title.content.trim())
        .filter(|title| !title.is_empty())
        .unwrap_or(feed_url)
        .to_string();
    let observed_at_ms = Utc::now().timestamp_millis();

    feed.entries
        .iter()
        .take(limit)
        .filter_map(|entry| {
            match candidate_from_entry(feed_url, &source_label, entry, observed_at_ms) {
                Ok(candidate) => Some(Ok(candidate)),
                Err(error) => {
                    tracing::debug!(%error, feed_url, entry_id = %entry.id, "skipping invalid feed entry");
                    None
                },
            }
        })
        .collect()
}

fn candidate_from_entry(
    feed_url: &str,
    source_label: &str,
    entry: &Entry,
    observed_at_ms: i64,
) -> Result<ContentCandidate> {
    let canonical_url = entry
        .links
        .iter()
        .find(|link| link.rel.as_deref().is_none_or(|rel| rel == "alternate"))
        .or_else(|| entry.links.first())
        .map(|link| canonicalize_http_url(&link.href))
        .transpose()?;
    let source_item_id = non_empty(&entry.id)
        .map(str::to_string)
        .or_else(|| canonical_url.clone())
        .ok_or_else(|| anyhow!("feed entry has neither id nor canonical URL"))?;
    let feed_identity = canonicalize_http_url(feed_url).unwrap_or_else(|_| feed_url.to_string());
    let item_id = format!(
        "{}/{}",
        blake3::hash(feed_identity.as_bytes()).to_hex(),
        blake3::hash(source_item_id.as_bytes()).to_hex()
    );
    let title = entry
        .title
        .as_ref()
        .and_then(|title| non_empty(&title.content))
        .unwrap_or("Untitled feed item");
    let title = bounded_chars(title, MAX_CANDIDATE_TITLE_CHARS);
    let cheap_text = entry
        .summary
        .as_ref()
        .and_then(|summary| non_empty(&summary.content))
        .map(str::to_string)
        .or_else(|| {
            entry
                .content
                .as_ref()
                .and_then(|content| content.body.as_deref())
                .and_then(non_empty)
                .map(str::to_string)
        })
        .unwrap_or_else(|| title.clone());
    let cheap_text = bounded_chars(&cheap_text, MAX_CANDIDATE_CHEAP_TEXT_CHARS);
    let published_at_ms = entry
        .published
        .as_ref()
        .or(entry.updated.as_ref())
        .map(|value| value.timestamp_millis());

    let mut metadata = BTreeMap::new();
    metadata.insert(
        "source_item_id".into(),
        json!(bounded_chars(&source_item_id, 2048)),
    );
    if !entry.categories.is_empty() {
        metadata.insert(
            "categories".into(),
            json!(entry
                .categories
                .iter()
                .map(|category| category.term.as_str())
                .collect::<Vec<_>>()),
        );
    }
    if !entry.authors.is_empty() {
        metadata.insert(
            "authors".into(),
            json!(entry
                .authors
                .iter()
                .map(|person| person.name.as_str())
                .collect::<Vec<_>>()),
        );
    }

    let hash_input = format!(
        "{title}\n{cheap_text}\n{}",
        canonical_url.as_deref().unwrap_or("")
    );
    let candidate = ContentCandidate {
        schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
        identity: SourceIdentity::new(RSS_ADAPTER_ID, item_id)?,
        title,
        cheap_text,
        canonical_url,
        published_at_ms,
        observed_at_ms,
        privacy: ContentPrivacy::Public,
        content_hash: Some(blake3::hash(hash_input.as_bytes()).to_hex().to_string()),
        provenance: ContentProvenance {
            source_label: source_label.to_string(),
            source_url: Some(feed_url.to_string()),
            retrieved_by: RSS_ADAPTER_ID.into(),
        },
        metadata,
    };
    candidate.validate()?;
    Ok(candidate)
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn bounded_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{collections::BTreeSet, sync::Mutex};

    use super::*;

    struct StaticTransport(Vec<u8>);

    #[async_trait]
    impl FeedTransport for StaticTransport {
        async fn fetch(
            &self,
            url: &str,
            _conditional: &ConditionalHttpRequest,
        ) -> Result<PublicHttpFetch> {
            Ok(PublicHttpFetch::Modified {
                final_url: url.to_string(),
                media_type: Some("application/rss+xml".into()),
                etag: Some("fixture-v1".into()),
                last_modified: None,
                body: self.0.clone(),
            })
        }
    }

    struct NotModifiedTransport {
        conditional: Mutex<Option<ConditionalHttpRequest>>,
    }

    #[async_trait]
    impl FeedTransport for NotModifiedTransport {
        async fn fetch(
            &self,
            url: &str,
            conditional: &ConditionalHttpRequest,
        ) -> Result<PublicHttpFetch> {
            *self
                .conditional
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(conditional.clone());
            Ok(PublicHttpFetch::NotModified {
                final_url: url.to_string(),
                etag: None,
                last_modified: None,
            })
        }
    }

    fn request(limit: usize) -> DiscoveryRequest {
        DiscoveryRequest {
            principal: "p".into(),
            workspace: "w".into(),
            intent: Some("AI research".into()),
            query: None,
            targets: vec!["https://example.com/feed.xml".into()],
            cursor: None,
            validators: BTreeMap::new(),
            limit,
            freshness: super::super::FreshnessPolicy::Fresh,
            remote_query_policy: super::super::RemoteDataPolicy::Deny,
            invocation_source: super::super::ContentInvocationSource::UserFeed,
            options: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn rss_adapter_normalizes_entries_without_sending_feed_intent() {
        let body = br#"<?xml version="1.0"?>
          <rss version="2.0"><channel><title>Example News</title>
          <item><guid>story-1</guid><title>Launch</title>
          <link>https://example.com/story?utm_source=rss&amp;id=1</link>
          <description>A useful launch summary.</description>
          <pubDate>Tue, 21 Jul 2026 10:00:00 GMT</pubDate></item>
          </channel></rss>"#;
        let adapter = RssDiscoveryAdapter::with_transport(Arc::new(StaticTransport(body.to_vec())));
        let page = adapter.discover(&request(10)).await.unwrap();

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].identity.adapter_id, "rss");
        assert!(page.items[0].identity.item_id.contains('/'));
        assert_eq!(page.items[0].metadata["source_item_id"], json!("story-1"));
        assert_eq!(
            page.items[0].canonical_url.as_deref(),
            Some("https://example.com/story?id=1")
        );
        assert_eq!(page.items[0].provenance.source_label, "Example News");
        assert!(!adapter.descriptor().capabilities.sends_user_intent);
        assert_eq!(page.transport.modified_targets, 1);
        assert_eq!(page.transport.not_modified_targets, 0);
        assert_eq!(page.transport.response_bytes, body.len() as u64);
    }

    #[tokio::test]
    async fn rss_adapter_honors_the_request_limit() {
        let body = br#"<?xml version="1.0"?>
          <feed xmlns="http://www.w3.org/2005/Atom"><title>Updates</title>
          <entry><id>one</id><title>One</title><summary>First</summary></entry>
          <entry><id>two</id><title>Two</title><summary>Second</summary></entry>
          </feed>"#;
        let adapter = RssDiscoveryAdapter::with_transport(Arc::new(StaticTransport(body.to_vec())));
        assert_eq!(adapter.discover(&request(1)).await.unwrap().items.len(), 1);
    }

    #[tokio::test]
    async fn json_feed_uses_the_same_candidate_contract() {
        let body = br#"{
          "version": "https://jsonfeed.org/version/1.1",
          "title": "JSON Updates",
          "items": [{
            "id": "json-one",
            "url": "https://example.com/json-one?utm_campaign=daily",
            "title": "JSON One",
            "summary": "A JSON Feed summary"
          }]
        }"#;
        let adapter = RssDiscoveryAdapter::with_transport(Arc::new(StaticTransport(body.to_vec())));
        let page = adapter.discover(&request(10)).await.unwrap();

        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "JSON One");
        assert_eq!(page.items[0].cheap_text, "A JSON Feed summary");
        assert_eq!(
            page.items[0].canonical_url.as_deref(),
            Some("https://example.com/json-one")
        );
    }

    #[tokio::test]
    async fn conditional_not_modified_retains_the_sent_validator_checkpoint() {
        let transport = Arc::new(NotModifiedTransport {
            conditional: Mutex::new(None),
        });
        let adapter = RssDiscoveryAdapter::with_transport(transport.clone());
        let mut request = request(10);
        request.validators.insert(
            request.targets[0].clone(),
            DiscoveryValidator {
                etag: Some("fixture-v1".into()),
                last_modified: Some("Wed, 22 Jul 2026 10:00:00 GMT".into()),
            },
        );

        let page = adapter.discover(&request).await.unwrap();

        assert!(page.items.is_empty());
        assert_eq!(
            transport
                .conditional
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .and_then(|validator| validator.etag.as_deref()),
            Some("fixture-v1")
        );
        assert_eq!(
            page.validators[&request.targets[0]].etag.as_deref(),
            Some("fixture-v1")
        );
        assert_eq!(page.transport.modified_targets, 0);
        assert_eq!(page.transport.not_modified_targets, 1);
        assert_eq!(page.transport.response_bytes, 0);
    }

    #[tokio::test]
    async fn malformed_feed_fails_without_returning_a_checkpoint() {
        let adapter = RssDiscoveryAdapter::with_transport(Arc::new(StaticTransport(
            b"this is not a feed".to_vec(),
        )));

        let error = adapter.discover(&request(10)).await.unwrap_err();

        assert!(error
            .to_string()
            .contains("parsing RSS, Atom, or JSON Feed"));
    }

    #[tokio::test]
    async fn large_feed_is_bounded_by_the_requested_candidate_limit() {
        let entries = (0..40)
            .map(|index| {
                format!(
                    "<item><guid>{index}</guid><title>Item {index}</title><link>https://example.com/{index}</link></item>"
                )
            })
            .collect::<String>();
        let body = format!(
            "<rss version=\"2.0\"><channel><title>Large fixture</title>{entries}</channel></rss>"
        );
        let adapter =
            RssDiscoveryAdapter::with_transport(Arc::new(StaticTransport(body.into_bytes())));

        let page = adapter.discover(&request(7)).await.unwrap();

        assert_eq!(page.items.len(), 7);
        assert_eq!(page.validators.len(), 1);
    }

    #[tokio::test]
    async fn multiple_feed_targets_are_round_robin_bounded_without_validator_starvation() {
        let adapter = RssDiscoveryAdapter::with_transport(Arc::new(StaticTransport(
            br#"<rss version="2.0"><channel><title>Fixture</title><item><guid>one</guid><title>One</title><link>https://example.com/one</link></item><item><guid>two</guid><title>Two</title><link>https://example.com/two</link></item></channel></rss>"#.to_vec(),
        )));
        let mut request = request(2);
        request.targets.push("https://example.org/feed.xml".into());

        let page = adapter.discover(&request).await.unwrap();

        assert_eq!(page.items.len(), 2);
        assert_eq!(page.validators.len(), 2);
        assert_eq!(
            page.items
                .iter()
                .filter_map(|item| item.provenance.source_url.as_deref())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "https://example.com/feed.xml",
                "https://example.org/feed.xml",
            ])
        );
    }

    #[test]
    fn rss_target_rejects_local_and_credentialed_urls() {
        assert!(validate_public_http_url("http://127.0.0.1/feed").is_err());
        assert!(validate_public_http_url("http://user:secret@example.com/feed").is_err());
        assert!(validate_public_http_url("https://example.com/feed").is_ok());
    }
}
