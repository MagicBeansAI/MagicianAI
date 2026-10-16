//! Replay engine for validated API capabilities
//!
//! Executes API calls directly (bypassing the browser UI) using learned
//! capabilities. The replay pipeline:
//!
//! 1. Load capability from registry
//! 2. Guard: tiered confidence (as of v0.6.514 every replayable side-
//!    effect class — read-only AND Write — engages at Candidate+; see
//!    `capability::min_replay_confidence_for` for the policy)
//! 3. Assemble session context (cookies, auth headers)
//! 4. Execute via extension `fetch()` (primary) — cookie/TLS parity
//! 5. Fallback to Rust `reqwest` if extension is unavailable
//! 6. Verify response (status 2xx + schema match)
//! 7. Update capability promotion/demotion
//!
//! Safety floors (v0.6.514): URL denylist
//! (`auth_replay_validation.url_denylist_substrings`), per-origin opt-in
//! (`OriginPolicyStore::allow_replay_for_origin`), and request-shape match (URL
//! template + body fingerprint + headers) together replace the old Trusted-tier
//! protection on DELETE.

use std::{collections::HashMap, net::SocketAddr, path::Path};

use super::{
    body_template::render_body_template_with_strings,
    capability::ApiCapability,
    registry::CapabilityRegistry,
    types::{ReplayResult, SessionContext, VerificationResult},
};

/// Maximum response body size stored in replay results (256 KB)
const MAX_RESPONSE_BODY_BYTES: usize = 256 * 1024;

/// Default replay timeout (10 seconds)
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Find the largest byte position ≤ `max_bytes` on a UTF-8 char boundary.
fn safe_truncate_pos(s: &str, max_bytes: usize) -> usize {
    let pos = s.len().min(max_bytes);
    (0..=pos)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0)
}

fn truncate_response_body_for_storage(body: &str) -> String {
    if body.len() <= MAX_RESPONSE_BODY_BYTES {
        return body.to_string();
    }
    let pos = safe_truncate_pos(body, MAX_RESPONSE_BODY_BYTES);
    body[..pos].to_string()
}

fn reqwest_method(method: &str) -> Result<reqwest::Method, String> {
    match method.to_uppercase().as_str() {
        "GET" => Ok(reqwest::Method::GET),
        "HEAD" => Ok(reqwest::Method::HEAD),
        "POST" => Ok(reqwest::Method::POST),
        "PUT" => Ok(reqwest::Method::PUT),
        "PATCH" => Ok(reqwest::Method::PATCH),
        "DELETE" => Ok(reqwest::Method::DELETE),
        "OPTIONS" => Ok(reqwest::Method::OPTIONS),
        other => Err(format!("Unsupported method: {}", other)),
    }
}

fn method_allows_body(method: &str) -> bool {
    !matches!(method.to_uppercase().as_str(), "GET" | "HEAD")
}

fn build_reqwest_request(
    client: &reqwest::Client,
    method: &str,
    url: &str,
) -> Result<reqwest::RequestBuilder, String> {
    Ok(client.request(reqwest_method(method)?, url))
}

// ─────────────────────────── Schema Verification ───────────────────────────

/// Verify a replay response against a capability's expected schema.
///
/// Checks:
/// 1. Status code is 2xx
/// 2. Response body JSON structure matches expected keys/types (if schema present)
///    - Array lengths may differ by up to 2x
///    - Nested objects must have the same key set
pub fn verify_response(
    status: u16,
    body: Option<&str>,
    capability: &ApiCapability,
) -> VerificationResult {
    let status_match = (200..300).contains(&status);

    // If no schema to compare against, pass on status alone
    let response_schema = match &capability.response_schema {
        Some(schema) => schema,
        None => {
            return VerificationResult {
                passed: status_match,
                status_match,
                schema_match: true, // No schema to violate
                detail: if status_match {
                    format!("Status {} OK (no schema to verify)", status)
                } else {
                    format!("Status {} is not 2xx (no schema to verify)", status)
                },
            };
        },
    };

    // Parse response body as JSON
    let body_json = match body {
        Some(b) => match serde_json::from_str::<serde_json::Value>(b) {
            Ok(v) => v,
            Err(e) => {
                return VerificationResult {
                    passed: false,
                    status_match,
                    schema_match: false,
                    detail: format!("Response body is not valid JSON: {}", e),
                };
            },
        },
        None => {
            return VerificationResult {
                passed: status_match,
                status_match,
                schema_match: true, // No body to verify
                detail: if status_match {
                    format!("Status {} OK (no response body)", status)
                } else {
                    format!("Status {} is not 2xx (no response body)", status)
                },
            };
        },
    };

    // Compare structure. Older capabilities may store an example response
    // body, while browser JSON promotion stores a JSON Schema sketch. Support
    // both shapes so replay validation does not demote valid schema-backed
    // capabilities by comparing schema metadata as literal response fields.
    let schema_match = if looks_like_json_schema(response_schema) {
        json_matches_schema(response_schema, &body_json)
    } else {
        json_structure_matches(response_schema, &body_json)
    };
    let passed = status_match && schema_match;

    VerificationResult {
        passed,
        status_match,
        schema_match,
        detail: if passed {
            format!("Status {} OK, schema matches", status)
        } else if !status_match {
            format!("Status {} is not 2xx", status)
        } else {
            "Schema structure mismatch".to_string()
        },
    }
}

/// Recursively compare JSON structure (keys + value types).
///
/// Rules:
/// - Objects: all expected keys must exist, value types must match
/// - Arrays: both must be arrays, length may differ by up to 2x,
///   element type is checked against first element of expected
/// - Primitives: type must match (string/number/bool/null)
fn json_structure_matches(expected: &serde_json::Value, actual: &serde_json::Value) -> bool {
    use serde_json::Value;

    match (expected, actual) {
        (Value::Object(exp_map), Value::Object(act_map)) => {
            // All expected keys must exist in actual with matching types
            for (key, exp_val) in exp_map {
                match act_map.get(key) {
                    Some(act_val) => {
                        if !json_structure_matches(exp_val, act_val) {
                            return false;
                        }
                    },
                    None => return false,
                }
            }
            true
        },
        (Value::Array(exp_arr), Value::Array(act_arr)) => {
            // Both are arrays — check length tolerance
            if !exp_arr.is_empty() {
                let exp_len = exp_arr.len();
                let act_len = act_arr.len();

                // Allow up to 2x length difference
                if act_len > exp_len * 2 || (exp_len > 2 && act_len == 0) {
                    return false;
                }

                // Compare first element structure (if both non-empty)
                if let (Some(exp_first), Some(act_first)) = (exp_arr.first(), act_arr.first()) {
                    return json_structure_matches(exp_first, act_first);
                }
            }
            true
        },
        // Type must match for primitives
        (Value::String(_), Value::String(_)) => true,
        (Value::Number(_), Value::Number(_)) => true,
        (Value::Bool(_), Value::Bool(_)) => true,
        (Value::Null, Value::Null) => true,
        // Allow null in actual for any expected type (optional fields)
        (_, Value::Null) => true,
        _ => false,
    }
}

fn looks_like_json_schema(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .map(|object| object.contains_key("type") || object.contains_key("properties"))
        .unwrap_or(false)
}

fn json_matches_schema(schema: &serde_json::Value, actual: &serde_json::Value) -> bool {
    use serde_json::Value;

    let Some(schema_object) = schema.as_object() else {
        return true;
    };
    if actual.is_null() {
        return true;
    }

    let schema_type = schema_object.get("type");
    if let Some(type_value) = schema_type {
        if !json_value_matches_schema_type(type_value, actual) {
            return false;
        }
    }

    if let Some(properties) = schema_object.get("properties").and_then(Value::as_object) {
        let Some(actual_object) = actual.as_object() else {
            return false;
        };

        let mut required_keys = std::collections::HashSet::new();
        if let Some(required) = schema_object.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                required_keys.insert(key);
            }
        }

        for key in &required_keys {
            if !actual_object.contains_key(*key) {
                return false;
            }
        }

        let mut matched_known_property = properties.is_empty();
        for (key, property_schema) in properties {
            let Some(actual_value) = actual_object.get(key) else {
                continue;
            };
            matched_known_property = true;
            if !json_matches_schema(property_schema, actual_value) {
                return false;
            }
        }
        if !matched_known_property {
            return false;
        }
    }

    if let Some(item_schema) = schema_object.get("items") {
        let Some(actual_array) = actual.as_array() else {
            return false;
        };
        if let Some(first) = actual_array.first() {
            return json_matches_schema(item_schema, first);
        }
    }

    true
}

