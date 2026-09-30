//! Physical owner for the first app-qualified bound-HTTP vertical.
//!
//! The ordinary compiled HTTP provider is intentionally not enough for Apps:
//! validating a hostname and then letting reqwest resolve it again leaves a
//! DNS-rebinding window, and process proxy configuration can resolve a
//! different destination altogether. This owner resolves once during
//! preparation, retains the reviewed socket addresses in a move-only value,
//! and hands those exact addresses to reqwest while preserving the original
//! URL for HTTP Host and TLS SNI. Redirects are processed one hop at a time;
//! every hop stays on the admitted origin, is resolved/revalidated, and is
//! pinned for that physical connection.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::Duration as StdDuration,
};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, CONTENT_LENGTH, LOCATION};
use serde::Serialize;
use url::{Host, Url};

use super::{
    models::{AppDigest, AppReference, AppRevision},
    policy::{AppEndpointClass, AttestedAppEndpoint},
    tool_disclosure::AttestedAppToolTarget,
};
use crate::magician_v2::{
    content_sources::{
        resolve_public_browser_url, validate_public_http_url, validate_public_socket_addresses,
    },
    execution::{
        actions::{ActionResult, HttpAction, HttpMethod},
        ExecutionError,
    },
};

pub(crate) const APP_BOUND_HTTP_INPUT_CEILING: u64 = 64 * 1024;
pub(crate) const APP_BOUND_HTTP_RESULT_CEILING: u64 = 4 * 1024 * 1024;
const APP_BOUND_HTTP_PIN_LIFETIME_SECONDS: i64 = 5 * 60;
const APP_BOUND_HTTP_MAX_REDIRECTS: usize = 5;
const APP_BOUND_HTTP_MAX_ADDRESSES_PER_HOP: usize = 32;

/// Exact connection identity retained from preparation through physical I/O.
/// It intentionally implements neither Clone, Debug nor Serde.
pub(crate) struct PreparedAppBoundHttp {
    initial_url: Url,
    initial_addresses: Vec<SocketAddr>,
    request_digest: AppDigest,
    prepared_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl PreparedAppBoundHttp {
    pub(crate) async fn prepare(action: &HttpAction, now: DateTime<Utc>) -> Result<Self> {
        validate_read_only_action(action)?;
        let resolver: Arc<dyn BoundHttpResolver> = Arc::new(SystemBoundHttpResolver);
        Self::prepare_with_resolver(action, now, resolver.as_ref()).await
    }

    async fn prepare_with_resolver(
        action: &HttpAction,
        now: DateTime<Utc>,
        resolver: &dyn BoundHttpResolver,
    ) -> Result<Self> {
        validate_read_only_action(action)?;
        let initial_url = validate_public_http_url(&action.url)?;
        let initial_addresses = resolver.resolve(&initial_url).await?;
        validate_pin(&initial_url, &initial_addresses)?;
        let request_digest = digest_action(action)?;
        let expires_at = now
            .checked_add_signed(Duration::seconds(APP_BOUND_HTTP_PIN_LIFETIME_SECONDS))
            .ok_or_else(|| anyhow!("bound-HTTP pin expiry overflow"))?;
        Ok(Self {
            initial_url,
            initial_addresses,
            request_digest,
            prepared_at: now,
            expires_at,
        })
    }

    pub(crate) fn matches_action(&self, action: &HttpAction) -> bool {
        digest_action(action).ok().as_ref() == Some(&self.request_digest)
    }

    pub(crate) fn attest(
        &self,
        tool_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Option<AttestedAppToolTarget> {
        if now < self.prepared_at || now >= self.expires_at {
            return None;
        }
        let host = normalized_host(&self.initial_url)?;
        let host_header = host_header_identity(&self.initial_url)?;
        let destination = AppReference::parse(format!("destination:{host}")).ok()?;
        let configuration_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
            "profile": "magician.app-bound-http.get.v1",
            "request_digest": &self.request_digest,
            "url": self.initial_url.as_str(),
            "addresses": address_strings(&self.initial_addresses),
            "host_header": &host_header,
            "tls_sni": &host,
            "proxy": "disabled",
            "redirect_policy": "same_origin_revalidate_and_pin_each_hop",
            "max_redirects": APP_BOUND_HTTP_MAX_REDIRECTS,
            "result_byte_ceiling": APP_BOUND_HTTP_RESULT_CEILING,
        }))
        .ok()?;
        let endpoint = AttestedAppEndpoint::from_trusted_resolver(
            AppReference::parse("endpoint:app-bound-http-get-v1").ok()?,
            AppEndpointClass::External,
            false,
            AppRevision::new(1).ok()?,
            configuration_digest,
            self.prepared_at,
            self.expires_at,
        )
        .ok()?;
        Some(AttestedAppToolTarget::from_trusted_external_dispatcher(
            tool_ref,
            endpoint,
            destination,
        ))
    }

    pub(crate) async fn execute(self, action: &HttpAction) -> Result<ActionResult, ExecutionError> {
        if !self.matches_action(action) {
            return Err(ExecutionError::Configuration(
                "protected app bound-HTTP action changed after DNS pinning".to_owned(),
            ));
        }
        if Utc::now() >= self.expires_at {
            return Err(ExecutionError::Configuration(
                "protected app bound-HTTP DNS pin expired before I/O".to_owned(),
            ));
        }
        let resolver: Arc<dyn BoundHttpResolver> = Arc::new(SystemBoundHttpResolver);
        let transport: Arc<dyn BoundHttpTransport> = Arc::new(ReqwestBoundHttpTransport);
        execute_with_components(
            action,
            self.initial_url,
            self.initial_addresses,
            resolver.as_ref(),
            transport.as_ref(),
        )
        .await
        .map_err(|error| ExecutionError::Step(format!("bound-HTTP request failed: {error}")))
    }
}

