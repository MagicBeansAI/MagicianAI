//! Type definitions for network trace capture

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Network trace event captured during browser automation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkTraceEvent {
    /// Unique request identifier from CDP
    pub request_id: String,

    /// HTTP method (GET, POST, etc.)
    pub method: String,

    /// Full request URL
    pub url: String,

    /// CDP resource type (XHR, Fetch, Document, Other)
    /// Present in JS extension output; defaults to None for backward compat with old traces
    #[serde(default)]
    pub resource_type: Option<String>,

    /// CDP frame ID where request originated (None for main frame)
    /// Used for action-to-network correlation and iframe tracking
    #[serde(default)]
    pub frame_id: Option<String>,

    /// Request headers
    pub request_headers: HashMap<String, String>,

    /// Request body (if present)
    pub request_body: Option<String>,

    /// Chrome tab the request was issued from, and the magician thread the
    /// capture belongs to — the join keys page signals carry as `tabId` /
    /// `threadId`. Without them, matching a trace to the DOM state around it
    /// falls back to task-directory plus timestamp proximity, which is
    /// guesswork; the DOM-effect discriminator needs the join to be exact.
    /// `serde(default)`: absent on traces captured before magicutor emitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<i64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

    /// Response headers
    pub response_headers: HashMap<String, String>,

    /// Response body (if present, max 1MB)
    pub response_body: Option<String>,

    /// Why `response_body` is absent on an otherwise-completed request, as
    /// reported by magicutor's capture (`cdp_error`, `binary_body_skipped`,
    /// `bridge_error`, …). Sequence steps can only feed workflow data-flow
    /// inference when they carry a body, so the empty-body rate is a primary
    /// health metric — this makes it attributable instead of guessed at.
    /// `serde(default)`: absent on traces captured before magicutor emitted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_unavailable_reason: Option<String>,

    /// `Network.loadingFailed` detail for requests recorded with `status = 0`.
    /// Distinguishes an ad-blocked tracker from a navigation-cancelled fetch
    /// from a genuine network error — which decides whether such events are
    /// noise to filter or a fault to fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_error_text: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_blocked_reason: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_canceled: Option<bool>,

    /// HTTP status code
    pub status: u16,

    /// Request timing information
    pub timing: RequestTiming,

    /// Request initiator (script, parser, etc.)
    pub initiator: RequestInitiator,

    /// Timestamp when request was captured
    pub timestamp: i64,

    /// Size of request body in bytes (u64 for cross-platform serialization stability)
    pub request_size: u64,

    /// Size of response body in bytes (u64 for cross-platform serialization stability)
    pub response_size: u64,

    /// Where this trace was captured. Lets the miner/correlator weight or
    /// segment data by transport — `cdp_proxy` carries the richest payload
    /// (request/response bodies, full initiator stack), `headed`/`headless`
    /// will carry whatever agent-browser's own capture path produces. Absent
    /// for older traces written before this field was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_source: Option<String>,
}

/// Transient authentication material drained from Magicutor's CDP capture.
/// This payload is consumed into the encrypted secret store and is never
/// persisted with normal API-mining traces.
#[derive(Debug, Clone, Deserialize)]
pub struct CapturedAuthEvent {
    pub request_id: String,
    pub url: String,
    pub auth_headers: HashMap<String, String>,
    #[serde(default)]
    pub cookie_header: Option<String>,
    pub timestamp: i64,
}

/// Request timing information from CDP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestTiming {
    /// Time when request was sent (milliseconds since epoch)
    pub request_time: f64,

    /// DNS lookup duration (milliseconds)
    pub dns_duration: Option<f64>,

    /// TCP connection duration (milliseconds)
    pub connect_duration: Option<f64>,

    /// TLS handshake duration (milliseconds)
    pub ssl_duration: Option<f64>,

    /// Time to first byte (milliseconds)
    pub ttfb: Option<f64>,

    /// Total request duration (milliseconds)
    pub total_duration: f64,
}

/// Request initiator information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestInitiator {
    /// Type: script, parser, preflight, etc.
    pub initiator_type: String,

    /// Stack trace if initiated by script
    pub stack: Option<Vec<StackFrame>>,

    /// URL of initiator document
    pub url: Option<String>,
}

