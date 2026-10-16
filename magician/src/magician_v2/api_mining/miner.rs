//! API Mining Clustering Engine
//!
//! Groups network traces by endpoint signature using deterministic, signature-based
//! clustering (Decision D8). No DBSCAN hyperparameter tuning needed.
//!
//! Clustering pipeline:
//! 1. Filter traces (XHR/Fetch/Document only, skip noise)
//! 2. Normalize URLs (numeric IDs → `{id}`, UUIDs → `{uuid}`, timestamps → `{ts}`)
//! 3. Group by signature: (method, host, normalized_path, content_type_category)
//! 4. Detect GraphQL by Content-Type + operationName in request body
//! 5. Diff-based parameterization to identify stable vs variable fields
//! 6. Emit ApiCluster for each group meeting min_samples threshold

use crate::magician_v2::api_mining::body_template::{body_placeholder, BodyPlaceholderKind};
use crate::magician_v2::api_mining::capability::{
    ApiCapability, AuthRequirements, GraphqlOperationKind,
};
use crate::magician_v2::api_mining::registry::CapabilityRegistry;
use crate::magician_v2::api_mining::relevance::{
    classify as classify_relevance, page_origin_for_trace, Relevance,
};
use crate::magician_v2::api_mining::trace_storage::is_sensitive_header_name;
use crate::magician_v2::api_mining::types::{NetworkTraceEvent, SessionContext};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use url::Url;

/// Represents a cluster of similar network requests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCluster {
    /// Generated signature for the cluster (e.g., "GET:mail.google.com:/sync/u/0/i/{param}")
    pub signature: String,

    /// Sample count in this cluster
    pub count: usize,

    /// Indices of traces belonging to this cluster (into the original trace slice)
    pub trace_indices: Vec<usize>,

    /// Representative URL template with `{param}` placeholders
    pub url_template: String,

    /// HTTP method
    pub method: String,

    /// Origin (scheme + host)
    pub origin: String,

    /// Content type category (json, html, xml, text, other)
    pub content_type_category: String,

    /// GraphQL operation name (if detected)
    pub graphql_operation: Option<String>,

    /// GraphQL operation kind (query/mutation/subscription/unknown) if detected.
    pub graphql_operation_kind: Option<GraphqlOperationKind>,

    /// Persisted query hash for APQ-style GraphQL requests when available.
    pub graphql_persisted_query_sha256: Option<String>,

    /// Body structure fingerprint (if body-aware splitting produced this cluster)
    pub body_fingerprint: Option<String>,

    /// Request body template for active replay of mutating endpoints.
    pub body_template: Option<String>,

    /// Stable headers observed across all samples (key → set of unique values)
    pub stable_headers: HashMap<String, Vec<String>>,

    /// Structural relevance computed from the representative trace. Recipe
    /// compilation may later promote this to Dependency/AnswerBearing.
    #[serde(default)]
    pub relevance: Relevance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance_reason: Option<String>,
}

/// Clustering configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusteringConfig {
    /// Minimum samples required to form a cluster (default: 2)
    pub min_samples: usize,

    /// Resource types to include in clustering
    pub included_resource_types: Vec<String>,

    /// Domain+path patterns that override noise filtering.
    /// Each entry is matched as a substring against "host/path" of the URL.
    /// If a URL's host+path contains any of these, the URL bypasses noise filtering.
    #[serde(default)]
    pub noise_filter_whitelist: Vec<String>,

    /// Additional domain+path patterns to blacklist as noise.
    /// Checked after whitelist — whitelisted URLs won't be blocked by this.
    #[serde(default)]
    pub noise_filter_blacklist: Vec<String>,

    /// Per-HTTP-method threshold for promoting a fresh capability
    /// from `Observed` to `Candidate`. Mirrors
    /// `ApiMiningConfig::takeover_min_samples_per_method`; the
    /// orchestrator clones the value through here so the miner can
    /// pass it to `ApiCapability::try_promote_with_threshold`. Empty
    /// map → fall back to `takeover_default_min_samples`.
    #[serde(default)]
    pub takeover_min_samples_per_method: std::collections::HashMap<String, usize>,

    /// Fallback `takeover_min_samples_per_method` value when a method
    /// isn't listed. Default 3 matches the historical
    /// `MIN_SAMPLES_FOR_CANDIDATE` constant.
    #[serde(default = "default_clustering_takeover_default_min_samples")]
    pub takeover_default_min_samples: usize,
}

fn default_clustering_takeover_default_min_samples() -> usize {
    3
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            min_samples: 2,
            included_resource_types: vec!["XHR".to_string(), "Fetch".to_string()],
            noise_filter_whitelist: Vec::new(),
            noise_filter_blacklist: Vec::new(),
            takeover_min_samples_per_method: std::collections::HashMap::new(),
            takeover_default_min_samples: 3,
        }
    }
}

impl ClusteringConfig {
    /// Resolve the per-method takeover threshold for promoting from
    /// Observed to Candidate. Case-insensitive on method.
    pub fn takeover_min_samples(&self, method: &str) -> usize {
        let upper = method.to_ascii_uppercase();
        self.takeover_min_samples_per_method
            .get(&upper)
            .copied()
            .unwrap_or(self.takeover_default_min_samples)
    }
}

/// Regex-free patterns for URL normalization
const UUID_LEN: usize = 36; // 8-4-4-4-12 with hyphens
const UUID_HEX_LEN: usize = 32; // Without hyphens

/// Compute a structural fingerprint from a request body (standalone version).
///
/// This allows callers (e.g., the executor's XHR validation pass) to fingerprint
/// observed XHR traces without creating an `ApiMiner` instance.
///
/// `request_body` — the raw request body string (None or empty → None).
/// `content_type` — the request Content-Type header value (hint, not gate).
/// `is_graphql` — whether this is a GraphQL request.
pub fn compute_body_fingerprint_standalone(
    request_body: Option<&str>,
    content_type: Option<&str>,
    is_graphql: bool,
) -> Option<String> {
    let body = request_body.filter(|b| !b.is_empty())?;

    if is_graphql {
        return compute_graphql_body_fingerprint(body);
    }

    let ct = content_type.unwrap_or("").to_lowercase();
    let trimmed_body = body.trim_start();

    // Some browser clients send JSON payloads with a form content type. Parse
    // JSON-looking bodies structurally before falling back to form keys so
    // search/RPC calls do not get fingerprinted as one giant literal form key.
    if ct.contains("json") || trimmed_body.starts_with('{') || trimmed_body.starts_with('[') {
        if let Some(fingerprint) = compute_json_body_fingerprint(body) {
            return Some(fingerprint);
        }
    }

    // Form-encoded gets priority check (unambiguous content-type)
    if ct.contains("x-www-form-urlencoded") {
        let mut keys: Vec<String> = body
            .split('&')
            .filter_map(|pair| {
                let key = pair.split('=').next()?;
                if key.is_empty() {
                    None
                } else {
                    Some(key.to_string())
                }
            })
            .collect();
        keys.sort();
        keys.dedup();
        return Some(format!("form_keys:{}", keys.join(",")));
    }

    // Try JSON parsing regardless of content-type
    compute_json_body_fingerprint(body)
}

fn compute_json_body_fingerprint(body: &str) -> Option<String> {
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body) {
        return match &parsed {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
                keys.sort();
                Some(format!("json_keys:{}", keys.join(",")))
            },
            serde_json::Value::Array(_) => {
                let elem_type = parsed
                    .as_array()
                    .and_then(|arr| arr.first())
                    .map(|v| match v {
                        serde_json::Value::Object(_) => "object",
                        serde_json::Value::Array(_) => "array",
                        serde_json::Value::String(_) => "string",
                        serde_json::Value::Number(_) => "number",
                        serde_json::Value::Bool(_) => "bool",
                        serde_json::Value::Null => "null",
                    })
                    .unwrap_or("empty");
                Some(format!("json_array:{}", elem_type))
            },
            _ => None,
        };
    }

    None
}

pub fn detect_graphql_operation_standalone(
    request_body: Option<&str>,
    content_type: Option<&str>,
) -> Option<String> {
    detect_graphql_request_info_standalone(request_body, content_type)
        .map(|info| info.operation_name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphqlRequestInfo {
    pub operation_name: String,
    pub operation_kind: GraphqlOperationKind,
    pub persisted_query_sha256: Option<String>,
}

pub fn detect_graphql_request_info_standalone(
    request_body: Option<&str>,
    content_type: Option<&str>,
) -> Option<GraphqlRequestInfo> {
    detect_graphql_request_info_with_knowledge_standalone(request_body, content_type, None, None)
}

pub fn detect_graphql_request_info_with_knowledge_standalone(
    request_body: Option<&str>,
    content_type: Option<&str>,
    origin: Option<&str>,
    knowledge: Option<&GraphqlKnowledge>,
) -> Option<GraphqlRequestInfo> {
    let is_json_ct = content_type.unwrap_or("").to_lowercase().contains("json");
    if !is_json_ct {
        return None;
    }

    let body = request_body?;
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    if !is_graphql_payload(&parsed) {
        return None;
    }

    let persisted_query_sha256 = extract_persisted_query_sha256(&parsed);
    let operation_name = sanitize_graphql_operation_name(
        parsed.get("operationName").and_then(|value| value.as_str()),
        persisted_query_sha256.as_deref(),
    );
    let mut operation_kind = parsed
        .get("query")
        .and_then(|value| value.as_str())
        .map(detect_graphql_operation_kind_from_query)
        .unwrap_or(GraphqlOperationKind::Unknown);
    if operation_kind == GraphqlOperationKind::Unknown {
        if let (Some(origin), Some(knowledge)) = (origin, knowledge) {
            if let Some(resolved_kind) = knowledge.resolve(
                origin,
                Some(operation_name.as_str()),
                persisted_query_sha256.as_deref(),
            ) {
                operation_kind = resolved_kind;
            }
        }
    }

    Some(GraphqlRequestInfo {
        operation_name,
        operation_kind,
        persisted_query_sha256,
    })
}

fn is_graphql_payload(parsed: &serde_json::Value) -> bool {
    parsed
        .get("query")
        .and_then(|value| value.as_str())
        .is_some()
        || parsed
            .get("operationName")
            .and_then(|value| value.as_str())
            .is_some()
        || parsed
            .get("extensions")
            .and_then(|value| value.get("persistedQuery"))
            .is_some()
}

fn sanitize_graphql_operation_name(
    raw: Option<&str>,
    persisted_query_sha256: Option<&str>,
) -> String {
    let op_name = raw.unwrap_or("anonymous");

    if op_name.is_empty() || op_name.len() > 128 {
        return persisted_query_sha256
            .map(synthetic_persisted_query_name)
            .unwrap_or_else(|| "anonymous".to_string());
    }

    if !op_name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return persisted_query_sha256
            .map(synthetic_persisted_query_name)
            .unwrap_or_else(|| "anonymous".to_string());
    }

    op_name.to_string()
}

fn extract_persisted_query_sha256(parsed: &serde_json::Value) -> Option<String> {
    let hash = parsed
        .get("extensions")
        .and_then(|value| value.get("persistedQuery"))
        .and_then(|value| value.get("sha256Hash"))
        .and_then(|value| value.as_str())?;
    let normalized = hash.trim().to_ascii_lowercase();
    if normalized.len() >= 12 && normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(normalized)
    } else {
        None
    }
}

fn synthetic_persisted_query_name(hash: &str) -> String {
    let prefix = &hash[..12.min(hash.len())];
    format!("persisted_{}", prefix)
}