fn json_value_matches_schema_type(
    schema_type: &serde_json::Value,
    actual: &serde_json::Value,
) -> bool {
    let matches_one = |type_name: &str| match type_name {
        "object" => actual.is_object(),
        "array" => actual.is_array(),
        "string" => actual.is_string(),
        "number" => actual.is_number(),
        "integer" => actual.as_i64().is_some() || actual.as_u64().is_some(),
        "boolean" => actual.is_boolean(),
        // Browser JSON promotion builds schema sketches from one sample. A
        // sampled null means "this field can be absent/nullable/unknown", not
        // that future replay bodies must always carry null there.
        "null" => true,
        _ => true,
    };

    match schema_type {
        serde_json::Value::String(type_name) => matches_one(type_name),
        serde_json::Value::Array(types) => types
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(matches_one),
        _ => true,
    }
}

fn response_body_matches_browser(
    api_body: Option<&str>,
    browser_body: Option<&str>,
) -> (bool, String) {
    let Some(browser_body) = browser_body else {
        return (true, "Browser response body unavailable".to_string());
    };
    let Some(api_body) = api_body else {
        return (false, "Replay response body missing".to_string());
    };

    let parsed_api = serde_json::from_str::<serde_json::Value>(api_body);
    let parsed_browser = serde_json::from_str::<serde_json::Value>(browser_body);

    match (parsed_api, parsed_browser) {
        (Ok(api_json), Ok(browser_json)) => {
            if api_json == browser_json {
                (
                    true,
                    "Replay JSON exactly matched browser response".to_string(),
                )
            } else if !json_structure_matches(&browser_json, &api_json)
                || !json_structure_matches(&api_json, &browser_json)
            {
                (
                    false,
                    "Replay JSON structure differed from browser-observed response".to_string(),
                )
            } else {
                let api_fingerprints = collect_stable_json_fingerprints(&api_json);
                let browser_fingerprints = collect_stable_json_fingerprints(&browser_json);

                if api_fingerprints.is_empty() || browser_fingerprints.is_empty() {
                    (
                        true,
                        "Replay JSON structure matched browser response (no stable leaf values to compare)"
                            .to_string(),
                    )
                } else {
                    let overlap = multiset_overlap(&api_fingerprints, &browser_fingerprints);
                    let total = api_fingerprints.len().max(browser_fingerprints.len());
                    let ratio = overlap as f64 / total as f64;
                    if ratio >= 0.5 {
                        (
                            true,
                            format!(
                                "Replay JSON was structurally compatible with browser response (stable overlap {:.0}%)",
                                ratio * 100.0
                            ),
                        )
                    } else {
                        (
                            false,
                            format!(
                                "Replay JSON stable values diverged from browser response (overlap {:.0}%)",
                                ratio * 100.0
                            ),
                        )
                    }
                }
            }
        },
        _ => {
            let normalized_api = normalize_text_body(api_body);
            let normalized_browser = normalize_text_body(browser_body);
            if normalized_api == normalized_browser
                || normalized_api.contains(&normalized_browser)
                || normalized_browser.contains(&normalized_api)
            {
                (
                    true,
                    "Replay body exactly matched browser response".to_string(),
                )
            } else {
                (
                    false,
                    "Replay body differed from browser-observed response".to_string(),
                )
            }
        },
    }
}

fn normalize_text_body(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_stable_json_fingerprints(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut path = Vec::new();
    collect_json_fingerprints_recursive(value, &mut path, &mut out);
    out
}

fn collect_json_fingerprints_recursive(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    out: &mut Vec<String>,
) {
    use serde_json::Value;

    match value {
        Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                collect_json_fingerprints_recursive(child, path, out);
                path.pop();
            }
        },
        Value::Array(values) => {
            path.push("[]".to_string());
            for child in values {
                collect_json_fingerprints_recursive(child, path, out);
            }
            path.pop();
        },
        Value::String(raw) => {
            if !path.iter().any(|token| is_volatile_json_token(token)) {
                out.push(format!("{}={}", normalize_json_path(path), raw));
            }
        },
        Value::Number(raw) => {
            if !path.iter().any(|token| is_volatile_json_token(token)) {
                out.push(format!("{}={}", normalize_json_path(path), raw));
            }
        },
        Value::Bool(raw) => {
            if !path.iter().any(|token| is_volatile_json_token(token)) {
                out.push(format!("{}={}", normalize_json_path(path), raw));
            }
        },
        Value::Null => {},
    }
}

fn normalize_json_path(path: &[String]) -> String {
    path.join(".")
}

fn is_volatile_json_token(token: &str) -> bool {
    let normalized = token.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "timestamp"
            | "time"
            | "request_id"
            | "requestid"
            | "trace_id"
            | "traceid"
            | "nonce"
            | "cursor"
            | "updated_at"
            | "updatedat"
            | "created_at"
            | "createdat"
            | "expires_at"
            | "expiresat"
            | "server_time"
            | "servertime"
            | "generated_at"
            | "generatedat"
    )
}

fn multiset_overlap(left: &[String], right: &[String]) -> usize {
    let mut counts = HashMap::new();
    for value in left {
        *counts.entry(value.as_str()).or_insert(0usize) += 1;
    }

    let mut overlap = 0usize;
    for value in right {
        if let Some(count) = counts.get_mut(value.as_str()) {
            if *count > 0 {
                *count -= 1;
                overlap += 1;
            }
        }
    }
    overlap
}

// ─────────────────────────── Replay Builder ────────────────────────────────

fn build_replay_request_base(
    capability: &ApiCapability,
    params: &HashMap<String, String>,
    session: &SessionContext,
) -> Result<ReplayRequest, String> {
    let url = apply_session_auth_query_params(
        &resolve_url_template(&capability.url_template, params)?,
        &capability.auth_requirements.query_params,
        &session.auth_query_params,
    );

    // Build headers: start with capability template, overlay session auth
    let mut headers = HashMap::new();

    // Apply capability header template
    for (key, value) in &capability.headers_template {
        let resolved = resolve_header_template(value, params)?;
        headers.insert(key.clone(), resolved);
    }

    // Apply session auth headers
    for (key, value) in &session.auth_headers {
        headers.insert(key.clone(), value.clone());
    }

    // Build cookie header from session cookies
    if let Some(cookie_str) = session.cookie_header_string() {
        headers.insert("cookie".to_string(), cookie_str);
    }

    Ok(ReplayRequest {
        method: capability.method.clone(),
        url,
        headers,
        timeout_ms: DEFAULT_TIMEOUT_MS,
        body: None,
    })
}

/// Builds a complete HTTP request from a capability and parameters.
///
/// Resolves template parameters in the URL, headers, and optional request body template.
pub fn build_replay_request(
    capability: &ApiCapability,
    params: &HashMap<String, String>,
    session: &SessionContext,
) -> Result<ReplayRequest, String> {
    let mut request = build_replay_request_base(capability, params, session)?;
    if let Some(template) = &capability.body_template {
        request.body = Some(render_body_template_with_strings(template, params)?);
    }
    Ok(request)
}

/// Build a replay request skeleton without resolving body templates.
///
/// This is used by the passive validation path, which overwrites `request.body`
/// with the browser-observed request body after routing.
pub fn build_replay_request_without_body(
    capability: &ApiCapability,
    params: &HashMap<String, String>,
    session: &SessionContext,
) -> Result<ReplayRequest, String> {
    build_replay_request_base(capability, params, session)
}