/// Stack frame in request initiator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub function_name: String,
    pub script_id: String,
    pub url: String,
    pub line_number: u32,
    pub column_number: u32,
}

/// Request for observing with network trace capture enabled.
///
/// NOTE: `base` holds the passthrough observe parameters. We avoid `#[serde(flatten)]`
/// on `serde_json::Value` to prevent silent key collisions (M3). The caller should
/// merge `network_trace` and `max_traces` into the Value before sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveRequest {
    /// Standard observe parameters (opaque passthrough)
    pub base: serde_json::Value,

    /// Enable network trace capture
    #[serde(default)]
    pub network_trace: bool,

    /// Maximum number of traces to return (default: 100)
    #[serde(default = "default_max_traces")]
    pub max_traces: usize,
}

fn default_max_traces() -> usize {
    100
}

/// Response from observe operation with network traces.
///
/// NOTE: `base` holds the standard observe response. We avoid `#[serde(flatten)]`
/// on `serde_json::Value` to prevent silent key collisions (M3). The caller should
/// extract `network_traces` separately from the response JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserveResponse {
    /// Standard observe response data (opaque passthrough)
    pub base: serde_json::Value,

    /// Captured network traces (if network_trace was enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_traces: Option<Vec<NetworkTraceEvent>>,
}

// ───────────────────────── Replay Types ──────────────────────────

/// Result of replaying an API capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayResult {
    /// Whether the replay succeeded end-to-end (including verification).
    pub success: bool,

    /// Capability ID that was replayed.
    pub capability_id: String,

    /// How the replay was executed: `"extension_fetch"` or `"rust_reqwest"`.
    pub replay_method: String,

    /// HTTP status code from the replayed request (0 if never sent).
    pub status: u16,

    /// Response headers (if available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<HashMap<String, String>>,

    /// Response body (truncated to 256 KB, if available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,

    /// Wall-clock latency of the replay call (milliseconds).
    pub timing_ms: u64,

    /// Verification result (if verification was performed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationResult>,

    /// Human-readable error message (if replay failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Reason for falling back to next strategy (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,

    /// Whether the failure was auth-related (401/403). When true, the caller
    /// should mark the vault entry stale rather than demoting the capability.
    #[serde(default)]
    pub auth_failure: bool,
}

/// Result of verifying a replay response against the capability's schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Whether verification passed overall.
    pub passed: bool,

    /// HTTP status was 2xx.
    pub status_match: bool,

    /// Response JSON structure matched the expected schema.
    pub schema_match: bool,

    /// Human-readable detail about what matched or failed.
    pub detail: String,
}

// Trusted session types retain their wire and Debug contracts in the shared core.
pub use magicvault_core::session::{SessionContext, SessionCookie};

// ───────────────────────── Trace Stats ───────────────────────────

/// Statistics for trace capture performance
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceCaptureStats {
    /// Total requests captured
    pub total_requests: usize,

    /// Requests dropped due to size limits
    pub dropped_oversized: usize,

    /// Requests dropped due to buffer full
    pub dropped_buffer_full: usize,

    /// Total bytes captured
    pub total_bytes_captured: usize,

    /// Average capture overhead per request (microseconds)
    pub avg_overhead_us: f64,

    /// Maximum capture overhead observed (microseconds)
    pub max_overhead_us: f64,
}

// ───────────────────────── XHR Validation Metrics ────────────────

/// Metrics for passive XHR/Fetch validation against learned capabilities.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct XhrValidationMetrics {
    /// Total XHR/Fetch traces observed in this session.
    pub total_xhr_observed: usize,
    /// Traces that matched a capability in the registry.
    pub matched_capabilities: usize,
    /// Background validation replays fired.
    pub validations_fired: usize,
    /// Validations where API response matched browser response.
    pub validations_passed: usize,
    /// Validations where API response differed from browser response.
    pub validations_failed: usize,
    /// Counts by HTTP method (e.g., {"GET": 5, "POST": 3}).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub by_method: HashMap<String, usize>,
}