fn compute_graphql_body_fingerprint(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    if !is_graphql_payload(&parsed) {
        return None;
    }

    if let Some(hash) = extract_persisted_query_sha256(&parsed) {
        return Some(format!("graphql_apq:{}", hash));
    }

    let query = parsed.get("query").and_then(|value| value.as_str())?;
    let normalized = normalize_graphql_document(query);
    if normalized.is_empty() {
        return None;
    }

    Some(format!(
        "graphql_doc:{}",
        blake3::hash(normalized.as_bytes()).to_hex()
    ))
}

fn normalize_graphql_document(query: &str) -> String {
    let bytes = query.as_bytes();
    let mut idx = 0usize;
    let mut normalized = String::with_capacity(query.len());

    while idx < bytes.len() {
        match bytes[idx] {
            b' ' | b'\n' | b'\r' | b'\t' | b',' => idx += 1,
            b'#' => {
                while idx < bytes.len() && bytes[idx] != b'\n' {
                    idx += 1;
                }
            },
            b'"' => copy_graphql_string(query, &mut idx, &mut normalized),
            _ => {
                normalized.push(bytes[idx] as char);
                idx += 1;
            },
        }
    }

    normalized
}

fn copy_graphql_string(query: &str, idx: &mut usize, out: &mut String) {
    let bytes = query.as_bytes();
    let start = *idx;

    if bytes.get(*idx..(*idx + 3)) == Some(br#""""#) {
        *idx += 3;
        while *idx + 2 < bytes.len() {
            if bytes.get(*idx..(*idx + 3)) == Some(br#""""#) {
                *idx += 3;
                out.push_str(&query[start..*idx]);
                return;
            }
            *idx += 1;
        }
        *idx = bytes.len();
        out.push_str(&query[start..]);
        return;
    }

    *idx += 1;
    while *idx < bytes.len() {
        match bytes[*idx] {
            b'\\' => *idx += 2,
            b'"' => {
                *idx += 1;
                out.push_str(&query[start..*idx]);
                return;
            },
            _ => *idx += 1,
        }
    }

    out.push_str(&query[start..]);
}

#[derive(Debug, Clone, Default)]
pub struct GraphqlKnowledge {
    by_origin_operation: HashMap<(String, String), GraphqlOperationKind>,
    by_origin_hash: HashMap<(String, String), GraphqlOperationKind>,
}

impl GraphqlKnowledge {
    pub fn from_registry(registry: &CapabilityRegistry) -> Self {
        let mut knowledge = Self::default();
        for origin in registry.index().origins.values() {
            for capability in &origin.capabilities {
                let Some(kind) = capability.graphql_operation_kind else {
                    continue;
                };
                if kind == GraphqlOperationKind::Unknown {
                    continue;
                }
                knowledge.record(
                    &origin.origin_url,
                    capability.graphql_operation.as_deref(),
                    capability.graphql_persisted_query_sha256.as_deref(),
                    kind,
                );
            }
        }
        knowledge
    }

    pub fn with_traces(&self, traces: &[(usize, &NetworkTraceEvent)]) -> Self {
        let mut knowledge = self.clone();
        for (_, trace) in traces {
            let content_type = trace
                .request_headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.as_str());
            let origin = super::router::extract_origin(&trace.url);
            if let Some(info) = detect_graphql_request_info_with_knowledge_standalone(
                trace.request_body.as_deref(),
                content_type,
                Some(origin.as_str()),
                Some(&knowledge),
            ) {
                knowledge.record(
                    origin.as_str(),
                    Some(info.operation_name.as_str()),
                    info.persisted_query_sha256.as_deref(),
                    info.operation_kind,
                );
            }
        }
        knowledge
    }

    pub fn resolve(
        &self,
        origin: &str,
        operation_name: Option<&str>,
        persisted_query_sha256: Option<&str>,
    ) -> Option<GraphqlOperationKind> {
        if let Some(hash) = persisted_query_sha256 {
            if let Some(kind) = self
                .by_origin_hash
                .get(&(origin.to_string(), hash.to_ascii_lowercase()))
                .copied()
            {
                return Some(kind);
            }
        }

        operation_name.and_then(|operation| {
            self.by_origin_operation
                .get(&(origin.to_string(), operation.to_string()))
                .copied()
        })
    }

    fn record(
        &mut self,
        origin: &str,
        operation_name: Option<&str>,
        persisted_query_sha256: Option<&str>,
        kind: GraphqlOperationKind,
    ) {
        if kind == GraphqlOperationKind::Unknown {
            return;
        }

        if let Some(operation_name) = operation_name {
            self.by_origin_operation
                .insert((origin.to_string(), operation_name.to_string()), kind);
        }

        if let Some(hash) = persisted_query_sha256 {
            self.by_origin_hash
                .insert((origin.to_string(), hash.to_ascii_lowercase()), kind);
        }
    }
}

fn detect_graphql_operation_kind_from_query(query: &str) -> GraphqlOperationKind {
    let bytes = query.as_bytes();
    let mut idx = 0usize;

    while idx < bytes.len() {
        skip_graphql_ignored(query, &mut idx);
        if idx >= bytes.len() {
            break;
        }

        match bytes[idx] {
            b'{' => return GraphqlOperationKind::Query,
            b'"' => {
                skip_graphql_string(query, &mut idx);
            },
            b if is_graphql_name_start(b) => {
                let token = read_graphql_name(query, &mut idx);
                match token {
                    "query" => return GraphqlOperationKind::Query,
                    "mutation" => return GraphqlOperationKind::Mutation,
                    "subscription" => return GraphqlOperationKind::Subscription,
                    "fragment" => skip_graphql_definition_body(query, &mut idx),
                    _ => return GraphqlOperationKind::Unknown,
                }
            },
            _ => idx += 1,
        }
    }

    GraphqlOperationKind::Unknown
}

fn skip_graphql_ignored(query: &str, idx: &mut usize) {
    let bytes = query.as_bytes();
    while *idx < bytes.len() {
        match bytes[*idx] {
            b' ' | b'\n' | b'\r' | b'\t' | b',' => *idx += 1,
            b'#' => {
                while *idx < bytes.len() && bytes[*idx] != b'\n' {
                    *idx += 1;
                }
            },
            _ => break,
        }
    }
}

fn skip_graphql_string(query: &str, idx: &mut usize) {
    let bytes = query.as_bytes();
    if bytes.get(*idx..(*idx + 3)) == Some(br#""""#) {
        *idx += 3;
        while *idx + 2 < bytes.len() {
            if bytes.get(*idx..(*idx + 3)) == Some(br#""""#) {
                *idx += 3;
                return;
            }
            *idx += 1;
        }
        *idx = bytes.len();
        return;
    }

    *idx += 1;
    while *idx < bytes.len() {
        match bytes[*idx] {
            b'\\' => *idx += 2,
            b'"' => {
                *idx += 1;
                return;
            },
            _ => *idx += 1,
        }
    }
}

fn skip_graphql_definition_body(query: &str, idx: &mut usize) {
    let bytes = query.as_bytes();
    let mut depth = 0usize;
    let mut saw_brace = false;

    while *idx < bytes.len() {
        match bytes[*idx] {
            b'#' => {
                while *idx < bytes.len() && bytes[*idx] != b'\n' {
                    *idx += 1;
                }
            },
            b'"' => skip_graphql_string(query, idx),
            b'{' => {
                depth += 1;
                saw_brace = true;
                *idx += 1;
            },
            b'}' => {
                depth = depth.saturating_sub(1);
                *idx += 1;
                if saw_brace && depth == 0 {
                    return;
                }
            },
            _ => *idx += 1,
        }
    }
}

fn is_graphql_name_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_graphql_name_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn read_graphql_name<'a>(query: &'a str, idx: &mut usize) -> &'a str {
    let bytes = query.as_bytes();
    let start = *idx;
    *idx += 1;
    while *idx < bytes.len() && is_graphql_name_continue(bytes[*idx]) {
        *idx += 1;
    }
    &query[start..*idx]
}

#[derive(Debug, Clone)]
enum JsonTemplateNode {
    Literal(serde_json::Value),
    Placeholder {
        kind: BodyPlaceholderKind,
        name: String,
    },
    Object(Vec<(String, JsonTemplateNode)>),
    Array(Vec<JsonTemplateNode>),
}

fn sanitize_body_path_segment(raw: &str) -> String {
    let cleaned = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "value".to_string()
    } else {
        trimmed.to_string()
    }
}

fn body_param_name(path: &str) -> String {
    let suffix = sanitize_body_path_segment(path);
    if suffix == "payload" {
        "body_payload".to_string()
    } else {
        format!("body_{}", suffix)
    }
}

fn body_placeholder_kind_for_value(value: &serde_json::Value) -> BodyPlaceholderKind {
    match value {
        serde_json::Value::String(_) => BodyPlaceholderKind::String,
        serde_json::Value::Number(_) => BodyPlaceholderKind::Number,
        serde_json::Value::Bool(_) => BodyPlaceholderKind::Boolean,
        serde_json::Value::Null | serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            BodyPlaceholderKind::Json
        },
    }
}

fn should_parameterize_stable_json_value(path: &str, value: &serde_json::Value) -> bool {
    let serde_json::Value::String(raw) = value else {
        return false;
    };
    if !is_runtime_search_body_key(path) || !is_safe_stable_search_value(raw) {
        return false;
    }

    let trimmed = raw.trim_start().to_ascii_lowercase();
    !(trimmed.starts_with("query ")
        || trimmed.starts_with("mutation ")
        || trimmed.starts_with("subscription ")
        || raw.contains('$')
        || raw.contains('{')
        || raw.contains('}'))
}

fn is_runtime_search_body_key(path: &str) -> bool {
    let leaf = path.rsplit('_').next().unwrap_or(path).to_ascii_lowercase();
    matches!(
        leaf.as_str(),
        "q" | "query" | "search" | "searchquery" | "keyword" | "keywords" | "term"
    )
}

fn is_safe_stable_search_value(raw: &str) -> bool {
    let trimmed = raw.trim();
    !trimmed.is_empty() && trimmed != "[REDACTED]" && trimmed.len() <= 256
}

fn extend_storage_auth_requirements(auth: &mut AuthRequirements, session: &SessionContext) {
    for key in session.available_local_storage_keys() {
        if looks_like_auth_storage_key(&key) && !auth.local_storage_keys.contains(&key) {
            auth.local_storage_keys.push(key);
        }
    }
    for key in session.available_session_storage_keys() {
        if looks_like_auth_storage_key(&key) && !auth.session_storage_keys.contains(&key) {
            auth.session_storage_keys.push(key);
        }
    }
}

fn extend_query_auth_requirements(auth: &mut AuthRequirements, url_template: &str) {
    let Ok(parsed) = Url::parse(url_template) else {
        return;
    };
    for (key, value) in parsed.query_pairs() {
        let key = key.to_string();
        if (value == "[REDACTED]" || looks_like_auth_query_param(&key))
            && !auth
                .query_params
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&key))
        {
            auth.query_params.push(key);
        }
    }
}

fn looks_like_auth_query_param(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("key")
        || lower.contains("auth")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("csrf")
        || lower.contains("xsrf")
}

fn looks_like_auth_storage_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("csrf")
        || lower.contains("xsrf")
        || lower.contains("refresh")
        || lower.contains("session")
        || lower.contains("auth")
        || lower.contains("jwt")
        || lower.contains("bearer")
}

pub struct ApiMiner {
    config: ClusteringConfig,
    graphql_knowledge: GraphqlKnowledge,
    noise: super::noise_filter::NoiseFilter,
}