pub fn resolve_url_template(
    template: &str,
    params: &HashMap<String, String>,
) -> Result<String, String> {
    let (path_template, query_template) = match template.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (template, None),
    };

    let resolved_path = path_template
        .split('/')
        .map(|segment| match placeholder_name(segment) {
            Some(name) => params
                .get(name)
                .map(|value| {
                    // Path segments need %20 for spaces, not + (which is literal in paths).
                    // form_urlencoded uses + for spaces per application/x-www-form-urlencoded,
                    // so we post-process to fix the encoding for path context.
                    let encoded: String =
                        url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
                    encoded.replace('+', "%20")
                })
                .ok_or_else(|| format!("Missing URL parameter '{}'", name)),
            None => Ok(segment.to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("/");

    let resolved_query = query_template
        .map(|query| {
            query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (key, value) = pair
                        .split_once('=')
                        .map(|(key, value)| (key, Some(value)))
                        .unwrap_or((pair, None));
                    let resolved_value = match value.and_then(placeholder_name) {
                        Some(name) => params
                            .get(name)
                            .map(|value| {
                                url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
                            })
                            .ok_or_else(|| format!("Missing URL parameter '{}'", name))?,
                        None => value.unwrap_or_default().to_string(),
                    };
                    Ok(format!("{}={}", key, resolved_value))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .transpose()?;

    let resolved = match resolved_query {
        Some(query) if !query.is_empty() => format!("{}?{}", resolved_path, query.join("&")),
        _ => resolved_path,
    };

    if contains_unresolved_placeholders(&resolved) {
        return Err(format!(
            "Replay URL still contains unresolved template placeholders: {}",
            resolved
        ));
    }

    Ok(resolved)
}

fn resolve_header_template(
    template: &str,
    params: &HashMap<String, String>,
) -> Result<String, String> {
    let mut resolved = template.to_string();
    for (key, value) in params {
        resolved = resolved.replace(&format!("{{{}}}", key), value);
    }

    if contains_unresolved_placeholders(&resolved) {
        return Err(format!(
            "Replay header still contains unresolved template placeholders: {}",
            resolved
        ));
    }

    Ok(resolved)
}

fn apply_session_auth_query_params(
    url: &str,
    required_params: &[String],
    session_params: &HashMap<String, String>,
) -> String {
    if session_params.is_empty() {
        return url.to_string();
    }

    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };

    let required = required_params
        .iter()
        .map(|param| param.to_ascii_lowercase())
        .collect::<std::collections::HashSet<_>>();
    let mut pairs = parsed
        .query_pairs()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    let mut changed = false;

    for (key, value) in &mut pairs {
        let lower = key.to_ascii_lowercase();
        let needs_secret =
            value == "[REDACTED]" || value.contains("[REDACTED]") || required.contains(&lower);
        if !needs_secret {
            continue;
        }
        if let Some(secret) = lookup_case_insensitive(session_params, key) {
            *value = secret.to_string();
            changed = true;
        }
    }

    for required_name in required_params {
        if pairs
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case(required_name))
        {
            continue;
        }
        if let Some(secret) = lookup_case_insensitive(session_params, required_name) {
            pairs.push((required_name.clone(), secret.to_string()));
            changed = true;
        }
    }

    if !changed {
        return url.to_string();
    }

    parsed.query_pairs_mut().clear().extend_pairs(
        pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    parsed.to_string()
}

fn lookup_case_insensitive<'a>(values: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    values.get(key).map(String::as_str).or_else(|| {
        values
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
    })
}

fn contains_unresolved_placeholders(value: &str) -> bool {
    value.split('{').skip(1).any(|segment| {
        segment
            .split('}')
            .next()
            .is_some_and(|token| !token.is_empty())
    })
}

fn placeholder_name(segment: &str) -> Option<&str> {
    segment
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .filter(|value| !value.is_empty())
}

/// A fully resolved HTTP request ready for execution
#[derive(Debug, Clone)]
pub struct ReplayRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub timeout_ms: u64,
    /// Optional request body for POST/PUT/PATCH/DELETE replays.
    pub body: Option<String>,
}

/// Result of validating an XHR/Fetch trace against a learned capability.
#[derive(Debug, Clone)]
pub struct XhrValidationResult {
    /// Capability ID that was validated.
    pub capability_id: String,
    /// Whether the API response matched the browser's response.
    pub matched: bool,
    /// HTTP status from the API replay.
    pub api_status: u16,
    /// HTTP status from the browser's original request.
    pub browser_status: u16,
    /// Whether the response JSON structure matched.
    pub schema_match: bool,
    /// Wall-clock latency of the validation call (milliseconds).
    pub timing_ms: u64,
    /// Human-readable detail about the comparison result.
    pub detail: String,
}

// ─────────────────────────── ApiRunner ──────────────────────────────────────

/// Runner that executes API replays with verification and promotion.
///
/// This is the main entry point for the replay pipeline. In production,
/// the extension executor path is preferred for cookie/TLS session parity.
/// The Rust reqwest fallback is used when:
/// - Extension executor is unavailable (headless mode)
/// - Extension replay fails with a network error
///
/// Usage:
/// ```ignore
/// let mut runner = ApiRunner::with_base_path("/tmp/test")?;
/// let result = runner.replay_with_reqwest(&origin, &capability_id, &params, &session, Some(url)).await?;
/// ```
pub struct ApiRunner {
    registry: CapabilityRegistry,
}

#[derive(Debug, Clone)]
pub struct ReplayDnsPin {
    pub host: String,
    pub addresses: Vec<SocketAddr>,
}