fn validate_read_only_action(action: &HttpAction) -> Result<()> {
    if action.method != HttpMethod::Get {
        bail!("the first app bound-HTTP owner admits GET only");
    }
    if action.body.as_deref().is_some_and(|body| !body.is_empty()) || action.content_type.is_some()
    {
        bail!("app bound-HTTP GET cannot carry a request body or content type");
    }
    let timeout = action.timeout_secs.unwrap_or(30);
    if !(1..=120).contains(&timeout) {
        bail!("app bound-HTTP timeout must be between 1 and 120 seconds");
    }
    const REFUSED_HEADERS: &[&str] = &[
        "host",
        "proxy-authorization",
        "proxy-connection",
        "transfer-encoding",
        "content-length",
        "x-http-method",
        "x-http-method-override",
        "x-method-override",
    ];
    if let Some(header) = action
        .headers
        .keys()
        .find(|name| REFUSED_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str()))
    {
        bail!("app bound-HTTP header `{header}` can change request identity");
    }
    let url = validate_public_http_url(&action.url)?;
    if !matches!(url.host(), Some(Host::Domain(_))) {
        // This first vertical proves DNS-name -> connect-IP -> HTTP Host/TLS
        // SNI continuity. Literal-IP authorities have different Host and TLS
        // identity semantics and remain outside the admitted shape.
        bail!("app bound-HTTP requires a DNS host");
    }
    if url.host_str().is_some_and(|host| host.ends_with('.')) {
        // reqwest's DNS override is keyed by the URL host. Reject the alternate
        // trailing-dot spelling so the normalized pin key and the transport's
        // lookup key can never diverge and trigger an ambient DNS lookup.
        bail!("app bound-HTTP host must use its canonical non-trailing-dot spelling");
    }
    Ok(())
}