impl ApiMiner {
    pub fn new(config: ClusteringConfig) -> Self {
        Self::with_graphql_knowledge(config, GraphqlKnowledge::default())
    }

    pub fn with_graphql_knowledge(
        config: ClusteringConfig,
        graphql_knowledge: GraphqlKnowledge,
    ) -> Self {
        let mut noise = super::noise_filter::NoiseFilter::seeded();
        noise
            .whitelist
            .extend(config.noise_filter_whitelist.clone());
        noise
            .blacklist
            .extend(config.noise_filter_blacklist.clone());
        Self::with_graphql_knowledge_and_noise(config, graphql_knowledge, noise)
    }

    pub fn with_graphql_knowledge_and_noise(
        config: ClusteringConfig,
        graphql_knowledge: GraphqlKnowledge,
        noise: super::noise_filter::NoiseFilter,
    ) -> Self {
        Self {
            config,
            graphql_knowledge,
            noise,
        }
    }

    /// Cluster a batch of network traces into API endpoint groups
    ///
    /// Returns clusters that meet the min_samples threshold. Each cluster
    /// represents a distinct API endpoint that could become an ApiCapability.
    pub fn cluster_traces(&self, traces: &[NetworkTraceEvent]) -> Vec<ApiCluster> {
        // Step 1: Filter to relevant resource types
        let filtered: Vec<(usize, &NetworkTraceEvent)> = traces
            .iter()
            .enumerate()
            .filter(|(_, t)| self.is_relevant_trace(t))
            .collect();

        if filtered.is_empty() {
            return Vec::new();
        }

        let graphql_knowledge = self.graphql_knowledge.with_traces(&filtered);

        // Step 2: Group by signature
        let mut groups: HashMap<String, Vec<(usize, &NetworkTraceEvent)>> = HashMap::new();

        for (idx, trace) in &filtered {
            let sig = self.compute_signature(trace, &graphql_knowledge);
            groups.entry(sig).or_default().push((*idx, trace));
        }

        // Step 3: Build clusters from groups meeting min_samples
        let mut clusters = Vec::new();

        for (signature, members) in groups {
            if members.len() < self.config.min_samples {
                continue;
            }

            let first = members[0].1;
            let url_template = self.build_url_template(&members);
            let origin = self.extract_origin(&first.url);
            let content_type = self.categorize_content_type(first);
            let graphql_info = self.detect_graphql_request_info(first, Some(&graphql_knowledge));
            let body_fingerprint = self.compute_body_fingerprint(first);
            let body_template = self.build_body_template(&members);
            let relevance =
                classify_relevance(first, &page_origin_for_trace(first), None, &self.noise);

            // Collect stable headers
            let stable_headers = self.find_stable_headers(&members);

            clusters.push(ApiCluster {
                signature,
                count: members.len(),
                trace_indices: members.iter().map(|(idx, _)| *idx).collect(),
                url_template,
                method: first.method.to_uppercase(),
                origin,
                content_type_category: content_type,
                graphql_operation: graphql_info
                    .as_ref()
                    .map(|info| info.operation_name.clone()),
                graphql_operation_kind: graphql_info.as_ref().map(|info| info.operation_kind),
                graphql_persisted_query_sha256: graphql_info
                    .as_ref()
                    .and_then(|info| info.persisted_query_sha256.clone()),
                body_fingerprint,
                body_template,
                stable_headers,
                relevance: relevance.relevance,
                relevance_reason: Some(relevance.reason),
            });
        }

        // Sort by count descending (most common endpoints first)
        clusters.sort_by(|a, b| b.count.cmp(&a.count));

        clusters
    }

