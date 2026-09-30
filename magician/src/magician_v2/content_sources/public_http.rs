use std::{
    collections::{BTreeSet, HashSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{
    HeaderMap, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH,
    LAST_MODIFIED, LOCATION,
};
use url::{Host, Url};

pub const MAX_PUBLIC_URL_CHARS: usize = 8 * 1024;
const MAX_CONFIGURED_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CONFIGURED_REDIRECTS: usize = 10;
const MAX_CONFIGURED_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConditionalHttpRequest {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicHttpFetch {
    Modified {
        final_url: String,
        media_type: Option<String>,
        etag: Option<String>,
        last_modified: Option<String>,
        body: Vec<u8>,
    },
    NotModified {
        final_url: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct PublicHttpFetchPolicy {
    pub max_response_bytes: usize,
    pub max_redirects: usize,
    pub timeout: Duration,
    pub user_agent: String,
    /// Empty means that the caller validates the decoded payload itself.
    pub accepted_media_types: BTreeSet<String>,
    pub allow_missing_media_type: bool,
}

impl PublicHttpFetchPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.max_response_bytes == 0 || self.max_response_bytes > MAX_CONFIGURED_RESPONSE_BYTES {
            bail!(
                "public HTTP response limit must be between 1 and {MAX_CONFIGURED_RESPONSE_BYTES} bytes"
            );
        }
        if self.max_redirects > MAX_CONFIGURED_REDIRECTS {
            bail!("public HTTP redirect limit exceeds {MAX_CONFIGURED_REDIRECTS}");
        }
        if self.timeout < Duration::from_secs(1)
            || self.timeout > Duration::from_secs(MAX_CONFIGURED_TIMEOUT_SECS)
        {
            bail!(
                "public HTTP timeout must be between 1 and {MAX_CONFIGURED_TIMEOUT_SECS} seconds"
            );
        }
        if self.user_agent.trim().is_empty() {
            bail!("public HTTP user agent must not be empty");
        }
        for media_type in &self.accepted_media_types {
            if media_type.trim().is_empty() || media_type != &media_type.to_ascii_lowercase() {
                bail!("accepted media types must be non-empty lowercase values");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct HttpHopResponse {
    pub(super) status: u16,
    pub(super) headers: HeaderMap,
    pub(super) body: Vec<u8>,
}

#[async_trait]
pub(super) trait PublicAddressResolver: Send + Sync {
    async fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>>;
}

struct TokioPublicAddressResolver;

#[async_trait]
impl PublicAddressResolver for TokioPublicAddressResolver {
    async fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("public HTTP URL requires a host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| anyhow!("public HTTP URL requires a known port"))?;
        let mut addresses = tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("resolving public HTTP host `{host}`"))?
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        validate_resolved_addresses(host, &addresses)?;
        Ok(addresses)
    }
}

#[async_trait]
pub(super) trait HttpHopTransport: Send + Sync {
    async fn send(
        &self,
        url: &Url,
        pinned_addresses: &[SocketAddr],
        conditional: &ConditionalHttpRequest,
        policy: &PublicHttpFetchPolicy,
    ) -> Result<HttpHopResponse>;
}

struct ReqwestHttpHopTransport;

#[async_trait]
impl HttpHopTransport for ReqwestHttpHopTransport {
    async fn send(
        &self,
        url: &Url,
        pinned_addresses: &[SocketAddr],
        conditional: &ConditionalHttpRequest,
        policy: &PublicHttpFetchPolicy,
    ) -> Result<HttpHopResponse> {
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("public HTTP URL requires a host"))?;
        let client = reqwest::Client::builder()
            // Redirects are processed by `PublicHttpFetcher`, so every hop is
            // syntax checked, DNS checked, and pinned independently.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(policy.timeout)
            .user_agent(&policy.user_agent)
            // A process-level HTTP proxy can resolve the host independently
            // and would defeat the DNS pinning above.
            .no_proxy()
            .resolve_to_addrs(host, pinned_addresses)
            .build()
            .context("building pinned public HTTP client")?;

        let mut request = client.get(url.clone());
        if let Some(etag) = conditional.etag.as_deref() {
            request = request.header(
                IF_NONE_MATCH,
                HeaderValue::from_str(etag).context("cached ETag is not a valid HTTP header")?,
            );
        }
        if let Some(last_modified) = conditional.last_modified.as_deref() {
            request = request.header(
                IF_MODIFIED_SINCE,
                HeaderValue::from_str(last_modified)
                    .context("cached Last-Modified value is not a valid HTTP header")?,
            );
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("fetching public URL `{url}`"))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if status == 304 || response.status().is_redirection() {
            return Ok(HttpHopResponse {
                status,
                headers,
                body: Vec::new(),
            });
        }
        if let Some(length) = headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
        {
            if length > policy.max_response_bytes as u64 {
                bail!(
                    "public HTTP response exceeds {} byte limit",
                    policy.max_response_bytes
                );
            }
        }

        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading public HTTP response body")?;
            if body.len().saturating_add(chunk.len()) > policy.max_response_bytes {
                bail!(
                    "public HTTP response exceeds {} byte limit",
                    policy.max_response_bytes
                );
            }
            body.extend_from_slice(&chunk);
        }
        Ok(HttpHopResponse {
            status,
            headers,
            body,
        })
    }
}

#[derive(Clone)]
pub struct PublicHttpFetcher {
    policy: PublicHttpFetchPolicy,
    resolver: Arc<dyn PublicAddressResolver>,
    transport: Arc<dyn HttpHopTransport>,
}

impl PublicHttpFetcher {
    pub fn new(policy: PublicHttpFetchPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            resolver: Arc::new(TokioPublicAddressResolver),
            transport: Arc::new(ReqwestHttpHopTransport),
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) fn with_components(
        policy: PublicHttpFetchPolicy,
        resolver: Arc<dyn PublicAddressResolver>,
        transport: Arc<dyn HttpHopTransport>,
    ) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            policy,
            resolver,
            transport,
        })
    }

    pub async fn fetch(
        &self,
        raw_url: &str,
        conditional: &ConditionalHttpRequest,
    ) -> Result<PublicHttpFetch> {
        tokio::time::timeout(self.policy.timeout, self.fetch_inner(raw_url, conditional))
            .await
            .map_err(|_| {
                anyhow!(
                    "public HTTP request exceeded {} second deadline",
                    self.policy.timeout.as_secs()
                )
            })?
    }

    async fn fetch_inner(
        &self,
        raw_url: &str,
        conditional: &ConditionalHttpRequest,
    ) -> Result<PublicHttpFetch> {
        let mut current = validate_public_http_url(raw_url)?;
        let mut visited = HashSet::new();
        let mut redirects = 0usize;

        loop {
            let visit_key = current.to_string();
            if !visited.insert(visit_key.clone()) {
                bail!("public HTTP redirect loop detected at `{visit_key}`");
            }
            let addresses = self.resolver.resolve(&current).await?;
            let host = current
                .host_str()
                .ok_or_else(|| anyhow!("public HTTP URL requires a host"))?;
            validate_resolved_addresses(host, &addresses)?;
            // Validators apply only to the original representation. A redirect
            // response must not receive validators belonging to its target.
            let hop_conditional = if redirects == 0 {
                conditional.clone()
            } else {
                ConditionalHttpRequest::default()
            };
            let response = self
                .transport
                .send(&current, &addresses, &hop_conditional, &self.policy)
                .await?;

            if (300..400).contains(&response.status) && response.status != 304 {
                let location = header_string(&response.headers, LOCATION)
                    .ok_or_else(|| anyhow!("redirect from `{current}` omitted Location"))?;
                let next = current
                    .join(&location)
                    .with_context(|| format!("resolving redirect `{location}`"))?;
                validate_public_http_url(next.as_str())?;
                if current.scheme() == "https" && next.scheme() != "https" {
                    bail!("public HTTP redirect must not downgrade HTTPS to HTTP");
                }
                redirects += 1;
                if redirects > self.policy.max_redirects {
                    bail!(
                        "public HTTP request exceeded {} redirects",
                        self.policy.max_redirects
                    );
                }
                current = next;
                continue;
            }

            let etag = header_string(&response.headers, ETAG);
            let last_modified = header_string(&response.headers, LAST_MODIFIED);
            if response.status == 304 {
                return Ok(PublicHttpFetch::NotModified {
                    final_url: current.to_string(),
                    etag,
                    last_modified,
                });
            }
            if !(200..300).contains(&response.status) {
                bail!(
                    "public HTTP request to `{current}` returned status {}",
                    response.status
                );
            }
            if response.body.len() > self.policy.max_response_bytes {
                bail!(
                    "public HTTP response exceeds {} byte limit",
                    self.policy.max_response_bytes
                );
            }
            let media_type = normalized_media_type(&response.headers);
            self.validate_media_type(media_type.as_deref())?;
            return Ok(PublicHttpFetch::Modified {
                final_url: current.to_string(),
                media_type,
                etag,
                last_modified,
                body: response.body,
            });
        }
    }

    fn validate_media_type(&self, media_type: Option<&str>) -> Result<()> {
        if self.policy.accepted_media_types.is_empty() {
            return Ok(());
        }
        let Some(media_type) = media_type else {
            if self.policy.allow_missing_media_type {
                return Ok(());
            }
            bail!("public HTTP response omitted Content-Type");
        };
        if !self.policy.accepted_media_types.contains(media_type) {
            bail!("unsupported public HTTP media type `{media_type}`");
        }
        Ok(())
    }
}

pub fn validate_public_http_url(raw: &str) -> Result<Url> {
    if raw.chars().count() > MAX_PUBLIC_URL_CHARS {
        bail!("public HTTP URL exceeds {MAX_PUBLIC_URL_CHARS} characters");
    }
    let url = Url::parse(raw).with_context(|| format!("parsing public HTTP URL `{raw}`"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("public HTTP URL must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("public HTTP URL must not contain embedded credentials");
    }
    match url.host() {
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            if host.is_empty()
                || host == "localhost"
                || host.ends_with(".localhost")
                || host.ends_with(".local")
            {
                bail!("public HTTP URL must not target a local host");
            }
        },
        Some(Host::Ipv4(ip)) if ipv4_is_non_global(ip) => {
            bail!("public HTTP URL must not target non-global IPv4 address `{ip}`");
        },
        Some(Host::Ipv6(ip)) if ipv6_is_non_global(ip) => {
            bail!("public HTTP URL must not target non-global IPv6 address `{ip}`");
        },
        Some(_) => {},
        None => bail!("public HTTP URL requires a host"),
    }
    Ok(url)
}

/// Resolve a public URL before handing it to a browser transport. Browser
/// engines perform their own DNS resolution, so this gate mirrors the static
/// reader's private-address rejection and prevents obvious SSRF targets from
/// entering the rendered-read rung.
pub async fn validate_public_browser_url(raw: &str) -> Result<Url> {
    resolve_public_browser_url(raw).await.map(|(url, _)| url)
}

pub async fn resolve_public_browser_url(raw: &str) -> Result<(Url, Vec<SocketAddr>)> {
    let url = validate_public_http_url(raw)?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("public browser URL requires a host"))?;
    let addresses = TokioPublicAddressResolver.resolve(&url).await?;
    validate_resolved_addresses(host, &addresses)?;
    Ok((url, addresses))
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalized_media_type(headers: &HeaderMap) -> Option<String> {
    header_string(headers, CONTENT_TYPE)
        .and_then(|value| value.split(';').next().map(str::trim).map(str::to_string))
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
}

fn ip_is_non_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ipv4_is_non_global(ip),
        IpAddr::V6(ip) => ipv6_is_non_global(ip),
    }
}