impl ApiRunner {
    /// Create a new runner with the default data path
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            registry: CapabilityRegistry::new()?,
        })
    }

    /// Create a runner with a custom base path (for testing)
    pub fn with_base_path<P: AsRef<Path>>(base: P) -> Result<Self, String> {
        Ok(Self {
            registry: CapabilityRegistry::with_base_path(base)?,
        })
    }

    /// Check if a capability is safe and eligible for replay
    pub fn can_replay(&self, origin: &str, capability_id: &str) -> Result<bool, String> {
        let capability = self.registry.get_capability(origin, capability_id)?;
        Ok(capability.is_replayable())
    }

    /// Build a replay request for a capability (without executing it)
    ///
    /// Returns None if the capability is not eligible for replay.
    pub fn prepare_replay(
        &self,
        origin: &str,
        capability_id: &str,
        params: &HashMap<String, String>,
        session: &SessionContext,
    ) -> Result<Option<(ReplayRequest, ApiCapability)>, String> {
        let capability = self.registry.get_capability(origin, capability_id)?;

        if !capability.is_replayable() {
            return Ok(None);
        }

        let request = build_replay_request(&capability, params, session)?;
        Ok(Some((request, capability)))
    }

    /// Execute a replay using Rust reqwest (fallback path)
    ///
    /// This bypasses the browser entirely. Use extension replay for
    /// cookie/TLS session parity when possible.
    ///
    /// `concrete_url` — the actual URL being navigated (e.g. with real IDs/values).
    /// When provided, template params are auto-extracted by matching the concrete URL
    /// against the capability's url_template. This allows callers to pass an empty
    /// `params` map and still get correct `{placeholder}` resolution.
    pub async fn replay_with_reqwest(
        &mut self,
        origin: &str,
        capability_id: &str,
        params: &HashMap<String, String>,
        session: &SessionContext,
        concrete_url: Option<&str>,
        // Dispatch attempt this replay belongs to. A workflow replay runs
        // several steps, so callers pass a PER-STEP key — each step is its own
        // effect. A derived value is what actually reaches the remote.
        effect_id: Option<&str>,
    ) -> Result<ReplayResult, String> {
        self.replay_with_reqwest_internal(
            origin,
            capability_id,
            params,
            session,
            concrete_url,
            None,
            effect_id,
        )
        .await
    }

    /// Replay a caller-validated public request against pinned DNS answers.
    /// The rebuilt request must retain the exact host and bypasses proxies.
    pub async fn replay_with_reqwest_pinned(
        &mut self,
        origin: &str,
        capability_id: &str,
        params: &HashMap<String, String>,
        session: &SessionContext,
        concrete_url: Option<&str>,
        dns_pin: &ReplayDnsPin,
        // Dispatch attempt this replay belongs to. A workflow replay runs
        // several steps, so callers pass a PER-STEP key — each step is its own
        // effect. A derived value is what actually reaches the remote.
        effect_id: Option<&str>,
    ) -> Result<ReplayResult, String> {
        self.replay_with_reqwest_internal(
            origin,
            capability_id,
            params,
            session,
            concrete_url,
            Some(dns_pin),
            effect_id,
        )
        .await
    }

    async fn replay_with_reqwest_internal(
        &mut self,
        origin: &str,
        capability_id: &str,
        params: &HashMap<String, String>,
        session: &SessionContext,
        concrete_url: Option<&str>,
        dns_pin: Option<&ReplayDnsPin>,
        effect_id: Option<&str>,
    ) -> Result<ReplayResult, String> {
        let start = std::time::Instant::now();

        // Load and validate capability
        let mut capability = self.registry.get_capability(origin, capability_id)?;

        // Check auth_requirements against session — warn on missing credentials
        // so we can diagnose 401s without a wasted network round-trip.
        if !capability.auth_requirements.cookies.is_empty() {
            let missing: Vec<&str> = capability
                .auth_requirements
                .cookies
                .iter()
                .filter(|c| !session.has_cookie_name(c.as_str()))
                .map(|c| c.as_str())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "[API_MINING] Replay for {} missing required cookies: {:?} (have: {:?})",
                    capability_id,
                    missing,
                    session.available_cookie_names(),
                );
            }
        }
        if !capability.auth_requirements.headers.is_empty() {
            let missing: Vec<&str> = capability
                .auth_requirements
                .headers
                .iter()
                .filter(|h| !session.auth_headers.contains_key(h.as_str()))
                .map(|h| h.as_str())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "[API_MINING] Replay for {} missing required headers: {:?}",
                    capability_id,
                    missing,
                );
            }
        }
        if !capability.auth_requirements.query_params.is_empty() {
            let missing: Vec<&str> = capability
                .auth_requirements
                .query_params
                .iter()
                .filter(|key| !session.has_auth_query_param(key.as_str()))
                .map(|key| key.as_str())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "[API_MINING] Replay for {} missing required auth query params: {:?} (have: {:?})",
                    capability_id,
                    missing,
                    session.available_auth_query_param_names(),
                );
            }
        }
        if !capability.auth_requirements.local_storage_keys.is_empty() {
            let missing: Vec<&str> = capability
                .auth_requirements
                .local_storage_keys
                .iter()
                .filter(|key| !session.has_local_storage_key(key.as_str()))
                .map(|key| key.as_str())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "[API_MINING] Replay for {} missing required localStorage keys: {:?} (have: {:?})",
                    capability_id,
                    missing,
                    session.available_local_storage_keys(),
                );
            }
        }
        if !capability.auth_requirements.session_storage_keys.is_empty() {
            let missing: Vec<&str> = capability
                .auth_requirements
                .session_storage_keys
                .iter()
                .filter(|key| !session.has_session_storage_key(key.as_str()))
                .map(|key| key.as_str())
                .collect();
            if !missing.is_empty() {
                tracing::warn!(
                    "[API_MINING] Replay for {} missing required sessionStorage keys: {:?} (have: {:?})",
                    capability_id,
                    missing,
                    session.available_session_storage_keys(),
                );
            }
        }

        if !capability.is_replayable() {
            return Ok(ReplayResult {
                success: false,
                capability_id: capability_id.to_string(),
                replay_method: "rust_reqwest".to_string(),
                status: 0,
                response_headers: None,
                response_body: None,
                timing_ms: start.elapsed().as_millis() as u64,
                verification: None,
                error: Some(format!(
                    "Capability not replayable: side_effects={:?}, confidence={:?}",
                    capability.effective_side_effects(),
                    capability.confidence
                )),
                fallback_reason: None,
                auth_failure: false,
            });
        }

        // Auto-extract template params from the concrete URL so callers don't need
        // to pre-resolve {placeholders}. Caller-supplied params override auto-extracted.
        let merged_params = if let Some(url) = concrete_url {
            let mut auto_params = crate::magician_v2::api_mining::router::extract_template_params(
                &capability.url_template,
                url,
            );
            for (k, v) in params {
                auto_params.insert(k.clone(), v.clone());
            }
            auto_params
        } else {
            params.clone()
        };

        let request = match build_replay_request(&capability, &merged_params, session) {
            Ok(mut request) => {
                // Only on a method that can commit a change. A lost response
                // to a replayed GET costs nothing to re-issue, so a key buys
                // the caller nothing there — and this path reproduces a
                // RECORDED browser request, where a header no browser ever sent
                // changes the fingerprint that bot detection reads and makes
                // the request non-simple for CORS preflight. Adding one to a
                // read would risk the replay failing for a reason unrelated to
                // the request the capability describes.
                //
                // When one IS warranted it replaces any recorded spelling of
                // the header rather than deferring to it. A key that came from
                // `headers_template` was recorded from a real past request, so
                // it is stale by construction: replay it verbatim and a remote
                // honouring the key answers with the ORIGINAL response, leaving
                // the replay to look like it succeeded while having done
                // nothing. Removed by key before insert because HTTP field
                // names are case-insensitive and the map's are not.
                let can_commit = matches!(
                    request.method.to_ascii_uppercase().as_str(),
                    "POST" | "PUT" | "PATCH" | "DELETE"
                );
                if let Some(effect_id) = effect_id.filter(|_| can_commit) {
                    request
                        .headers
                        .retain(|key, _| !key.eq_ignore_ascii_case("idempotency-key"));
                    request.headers.insert(
                        "Idempotency-Key".to_string(),
                        crate::magician_v2::analytics::llm_tool_lineage::
                            LlmToolLineageIdentity::far_side_idempotency_key(effect_id),
                    );
                }
                request
            },
            Err(e) => {
                return Ok(ReplayResult {
                    success: false,
                    capability_id: capability_id.to_string(),
                    replay_method: "rust_reqwest".to_string(),
                    status: 0,
                    response_headers: None,
                    response_body: None,
                    timing_ms: start.elapsed().as_millis() as u64,
                    verification: None,
                    error: Some(format!("Failed to build replay request: {}", e)),
                    fallback_reason: None,
                    auth_failure: false,
                });
            },
        };

        // Execute via reqwest
        let mut client_builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(request.timeout_ms))
            // SECURITY: Disable redirects to prevent SSRF — a compromised endpoint could
            // redirect to a different origin, leaking auth headers/cookies.
            .redirect(reqwest::redirect::Policy::none());
        if let Some(pin) = dns_pin {
            let request_host = url::Url::parse(&request.url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
            if request_host.as_deref() != Some(pin.host.as_str()) || pin.addresses.is_empty() {
                return Err("replay request no longer matches its validated DNS pin".into());
            }
            client_builder = client_builder
                .no_proxy()
                .resolve_to_addrs(&pin.host, &pin.addresses);
        }
        let client = client_builder
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

        let mut req_builder = match build_reqwest_request(&client, &request.method, &request.url) {
            Ok(builder) => builder,
            Err(e) => {
                return Ok(ReplayResult {
                    success: false,
                    capability_id: capability_id.to_string(),
                    replay_method: "rust_reqwest".to_string(),
                    status: 0,
                    response_headers: None,
                    response_body: None,
                    timing_ms: start.elapsed().as_millis() as u64,
                    verification: None,
                    error: Some(format!("Unsupported method for replay: {}", e)),
                    fallback_reason: None,
                    auth_failure: false,
                });
            },
        };

        if request.body.is_some() && !method_allows_body(&request.method) {
            return Ok(ReplayResult {
                success: false,
                capability_id: capability_id.to_string(),
                replay_method: "rust_reqwest".to_string(),
                status: 0,
                response_headers: None,
                response_body: None,
                timing_ms: start.elapsed().as_millis() as u64,
                verification: None,
                error: Some(format!(
                    "Request body not allowed for {} replay",
                    request.method.to_uppercase()
                )),
                fallback_reason: None,
                auth_failure: false,
            });
        }

        // Add body for methods that permit one.
        if let Some(ref body) = request.body {
            req_builder = req_builder.body(body.clone());
        }

        // Add headers
        for (key, value) in &request.headers {
            req_builder = req_builder.header(key, value);
        }

        // Execute
        let response = match req_builder.send().await {
            Ok(resp) => resp,
            Err(e) => {
                capability.record_replay_failure();
                self.registry.register(&capability)?;

                return Ok(ReplayResult {
                    success: false,
                    capability_id: capability_id.to_string(),
                    replay_method: "rust_reqwest".to_string(),
                    status: 0,
                    response_headers: None,
                    response_body: None,
                    timing_ms: start.elapsed().as_millis() as u64,
                    verification: None,
                    error: Some(format!("HTTP request failed: {}", e)),
                    fallback_reason: None,
                    auth_failure: false,
                });
            },
        };

        let status = response.status().as_u16();

        // Collect response headers
        let resp_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();

        // SECURITY: Check Content-Length before reading to prevent OOM from malicious servers.
        // Reject responses that advertise more than 4× the truncation limit.
        const MAX_DOWNLOAD_BYTES: u64 = (MAX_RESPONSE_BODY_BYTES as u64) * 4;
        if let Some(cl) = response.content_length() {
            if cl > MAX_DOWNLOAD_BYTES {
                return Ok(ReplayResult {
                    success: false,
                    capability_id: capability_id.to_string(),
                    replay_method: "rust_reqwest".to_string(),
                    status,
                    response_headers: Some(resp_headers),
                    response_body: None,
                    timing_ms: start.elapsed().as_millis() as u64,
                    verification: None,
                    error: Some(format!(
                        "Response body too large: {} bytes (max: {})",
                        cl, MAX_DOWNLOAD_BYTES
                    )),
                    fallback_reason: Some("Response too large for replay".to_string()),
                    auth_failure: false,
                });
            }
        }

        // Read body under the MAX_DOWNLOAD_BYTES cap. Verification must use
        // the full bounded body; truncating first can make large JSON invalid
        // and incorrectly demote otherwise healthy API capabilities.
        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?;

        let full_body = String::from_utf8_lossy(&body_bytes).to_string();
        let body = Some(truncate_response_body_for_storage(&full_body));

        // Verify response
        let verification = verify_response(status, Some(full_body.as_str()), &capability);

        // Update promotion/demotion — auth failures (401/403) are tracked separately
        // and do NOT demote the capability (the endpoint works, creds are stale).
        if is_auth_failure(status) {
            capability.record_auth_failure();
        } else if verification.passed {
            capability.record_replay_success();
        } else {
            capability.record_replay_failure();
        }
        self.registry.register(&capability)?;

        let timing_ms = start.elapsed().as_millis() as u64;

        Ok(ReplayResult {
            success: verification.passed,
            capability_id: capability_id.to_string(),
            replay_method: "rust_reqwest".to_string(),
            status,
            response_headers: Some(resp_headers),
            response_body: body,
            timing_ms,
            verification: Some(verification),
            error: None,
            fallback_reason: None,
            auth_failure: is_auth_failure(status),
        })
    }

    /// Process the result of an extension-side replay.
    ///
    /// Called after the extension executor returns the ApiReplay result.
    /// Handles verification and promotion/demotion updates.
    pub fn process_extension_replay_result(
        &mut self,
        origin: &str,
        capability_id: &str,
        status: u16,
        body: Option<&str>,
        headers: Option<HashMap<String, String>>,
        timing_ms: u64,
        error: Option<&str>,
    ) -> Result<ReplayResult, String> {
        let mut capability = self.registry.get_capability(origin, capability_id)?;

        // If the extension returned an error, record failure
        if let Some(err_msg) = error {
            capability.record_replay_failure();
            self.registry.register(&capability)?;

            return Ok(ReplayResult {
                success: false,
                capability_id: capability_id.to_string(),
                replay_method: "extension_fetch".to_string(),
                status,
                response_headers: headers,
                response_body: body.map(|b| {
                    if b.len() > MAX_RESPONSE_BODY_BYTES {
                        let pos = safe_truncate_pos(b, MAX_RESPONSE_BODY_BYTES);
                        b[..pos].to_string()
                    } else {
                        b.to_string()
                    }
                }),
                timing_ms,
                verification: None,
                error: Some(err_msg.to_string()),
                fallback_reason: None,
                auth_failure: is_auth_failure(status),
            });
        }

        // Verify the extension's response
        let verification = verify_response(status, body, &capability);

        // Auth failures (401/403) tracked separately — don't demote capability
        if is_auth_failure(status) {
            capability.record_auth_failure();
        } else if verification.passed {
            capability.record_replay_success();
        } else {
            capability.record_replay_failure();
        }
        self.registry.register(&capability)?;

        Ok(ReplayResult {
            success: verification.passed,
            capability_id: capability_id.to_string(),
            replay_method: "extension_fetch".to_string(),
            status,
            response_headers: headers,
            response_body: body.map(|b| {
                if b.len() > MAX_RESPONSE_BODY_BYTES {
                    let pos = safe_truncate_pos(b, MAX_RESPONSE_BODY_BYTES);
                    b[..pos].to_string()
                } else {
                    b.to_string()
                }
            }),
            timing_ms,
            verification: Some(verification),
            error: None,
            fallback_reason: None,
            auth_failure: is_auth_failure(status),
        })
    }

    /// Validate an XHR/Fetch trace by replaying the request and comparing with the
    /// browser's response. Used for passive capability validation — the browser already
    /// made this request, so replaying it has no additional side-effect risk.
    ///
    /// Supports the same HTTP methods as replay (`GET`, `HEAD`, `POST`, `PUT`,
    /// `PATCH`, `DELETE`, `OPTIONS`) subject to the router's safety gate.
    /// On match, promotes the capability; on mismatch, demotes.
    pub async fn validate_with_reqwest(
        &mut self,
        origin: &str,
        capability_id: &str,
        request: &ReplayRequest,
        browser_status: u16,
        browser_body: Option<&str>,
    ) -> Result<XhrValidationResult, String> {
        let start = std::time::Instant::now();

        let mut capability = self.registry.get_capability(origin, capability_id)?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(request.timeout_ms))
            // SECURITY: Disable redirects to prevent SSRF
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

        let mut req_builder = match build_reqwest_request(&client, &request.method, &request.url) {
            Ok(builder) => builder,
            Err(e) => {
                return Ok(XhrValidationResult {
                    capability_id: capability_id.to_string(),
                    matched: false,
                    api_status: 0,
                    browser_status,
                    schema_match: false,
                    timing_ms: start.elapsed().as_millis() as u64,
                    detail: format!("Unsupported method for validation: {}", e),
                });
            },
        };

        if request.body.is_some() && !method_allows_body(&request.method) {
            return Ok(XhrValidationResult {
                capability_id: capability_id.to_string(),
                matched: false,
                api_status: 0,
                browser_status,
                schema_match: false,
                timing_ms: start.elapsed().as_millis() as u64,
                detail: format!(
                    "Request body not allowed for {} validation",
                    request.method.to_uppercase()
                ),
            });
        }

        // Add headers
        for (key, value) in &request.headers {
            req_builder = req_builder.header(key, value);
        }

        // Add body for methods that permit one.
        if let Some(ref body) = request.body {
            req_builder = req_builder.body(body.clone());
        }

        let response = match req_builder.send().await {
            Ok(resp) => resp,
            Err(e) => {
                capability.record_replay_failure();
                let _ = self.registry.register(&capability);
                return Ok(XhrValidationResult {
                    capability_id: capability_id.to_string(),
                    matched: false,
                    api_status: 0,
                    browser_status,
                    schema_match: false,
                    timing_ms: start.elapsed().as_millis() as u64,
                    detail: format!("HTTP request failed: {}", e),
                });
            },
        };

        let api_status = response.status().as_u16();

        // SECURITY: Check Content-Length before reading
        const MAX_DOWNLOAD_BYTES: u64 = (MAX_RESPONSE_BODY_BYTES as u64) * 4;
        if let Some(cl) = response.content_length() {
            if cl > MAX_DOWNLOAD_BYTES {
                return Ok(XhrValidationResult {
                    capability_id: capability_id.to_string(),
                    matched: false,
                    api_status,
                    browser_status,
                    schema_match: false,
                    timing_ms: start.elapsed().as_millis() as u64,
                    detail: format!("Response too large: {} bytes", cl),
                });
            }
        }

        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read response body: {}", e))?;

        let full_api_body = String::from_utf8_lossy(&body_bytes).to_string();

        // Compare: status code match + schema verification
        let status_match = api_status == browser_status
            || ((200..300).contains(&api_status) && (200..300).contains(&browser_status));

        let verification = verify_response(api_status, Some(full_api_body.as_str()), &capability);
        let schema_match = verification.schema_match;

        let (body_match, body_match_detail) =
            response_body_matches_browser(Some(full_api_body.as_str()), browser_body);
        let matched = status_match && schema_match && body_match;

        if matched {
            capability.record_replay_success();
        } else {
            capability.record_replay_failure();
        }
        let _ = self.registry.register(&capability);

        let timing_ms = start.elapsed().as_millis() as u64;

        Ok(XhrValidationResult {
            capability_id: capability_id.to_string(),
            matched,
            api_status,
            browser_status,
            schema_match,
            timing_ms,
            detail: if matched {
                format!(
                    "Validation passed: API={} browser={}",
                    api_status, browser_status
                )
            } else if !status_match {
                format!(
                    "Status mismatch: API={} browser={}",
                    api_status, browser_status
                )
            } else if !body_match {
                body_match_detail
            } else {
                format!("Schema mismatch: {}", verification.detail)
            },
        })
    }

    /// Get the inner registry for inspection
    pub fn registry(&self) -> &CapabilityRegistry {
        &self.registry
    }

    /// Get mutable access to the registry
    pub fn registry_mut(&mut self) -> &mut CapabilityRegistry {
        &mut self.registry
    }
}