    /// Convert a cluster into a preliminary ApiCapability
    pub fn cluster_to_capability(&self, cluster: &ApiCluster) -> ApiCapability {
        let name = self.generate_capability_name(cluster);

        let mut cap = ApiCapability::new(
            name,
            cluster.origin.clone(),
            cluster.method.clone(),
            cluster.url_template.clone(),
        );

        // Set sample count from cluster and promote if threshold met
        cap.sample_count = cluster.count;
        cap.graphql_operation = cluster.graphql_operation.clone();
        cap.graphql_operation_kind = cluster.graphql_operation_kind;
        cap.graphql_persisted_query_sha256 = cluster.graphql_persisted_query_sha256.clone();
        cap.body_fingerprint = cluster.body_fingerprint.clone();
        cap.body_template = cluster.body_template.clone();
        cap.relevance = cluster.relevance;
        cap.relevance_reason = cluster.relevance_reason.clone();
        cap.refresh_side_effects();
        // PL Task 7: resolve the per-method takeover threshold so
        // GET / HEAD endpoints reach Candidate on the second visit
        // (threshold 1) instead of the third (threshold 3 default).
        let candidate_min_samples = self.config.takeover_min_samples(&cap.method);
        cap.try_promote_with_threshold(candidate_min_samples);

        // Extract auth requirements from stable headers and cookies (L5: comprehensive detection)
        let mut auth = AuthRequirements::default();
        for key in cluster.stable_headers.keys() {
            let lower = key.to_lowercase();
            if lower != "cookie" && lower != "set-cookie" && is_sensitive_header_name(key) {
                auth.headers.push(key.clone());
            }
            // Detect cookie header → extract cookie names
            if lower == "cookie" {
                if let Some(values) = cluster.stable_headers.get(key) {
                    for cookie_str in values {
                        // Skip redacted values — cookie names can't be recovered
                        // after trace_storage::redact_trace() replaces them.
                        if cookie_str == "[REDACTED]" {
                            continue;
                        }
                        for pair in cookie_str.split(';') {
                            if let Some(name) = pair.trim().split('=').next() {
                                let name = name.trim();
                                if !name.is_empty() && !auth.cookies.contains(&name.to_string()) {
                                    auth.cookies.push(name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        // If auth headers require cookies but none were explicitly found, mark as needs_cookie
        if auth
            .headers
            .iter()
            .any(|h| h.to_lowercase() == "authorization")
            && auth.cookies.is_empty()
        {
            // Authorization header present — likely token-based, cookies optional
        } else if !auth.cookies.is_empty() {
            // Cookies found — needs cookie-based session
        }
        extend_query_auth_requirements(&mut auth, &cluster.url_template);
        cap.auth_requirements = auth;

        // Populate parent_origin from Referer header of first sample.
        // Used for active auth recovery on API-only sub-origins (e.g., clients6.google.com).
        if let Some(referer_values) = cluster
            .stable_headers
            .get("referer")
            .or_else(|| cluster.stable_headers.get("Referer"))
        {
            if let Some(referer) = referer_values.first() {
                if let Ok(parsed) = url::Url::parse(referer) {
                    let parent =
                        format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or(""));
                    if parent != cap.origin {
                        cap.parent_origin = Some(parent);
                    }
                }
            }
        }

        // L7: Populate headers_template from stable headers that have a single unique value
        // (headers with multiple values are variable and shouldn't be templated)
        for (key, values) in &cluster.stable_headers {
            let lower = key.to_lowercase();
            // Skip standard browser headers that add noise
            if lower == "cookie"
                || lower == "host"
                || lower == "user-agent"
                || lower == "accept"
                || lower == "accept-language"
                || lower == "accept-encoding"
                || lower == "connection"
                || lower == "referer"
                || lower == "origin"
                || lower == "sec-fetch-mode"
                || lower == "sec-fetch-site"
                || lower == "sec-fetch-dest"
            {
                continue;
            }
            // Skip auth/session headers — trace storage redacts these to "[REDACTED]",
            // so templating them would poison replay with literal "[REDACTED]" values.
            // Live auth is supplied via SessionContext at replay time instead.
            if is_sensitive_header_name(key) {
                continue;
            }
            if values.len() == 1 {
                // Extra safety: skip any value that was redacted by trace storage
                if values[0].contains("[REDACTED]") {
                    continue;
                }
                cap.headers_template.insert(key.clone(), values[0].clone());
            }
        }

        // Set trace IDs
        cap.trace_ids = cluster
            .trace_indices
            .iter()
            .map(|i| format!("trace-{}", i))
            .collect();

        cap
    }

    /// Convert a cluster into a preliminary ApiCapability and optionally enrich
    /// auth requirements from the currently captured browser session state.
    pub fn cluster_to_capability_with_session_context(
        &self,
        cluster: &ApiCluster,
        session: Option<&SessionContext>,
    ) -> ApiCapability {
        let mut capability = self.cluster_to_capability(cluster);
        if let Some(session) = session {
            extend_storage_auth_requirements(&mut capability.auth_requirements, session);
        }
        capability
    }

    /// Check if a trace is relevant for clustering (XHR/Fetch only, no noise)
    fn is_relevant_trace(&self, trace: &NetworkTraceEvent) -> bool {
        // Filter by resource type if available
        if let Some(ref rt) = trace.resource_type {
            if !self.config.included_resource_types.contains(rt) {
                return false;
            }
        }

        // Skip data URIs and blob URLs
        if trace.url.starts_with("data:") || trace.url.starts_with("blob:") {
            return false;
        }

        // Skip non-HTTP
        if !trace.url.starts_with("http://") && !trace.url.starts_with("https://") {
            return false;
        }

        // Skip failed requests with no response
        if trace.status == 0 {
            return false;
        }

        classify_relevance(trace, &page_origin_for_trace(trace), None, &self.noise).relevance
            != Relevance::Telemetry
    }

    /// Compute a deterministic signature for grouping traces
    ///
    /// Format:
    /// `{METHOD}:{host}:{normalized_path}:{sorted_query_keys}:{content_type_category}`
    /// or `{METHOD}:{host}:graphql:{kind}:{op}:{content_type_category}[:{body_fingerprint}]`
    fn compute_signature(
        &self,
        trace: &NetworkTraceEvent,
        graphql_knowledge: &GraphqlKnowledge,
    ) -> String {
        let method = trace.method.to_uppercase();
        let content_type = self.categorize_content_type(trace);

        // Parse URL once — all sub-operations use the parsed result.
        let parsed = match Url::parse(&trace.url) {
            Ok(u) => u,
            Err(_) => {
                return format!("{}:unknown:{}:{}", method, trace.url, content_type);
            },
        };

        let host = parsed.host_str().unwrap_or("unknown").to_string();

        // Check for GraphQL — cluster by operationName instead of URL path
        if let Some(info) = self.detect_graphql_request_info(trace, Some(graphql_knowledge)) {
            let mut signature = format!(
                "{}:{}:graphql:{}:{}:{}",
                method,
                host,
                info.operation_kind.as_str(),
                info.operation_name,
                content_type
            );
            if let Some(fp) = self.compute_body_fingerprint(trace) {
                signature.push(':');
                signature.push_str(&fp);
            }
            return signature;
        }

        let segments: Vec<String> = parsed
            .path_segments()
            .map(|segs| segs.map(|seg| self.normalize_segment(seg)).collect())
            .unwrap_or_default();
        let normalized_path = format!("/{}", segments.join("/"));

        let mut keys: Vec<String> = parsed
            .query_pairs()
            .map(|(k, _)| k.to_lowercase())
            .collect();
        keys.sort();
        keys.dedup();
        let query_keys = keys.join(",");

        let base_sig = format!(
            "{}:{}:{}:{}:{}",
            method, host, normalized_path, query_keys, content_type
        );

        // Append body fingerprint for methods that may legitimately carry a body.
        if !matches!(method.as_str(), "GET" | "HEAD") {
            if let Some(fp) = self.compute_body_fingerprint(trace) {
                return format!("{}:{}", base_sig, fp);
            }
        }

        base_sig
    }

    /// Extract origin (scheme + host + port) from a URL.
    /// Delegates to `router::extract_origin` for consistent origin keys across the pipeline.
    fn extract_origin(&self, url: &str) -> String {
        super::router::extract_origin(url)
    }

    /// Normalize a URL path by replacing dynamic segments with placeholders
    ///
    /// - Numeric IDs: `/users/12345` → `/users/{id}`
    /// - UUIDs: `/items/a1b2c3d4-e5f6-...` → `/items/{uuid}`
    /// - Timestamps: `/events/1707456000` → `/events/{ts}`
    /// - Hex strings (32+ chars): `/files/abcdef1234...` → `/files/{hash}`
    fn normalize_path(&self, url: &str) -> String {
        let parsed = match Url::parse(url) {
            Ok(u) => u,
            Err(_) => return url.to_string(),
        };

        let segments: Vec<String> = parsed
            .path_segments()
            .map(|segs| segs.map(|seg| self.normalize_segment(seg)).collect())
            .unwrap_or_default();

        format!("/{}", segments.join("/"))
    }

    /// Normalize a single path segment
    fn normalize_segment(&self, segment: &str) -> String {
        if segment.is_empty() {
            return String::new();
        }

        // UUID with hyphens: 8-4-4-4-12
        if segment.len() == UUID_LEN && Self::is_uuid_with_hyphens(segment) {
            return "{uuid}".to_string();
        }

        // UUID without hyphens: 32 hex chars
        if segment.len() == UUID_HEX_LEN && segment.chars().all(|c| c.is_ascii_hexdigit()) {
            return "{uuid}".to_string();
        }

        // Pure numeric (could be ID or timestamp)
        if segment.chars().all(|c| c.is_ascii_digit()) {
            let len = segment.len();
            // M11: Validate epoch range instead of just checking digit count.
            // Epoch seconds (10 digits) must be in plausible range: 2000-01-01 to 2100-01-01
            // Epoch millis (13 digits) similarly validated.
            if len == 10 {
                if let Ok(val) = segment.parse::<u64>() {
                    // 946684800 = 2000-01-01, 4102444800 = 2100-01-01
                    if (946_684_800..=4_102_444_800).contains(&val) {
                        return "{ts}".to_string();
                    }
                }
            } else if len == 13 {
                if let Ok(val) = segment.parse::<u64>() {
                    if (946_684_800_000..=4_102_444_800_000).contains(&val) {
                        return "{ts}".to_string();
                    }
                }
            }
            return "{id}".to_string();
        }

        // Long hex strings (likely hashes or tokens)
        if segment.len() >= 32 && segment.chars().all(|c| c.is_ascii_hexdigit()) {
            return "{hash}".to_string();
        }

        // Mixed alphanumeric that looks like a generated ID (e.g., "abc123def456")
        if segment.len() >= 16
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && segment.chars().any(|c| c.is_ascii_digit())
            && segment.chars().any(|c| c.is_ascii_alphabetic())
        {
            // Only normalize if mostly random-looking (high digit ratio)
            let digit_ratio = segment.chars().filter(|c| c.is_ascii_digit()).count() as f64
                / segment.len() as f64;
            if digit_ratio > 0.3 {
                return "{param}".to_string();
            }
        }

        segment.to_string()
    }

    /// Check if a string matches the UUID format (8-4-4-4-12 hex with hyphens)
    fn is_uuid_with_hyphens(s: &str) -> bool {
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() != 5 {
            return false;
        }
        let expected_lens = [8, 4, 4, 4, 12];
        for (part, &expected) in parts.iter().zip(&expected_lens) {
            if part.len() != expected || !part.chars().all(|c| c.is_ascii_hexdigit()) {
                return false;
            }
        }
        true
    }

    /// Categorize the content type from response headers
    fn categorize_content_type(&self, trace: &NetworkTraceEvent) -> String {
        let ct = trace
            .response_headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == "content-type")
            .map(|(_, v)| v.to_lowercase())
            .unwrap_or_default();

        if ct.contains("json") {
            "json".to_string()
        } else if ct.contains("html") {
            "html".to_string()
        } else if ct.contains("xml") {
            "xml".to_string()
        } else if ct.contains("text") {
            "text".to_string()
        } else {
            "other".to_string()
        }
    }

    /// Detect GraphQL operation from request body
    ///
    /// GraphQL is identified by:
    /// 1. Content-Type containing "json"
    /// 2. GraphQL-shaped request payload (`query`, `operationName`, or persisted-query metadata)
    /// 3. Operation kind parsed from the GraphQL document when available
    fn detect_graphql_request_info(
        &self,
        trace: &NetworkTraceEvent,
        graphql_knowledge: Option<&GraphqlKnowledge>,
    ) -> Option<GraphqlRequestInfo> {
        let content_type = trace
            .request_headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == "content-type")
            .map(|(_, v)| v.as_str());
        let origin = Some(super::router::extract_origin(&trace.url));

        detect_graphql_request_info_with_knowledge_standalone(
            trace.request_body.as_deref(),
            content_type,
            origin.as_deref(),
            graphql_knowledge.or(Some(&self.graphql_knowledge)),
        )
    }

    /// Compute a structural fingerprint from the request body for body-aware splitting.
    ///
    /// Delegates to the standalone `compute_body_fingerprint_standalone()` function,
    /// extracting the needed fields from the trace.
    fn compute_body_fingerprint(&self, trace: &NetworkTraceEvent) -> Option<String> {
        let is_graphql = self.detect_graphql_request_info(trace, None).is_some();
        let content_type = trace
            .request_headers
            .iter()
            .find(|(k, _)| k.to_lowercase() == "content-type")
            .map(|(_, v)| v.as_str());

        compute_body_fingerprint_standalone(trace.request_body.as_deref(), content_type, is_graphql)
    }

    fn build_body_template(&self, members: &[(usize, &NetworkTraceEvent)]) -> Option<String> {
        if members.is_empty() {
            return None;
        }

        let bodies: Vec<&str> = members
            .iter()
            .filter_map(|(_, trace)| trace.request_body.as_deref())
            .filter(|body| !body.is_empty())
            .collect();
        if bodies.is_empty() {
            return None;
        }

        if bodies.iter().all(|body| *body == bodies[0]) && !bodies[0].contains("[REDACTED]") {
            if let Some(template) = self.build_json_body_template(members) {
                if template != bodies[0] {
                    return Some(template);
                }
            }
            if let Some(template) = self.build_form_body_template(members) {
                if template != bodies[0] {
                    return Some(template);
                }
            }
            return Some(bodies[0].to_string());
        }

        if let Some(template) = self.build_json_body_template(members) {
            return Some(template);
        }

        self.build_form_body_template(members)
    }

    fn build_json_body_template(&self, members: &[(usize, &NetworkTraceEvent)]) -> Option<String> {
        let mut parsed = Vec::new();
        for (_, trace) in members {
            let body = trace.request_body.as_ref()?;
            parsed.push(serde_json::from_str::<serde_json::Value>(body).ok()?);
        }

        let templated = self.merge_json_template(&parsed, "payload");
        let rendered = Self::render_json_template_node(&templated);
        if rendered.contains("[REDACTED]") {
            return None;
        }
        Some(rendered)
    }

    fn build_form_body_template(&self, members: &[(usize, &NetworkTraceEvent)]) -> Option<String> {
        let mut key_values: HashMap<String, Vec<String>> = HashMap::new();
        let mut total = 0usize;

        for (_, trace) in members {
            let content_type = trace
                .request_headers
                .iter()
                .find(|(k, _)| k.to_lowercase() == "content-type")
                .map(|(_, v)| v.to_lowercase())
                .unwrap_or_default();
            if !content_type.contains("x-www-form-urlencoded") {
                return None;
            }

            let body = trace.request_body.as_ref()?;
            total += 1;
            for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
                key_values
                    .entry(key.to_string())
                    .or_default()
                    .push(value.to_string());
            }
        }

        let mut parts = Vec::new();
        for (key, values) in key_values {
            if values.len() != total {
                return None;
            }

            if values.iter().all(|value| value == &values[0]) && values[0] != "[REDACTED]" {
                if is_runtime_search_body_key(&key) && is_safe_stable_search_value(&values[0]) {
                    parts.push(format!(
                        "{}={}",
                        key,
                        body_placeholder(
                            BodyPlaceholderKind::String,
                            &format!("body_form_{}", sanitize_body_path_segment(&key)),
                        )
                    ));
                    continue;
                }
                parts.push(format!("{}={}", key, values[0]));
            } else {
                parts.push(format!(
                    "{}={}",
                    key,
                    body_placeholder(
                        BodyPlaceholderKind::String,
                        &format!("body_form_{}", sanitize_body_path_segment(&key)),
                    )
                ));
            }
        }

        parts.sort();
        Some(parts.join("&"))
    }

    fn merge_json_template(&self, values: &[serde_json::Value], path: &str) -> JsonTemplateNode {
        if values.is_empty() {
            return JsonTemplateNode::Literal(serde_json::Value::Null);
        }

        if values.iter().all(|value| value == &values[0])
            && !matches!(&values[0], serde_json::Value::String(s) if s == "[REDACTED]")
        {
            if should_parameterize_stable_json_value(path, &values[0]) {
                return JsonTemplateNode::Placeholder {
                    kind: body_placeholder_kind_for_value(&values[0]),
                    name: body_param_name(path),
                };
            }
            if !matches!(
                values[0],
                serde_json::Value::Object(_) | serde_json::Value::Array(_)
            ) {
                return JsonTemplateNode::Literal(values[0].clone());
            }
        }

        match &values[0] {
            serde_json::Value::Object(first_map)
                if values
                    .iter()
                    .all(|value| matches!(value, serde_json::Value::Object(_))) =>
            {
                let first_keys = first_map.keys().cloned().collect::<Vec<_>>();
                let same_keys = values.iter().all(|value| match value {
                    serde_json::Value::Object(map) => {
                        map.len() == first_map.len()
                            && first_keys.iter().all(|key| map.contains_key(key))
                    },
                    _ => false,
                });
                if !same_keys {
                    return JsonTemplateNode::Placeholder {
                        kind: BodyPlaceholderKind::Json,
                        name: body_param_name(path),
                    };
                }

                let mut entries = Vec::new();
                for key in first_keys {
                    let child_values = values
                        .iter()
                        .map(|value| match value {
                            serde_json::Value::Object(map) => {
                                map.get(&key).cloned().unwrap_or(serde_json::Value::Null)
                            },
                            _ => serde_json::Value::Null,
                        })
                        .collect::<Vec<_>>();
                    let child_path = format!("{}_{}", path, sanitize_body_path_segment(&key));
                    entries.push((key, self.merge_json_template(&child_values, &child_path)));
                }
                JsonTemplateNode::Object(entries)
            },
            serde_json::Value::Array(first_arr)
                if values
                    .iter()
                    .all(|value| matches!(value, serde_json::Value::Array(_))) =>
            {
                let same_len = values.iter().all(|value| match value {
                    serde_json::Value::Array(arr) => arr.len() == first_arr.len(),
                    _ => false,
                });
                if !same_len {
                    return JsonTemplateNode::Placeholder {
                        kind: BodyPlaceholderKind::Json,
                        name: body_param_name(path),
                    };
                }

                let mut items = Vec::new();
                for idx in 0..first_arr.len() {
                    let child_values = values
                        .iter()
                        .map(|value| match value {
                            serde_json::Value::Array(arr) => arr[idx].clone(),
                            _ => serde_json::Value::Null,
                        })
                        .collect::<Vec<_>>();
                    let child_path = format!("{}_{}", path, idx);
                    items.push(self.merge_json_template(&child_values, &child_path));
                }
                JsonTemplateNode::Array(items)
            },
            serde_json::Value::String(_) => JsonTemplateNode::Placeholder {
                kind: BodyPlaceholderKind::String,
                name: body_param_name(path),
            },
            serde_json::Value::Number(_) => JsonTemplateNode::Placeholder {
                kind: BodyPlaceholderKind::Number,
                name: body_param_name(path),
            },
            serde_json::Value::Bool(_) => JsonTemplateNode::Placeholder {
                kind: BodyPlaceholderKind::Boolean,
                name: body_param_name(path),
            },
            serde_json::Value::Null => JsonTemplateNode::Placeholder {
                kind: BodyPlaceholderKind::Json,
                name: body_param_name(path),
            },
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                JsonTemplateNode::Placeholder {
                    kind: BodyPlaceholderKind::Json,
                    name: body_param_name(path),
                }
            },
        }
    }

    fn render_json_template_node(node: &JsonTemplateNode) -> String {
        match node {
            JsonTemplateNode::Literal(value) => value.to_string(),
            JsonTemplateNode::Placeholder { kind, name } => body_placeholder(*kind, name),
            JsonTemplateNode::Object(entries) => {
                let rendered = entries
                    .iter()
                    .map(|(key, value)| {
                        format!(
                            "{}:{}",
                            serde_json::Value::String(key.clone()),
                            Self::render_json_template_node(value)
                        )
                    })
                    .collect::<Vec<_>>();
                format!("{{{}}}", rendered.join(","))
            },
            JsonTemplateNode::Array(items) => {
                let rendered = items
                    .iter()
                    .map(Self::render_json_template_node)
                    .collect::<Vec<_>>();
                format!("[{}]", rendered.join(","))
            },
        }
    }

    /// Build a representative URL template from multiple trace samples
    ///
    /// For each path segment position, if all samples agree → keep literal.
    /// If samples differ → replace with `{param}`.
    fn build_url_template(&self, members: &[(usize, &NetworkTraceEvent)]) -> String {
        if members.is_empty() {
            return String::new();
        }

        // Use the first URL as the base
        let first_url = &members[0].1.url;
        let _base = match Url::parse(first_url) {
            Ok(u) => u,
            Err(_) => return self.normalize_path(first_url),
        };

        // Normalize all paths
        let normalized_paths: Vec<String> = members
            .iter()
            .map(|(_, t)| self.normalize_path(&t.url))
            .collect();

        // If all normalized paths are the same, use that
        if normalized_paths.iter().all(|p| p == &normalized_paths[0]) {
            let origin = super::router::extract_origin(first_url);
            let base_url = format!("{}{}", origin, normalized_paths[0]);
            let query_template = self.build_query_template(members);
            return if query_template.is_empty() {
                base_url
            } else {
                format!("{}?{}", base_url, query_template)
            };
        }

        // Otherwise, diff path segments to find variable positions
        let all_segments: Vec<Vec<&str>> = normalized_paths
            .iter()
            .map(|p| p.split('/').collect())
            .collect();

        let max_len = all_segments.iter().map(|s| s.len()).max().unwrap_or(0);
        let mut template_segments = Vec::new();

        for i in 0..max_len {
            let values_at_pos: Vec<&str> = all_segments
                .iter()
                .filter_map(|segs| segs.get(i).copied())
                .collect();

            if values_at_pos.is_empty() {
                continue;
            }

            // If all values at this position are the same, keep literal
            if values_at_pos.iter().all(|v| v == &values_at_pos[0]) {
                template_segments.push(values_at_pos[0].to_string());
            } else {
                template_segments.push("{param}".to_string());
            }
        }

        let origin = super::router::extract_origin(first_url);
        let joined = template_segments.join("/");
        // template_segments[0] is "" from the leading "/" in paths like "/api/users",
        // so joined already starts with "/". Avoid double-slash.
        let base_url = if joined.starts_with('/') {
            format!("{}{}", origin, joined)
        } else {
            format!("{}/{}", origin, joined)
        };

        // Build query string template: stable values stay literal, varying values → {key}
        let query_template = self.build_query_template(members);
        if query_template.is_empty() {
            base_url
        } else {
            format!("{}?{}", base_url, query_template)
        }
    }

    /// Build a query string template from cluster members.
    ///
    /// For each query parameter key that appears in ALL members:
    /// - If the value is the same across all → keep literal: `key=value`
    /// - If the value varies → parameterize: `key={key}`
    /// Keys that only appear in some members are dropped.
    fn build_query_template(&self, members: &[(usize, &NetworkTraceEvent)]) -> String {
        if members.is_empty() {
            return String::new();
        }

        // Collect all query key→values across all members
        let mut key_values: HashMap<String, Vec<String>> = HashMap::new();
        let total = members.len();

        for (_, trace) in members {
            if let Ok(parsed) = Url::parse(&trace.url) {
                let mut seen_keys = std::collections::HashSet::new();
                for (k, v) in parsed.query_pairs() {
                    let key = k.to_string();
                    if seen_keys.insert(key.clone()) {
                        key_values.entry(key).or_default().push(v.to_string());
                    }
                }
            }
        }

        // Only include keys that appear in ALL members
        let mut template_parts: Vec<(String, String)> = key_values
            .into_iter()
            .filter(|(_, vals)| vals.len() == total)
            .map(|(key, vals)| {
                // Check if all values are identical
                if vals.iter().all(|v| v == &vals[0]) {
                    (key.clone(), format!("{}={}", key, vals[0]))
                } else {
                    (key.clone(), format!("{}={{{}}}", key, key))
                }
            })
            .collect();

        // Sort for determinism
        template_parts.sort_by(|a, b| a.0.cmp(&b.0));
        template_parts
            .into_iter()
            .map(|(_, part)| part)
            .collect::<Vec<_>>()
            .join("&")
    }

    /// Find headers that appear consistently across all samples in a cluster
    ///
    /// L6 fix: Collect candidate keys from ALL samples (union), then check which
    /// appear in every sample. This avoids bias toward the first sample's key set.
    fn find_stable_headers(
        &self,
        members: &[(usize, &NetworkTraceEvent)],
    ) -> HashMap<String, Vec<String>> {
        if members.is_empty() {
            return HashMap::new();
        }

        // Collect all unique header keys across ALL samples (union, case-preserving)
        let mut all_keys: HashMap<String, String> = HashMap::new(); // lowercase → original
        for (_, trace) in members {
            for key in trace.request_headers.keys() {
                let lower = key.to_lowercase();
                all_keys.entry(lower).or_insert_with(|| key.clone());
            }
        }

        let mut stable = HashMap::new();

        for (lower_key, original_key) in &all_keys {
            // Check if this header (case-insensitive) appears in ALL samples
            let values: Vec<String> = members
                .iter()
                .filter_map(|(_, t)| {
                    t.request_headers
                        .iter()
                        .find(|(k, _)| k.to_lowercase() == *lower_key)
                        .map(|(_, v)| v.clone())
                })
                .collect();

            if values.len() == members.len() {
                // Header present in all samples — collect unique values
                let mut unique: Vec<String> = values;
                unique.sort();
                unique.dedup();
                stable.insert(original_key.clone(), unique);
            }
        }

        stable
    }

    /// Generate a human-readable name for a capability from its cluster
    fn generate_capability_name(&self, cluster: &ApiCluster) -> String {
        if let Some(ref op) = cluster.graphql_operation {
            let kind = cluster
                .graphql_operation_kind
                .map(|kind| kind.as_str())
                .unwrap_or("unknown");
            return format!(
                "{}_graphql_{}_{}",
                self.sanitize_host(&cluster.origin),
                kind,
                op.to_lowercase()
            );
        }

        let host = self.sanitize_host(&cluster.origin);
        let path_hint = cluster
            .url_template
            .split('/')
            .filter(|s| !s.is_empty() && !s.starts_with('{') && !s.contains("://"))
            .take(3)
            .collect::<Vec<_>>()
            .join("_");

        let base_name = if path_hint.is_empty() {
            format!("{}_{}_api", host, cluster.method.to_lowercase())
        } else {
            format!("{}_{}_{}", host, cluster.method.to_lowercase(), path_hint)
        };

        // Append a short hint from the body fingerprint for disambiguation
        if let Some(ref fp) = cluster.body_fingerprint {
            let hint: String = fp
                .split(':')
                .nth(1) // skip "json_keys" / "form_keys" prefix
                .unwrap_or(fp)
                .replace(',', "_")
                .chars()
                .take(40)
                .collect();
            format!("{}_{}", base_name, hint)
        } else {
            base_name
        }
    }

    /// Sanitize a host string for use in a capability name
    fn sanitize_host(&self, origin: &str) -> String {
        origin
            .replace("https://", "")
            .replace("http://", "")
            .replace(['.', ':'], "_")
            .replace('/', "")
    }
}

// ───────────────────────── Noise URL Filtering ───────────────────────────

/// Known-noise hosts that NEVER serve functional APIs.
/// Pure telemetry, analytics, consent, or beacon hosts.
pub(crate) const NOISE_HOSTS: &[&str] = &[
    // Microsoft
    "h.clarity.ms",
    // Product analytics
    "data.pendo.io",
    "edge.beacon.li",
    // Google consent/signals
    "fundingchoicesmessages.google.com",
    "signaler-pa.clients6.google.com",
    // Facebook/Meta
    "connect.facebook.net",
    "pixel.facebook.com",
    "tr.snapchat.com",
    // Analytics services
    "cdn.segment.com",
    "api.segment.io",
    "cdn.mxpnl.com",
    "api-js.mixpanel.com",
    "rs.fullstory.com",
    "static.hotjar.com",
    "script.hotjar.com",
    "vars.hotjar.com",
    "in.hotjar.com",
    "bat.bing.com",
    "stats.g.doubleclick.net",
    "www.google-analytics.com",
    "ssl.google-analytics.com",
    "analytics.google.com",
    // Error tracking
    "browser.sentry-cdn.com",
    // Tag managers
    "www.googletagmanager.com",
    // Ad tech
    "pagead2.googlesyndication.com",
    "securepubads.g.doubleclick.net",
    "tpc.googlesyndication.com",
    "ad.doubleclick.net",
];

/// Host suffixes where ALL content is static CDN (not APIs).
/// NOTE: Do NOT add `.googleapis.com` — it hosts both static JS AND real APIs
/// (e.g., content.googleapis.com/drive/v2beta/apps).
#[cfg(any(test, feature = "test-fixtures"))]
const STATIC_CDN_HOSTS: &[&str] = &[
    ".gstatic.com", // Google static assets (JS, CSS, fonts, images)
];

/// Known telemetry path patterns.
pub(crate) const NOISE_PATHS: &[&str] = &[
    "/beacon",
    "/collect",
    "/pixel",
    "/tracking",
    "/telemetry",
    "/log", // Telemetry logging endpoints (e.g., play.google.com/log)
];

/// Check if a URL is a known-noise endpoint (telemetry, analytics, static CDN).
///
/// Returns true if the URL should be excluded from mining. Uses (host, path)
/// tuples to avoid blocking origins that serve both static and API content.
///
/// `whitelist` — domain+path substrings that override noise filtering.
/// `blacklist` — additional domain+path substrings to treat as noise.
///
/// Priority: whitelist > blacklist > built-in rules.
/// If a URL matches the whitelist, it is never noise (even if blacklisted).
#[cfg(any(test, feature = "test-fixtures"))]
fn is_noise_url(url: &str, whitelist: &[String], blacklist: &[String]) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };

    let host = parsed.host_str().unwrap_or_default();
    let path = parsed.path();
    let host_path = format!("{}{}", host, path);

    // Check whitelist first — domain+path overrides beat all noise rules.
    if !whitelist.is_empty() {
        for pattern in whitelist {
            if host_path.contains(pattern.as_str()) {
                return false;
            }
        }
    }

    // Check exact noise hosts
    for noise_host in NOISE_HOSTS {
        if host == *noise_host {
            return true;
        }
    }

    // Check static CDN host suffixes
    for cdn_suffix in STATIC_CDN_HOSTS {
        if host.ends_with(cdn_suffix) {
            return true;
        }
    }

    // Check Azure Application Insights telemetry
    if host.ends_with(".applicationinsights.azure.com") {
        return true;
    }

    // Check telemetry path patterns (segment-boundary matching to avoid
    // false positives like "/order-tracking" matching "/tracking").
    // A match requires the noise pattern to appear as a full path segment:
    // preceded by nothing (starts_with) and followed by '/', '?', or end-of-string.
    for noise_path in NOISE_PATHS {
        if let Some(pos) = path.find(noise_path) {
            let after = pos + noise_path.len();
            if after >= path.len()
                || path.as_bytes()[after] == b'/'
                || path.as_bytes()[after] == b'?'
            {
                return true;
            }
        }
    }

    // play.google.com/log is telemetry (but play.google.com has other endpoints)
    if host == "play.google.com" && path == "/log" {
        return true;
    }

    // Check user-configured blacklist (after built-in rules, but whitelist already checked)
    if !blacklist.is_empty() {
        for pattern in blacklist {
            if host_path.contains(pattern.as_str()) {
                return true;
            }
        }
    }

    false
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::registry::CapabilityRegistry;
    use crate::magician_v2::api_mining::types::{RequestInitiator, RequestTiming};
    use tempfile::TempDir;

    fn make_trace(method: &str, url: &str, status: u16) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: uuid::Uuid::new_v4().to_string(),
            method: method.to_string(),
            url: url.to_string(),
            resource_type: Some("XHR".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::new(),
            request_body: None,
            response_headers: {
                let mut h = HashMap::new();
                h.insert("content-type".to_string(), "application/json".to_string());
                h
            },
            response_body: Some(r#"{"data":"test"}"#.to_string()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 100.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: None,
            },
            timestamp: chrono::Utc::now().timestamp_millis(),
            request_size: 0,
            response_size: 15,
            capture_source: None,
        }
    }