fn digest_action(action: &HttpAction) -> Result<AppDigest> {
    #[derive(Serialize)]
    struct RequestIdentity<'a> {
        profile: &'static str,
        method: &'static str,
        url: &'a str,
        headers: Vec<(&'a str, &'a str)>,
        timeout_secs: u64,
        follow_redirects: bool,
    }
    validate_read_only_action(action)?;
    let mut headers = action
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    headers.sort_unstable();
    let identity = serde_json::to_value(RequestIdentity {
        profile: "magician.app-bound-http-request.v1",
        method: "GET",
        url: &action.url,
        headers,
        timeout_secs: action.timeout_secs.unwrap_or(30),
        follow_redirects: action.follow_redirects,
    })?;
    Ok(AppDigest::blake3_canonical_json(&identity)?)
}

#[async_trait]
trait BoundHttpResolver: Send + Sync {
    async fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>>;
}

struct SystemBoundHttpResolver;

#[async_trait]
impl BoundHttpResolver for SystemBoundHttpResolver {
    async fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>> {
        resolve_public_browser_url(url.as_str())
            .await
            .map(|(_, addresses)| addresses)
    }
}

struct BoundHttpHop {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[async_trait]
trait BoundHttpTransport: Send + Sync {
    async fn send(
        &self,
        action: &HttpAction,
        url: &Url,
        addresses: &[SocketAddr],
    ) -> Result<BoundHttpHop>;
}

struct ReqwestBoundHttpTransport;

#[async_trait]
impl BoundHttpTransport for ReqwestBoundHttpTransport {
    async fn send(
        &self,
        action: &HttpAction,
        url: &Url,
        addresses: &[SocketAddr],
    ) -> Result<BoundHttpHop> {
        let host = normalized_host(url).ok_or_else(|| anyhow!("bound-HTTP URL requires a host"))?;
        validate_public_socket_addresses(&host, addresses)?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(StdDuration::from_secs(action.timeout_secs.unwrap_or(30)));
        // Domain URLs keep their original host in the request URI. reqwest
        // therefore emits that Host value and supplies it as rustls SNI while
        // connecting only to the retained socket addresses. A public literal
        // IP already names its exact connect target and needs no DNS override.
        if matches!(url.host(), Some(Host::Domain(_))) {
            builder = builder.resolve_to_addrs(&host, addresses);
        }
        let client = builder
            .build()
            .context("building pinned bound-HTTP client")?;
        let mut request = client.get(url.clone());
        for (name, value) in &action.headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("sending pinned GET `{url}`"))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if let Some(length) = headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
        {
            if length > APP_BOUND_HTTP_RESULT_CEILING {
                bail!("bound-HTTP response exceeds reviewed byte ceiling");
            }
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading bound-HTTP response")?;
            if body.len().saturating_add(chunk.len()) > APP_BOUND_HTTP_RESULT_CEILING as usize {
                bail!("bound-HTTP response exceeds reviewed byte ceiling");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(BoundHttpHop {
            status,
            headers,
            body,
        })
    }
}

async fn execute_with_components(
    action: &HttpAction,
    initial_url: Url,
    initial_addresses: Vec<SocketAddr>,
    resolver: &dyn BoundHttpResolver,
    transport: &dyn BoundHttpTransport,
) -> Result<ActionResult> {
    validate_read_only_action(action)?;
    let admitted_origin = origin(&initial_url)?;
    let mut current = initial_url;
    let mut addresses = initial_addresses;
    let mut visited = HashSet::new();
    let mut redirect_count = 0usize;
    loop {
        let visit = current.to_string();
        if !visited.insert(visit.clone()) {
            bail!("bound-HTTP redirect loop at `{visit}`");
        }
        validate_pin(&current, &addresses)?;
        let response = transport.send(action, &current, &addresses).await?;
        if (300..400).contains(&response.status)
            && response.status != 304
            && action.follow_redirects
        {
            let location = response
                .headers
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("bound-HTTP redirect omitted Location"))?;
            let next = current
                .join(location)
                .with_context(|| format!("resolving bound-HTTP redirect `{location}`"))?;
            validate_public_http_url(next.as_str())?;
            if origin(&next)? != admitted_origin {
                bail!("bound-HTTP redirect escaped the admitted origin");
            }
            redirect_count += 1;
            if redirect_count > APP_BOUND_HTTP_MAX_REDIRECTS {
                bail!("bound-HTTP request exceeded redirect ceiling");
            }
            addresses = resolver.resolve(&next).await?;
            validate_pin(&next, &addresses)?;
            current = next;
            continue;
        }
        let headers = bounded_headers(&response.headers, response.body.len())?;
        let body = String::from_utf8_lossy(&response.body).into_owned();
        return Ok(ActionResult::Http {
            status: response.status,
            headers,
            body,
        });
    }
}

fn validate_pin(url: &Url, addresses: &[SocketAddr]) -> Result<()> {
    if url.host_str().is_some_and(|host| host.ends_with('.')) {
        bail!("bound-HTTP host must use its canonical non-trailing-dot spelling");
    }
    if !matches!(url.host(), Some(Host::Domain(_))) {
        bail!("bound-HTTP pin requires a DNS host");
    }
    let host = normalized_host(url).ok_or_else(|| anyhow!("bound-HTTP URL requires a host"))?;
    let expected_port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("bound-HTTP URL requires a known port"))?;
    if addresses.len() > APP_BOUND_HTTP_MAX_ADDRESSES_PER_HOP {
        bail!("bound-HTTP resolver returned too many connection addresses");
    }
    if addresses
        .iter()
        .any(|address| address.port() != expected_port)
    {
        bail!("bound-HTTP resolver returned an address for a different port");
    }
    validate_public_socket_addresses(&host, addresses)
}