// ───────────────────────── Path Safety ───────────────────────────

/// Sanitize a string for use as a single filesystem path component.
///
/// Rejects path traversal sequences (`..`, `/`, `\`) and restricts to
/// alphanumeric characters, hyphens, underscores, and periods. Additionally
/// verifies the resolved path stays under `base` (defense-in-depth).
pub fn sanitize_path_component(input: &str) -> Result<String, String> {
    if input.is_empty() {
        return Err("Path component must not be empty".to_string());
    }
    if input.contains("..") || input.contains('/') || input.contains('\\') {
        return Err(format!(
            "Invalid path component (traversal detected): {}",
            input.chars().take(40).collect::<String>()
        ));
    }
    // Allow only safe characters: alphanumeric, dash, underscore, period
    let sanitized: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(sanitized)
}

/// Join a path component to a base path, verifying the result stays under base.
pub fn safe_join(base: &std::path::Path, component: &str) -> Result<std::path::PathBuf, String> {
    let sanitized = sanitize_path_component(component)?;
    let joined = base.join(&sanitized);
    // Defense-in-depth: canonicalize would resolve symlinks, but the dirs may not
    // exist yet. Instead verify no `..` in the final path.
    let joined_str = joined.to_string_lossy();
    if joined_str.contains("..") {
        return Err(format!(
            "Path traversal detected after join: {}",
            joined_str
        ));
    }
    Ok(joined)
}

#[cfg(test)]
mod session_debug_tests {
    use super::*;

    const SENTINEL: &str = "PLACEHOLDER-SESSION-SECRET-0123456789";

    #[test]
    fn session_cookie_debug_never_prints_the_value() {
        let cookie = SessionCookie {
            name: "sid".to_string(),
            value: SENTINEL.to_string(),
        };
        let rendered = format!("{cookie:?}");
        assert!(
            !rendered.contains(SENTINEL),
            "SessionCookie Debug leaked: {rendered}"
        );
        assert!(rendered.contains("sid"));
        assert!(rendered.contains(&format!("<{} bytes>", SENTINEL.len())));
    }

    #[test]
    fn session_context_debug_redacts_every_plaintext_map() {
        let context = SessionContext {
            cookie_metadata: Vec::new(),
            cookie_header_values: vec![SessionCookie {
                name: "sid".to_string(),
                value: SENTINEL.to_string(),
            }],
            cookies: HashMap::from([("c".to_string(), SENTINEL.to_string())]),
            auth_headers: HashMap::from([("authorization".to_string(), SENTINEL.to_string())]),
            auth_query_params: HashMap::from([("key".to_string(), SENTINEL.to_string())]),
            local_storage: HashMap::from([("refresh".to_string(), SENTINEL.to_string())]),
            session_storage: HashMap::from([("csrf".to_string(), SENTINEL.to_string())]),
        };
        let rendered = format!("{context:?}");
        assert!(
            !rendered.contains(SENTINEL),
            "SessionContext Debug leaked: {rendered}"
        );
        for name in ["sid", "c", "authorization", "key", "refresh", "csrf"] {
            assert!(
                rendered.contains(name),
                "key {name} should survive redaction"
            );
        }
    }

    #[test]
    fn session_cookie_routing_metadata_is_runtime_only() {
        let cookie = crate::magician_v2::secrets::CookieWithMetadata {
            name: "session".into(),
            value: SENTINEL.into(),
            domain: "example.test".into(),
            path: "/private".into(),
            secure: true,
            http_only: true,
            same_site: crate::magician_v2::secrets::SameSite::Lax,
            expires: None,
        };
        let context = SessionContext {
            cookie_metadata: vec![cookie.clone()],
            ..Default::default()
        };
        let json = serde_json::to_value(&context).unwrap();
        assert!(json.get("cookie_metadata").is_none());
        assert!(!json.to_string().contains(SENTINEL));
        assert!(!format!("{context:?}").contains(SENTINEL));
        let from_wire: SessionContext =
            serde_json::from_value(serde_json::json!({"cookie_metadata":[cookie]})).unwrap();
        assert!(from_wire.cookie_metadata.is_empty());
    }
}