    fn make_graphql_trace_with_query(op_name: &str, query: &str) -> NetworkTraceEvent {
        let mut trace = make_trace("POST", "https://api.example.com/graphql", 200);
        // GraphQL detection requires JSON Content-Type in request headers (M8)
        trace
            .request_headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        trace.request_body = Some(
            serde_json::json!({
                "operationName": op_name,
                "query": query,
                "variables": {}
            })
            .to_string(),
        );
        trace
    }

    fn make_graphql_trace(op_name: &str) -> NetworkTraceEvent {
        make_graphql_trace_with_query(op_name, "query { user { id name } }")
    }

    #[test]
    fn test_normalize_numeric_id() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        assert_eq!(
            miner.normalize_path("https://example.com/api/users/12345"),
            "/api/users/{id}"
        );
    }

    #[test]
    fn test_normalize_uuid() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        assert_eq!(
            miner.normalize_path("https://example.com/items/a1b2c3d4-e5f6-7890-abcd-ef1234567890"),
            "/items/{uuid}"
        );
    }

    #[test]
    fn test_normalize_timestamp() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        assert_eq!(
            miner.normalize_path("https://example.com/events/1707456000"),
            "/events/{ts}"
        );
    }

    #[test]
    fn test_normalize_preserves_stable_segments() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        assert_eq!(
            miner.normalize_path("https://example.com/api/v1/sync"),
            "/api/v1/sync"
        );
    }

    #[test]
    fn test_cluster_basic() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_trace("GET", "https://example.com/api/users/1", 200),
            make_trace("GET", "https://example.com/api/users/2", 200),
            make_trace("GET", "https://example.com/api/users/3", 200),
            make_trace("POST", "https://example.com/api/submit", 201), // Different method
        ];

        let clusters = miner.cluster_traces(&traces);

        // Should have one cluster for GET /api/users/{id} (3 samples)
        // POST /api/submit has only 1 sample, below min_samples=2
        assert!(!clusters.is_empty());
        let users_cluster = clusters.iter().find(|c| c.method == "GET").unwrap();
        assert_eq!(users_cluster.count, 3);
        assert!(users_cluster.url_template.contains("{id}"));
    }

    #[test]
    fn test_cluster_graphql_by_operation() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_graphql_trace("GetUser"),
            make_graphql_trace("GetUser"),
            make_graphql_trace("GetUser"),
            make_graphql_trace("ListPosts"),
            make_graphql_trace("ListPosts"),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert_eq!(clusters.len(), 2);

        let get_user = clusters.iter().find(|c| c.signature.contains("GetUser"));
        assert!(get_user.is_some());
        assert_eq!(get_user.unwrap().count, 3);
    }

    #[test]
    fn test_cluster_graphql_anonymous_operations_by_document() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_graphql_trace_with_query("anonymous", "{ viewer { id email } }"),
            make_graphql_trace_with_query("anonymous", "{ viewer { id email } }"),
            make_graphql_trace_with_query("anonymous", "{ me { id teams { id } } }"),
            make_graphql_trace_with_query("anonymous", "{ me { id teams { id } } }"),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert_eq!(clusters.len(), 2);
        assert!(clusters
            .iter()
            .all(|cluster| cluster.graphql_operation.as_deref() == Some("anonymous")));
        assert_eq!(
            clusters
                .iter()
                .filter_map(|cluster| cluster.body_fingerprint.clone())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2,
            "anonymous GraphQL documents should not collapse into one capability"
        );
    }

    #[test]
    fn test_cluster_filters_non_http() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 1,
            ..Default::default()
        });

        let mut traces = vec![make_trace("GET", "https://example.com/api/data", 200)];
        // Add a data URI trace that should be filtered
        let data_trace = make_trace("GET", "data:text/plain;base64,abc", 200);
        traces.push(data_trace);

        let clusters = miner.cluster_traces(&traces);
        // Only the https trace should be clustered
        assert_eq!(clusters.len(), 1);
    }

    #[test]
    fn test_cluster_to_capability() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_trace("GET", "https://example.com/api/users/1", 200),
            make_trace("GET", "https://example.com/api/users/2", 200),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert!(!clusters.is_empty());

        let cap = miner.cluster_to_capability(&clusters[0]);
        assert_eq!(cap.method, "GET");
        assert!(cap.origin.contains("example.com"));
        assert_eq!(cap.sample_count, 2);
    }

    #[test]
    fn test_cluster_to_capability_preserves_query_template_when_path_is_stable() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_trace("GET", "https://example.com/api/search?q=rust&page=1", 200),
            make_trace("GET", "https://example.com/api/search?q=swift&page=1", 200),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert!(!clusters.is_empty());

        let cap = miner.cluster_to_capability(&clusters[0]);
        assert_eq!(
            cap.url_template,
            "https://example.com/api/search?page=1&q={q}"
        );
    }

    #[test]
    fn test_cluster_to_capability_parameterizes_stable_json_search_query_body() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let mut trace_a = make_trace(
            "POST",
            "https://search.example.com/1/indexes/items/query",
            200,
        );
        trace_a.request_headers.insert(
            "content-type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        );
        trace_a.request_body = Some(
            serde_json::json!({
                "query": "rust",
                "page": 0,
                "hitsPerPage": 30
            })
            .to_string(),
        );

        let mut trace_b = trace_a.clone();
        trace_b.request_id = uuid::Uuid::new_v4().to_string();

        let clusters = miner.cluster_traces(&[trace_a, trace_b]);
        assert_eq!(clusters.len(), 1);

        let cap = miner.cluster_to_capability(&clusters[0]);
        let body_template = cap.body_template.expect("body template");
        assert!(
            body_template.contains(r#""query":{{string:body_payload_query}}"#),
            "stable search query should be replay-parameterized: {body_template}"
        );
        assert!(
            body_template.contains(r#""page":0"#),
            "unrelated stable literals should remain literal: {body_template}"
        );
    }

    #[test]
    fn test_cluster_to_capability_adds_auth_query_requirements() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_trace(
                "POST",
                "https://search.example.com/1/indexes/Item/query?x-api-key=[REDACTED]&page=1",
                200,
            ),
            make_trace(
                "POST",
                "https://search.example.com/1/indexes/Item/query?x-api-key=[REDACTED]&page=2",
                200,
            ),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert!(!clusters.is_empty());

        let cap = miner.cluster_to_capability(&clusters[0]);
        assert_eq!(
            cap.auth_requirements.query_params,
            vec!["x-api-key".to_string()]
        );
    }

    #[test]
    fn test_cluster_to_capability_adds_auth_like_storage_requirements() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let traces = vec![
            make_trace("GET", "https://example.com/api/users/1", 200),
            make_trace("GET", "https://example.com/api/users/2", 200),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert!(!clusters.is_empty());

        let session = SessionContext {
            local_storage: HashMap::from([
                ("refresh".to_string(), "refresh-token".to_string()),
                ("feature_flag".to_string(), "beta-ui".to_string()),
            ]),
            session_storage: HashMap::from([
                ("csrf".to_string(), "csrf-token".to_string()),
                ("active_tab".to_string(), "settings".to_string()),
            ]),
            ..Default::default()
        };

        let cap = miner.cluster_to_capability_with_session_context(&clusters[0], Some(&session));
        assert_eq!(
            cap.auth_requirements.local_storage_keys,
            vec!["refresh".to_string()]
        );
        assert_eq!(
            cap.auth_requirements.session_storage_keys,
            vec!["csrf".to_string()]
        );
    }

    #[test]
    fn test_cluster_to_capability_preserves_graphql_body_template() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let mut trace_a = make_graphql_trace("GetUser");
        trace_a.request_body = Some(
            serde_json::json!({
                "operationName": "GetUser",
                "query": "query GetUser($id: ID!, $includePosts: Boolean!, $limit: Int!) { user(id: $id) { id name } }",
                "variables": {
                    "id": "user-1",
                    "includePosts": true,
                    "limit": 10
                }
            })
            .to_string(),
        );

        let mut trace_b = make_graphql_trace("GetUser");
        trace_b.request_body = Some(
            serde_json::json!({
                "operationName": "GetUser",
                "query": "query GetUser($id: ID!, $includePosts: Boolean!, $limit: Int!) { user(id: $id) { id name } }",
                "variables": {
                    "id": "user-2",
                    "includePosts": false,
                    "limit": 25
                }
            })
            .to_string(),
        );

        let clusters = miner.cluster_traces(&[trace_a, trace_b]);
        assert_eq!(clusters.len(), 1);

        let cap = miner.cluster_to_capability(&clusters[0]);
        assert_eq!(cap.graphql_operation.as_deref(), Some("GetUser"));
        assert_eq!(
            cap.graphql_operation_kind,
            Some(GraphqlOperationKind::Query)
        );

        let body_template = cap.body_template.expect("graphql body template");
        assert!(body_template.contains("\"operationName\":\"GetUser\""));
        assert!(body_template.contains("{{string:body_payload_variables_id}}"));
        assert!(body_template.contains("{{boolean:body_payload_variables_includeposts}}"));
        assert!(body_template.contains("{{number:body_payload_variables_limit}}"));
    }

    #[test]
    fn test_uuid_detection() {
        assert!(ApiMiner::is_uuid_with_hyphens(
            "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
        ));
        assert!(!ApiMiner::is_uuid_with_hyphens("not-a-uuid"));
        assert!(!ApiMiner::is_uuid_with_hyphens("12345"));
    }

    #[test]
    fn test_graphql_detection() {
        let miner = ApiMiner::new(ClusteringConfig::default());

        let trace = make_graphql_trace("GetUser");
        let info = miner
            .detect_graphql_request_info(&trace, None)
            .expect("graphql request info");
        assert_eq!(info.operation_name, "GetUser");
        assert_eq!(info.operation_kind, GraphqlOperationKind::Query);

        let plain_trace = make_trace("GET", "https://example.com/api", 200);
        assert_eq!(miner.detect_graphql_request_info(&plain_trace, None), None);
    }

    #[test]
    fn test_graphql_detection_for_mutation_and_implicit_query() {
        let miner = ApiMiner::new(ClusteringConfig::default());

        let mutation_trace = make_graphql_trace_with_query(
            "ArchiveEmail",
            "mutation ArchiveEmail($id: ID!) { archiveEmail(id: $id) { ok } }",
        );
        let mutation_info = miner
            .detect_graphql_request_info(&mutation_trace, None)
            .expect("mutation request info");
        assert_eq!(mutation_info.operation_name, "ArchiveEmail");
        assert_eq!(mutation_info.operation_kind, GraphqlOperationKind::Mutation);

        let implicit_query_trace =
            make_graphql_trace_with_query("anonymous", "{ viewer { id email } }");
        let implicit_query_info = miner
            .detect_graphql_request_info(&implicit_query_trace, None)
            .expect("implicit query request info");
        assert_eq!(
            implicit_query_info.operation_kind,
            GraphqlOperationKind::Query
        );
    }

    #[test]
    fn test_graphql_detection_for_persisted_query_without_document_is_unknown() {
        let info = detect_graphql_request_info_standalone(
            Some(
                &serde_json::json!({
                    "operationName": "GetUser",
                    "extensions": {
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": "abc123"
                        }
                    },
                    "variables": {
                        "id": "user-42"
                    }
                })
                .to_string(),
            ),
            Some("application/json"),
        )
        .expect("persisted query info");

        assert_eq!(info.operation_name, "GetUser");
        assert_eq!(info.operation_kind, GraphqlOperationKind::Unknown);
    }

    #[test]
    fn test_graphql_detection_recovers_apq_query_kind_from_batch_knowledge() {
        let full_trace = make_graphql_trace_with_query(
            "GetUser",
            "query GetUser($id: ID!) { user(id: $id) { id } }",
        );
        let mut known_trace = full_trace.clone();
        known_trace.request_body = Some(
            serde_json::json!({
                "operationName": "GetUser",
                "query": "query GetUser($id: ID!) { user(id: $id) { id } }",
                "variables": {"id": "user-1"},
                "extensions": {"persistedQuery": {"version": 1, "sha256Hash": "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"}}
            })
            .to_string(),
        );
        let knowledge = GraphqlKnowledge::default().with_traces(&[(0usize, &known_trace)]);

        let info = detect_graphql_request_info_with_knowledge_standalone(
            Some(
                &serde_json::json!({
                    "operationName": "GetUser",
                    "extensions": {
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"
                        }
                    },
                    "variables": {
                        "id": "user-42"
                    }
                })
                .to_string(),
            ),
            Some("application/json"),
            Some("https://api.example.com"),
            Some(&knowledge),
        )
        .expect("persisted query info");

        assert_eq!(info.operation_name, "GetUser");
        assert_eq!(info.operation_kind, GraphqlOperationKind::Query);
    }

    #[test]
    fn test_graphql_knowledge_from_registry_seeds_persisted_query_hash() {
        let temp = TempDir::new().unwrap();
        let mut registry = CapabilityRegistry::with_base_path(temp.path()).unwrap();

        let mut capability = ApiCapability::new(
            "graphql_get_user".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/graphql".to_string(),
        );
        capability.graphql_operation = Some("persisted_abcdefabcdef".to_string());
        capability.graphql_operation_kind = Some(GraphqlOperationKind::Query);
        capability.graphql_persisted_query_sha256 =
            Some("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string());
        registry.register(&capability).unwrap();

        let knowledge = GraphqlKnowledge::from_registry(&registry);
        let info = detect_graphql_request_info_with_knowledge_standalone(
            Some(
                &serde_json::json!({
                    "extensions": {
                        "persistedQuery": {
                            "version": 1,
                            "sha256Hash": "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"
                        }
                    },
                    "variables": {"id": "user-42"}
                })
                .to_string(),
            ),
            Some("application/json"),
            Some("https://api.example.com"),
            Some(&knowledge),
        )
        .expect("persisted query info");

        assert_eq!(info.operation_kind, GraphqlOperationKind::Query);
        assert_eq!(
            info.persisted_query_sha256.as_deref(),
            Some("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd")
        );
    }

    #[test]
    fn test_min_samples_filtering() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 5,
            ..Default::default()
        });

        // Only 3 samples — below min_samples=5
        let traces = vec![
            make_trace("GET", "https://example.com/api/data", 200),
            make_trace("GET", "https://example.com/api/data", 200),
            make_trace("GET", "https://example.com/api/data", 200),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert!(clusters.is_empty());
    }

    // ── Body-aware capability splitting tests ──

    fn make_post_trace_with_json_body(url: &str, body: &str) -> NetworkTraceEvent {
        let mut trace = make_trace("POST", url, 200);
        trace
            .request_headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        trace.request_body = Some(body.to_string());
        trace
    }

    fn make_post_trace_with_form_body(url: &str, body: &str) -> NetworkTraceEvent {
        let mut trace = make_trace("POST", url, 200);
        trace.request_headers.insert(
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        );
        trace.request_body = Some(body.to_string());
        trace
    }

    #[test]
    fn test_body_fingerprint_json_object() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        let trace = make_post_trace_with_json_body(
            "https://example.com/api/sync",
            r#"{"action":"mark_read","ids":["1","2"],"labels":[]}"#,
        );
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(fp, Some("json_keys:action,ids,labels".to_string()));
    }

    #[test]
    fn test_body_fingerprint_form_encoded() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        let trace = make_post_trace_with_form_body(
            "https://example.com/api/submit",
            "username=alice&password=secret&remember=true",
        );
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(fp, Some("form_keys:password,remember,username".to_string()));
    }

    #[test]
    fn test_body_fingerprint_json_body_with_form_content_type() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        let trace = make_post_trace_with_form_body(
            "https://search.example.com/1/indexes/items/query",
            r#"{"query":"rust","page":0,"hitsPerPage":30}"#,
        );
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(
            fp,
            Some("json_keys:hitsPerPage,page,query".to_string()),
            "JSON-looking bodies should not be fingerprinted as one literal form key"
        );
    }

    #[test]
    fn test_body_fingerprint_none_for_get() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        // GET requests should never produce a body fingerprint (even if body present)
        let mut trace = make_trace("GET", "https://example.com/api/data", 200);
        trace.request_body = Some(r#"{"key":"value"}"#.to_string());
        trace
            .request_headers
            .insert("Content-Type".to_string(), "application/json".to_string());
        // compute_body_fingerprint itself doesn't check method — but compute_signature
        // only calls it for POST/PUT/PATCH, so the fingerprint won't appear in the signature.
        // We verify this via the signature instead:
        let sig = miner.compute_signature(&trace, &GraphqlKnowledge::default());
        assert!(
            !sig.contains("json_keys"),
            "GET signature should not contain body fingerprint"
        );
    }

    #[test]
    fn test_body_fingerprint_none_for_binary() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        let mut trace = make_trace("POST", "https://example.com/upload", 200);
        trace.request_headers.insert(
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        );
        trace.request_body = Some("binary data here".to_string());
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(fp, None, "Binary bodies should not produce a fingerprint");
    }

    #[test]
    fn test_body_fingerprint_json_in_text_plain() {
        // Many real-world APIs send JSON as text/plain (Google, etc.)
        // Fingerprinting must work regardless of content-type.
        let miner = ApiMiner::new(ClusteringConfig::default());
        let mut trace = make_trace("POST", "https://www.google.com/log", 200);
        trace.request_headers.insert(
            "Content-Type".to_string(),
            "text/plain;charset=UTF-8".to_string(),
        );
        trace.request_body = Some(r#"{"event":"click","target":"btn","ts":1234}"#.to_string());
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(
            fp,
            Some("json_keys:event,target,ts".to_string()),
            "JSON body in text/plain should still be fingerprinted"
        );
    }

    #[test]
    fn test_body_fingerprint_json_no_content_type() {
        // Some APIs send JSON with no content-type header at all
        let miner = ApiMiner::new(ClusteringConfig::default());
        let mut trace = make_trace("POST", "https://example.com/api/rpc", 200);
        // No Content-Type header set
        trace.request_body = Some(r#"{"method":"getUser","params":{"id":1}}"#.to_string());
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(
            fp,
            Some("json_keys:method,params".to_string()),
            "JSON body with no content-type should still be fingerprinted"
        );
    }

    #[test]
    fn test_body_aware_clustering() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let url = "https://mail.google.com/sync/u/0/i/s";

        // 2 traces for "mark as read" (keys: action, ids)
        let mut traces = vec![
            make_post_trace_with_json_body(url, r#"{"action":"mark_read","ids":["1"]}"#),
            make_post_trace_with_json_body(url, r#"{"action":"mark_read","ids":["2"]}"#),
        ];

        // 2 traces for "archive" (keys: action, ids, labels)
        traces.push(make_post_trace_with_json_body(
            url,
            r#"{"action":"archive","ids":["3"],"labels":["inbox"]}"#,
        ));
        traces.push(make_post_trace_with_json_body(
            url,
            r#"{"action":"archive","ids":["4"],"labels":["inbox"]}"#,
        ));

        let clusters = miner.cluster_traces(&traces);
        assert_eq!(
            clusters.len(),
            2,
            "Different body keys should produce separate clusters"
        );

        let fps: Vec<Option<String>> = clusters
            .iter()
            .map(|c| c.body_fingerprint.clone())
            .collect();
        assert!(fps.contains(&Some("json_keys:action,ids".to_string())));
        assert!(fps.contains(&Some("json_keys:action,ids,labels".to_string())));
    }

    #[test]
    fn test_body_aware_clustering_same_keys_merge() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let url = "https://mail.google.com/sync/u/0/i/s";

        // 3 traces all with same keys → should merge into ONE cluster
        let traces = vec![
            make_post_trace_with_json_body(url, r#"{"action":"mark_read","ids":["1"]}"#),
            make_post_trace_with_json_body(url, r#"{"action":"mark_unread","ids":["2"]}"#),
            make_post_trace_with_json_body(url, r#"{"action":"star","ids":["3"]}"#),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert_eq!(
            clusters.len(),
            1,
            "Same body keys should merge into one cluster"
        );
        assert_eq!(
            clusters[0].body_fingerprint,
            Some("json_keys:action,ids".to_string())
        );
    }

    #[test]
    fn test_graphql_body_fingerprint_uses_document_identity() {
        let miner = ApiMiner::new(ClusteringConfig::default());
        let trace = make_graphql_trace("GetUser");
        let fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(
            fp,
            Some(format!(
                "graphql_doc:{}",
                blake3::hash("query{user{idname}}".as_bytes()).to_hex()
            )),
            "GraphQL traces should fingerprint the normalized document"
        );
    }

    #[test]
    fn test_cluster_to_capability_preserves_fingerprint() {
        let miner = ApiMiner::new(ClusteringConfig {
            min_samples: 2,
            ..Default::default()
        });

        let url = "https://example.com/api/sync";
        let traces = vec![
            make_post_trace_with_json_body(url, r#"{"action":"read","ids":["1"]}"#),
            make_post_trace_with_json_body(url, r#"{"action":"read","ids":["2"]}"#),
        ];

        let clusters = miner.cluster_traces(&traces);
        assert!(!clusters.is_empty());

        let cap = miner.cluster_to_capability(&clusters[0]);
        assert_eq!(
            cap.body_fingerprint,
            Some("json_keys:action,ids".to_string())
        );
        // Name should include the fingerprint hint
        assert!(
            cap.name.contains("action_ids"),
            "Name should include body fingerprint hint: {}",
            cap.name
        );
    }

    // ── Standalone fingerprint tests ──

    #[test]
    fn test_standalone_fingerprint_json() {
        let body = r#"{"action":"mark_read","ids":["1","2"]}"#;
        let fp = compute_body_fingerprint_standalone(Some(body), Some("application/json"), false);
        // Must match the miner method version
        let miner = ApiMiner::new(ClusteringConfig::default());
        let trace = make_post_trace_with_json_body("https://example.com/api/sync", body);
        let miner_fp = miner.compute_body_fingerprint(&trace);
        assert_eq!(fp, miner_fp);
        assert_eq!(fp, Some("json_keys:action,ids".to_string()));
    }

    #[test]
    fn test_standalone_fingerprint_form() {
        let fp = compute_body_fingerprint_standalone(
            Some("username=alice&password=secret"),
            Some("application/x-www-form-urlencoded"),
            false,
        );
        assert_eq!(fp, Some("form_keys:password,username".to_string()));
    }

    #[test]
    fn test_standalone_fingerprint_graphql_uses_document_identity() {
        let fp = compute_body_fingerprint_standalone(
            Some(r#"{"query":"{ user { id } }","operationName":"GetUser"}"#),
            Some("application/json"),
            true, // is_graphql
        );
        assert_eq!(
            fp,
            Some(format!(
                "graphql_doc:{}",
                blake3::hash("{user{id}}".as_bytes()).to_hex()
            ))
        );
    }

    // ─── Noise URL Filtering Tests ────────────────────────────

    #[test]
    fn test_noise_url_telemetry_hosts() {
        assert!(is_noise_url("https://h.clarity.ms/collect", &[], &[]));
        assert!(is_noise_url(
            "https://data.pendo.io/data/ptm.gif/abc",
            &[],
            &[]
        ));
        assert!(is_noise_url(
            "https://edge.beacon.li/api/v1/copilot/x/sessions",
            &[],
            &[]
        ));
        assert!(is_noise_url(
            "https://fundingchoicesmessages.google.com/el/test",
            &[],
            &[]
        ));
    }

    #[test]
    fn test_noise_url_expanded_trackers() {
        // Facebook/Meta
        assert!(is_noise_url(
            "https://connect.facebook.net/en_US/fbevents.js",
            &[],
            &[]
        ));
        assert!(is_noise_url("https://pixel.facebook.com/tr", &[], &[]));
        // Analytics services
        assert!(is_noise_url(
            "https://cdn.segment.com/analytics.js/v1/abc/analytics.min.js",
            &[],
            &[]
        ));
        assert!(is_noise_url("https://api.segment.io/v1/t", &[], &[]));
        assert!(is_noise_url(
            "https://cdn.mxpnl.com/libs/mixpanel.js",
            &[],
            &[]
        ));
        assert!(is_noise_url("https://api-js.mixpanel.com/track", &[], &[]));
        assert!(is_noise_url(
            "https://rs.fullstory.com/rec/bundle",
            &[],
            &[]
        ));
        assert!(is_noise_url(
            "https://static.hotjar.com/c/hotjar.js",
            &[],
            &[]
        ));
        assert!(is_noise_url(
            "https://www.google-analytics.com/analytics.js",
            &[],
            &[]
        ));
        assert!(is_noise_url(
            "https://www.googletagmanager.com/gtag/js",
            &[],
            &[]
        ));
        // Error tracking
        assert!(is_noise_url(
            "https://browser.sentry-cdn.com/7.0.0/bundle.min.js",
            &[],
            &[]
        ));
        // Ad tech
        assert!(is_noise_url(
            "https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js",
            &[],
            &[]
        ));
    }

    #[test]
    fn test_noise_url_static_cdn() {
        assert!(is_noise_url(
            "https://www.gstatic.com/images/favicon.ico",
            &[],
            &[]
        ));
        assert!(is_noise_url(
            "https://ssl.gstatic.com/docs/spreadsheets/forms/favicon.png",
            &[],
            &[]
        ));
    }

    #[test]
    fn test_noise_url_azure_telemetry() {
        assert!(is_noise_url(
            "https://centralindia-0.in.applicationinsights.azure.com/v2/track",
            &[],
            &[]
        ));
    }

    #[test]
    fn test_noise_url_play_google_log() {
        assert!(is_noise_url("https://play.google.com/log", &[], &[]));
        // Other play.google.com paths should NOT be noise
        assert!(!is_noise_url(
            "https://play.google.com/store/apps",
            &[],
            &[]
        ));
    }

    #[test]
    fn test_noise_url_allows_functional_apis() {
        // Gmail APIs should NOT be filtered
        assert!(!is_noise_url(
            "https://mail.google.com/mail/u/0/?view=av&permmsgid=123",
            &[],
            &[]
        ));
        assert!(!is_noise_url(
            "https://mail.google.com/sync/u/0/i/fd",
            &[],
            &[]
        ));
        // Google Forms should NOT be filtered
        assert!(!is_noise_url(
            "https://docs.google.com/forms/d/e/abc/viewform",
            &[],
            &[]
        ));
        // Drive APIs should NOT be filtered
        assert!(!is_noise_url(
            "https://clients6.google.com/batch/drive/v2internal",
            &[],
            &[]
        ));
        assert!(!is_noise_url(
            "https://content.googleapis.com/drive/v2beta/apps",
            &[],
            &[]
        ));
        // Keka HR API should NOT be filtered
        assert!(!is_noise_url(
            "https://acme.keka.com/k/dashboard/api/me/expenses/policy",
            &[],
            &[]
        ));
    }

    #[test]
    fn test_noise_url_path_patterns() {
        assert!(is_noise_url("https://example.com/api/beacon", &[], &[]));
        assert!(is_noise_url("https://example.com/v1/collect", &[], &[]));
        assert!(is_noise_url("https://example.com/pixel", &[], &[]));
        assert!(is_noise_url("https://example.com/tracking", &[], &[]));
        assert!(is_noise_url("https://example.com/telemetry", &[], &[]));
        // Noise paths followed by sub-paths should still match
        assert!(is_noise_url(
            "https://example.com/tracking/events",
            &[],
            &[]
        ));
        assert!(is_noise_url("https://example.com/collect/v1", &[], &[]));
        // Should not match partial path segments (segment-boundary check)
        assert!(!is_noise_url("https://example.com/api/users", &[], &[]));
        assert!(!is_noise_url(
            "https://api.shop.com/order-tracking/status",
            &[],
            &[]
        ));
        assert!(!is_noise_url(
            "https://api.shop.com/shipment-tracking",
            &[],
            &[]
        ));
        assert!(!is_noise_url(
            "https://example.com/lightbeacon/status",
            &[],
            &[]
        ));
        assert!(!is_noise_url(
            "https://example.com/api/pixel-perfect/render",
            &[],
            &[]
        ));
        // /log should match as a telemetry path
        assert!(is_noise_url("https://analytics.example.com/log", &[], &[]));
        assert!(is_noise_url(
            "https://analytics.example.com/log/events",
            &[],
            &[]
        ));
        // But not as a substring of another path
        assert!(!is_noise_url("https://example.com/blog", &[], &[]));
        assert!(!is_noise_url("https://example.com/catalog", &[], &[]));
    }

    #[test]
    fn test_noise_url_whitelist_overrides_noise_host() {
        // h.clarity.ms is a noise host, but whitelist can override
        let whitelist = vec!["h.clarity.ms/api/v2".to_string()];
        assert!(!is_noise_url(
            "https://h.clarity.ms/api/v2/data",
            &whitelist,
            &[]
        ));
        // Other paths on same host still blocked
        assert!(is_noise_url(
            "https://h.clarity.ms/collect",
            &whitelist,
            &[]
        ));
    }

    #[test]
    fn test_noise_url_whitelist_overrides_noise_path() {
        // /collect is a noise path, but domain+path whitelist overrides it
        let whitelist = vec!["myapp.com/collect".to_string()];
        assert!(!is_noise_url(
            "https://myapp.com/collect/orders",
            &whitelist,
            &[]
        ));
        // Other domains with /collect still blocked
        assert!(is_noise_url(
            "https://other.com/v1/collect",
            &whitelist,
            &[]
        ));
    }

    #[test]
    fn test_noise_url_whitelist_overrides_cdn_suffix() {
        // .gstatic.com is a CDN suffix, but whitelist overrides specific paths
        let whitelist = vec!["fonts.gstatic.com/api".to_string()];
        assert!(!is_noise_url(
            "https://fonts.gstatic.com/api/v1/fonts",
            &whitelist,
            &[]
        ));
        // Other gstatic paths still blocked
        assert!(is_noise_url(
            "https://www.gstatic.com/images/favicon.ico",
            &whitelist,
            &[]
        ));
    }

    #[test]
    fn test_noise_url_empty_whitelist_no_effect() {
        // Empty whitelist doesn't change behavior
        assert!(is_noise_url("https://h.clarity.ms/collect", &[], &[]));
        assert!(!is_noise_url("https://example.com/api/users", &[], &[]));
    }

    #[test]
    fn test_noise_url_blacklist_blocks_custom_domains() {
        let blacklist = vec!["internal-analytics.corp.com".to_string()];
        assert!(is_noise_url(
            "https://internal-analytics.corp.com/v1/events",
            &[],
            &blacklist
        ));
        // Other domains not in blacklist are unaffected
        assert!(!is_noise_url("https://api.corp.com/users", &[], &blacklist));
    }

    #[test]
    fn test_noise_url_blacklist_blocks_domain_path_combo() {
        let blacklist = vec!["myapp.com/internal/metrics".to_string()];
        // Matching path blocked
        assert!(is_noise_url(
            "https://myapp.com/internal/metrics/cpu",
            &[],
            &blacklist
        ));
        // Non-matching path on same domain NOT blocked
        assert!(!is_noise_url(
            "https://myapp.com/api/users",
            &[],
            &blacklist
        ));
    }

    #[test]
    fn test_noise_url_whitelist_beats_blacklist() {
        // Same domain+path in both whitelist and blacklist — whitelist wins
        let whitelist = vec!["myapp.com/collect".to_string()];
        let blacklist = vec!["myapp.com/collect".to_string()];
        assert!(!is_noise_url(
            "https://myapp.com/collect/data",
            &whitelist,
            &blacklist
        ));
    }
}