/// Whether an HTTP status code indicates an auth failure (not a capability failure).
/// 401/403 responses should trigger vault staleness, not capability demotion.
pub fn is_auth_failure(status: u16) -> bool {
    status == 401 || status == 403
}

// ─────────────────────────── Tests ─────────────────────────────────────────

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::capability::ConfidenceLevel;
    use super::super::types::SessionCookie;
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_capability(
        method: &str,
        url: &str,
        response_schema: Option<serde_json::Value>,
    ) -> ApiCapability {
        let mut cap = ApiCapability::new(
            "test_api".to_string(),
            "https://example.com".to_string(),
            method.to_string(),
            url.to_string(),
        );
        cap.response_schema = response_schema;
        cap
    }

    fn make_replayable_capability() -> ApiCapability {
        let mut cap = make_capability(
            "GET",
            "https://example.com/api/users/{id}",
            Some(json!({
                "id": 1,
                "name": "Alice",
                "email": "alice@example.com"
            })),
        );
        // Promote to Candidate (3 samples)
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        cap
    }

    // ─── Schema Verification Tests ──────────────────────────────

    #[test]
    fn test_verify_status_ok_no_schema() {
        let cap = make_capability("GET", "https://example.com/api/data", None);
        let result = verify_response(200, Some(r#"{"ok": true}"#), &cap);
        assert!(result.passed);
        assert!(result.status_match);
        assert!(result.schema_match);
    }

    #[test]
    fn test_verify_status_fail() {
        let cap = make_capability("GET", "https://example.com/api/data", None);
        let result = verify_response(404, Some(r#"{"error": "not found"}"#), &cap);
        assert!(!result.passed);
        assert!(!result.status_match);
    }

    #[test]
    fn test_verify_schema_match_object() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/user",
            Some(json!({
                "id": 1,
                "name": "Alice",
                "active": true
            })),
        );

        // Response with same keys, different values — should match
        let body = r#"{"id": 42, "name": "Bob", "active": false, "extra": "field"}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
        assert!(result.schema_match);
    }

    #[test]
    fn test_verify_json_schema_sketch_match_object() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/search",
            Some(json!({
                "type": "object",
                "properties": {
                    "hits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "objectID": { "type": "string" },
                                "title": { "type": "string" },
                                "url": { "type": "string" }
                            }
                        }
                    }
                }
            })),
        );

        let body = r#"{"hits":[{"objectID":"22238335","title":"Why Discord is switching from Go to Rust","url":null,"points":1582}],"page":0}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
        assert!(result.schema_match);
    }

    #[test]
    fn test_verify_json_schema_sketch_optional_properties() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/item",
            Some(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "number" },
                    "title": { "type": "string" },
                    "optional_url": { "type": "string" }
                }
            })),
        );

        let body = r#"{"id":42,"title":"Present field still validates"}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
        assert!(result.schema_match);
    }

    #[test]
    fn test_verify_json_schema_sketch_required_properties() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/item",
            Some(json!({
                "type": "object",
                "required": ["id", "title"],
                "properties": {
                    "id": { "type": "number" },
                    "title": { "type": "string" }
                }
            })),
        );

        let body = r#"{"id":42}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(!result.passed);
        assert!(!result.schema_match);
    }

    #[test]
    fn test_verify_json_schema_sketch_sampled_null_is_wildcard() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/item",
            Some(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "number" },
                    "url": { "type": "null" }
                }
            })),
        );

        let body = r#"{"id":42,"url":"https://example.com/article"}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
        assert!(result.schema_match);
    }

    #[test]
    fn test_large_json_body_must_verify_before_storage_truncation() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/large-item",
            Some(json!({
                "type": "object",
                "properties": {
                    "id": { "type": "number" },
                    "payload": { "type": "string" }
                }
            })),
        );
        let body = format!(
            r#"{{"id":42,"payload":"{}"}}"#,
            "x".repeat(MAX_RESPONSE_BODY_BYTES + 1024)
        );

        let full_result = verify_response(200, Some(body.as_str()), &cap);
        assert!(full_result.passed);
        assert!(full_result.schema_match);

        let stored_body = truncate_response_body_for_storage(&body);
        assert!(stored_body.len() <= MAX_RESPONSE_BODY_BYTES);
        let truncated_result = verify_response(200, Some(stored_body.as_str()), &cap);
        assert!(!truncated_result.passed);
    }

    #[test]
    fn test_verify_schema_mismatch_missing_key() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/user",
            Some(json!({
                "id": 1,
                "name": "Alice"
            })),
        );

        // Response missing "name" key
        let body = r#"{"id": 42}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(!result.passed);
        assert!(!result.schema_match);
    }

    #[test]
    fn test_verify_schema_mismatch_wrong_type() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/user",
            Some(json!({
                "id": 1,
                "name": "Alice"
            })),
        );

        // "id" is string instead of number
        let body = r#"{"id": "not-a-number", "name": "Bob"}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(!result.passed);
        assert!(!result.schema_match);
    }

    #[test]
    fn test_verify_schema_array_tolerance() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/items",
            Some(json!([
                {"id": 1, "title": "Item 1"},
                {"id": 2, "title": "Item 2"}
            ])),
        );

        // 3 items (within 2x tolerance of 2)
        let body =
            r#"[{"id": 10, "title": "A"}, {"id": 20, "title": "B"}, {"id": 30, "title": "C"}]"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
        assert!(result.schema_match);
    }

    #[test]
    fn test_response_body_matches_browser_rejects_stable_json_mismatch() {
        let (matched, detail) = response_body_matches_browser(
            Some(r#"{"data":{"user":{"id":"42"}}}"#),
            Some(r#"{"data":{"user":{"id":"42"}}}"#),
        );
        assert!(matched, "{detail}");

        let (matched, _) = response_body_matches_browser(
            Some(r#"{"data":{"user":{"id":"43"}}}"#),
            Some(r#"{"data":{"user":{"id":"42"}}}"#),
        );
        assert!(!matched);
    }

    #[test]
    fn test_response_body_matches_browser_tolerates_volatile_json_and_array_reordering() {
        let (matched, detail) = response_body_matches_browser(
            Some(
                r#"{"data":{"items":[{"id":"2","name":"B"},{"id":"1","name":"A"}],"timestamp":"2026-03-06T19:00:00Z","cursor":"def"}} "#,
            ),
            Some(
                r#"{"data":{"items":[{"id":"1","name":"A"},{"id":"2","name":"B"}],"timestamp":"2026-03-06T18:59:59Z","cursor":"abc"}} "#,
            ),
        );
        assert!(matched, "{detail}");
    }

    #[test]
    fn test_verify_schema_array_exceeds_tolerance() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/items",
            Some(json!([
                {"id": 1}
            ])),
        );

        // 3 items, but expected only 1 — 2x tolerance allows up to 2
        let body = r#"[{"id": 1}, {"id": 2}, {"id": 3}]"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(!result.passed);
        assert!(!result.schema_match);
    }

    #[test]
    fn test_verify_schema_nested_object() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/profile",
            Some(json!({
                "user": {
                    "id": 1,
                    "settings": {
                        "theme": "dark"
                    }
                }
            })),
        );

        let body = r#"{"user": {"id": 99, "settings": {"theme": "light"}}}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
    }

    #[test]
    fn test_verify_null_actual_accepted() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/data",
            Some(json!({
                "name": "test",
                "email": "test@example.com"
            })),
        );

        // null is accepted for any expected type (optional fields)
        let body = r#"{"name": "test", "email": null}"#;
        let result = verify_response(200, Some(body), &cap);
        assert!(result.passed);
    }

    #[test]
    fn test_verify_no_body() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/data",
            Some(json!({"key": "value"})),
        );
        let result = verify_response(200, None, &cap);
        assert!(result.passed);
        assert!(result.status_match);
        assert!(result.schema_match); // No body = no violation
    }

    #[test]
    fn test_verify_invalid_json_body() {
        let cap = make_capability(
            "GET",
            "https://example.com/api/data",
            Some(json!({"key": "value"})),
        );
        let result = verify_response(200, Some("not json"), &cap);
        assert!(!result.passed);
        assert!(!result.schema_match);
    }

    // ─── JSON Structure Matching Tests ──────────────────────────

    #[test]
    fn test_structure_primitive_types() {
        assert!(json_structure_matches(&json!("hello"), &json!("world")));
        assert!(json_structure_matches(&json!(42), &json!(100)));
        assert!(json_structure_matches(&json!(true), &json!(false)));
        assert!(json_structure_matches(&json!(null), &json!(null)));

        assert!(!json_structure_matches(&json!("hello"), &json!(42)));
        assert!(!json_structure_matches(&json!(true), &json!("true")));
    }

    #[test]
    fn test_structure_empty_objects() {
        assert!(json_structure_matches(&json!({}), &json!({})));
        assert!(json_structure_matches(
            &json!({}),
            &json!({"extra": "field"})
        ));
    }

    #[test]
    fn test_structure_empty_arrays() {
        assert!(json_structure_matches(&json!([]), &json!([])));
        assert!(json_structure_matches(&json!([]), &json!([1, 2, 3])));
    }

    // ─── Build Replay Request Tests ─────────────────────────────

    #[test]
    fn test_build_request_url_template() {
        let cap = make_replayable_capability();
        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());

        let session = SessionContext::default();
        let request = build_replay_request(&cap, &params, &session).unwrap();

        assert_eq!(request.method, "GET");
        assert_eq!(request.url, "https://example.com/api/users/42");
    }

    #[test]
    fn test_build_request_url_template_encodes_query_values() {
        let mut cap = make_replayable_capability();
        cap.url_template = "https://example.com/api/search?q={q}".to_string();

        let mut params = HashMap::new();
        params.insert("q".to_string(), "latest report".to_string());

        let request = build_replay_request(&cap, &params, &SessionContext::default()).unwrap();
        assert_eq!(
            request.url,
            "https://example.com/api/search?q=latest+report"
        );
    }

    #[test]
    fn test_build_request_replaces_redacted_auth_query_param_from_session() {
        let mut cap = make_replayable_capability();
        cap.url_template = "https://example.com/api/search?api_key=[REDACTED]&q={q}".to_string();
        cap.auth_requirements.query_params = vec!["api_key".to_string()];

        let params = HashMap::from([("q".to_string(), "rust".to_string())]);
        let mut session = SessionContext::default();
        session
            .auth_query_params
            .insert("api_key".to_string(), "secret-key".to_string());

        let request = build_replay_request(&cap, &params, &session).unwrap();
        assert_eq!(
            request.url,
            "https://example.com/api/search?api_key=secret-key&q=rust"
        );
    }

    #[test]
    fn test_build_request_fails_on_unresolved_url_placeholder() {
        let mut cap = make_replayable_capability();
        cap.url_template = "https://example.com/api/search?q={q}".to_string();

        let error = build_replay_request(&cap, &HashMap::new(), &SessionContext::default())
            .expect_err("missing placeholder should fail");
        assert!(error.contains("Missing URL parameter 'q'"));
    }

    #[test]
    fn test_build_request_with_cookies() {
        let cap = make_replayable_capability();
        let params = HashMap::from([("id".to_string(), "42".to_string())]);
        let mut session = SessionContext::default();
        session
            .cookies
            .insert("SID".to_string(), "abc123".to_string());
        session
            .cookies
            .insert("HSID".to_string(), "def456".to_string());

        let request = build_replay_request(&cap, &params, &session).unwrap();
        let cookie_header = request.headers.get("cookie").unwrap();
        assert_eq!(cookie_header, "HSID=def456; SID=abc123");
    }

    #[test]
    fn test_build_request_preserves_duplicate_cookie_names() {
        let cap = make_replayable_capability();
        let params = HashMap::from([("id".to_string(), "42".to_string())]);
        let session = SessionContext {
            cookie_header_values: vec![
                SessionCookie {
                    name: "session".to_string(),
                    value: "admin-cookie".to_string(),
                },
                SessionCookie {
                    name: "session".to_string(),
                    value: "root-cookie".to_string(),
                },
            ],
            ..SessionContext::default()
        };

        let request = build_replay_request(&cap, &params, &session).unwrap();
        assert_eq!(
            request.headers.get("cookie").map(String::as_str),
            Some("session=admin-cookie; session=root-cookie")
        );
    }

    #[test]
    fn test_build_request_with_auth_headers() {
        let cap = make_replayable_capability();
        let params = HashMap::from([("id".to_string(), "42".to_string())]);
        let mut session = SessionContext::default();
        session
            .auth_headers
            .insert("authorization".to_string(), "Bearer tok123".to_string());

        let request = build_replay_request(&cap, &params, &session).unwrap();
        assert_eq!(
            request.headers.get("authorization").unwrap(),
            "Bearer tok123"
        );
    }

    #[test]
    fn test_build_request_header_templates() {
        let mut cap = make_replayable_capability();
        cap.headers_template
            .insert("x-api-key".to_string(), "{api_key}".to_string());

        let mut params = HashMap::new();
        params.insert("api_key".to_string(), "secret_key".to_string());
        params.insert("id".to_string(), "1".to_string());

        let session = SessionContext::default();
        let request = build_replay_request(&cap, &params, &session).unwrap();

        assert_eq!(request.headers.get("x-api-key").unwrap(), "secret_key");
    }

    #[test]
    fn test_build_request_body_template_for_graphql() {
        let mut cap = make_capability("POST", "https://example.com/graphql", None);
        cap.body_template = Some(
            r#"{"operationName":"GetUser","query":"query GetUser($id: ID!) { user(id: $id) { id name } }","variables":{"id":{{string:body_payload_variables_id}}}}"#
                .to_string(),
        );
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        cap.record_replay_success();
        cap.record_replay_success();
        cap.record_replay_success();

        let mut params = HashMap::new();
        params.insert(
            "body_payload_variables_id".to_string(),
            "user-42".to_string(),
        );

        let request = build_replay_request(&cap, &params, &SessionContext::default()).unwrap();
        assert_eq!(
            request.body.as_deref(),
            Some(
                r#"{"operationName":"GetUser","query":"query GetUser($id: ID!) { user(id: $id) { id name } }","variables":{"id":"user-42"}}"#
            )
        );
    }

    // ─── ApiRunner Tests ────────────────────────────────────────

    #[test]
    fn test_replay_request_body_field() {
        let cap = make_replayable_capability();
        let params = HashMap::from([("id".to_string(), "42".to_string())]);
        let session = SessionContext::default();
        let mut request = build_replay_request(&cap, &params, &session).unwrap();
        assert!(request.body.is_none());

        request.body = Some(r#"{"action":"test"}"#.to_string());
        assert_eq!(request.body.as_deref(), Some(r#"{"action":"test"}"#));

        // Clone round-trip
        let cloned = request.clone();
        assert_eq!(cloned.body, request.body);
    }

    #[test]
    fn test_reqwest_method_supports_options() {
        assert_eq!(reqwest_method("OPTIONS").unwrap(), reqwest::Method::OPTIONS);
    }

    #[test]
    fn test_method_allows_body_rejects_get_and_head_only() {
        assert!(!method_allows_body("GET"));
        assert!(!method_allows_body("HEAD"));
        assert!(method_allows_body("POST"));
        assert!(method_allows_body("DELETE"));
        assert!(method_allows_body("OPTIONS"));
    }

    #[test]
    fn test_runner_can_replay_observed() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_capability("GET", "https://example.com/api/data", None);
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        // Observed capability should NOT be replayable
        assert!(!runner.can_replay("https://example.com", &cap_id).unwrap());
    }

    #[test]
    fn test_runner_can_replay_candidate() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        // Candidate GET capability should be replayable
        assert!(runner.can_replay("https://example.com", &cap_id).unwrap());
    }

    #[test]
    fn test_runner_can_replay_write_at_candidate_v_0_6_514() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let mut cap = make_capability("POST", "https://example.com/api/submit", None);
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        // v0.6.514: POST at Candidate IS replayable
        assert!(runner.can_replay("https://example.com", &cap_id).unwrap());
    }

    #[test]
    fn test_runner_can_replay_validated_post() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let mut cap = make_capability("POST", "https://example.com/api/submit", None);
        // Promote to Candidate
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        // Promote to Validated (3 replays)
        cap.record_replay_success();
        cap.record_replay_success();
        cap.record_replay_success();
        assert_eq!(cap.confidence, ConfidenceLevel::Validated);

        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        // POST at Validated IS replayable
        assert!(runner.can_replay("https://example.com", &cap_id).unwrap());
    }

    #[test]
    fn test_runner_prepare_replay() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());
        let session = SessionContext::default();

        let result = runner
            .prepare_replay("https://example.com", &cap_id, &params, &session)
            .unwrap();
        assert!(result.is_some());

        let (request, _) = result.unwrap();
        assert_eq!(request.url, "https://example.com/api/users/42");
    }

    #[test]
    fn test_runner_prepare_replay_not_eligible() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_capability("GET", "https://example.com/api/data", None);
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        let params = HashMap::new();
        let session = SessionContext::default();

        // Observed capability — prepare_replay returns None
        let result = runner
            .prepare_replay("https://example.com", &cap_id, &params, &session)
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn pinned_replay_rejects_a_rebuilt_request_outside_the_validated_host() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();
        let params = HashMap::from([("id".to_string(), "42".to_string())]);
        let pin = ReplayDnsPin {
            host: "other.example".into(),
            addresses: vec!["93.184.216.34:443".parse().unwrap()],
        };

        let error = runner
            .replay_with_reqwest_pinned(
                "https://example.com",
                &cap_id,
                &params,
                &SessionContext::default(),
                None,
                &pin,
                None,
            )
            .await
            .unwrap_err();
        assert!(error.contains("validated DNS pin"));
    }

    #[test]
    fn test_runner_process_extension_result_success() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        let body = r#"{"id": 42, "name": "Bob", "email": "bob@example.com"}"#;
        let result = runner
            .process_extension_replay_result(
                "https://example.com",
                &cap_id,
                200,
                Some(body),
                None,
                150,
                None,
            )
            .unwrap();

        assert!(result.success);
        assert_eq!(result.replay_method, "extension_fetch");
        assert_eq!(result.status, 200);
        assert!(result.verification.unwrap().passed);

        // Verify promotion: should now have replay_success_count = 1
        let updated = runner
            .registry()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.replay_success_count, 1);
        assert_eq!(updated.consecutive_failures, 0);
    }

    #[test]
    fn test_runner_process_extension_result_failure() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        let result = runner
            .process_extension_replay_result(
                "https://example.com",
                &cap_id,
                500,
                Some(r#"{"error": "internal"}"#),
                None,
                50,
                None,
            )
            .unwrap();

        assert!(!result.success);
        assert!(!result.verification.unwrap().passed);

        // Verify demotion tracking
        let updated = runner
            .registry()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.replay_failure_count, 1);
        assert_eq!(updated.consecutive_failures, 1);
    }

    #[test]
    fn test_runner_process_extension_result_with_error() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        let result = runner
            .process_extension_replay_result(
                "https://example.com",
                &cap_id,
                0,
                None,
                None,
                0,
                Some("Network error: connection refused"),
            )
            .unwrap();

        assert!(!result.success);
        assert_eq!(result.error.unwrap(), "Network error: connection refused");
        assert!(result.verification.is_none()); // No verification on error

        let updated = runner
            .registry()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.replay_failure_count, 1);
    }

    #[test]
    fn test_runner_promotion_through_replays() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        let cap = make_replayable_capability();
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        let body = r#"{"id": 1, "name": "Test", "email": "test@example.com"}"#;

        // 3 successful replays → Candidate → Validated
        for _ in 0..3 {
            runner
                .process_extension_replay_result(
                    "https://example.com",
                    &cap_id,
                    200,
                    Some(body),
                    None,
                    100,
                    None,
                )
                .unwrap();
        }

        let updated = runner
            .registry()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.confidence, ConfidenceLevel::Validated);
        assert_eq!(updated.replay_success_count, 3);

        // 2 more → Validated → Trusted (needs 5 total)
        for _ in 0..2 {
            runner
                .process_extension_replay_result(
                    "https://example.com",
                    &cap_id,
                    200,
                    Some(body),
                    None,
                    100,
                    None,
                )
                .unwrap();
        }

        let updated = runner
            .registry()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.confidence, ConfidenceLevel::Trusted);
    }

    #[test]
    fn test_runner_demotion_on_consecutive_failures() {
        let temp = TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();

        // Create a Trusted capability
        let mut cap = make_replayable_capability();
        for _ in 0..5 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);
        let cap_id = cap.id.clone();
        runner.registry_mut().register(&cap).unwrap();

        // 3 consecutive failures → demote from Trusted to Validated
        for _ in 0..3 {
            runner
                .process_extension_replay_result(
                    "https://example.com",
                    &cap_id,
                    500,
                    Some(r#"{"error": "fail"}"#),
                    None,
                    50,
                    None,
                )
                .unwrap();
        }

        let updated = runner
            .registry()
            .get_capability("https://example.com", &cap_id)
            .unwrap();
        assert_eq!(updated.confidence, ConfidenceLevel::Validated);
    }

    #[test]
    fn auth_failure_classification() {
        assert!(is_auth_failure(401));
        assert!(is_auth_failure(403));
        assert!(!is_auth_failure(200));
        assert!(!is_auth_failure(500));
        assert!(!is_auth_failure(404));
    }
}