fn bounded_headers(headers: &HeaderMap, body_bytes: usize) -> Result<HashMap<String, String>> {
    let mut total = body_bytes;
    let mut out = HashMap::new();
    for (name, value) in headers {
        let Ok(value) = value.to_str() else {
            continue;
        };
        total = total
            .checked_add(name.as_str().len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or_else(|| anyhow!("bound-HTTP result size overflow"))?;
        if total > APP_BOUND_HTTP_RESULT_CEILING as usize {
            bail!("bound-HTTP result exceeds reviewed byte ceiling");
        }
        out.insert(name.as_str().to_owned(), value.to_owned());
    }
    Ok(out)
}

fn normalized_host(url: &Url) -> Option<String> {
    url.host_str()
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
}

fn host_header_identity(url: &Url) -> Option<String> {
    let host = normalized_host(url)?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn origin(url: &Url) -> Result<(String, String, u16)> {
    Ok((
        url.scheme().to_owned(),
        normalized_host(url).ok_or_else(|| anyhow!("bound-HTTP URL requires a host"))?,
        url.port_or_known_default()
            .ok_or_else(|| anyhow!("bound-HTTP URL requires a known port"))?,
    ))
}

fn address_strings(addresses: &[SocketAddr]) -> Vec<String> {
    let mut addresses = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    addresses
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        net::{IpAddr, Ipv4Addr},
        sync::Mutex,
    };

    use super::*;

    struct QueueResolver {
        answers: Mutex<VecDeque<Vec<SocketAddr>>>,
    }

    #[async_trait]
    impl BoundHttpResolver for QueueResolver {
        async fn resolve(&self, _url: &Url) -> Result<Vec<SocketAddr>> {
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("resolver answer exhausted"))
        }
    }

    struct QueueTransport {
        responses: Mutex<VecDeque<BoundHttpHop>>,
        seen: Mutex<Vec<(String, Vec<SocketAddr>)>>,
    }

    #[async_trait]
    impl BoundHttpTransport for QueueTransport {
        async fn send(
            &self,
            _action: &HttpAction,
            url: &Url,
            addresses: &[SocketAddr],
        ) -> Result<BoundHttpHop> {
            self.seen
                .lock()
                .unwrap()
                .push((url.to_string(), addresses.to_vec()));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| anyhow!("transport response exhausted"))
        }
    }

    fn public(port: u16, suffix: u8) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, suffix)), port)
    }

    fn action(url: &str) -> HttpAction {
        HttpAction {
            method: HttpMethod::Get,
            url: url.to_owned(),
            headers: HashMap::new(),
            body: None,
            content_type: None,
            timeout_secs: Some(10),
            follow_redirects: true,
            carries_credential: false,
        }
    }

    fn hop(status: u16, location: Option<&str>, body: &[u8]) -> BoundHttpHop {
        let mut headers = HeaderMap::new();
        if let Some(location) = location {
            headers.insert(LOCATION, location.parse().unwrap());
        }
        BoundHttpHop {
            status,
            headers,
            body: body.to_vec(),
        }
    }

    #[tokio::test]
    async fn exact_prepared_pin_is_held_through_first_io() {
        let resolver = QueueResolver {
            answers: Mutex::new(VecDeque::from([vec![public(443, 34)]])),
        };
        let request = action("https://example.com/start");
        let prepared = PreparedAppBoundHttp::prepare_with_resolver(&request, Utc::now(), &resolver)
            .await
            .unwrap();
        let transport = QueueTransport {
            responses: Mutex::new(VecDeque::from([hop(200, None, b"ok")])),
            seen: Mutex::new(Vec::new()),
        };
        execute_with_components(
            &request,
            prepared.initial_url,
            prepared.initial_addresses,
            &resolver,
            &transport,
        )
        .await
        .unwrap();
        assert_eq!(transport.seen.lock().unwrap()[0].1, vec![public(443, 34)]);
        assert!(resolver.answers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dns_rebind_to_private_address_is_denied_on_redirect() {
        let request = action("https://example.com/start");
        let resolver = QueueResolver {
            answers: Mutex::new(VecDeque::from([vec![SocketAddr::from((
                [127, 0, 0, 1],
                443,
            ))]])),
        };
        let transport = QueueTransport {
            responses: Mutex::new(VecDeque::from([hop(302, Some("/next"), b"")])),
            seen: Mutex::new(Vec::new()),
        };
        let error = execute_with_components(
            &request,
            Url::parse(&request.url).unwrap(),
            vec![public(443, 34)],
            &resolver,
            &transport,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("disallowed address"));
        assert_eq!(transport.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cross_origin_redirect_is_denied_before_second_resolution() {
        let request = action("https://example.com/start");
        let resolver = QueueResolver {
            answers: Mutex::new(VecDeque::new()),
        };
        let transport = QueueTransport {
            responses: Mutex::new(VecDeque::from([hop(
                302,
                Some("https://attacker.example/next"),
                b"",
            )])),
            seen: Mutex::new(Vec::new()),
        };
        let error = execute_with_components(
            &request,
            Url::parse(&request.url).unwrap(),
            vec![public(443, 34)],
            &resolver,
            &transport,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("escaped the admitted origin"));
        assert!(resolver.answers.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn action_substitution_after_pin_is_denied() {
        let resolver = QueueResolver {
            answers: Mutex::new(VecDeque::from([vec![public(443, 34)]])),
        };
        let request = action("https://example.com/start");
        let prepared = PreparedAppBoundHttp::prepare_with_resolver(&request, Utc::now(), &resolver)
            .await
            .unwrap();
        let substituted = action("https://example.com/other");
        assert!(!prepared.matches_action(&substituted));
    }

    #[test]
    fn trailing_dot_cannot_escape_the_reqwest_dns_override_key() {
        let error = validate_read_only_action(&action("https://example.com./x")).unwrap_err();
        assert!(error.to_string().contains("non-trailing-dot"));
    }

    #[test]
    fn literal_ip_cannot_bypass_dns_host_and_sni_identity() {
        let error = validate_read_only_action(&action("https://93.184.216.34/x")).unwrap_err();
        assert!(error.to_string().contains("requires a DNS host"));
    }
}