pub(crate) fn validate_public_socket_addresses(host: &str, addresses: &[SocketAddr]) -> Result<()> {
    validate_resolved_addresses(host, addresses)
}

fn validate_resolved_addresses(host: &str, addresses: &[SocketAddr]) -> Result<()> {
    if addresses.is_empty() {
        bail!("public HTTP host `{host}` did not resolve");
    }
    if let Some(address) = addresses
        .iter()
        .find(|address| ip_is_non_global(address.ip()))
    {
        bail!(
            "public HTTP host `{host}` resolved to disallowed address `{}`",
            address.ip()
        );
    }
    Ok(())
}

fn ipv4_is_non_global(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && matches!(b, 18 | 19))
        || a >= 240
}

fn ipv6_is_non_global(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return ipv4_is_non_global(mapped);
    }
    let global_unicast = (segments[0] & 0xe000) == 0x2000;
    !global_unicast
        // IETF protocol assignments include Teredo, benchmarking, ORCHID,
        // and other special-purpose ranges that must not cross this boundary.
        || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;

    struct StaticResolver {
        addresses: Vec<SocketAddr>,
    }

    #[async_trait]
    impl PublicAddressResolver for StaticResolver {
        async fn resolve(&self, _url: &Url) -> Result<Vec<SocketAddr>> {
            Ok(self.addresses.clone())
        }
    }

    struct QueueTransport {
        responses: Mutex<VecDeque<HttpHopResponse>>,
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl HttpHopTransport for QueueTransport {
        async fn send(
            &self,
            url: &Url,
            _pinned_addresses: &[SocketAddr],
            _conditional: &ConditionalHttpRequest,
            _policy: &PublicHttpFetchPolicy,
        ) -> Result<HttpHopResponse> {
            self.seen.lock().unwrap().push(url.to_string());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("no queued response"))
        }
    }

    fn policy() -> PublicHttpFetchPolicy {
        PublicHttpFetchPolicy {
            max_response_bytes: 1024,
            max_redirects: 2,
            timeout: Duration::from_secs(5),
            user_agent: "test".into(),
            accepted_media_types: BTreeSet::from(["text/html".into()]),
            allow_missing_media_type: false,
        }
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> HttpHopResponse {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        HttpHopResponse {
            status,
            headers: map,
            body: body.to_vec(),
        }
    }

    #[test]
    fn syntax_boundary_rejects_credentials_and_non_public_literals() {
        for url in [
            "file:///etc/passwd",
            "http://user:secret@example.com",
            "http://localhost/admin",
            "http://127.0.0.1/admin",
            "http://169.254.169.254/latest/meta-data",
            "http://192.88.99.1/relay",
            "http://[::1]/admin",
            "http://[::2]/reserved",
            "http://[100::1]/discard",
            "http://[fec0::1]/site-local",
            "http://[::ffff:127.0.0.1]/admin",
        ] {
            assert!(validate_public_http_url(url).is_err(), "accepted {url}");
        }
        assert!(validate_public_http_url("https://example.com/article").is_ok());
        assert!(validate_public_http_url("https://[2606:4700:4700::1111]/").is_ok());
    }

    #[test]
    fn dns_boundary_rejects_empty_private_and_mixed_answers() {
        assert!(validate_resolved_addresses("empty.example", &[]).is_err());
        assert!(
            validate_resolved_addresses("private.example", &["10.0.0.1:443".parse().unwrap()])
                .is_err()
        );
        assert!(validate_resolved_addresses(
            "mixed.example",
            &[
                "93.184.216.34:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
            ]
        )
        .is_err());
        assert!(validate_resolved_addresses(
            "public.example",
            &["93.184.216.34:443".parse().unwrap()]
        )
        .is_ok());
    }

    #[test]
    fn policy_rejects_invalid_deadlines_and_response_bounds() {
        let mut invalid = policy();
        invalid.timeout = Duration::from_millis(999);
        assert!(invalid.validate().is_err());
        invalid.timeout = Duration::from_secs(MAX_CONFIGURED_TIMEOUT_SECS + 1);
        assert!(invalid.validate().is_err());

        let mut response_limit = policy();
        response_limit.max_response_bytes = MAX_CONFIGURED_RESPONSE_BYTES;
        assert!(response_limit.validate().is_ok());
        response_limit.max_response_bytes = MAX_CONFIGURED_RESPONSE_BYTES + 1;
        assert!(response_limit.validate().is_err());
    }

    #[tokio::test]
    async fn validates_and_pins_each_redirect_hop() {
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                response(302, &[("location", "https://news.example/story")], b""),
                response(
                    200,
                    &[("content-type", "text/html; charset=utf-8")],
                    b"<article>story</article>",
                ),
            ])),
            seen: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            policy(),
            Arc::new(StaticResolver {
                addresses: vec!["93.184.216.34:443".parse().unwrap()],
            }),
            transport.clone(),
        )
        .unwrap();
        let result = fetcher
            .fetch(
                "https://example.com/start",
                &ConditionalHttpRequest::default(),
            )
            .await
            .unwrap();
        assert!(matches!(result, PublicHttpFetch::Modified { .. }));
        assert_eq!(transport.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn fetcher_enforces_public_dns_answers_from_any_resolver() {
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([response(
                200,
                &[("content-type", "text/html")],
                b"<article>story</article>",
            )])),
            seen: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            policy(),
            Arc::new(StaticResolver {
                addresses: vec!["127.0.0.1:443".parse().unwrap()],
            }),
            transport.clone(),
        )
        .unwrap();

        assert!(fetcher
            .fetch(
                "https://example.com/story",
                &ConditionalHttpRequest::default()
            )
            .await
            .is_err());
        assert!(transport.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejects_oversized_responses_even_with_fake_transport() {
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([response(
                200,
                &[("content-type", "application/octet-stream")],
                &[0; 2048],
            )])),
            seen: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            policy(),
            Arc::new(StaticResolver {
                addresses: vec!["93.184.216.34:443".parse().unwrap()],
            }),
            transport,
        )
        .unwrap();
        assert!(fetcher
            .fetch(
                "https://example.com/file",
                &ConditionalHttpRequest::default()
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rejects_unsupported_content_type() {
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([response(
                200,
                &[("content-type", "application/octet-stream")],
                b"small binary payload",
            )])),
            seen: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            policy(),
            Arc::new(StaticResolver {
                addresses: vec!["93.184.216.34:443".parse().unwrap()],
            }),
            transport,
        )
        .unwrap();
        assert!(fetcher
            .fetch(
                "https://example.com/file",
                &ConditionalHttpRequest::default()
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rejects_private_redirect_targets_and_https_downgrades() {
        for location in ["http://127.0.0.1/admin", "http://public.example/insecure"] {
            let transport = Arc::new(QueueTransport {
                responses: Mutex::new(VecDeque::from([response(
                    302,
                    &[("location", location)],
                    b"",
                )])),
                seen: Mutex::new(Vec::new()),
            });
            let fetcher = PublicHttpFetcher::with_components(
                policy(),
                Arc::new(StaticResolver {
                    addresses: vec!["93.184.216.34:443".parse().unwrap()],
                }),
                transport,
            )
            .unwrap();
            assert!(fetcher
                .fetch(
                    "https://example.com/start",
                    &ConditionalHttpRequest::default()
                )
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn preserves_conditional_not_modified_response() {
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([response(304, &[("etag", "\"v2\"")], b"")])),
            seen: Mutex::new(Vec::new()),
        });
        let fetcher = PublicHttpFetcher::with_components(
            policy(),
            Arc::new(StaticResolver {
                addresses: vec!["93.184.216.34:443".parse().unwrap()],
            }),
            transport,
        )
        .unwrap();
        let result = fetcher
            .fetch(
                "https://example.com/story",
                &ConditionalHttpRequest {
                    etag: Some("\"v1\"".into()),
                    last_modified: None,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            result,
            PublicHttpFetch::NotModified { etag: Some(value), .. } if value == "\"v2\""
        ));
    }
}
