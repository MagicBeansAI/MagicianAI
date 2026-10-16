//! Action-to-network trace correlation
//!
//! Links browser actions (click, type, navigate) to the network requests
//! they trigger. Uses three independent heuristics:
//!
//! 1. **Timing** — requests within a configurable window after action start
//! 2. **Frame** — requests from the same CDP frame as the action target
//! 3. **Value** — request bodies containing user-provided input values
//!
//! Each heuristic produces a partial confidence score; the final score is
//! the min(sum, 1.0). The correlator is deliberately deterministic—no ML
//! or probabilistic weighting—so results are reproducible.

use crate::magician_v2::api_mining::types::NetworkTraceEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ───────────────────────── Configuration ─────────────────────────

/// Default time window (ms) after action start within which a network
/// request is eligible for correlation.
const DEFAULT_WINDOW_MS: i64 = 2_000;

/// Minimum confidence score for a correlation to be kept.
const MIN_CONFIDENCE: f64 = 0.2;

/// Confidence weight for timing-based correlation.
const TIMING_WEIGHT: f64 = 0.30;

/// Bonus confidence when the request occurs very shortly after the action
/// (<300ms), indicating a direct causal link.
const TIMING_CLOSE_BONUS: f64 = 0.10;

/// Confidence weight for frame-based correlation.
const FRAME_WEIGHT: f64 = 0.30;

/// Confidence weight for initiator chain matching (weaker than frame match).
const INITIATOR_WEIGHT: f64 = 0.20;

/// Confidence weight for value-based correlation.
const VALUE_WEIGHT: f64 = 0.40;

/// Bonus for XHR/Fetch resource types (vs Document or Other).
const XHR_FETCH_BONUS: f64 = 0.05;

// ───────────────────────── Public Types ──────────────────────────

/// A browser/capture action event, as seen by the correlator.
///
/// This is a *minimal* action descriptor — callers should map from whatever
/// execution-side or ambient-capture representation they use into this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionEvent {
    /// Unique identifier for the action (e.g., step index or UUID).
    pub action_id: String,

    /// Kind of action (Click, Type, Navigate, Select, etc.).
    pub action_type: String,

    /// Timestamp when the action started (epoch milliseconds).
    pub timestamp_ms: i64,

    /// CDP frame ID targeted by the action (if known).
    /// Used for frame-based correlation.
    #[serde(default)]
    pub frame_id: Option<String>,

    /// User-supplied input values associated with the action
    /// (e.g., text typed into an input, search query, form value).
    /// Used for value-based correlation.
    #[serde(default)]
    pub user_values: Vec<String>,

    /// URL of the page when the action was performed.
    /// Used as a weak tie-breaker via initiator matching.
    #[serde(default)]
    pub page_url: Option<String>,

    /// Stable action signature used for persisted action-to-API bindings.
    #[serde(default)]
    pub action_signature: Option<String>,

    /// Phase 0 Gap 2 (v0.6.515): semantic signature derived from
    /// stable element attributes (tag, name, data-testid, aria-label,
    /// role, visible-text-truncated). Used as a fallback match key
    /// when the exact CSS selector rotates (React re-renders,
    /// class-name churn, A/B tests).
    ///
    /// Producer: the magicutor browser-extension `observe.js` ships
    /// element metadata in the ActionEvent payload; the executor's
    /// observe handler synthesises this signature from those fields.
    /// Until the extension ships that data, this stays None and
    /// registry matching falls back to exact-signature only.
    #[serde(default)]
    pub semantic_signature: Option<String>,

    /// Raw element-metadata blob from the extension's observe handler.
    /// Used by the executor to compute `semantic_signature`. Stored
    /// for debug visibility; not consumed by the matching path.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub element_metadata: HashMap<String, String>,

    /// Structured action parameters keyed by semantic name (for example `text`,
    /// `field_email`, `value`). Used to map UI inputs back onto capability placeholders.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub action_params: HashMap<String, String>,

    /// Normalized page origin at action time, if known.
    #[serde(default)]
    pub page_origin: Option<String>,

    /// Normalized page path template at action time, if known.
    #[serde(default)]
    pub page_path_template: Option<String>,
}

/// A network request linked to a triggering action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelatedRequest {
    /// The network trace event that was correlated.
    pub trace: NetworkTraceEvent,

    /// ID of the action that triggered this request.
    pub action_id: String,

    /// Confidence score ∈ [0.0, 1.0].
    pub confidence: f64,

    /// Which heuristics contributed to the confidence.
    pub signals: CorrelationSignals,
}

/// Bitfield-like record of which heuristics fired.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CorrelationSignals {
    /// Timing heuristic matched.
    pub timing: bool,
    /// Very close timing (<300 ms) matched.
    pub timing_close: bool,
    /// Frame ID matched action target.
    pub frame_match: bool,
    /// Initiator chain matched action target page.
    pub initiator_match: bool,
    /// Request body contained user-provided value.
    pub value_match: bool,
    /// Resource type is XHR or Fetch.
    pub xhr_or_fetch: bool,
}

/// Configuration knobs for the correlator.
#[derive(Debug, Clone)]
pub struct CorrelatorConfig {
    /// Time window after action start (milliseconds).
    pub window_ms: i64,
    /// Minimum confidence to keep a correlation.
    pub min_confidence: f64,
    /// Maximum correlations to return per action (0 = unlimited).
    pub max_per_action: usize,
}

impl Default for CorrelatorConfig {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_WINDOW_MS,
            min_confidence: MIN_CONFIDENCE,
            max_per_action: 10,
        }
    }
}

// ───────────────────────── Correlator ────────────────────────────

/// Stateless correlator — given actions and traces, produces correlations.
pub struct Correlator {
    config: CorrelatorConfig,
}

impl Correlator {
    pub fn new(config: CorrelatorConfig) -> Self {
        Self { config }
    }

    /// Correlate a single action against a set of network traces.
    ///
    /// Returns correlated requests sorted by confidence (highest first).
    pub fn correlate_action(
        &self,
        action: &ActionEvent,
        traces: &[NetworkTraceEvent],
    ) -> Vec<CorrelatedRequest> {
        let mut results: Vec<CorrelatedRequest> = Vec::new();

        for trace in traces {
            let (confidence, signals) = self.score(action, trace);

            if confidence >= self.config.min_confidence {
                results.push(CorrelatedRequest {
                    trace: trace.clone(),
                    action_id: action.action_id.clone(),
                    confidence,
                    signals,
                });
            }
        }

        // Sort highest confidence first, then by request time (earlier first).
        results.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.trace
                        .timing
                        .request_time
                        .partial_cmp(&b.trace.timing.request_time)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        // Truncate if max_per_action configured.
        if self.config.max_per_action > 0 && results.len() > self.config.max_per_action {
            results.truncate(self.config.max_per_action);
        }

        results
    }

    /// Correlate *all* actions in an execution timeline against traces.
    ///
    /// Actions should be sorted chronologically. Each trace is matched
    /// against the *closest preceding* action whose window it falls into.
    /// A trace is allowed to match multiple actions (e.g., a navigation
    /// triggered by a click that also causes a follow-up XHR).
    pub fn correlate_all(
        &self,
        actions: &[ActionEvent],
        traces: &[NetworkTraceEvent],
    ) -> Vec<CorrelatedRequest> {
        let mut all_results: Vec<CorrelatedRequest> = Vec::new();

        for action in actions {
            let mut action_results = self.correlate_action(action, traces);
            all_results.append(&mut action_results);
        }

        // Deduplicate: if the same trace matches multiple actions, keep
        // only the highest-confidence pairing.
        all_results.sort_by(|a, b| {
            a.trace.request_id.cmp(&b.trace.request_id).then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        all_results.dedup_by(|a, b| {
            if a.trace.request_id == b.trace.request_id {
                // Keep the one with higher confidence (b, since dedup_by
                // keeps the *first* in each group and we sorted desc).
                true
            } else {
                false
            }
        });

        // Final sort by confidence.
        all_results.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        all_results
    }

    // ─────────────── Scoring ───────────────

    /// Compute (confidence, signals) for one (action, trace) pair.
    fn score(&self, action: &ActionEvent, trace: &NetworkTraceEvent) -> (f64, CorrelationSignals) {
        let mut confidence = 0.0_f64;
        let mut signals = CorrelationSignals::default();

        // 1. Timing heuristic
        let delta_ms = trace.timestamp - action.timestamp_ms;
        if delta_ms >= 0 && delta_ms <= self.config.window_ms {
            confidence += TIMING_WEIGHT;
            signals.timing = true;

            // Close timing bonus (<300 ms).
            if delta_ms < 300 {
                confidence += TIMING_CLOSE_BONUS;
                signals.timing_close = true;
            }
        }

        // 2. Frame heuristic
        if let (Some(action_frame), Some(trace_frame)) = (&action.frame_id, &trace.frame_id) {
            if action_frame == trace_frame {
                confidence += FRAME_WEIGHT;
                signals.frame_match = true;
            }
        }

        // 2b. Initiator chain fallback — check if any stack frame URL
        //     matches the action's page URL.
        if !signals.frame_match {
            if let Some(page_url) = &action.page_url {
                if self.initiator_matches_url(&trace.initiator, page_url) {
                    confidence += INITIATOR_WEIGHT;
                    signals.initiator_match = true;
                }
            }
        }

        // 3. Value heuristic — check request body / URL for user values.
        if !action.user_values.is_empty() {
            let haystack = build_haystack(trace);
            for value in &action.user_values {
                if value.len() >= 2 && haystack_contains(&haystack, value) {
                    confidence += VALUE_WEIGHT;
                    signals.value_match = true;
                    break; // one match is enough
                }
            }
        }

        // 4. Resource type bonus.
        if let Some(ref rt) = trace.resource_type {
            let rt_lower = rt.to_ascii_lowercase();
            if rt_lower == "xhr" || rt_lower == "fetch" {
                confidence += XHR_FETCH_BONUS;
                signals.xhr_or_fetch = true;
            }
        }

        // Clamp to [0, 1].
        confidence = confidence.min(1.0);

        (confidence, signals)
    }

    /// Check if any stack frame URL in the initiator chain matches the
    /// given page URL.
    fn initiator_matches_url(
        &self,
        initiator: &crate::magician_v2::api_mining::types::RequestInitiator,
        page_url: &str,
    ) -> bool {
        // Check the top-level initiator URL.
        if let Some(ref init_url) = initiator.url {
            if urls_same_origin(init_url, page_url) {
                return true;
            }
        }

        // Check stack frames.
        if let Some(ref stack) = initiator.stack {
            for frame in stack {
                if urls_same_origin(&frame.url, page_url) {
                    return true;
                }
            }
        }

        false
    }
}

impl Default for Correlator {
    fn default() -> Self {
        Self::new(CorrelatorConfig::default())
    }
}

// ───────────────────────── Helpers ───────────────────────────────

/// Build a case-insensitive searchable haystack from the trace's URL,
/// request body, and query parameters.
fn build_haystack(trace: &NetworkTraceEvent) -> String {
    let mut haystack = String::with_capacity(
        trace.url.len() + trace.request_body.as_ref().map_or(0, |b| b.len()) + 64,
    );

    haystack.push_str(&trace.url);
    haystack.push('\n');

    if let Some(ref body) = trace.request_body {
        haystack.push_str(body);
        haystack.push('\n');
    }

    // Include query parameters decoded for matching
    if let Ok(parsed) = url::Url::parse(&trace.url) {
        for (k, v) in parsed.query_pairs() {
            haystack.push_str(&k);
            haystack.push('=');
            haystack.push_str(&v);
            haystack.push('&');
        }
    }

    haystack.to_ascii_lowercase()
}

/// Case-insensitive substring check against a lowered haystack.
fn haystack_contains(lowered_haystack: &str, needle: &str) -> bool {
    lowered_haystack.contains(&needle.to_ascii_lowercase())
}

/// Compare two URLs and check if they share the same origin
/// (scheme + host + port). Tolerates trailing slashes and minor
/// path differences.
fn urls_same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(ua), Ok(ub)) => ua.origin() == ub.origin(),
        _ => false,
    }
}

// ───────────────────────── Tests ─────────────────────────────────

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::types::{RequestInitiator, RequestTiming, StackFrame};
    use std::collections::HashMap;

    // ---- Helpers ----

    fn base_timing() -> RequestTiming {
        RequestTiming {
            request_time: 1000.0,
            dns_duration: None,
            connect_duration: None,
            ssl_duration: None,
            ttfb: None,
            total_duration: 50.0,
        }
    }

    fn base_initiator() -> RequestInitiator {
        RequestInitiator {
            initiator_type: "script".to_string(),
            stack: None,
            url: None,
        }
    }

    fn make_trace(id: &str, url: &str, method: &str, timestamp_ms: i64) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: id.to_string(),
            method: method.to_string(),
            url: url.to_string(),
            resource_type: Some("XHR".to_string()),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::new(),
            request_body: None,
            response_headers: HashMap::new(),
            response_body: None,
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: base_timing(),
            initiator: base_initiator(),
            timestamp: timestamp_ms,
            request_size: 0,
            response_size: 0,
            capture_source: None,
        }
    }

    fn make_action(id: &str, timestamp_ms: i64) -> ActionEvent {
        ActionEvent {
            action_id: id.to_string(),
            action_type: "Click".to_string(),
            timestamp_ms,
            frame_id: None,
            user_values: vec![],
            page_url: None,
            action_signature: None,
            semantic_signature: None,
            element_metadata: std::collections::HashMap::new(),
            action_params: std::collections::HashMap::new(),
            page_origin: None,
            page_path_template: None,
        }
    }

    // ---- Timing heuristic ----

    #[test]
    fn test_timing_within_window() {
        let correlator = Correlator::default();
        let action = make_action("a1", 1000);
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.timing);
        assert!(results[0].confidence >= TIMING_WEIGHT);
    }

    #[test]
    fn test_timing_close_bonus() {
        let correlator = Correlator::default();
        let action = make_action("a1", 1000);
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 1100);

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.timing_close);
        assert!(results[0].confidence >= TIMING_WEIGHT + TIMING_CLOSE_BONUS);
    }

    #[test]
    fn test_timing_outside_window() {
        let correlator = Correlator::default();
        let action = make_action("a1", 1000);
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 5000);

        let results = correlator.correlate_action(&action, &[trace]);
        // XHR bonus alone (0.05) < MIN_CONFIDENCE (0.2), so filtered out.
        assert!(results.is_empty());
    }

    #[test]
    fn test_timing_before_action() {
        let correlator = Correlator::default();
        let action = make_action("a1", 2000);
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 1000);

        let results = correlator.correlate_action(&action, &[trace]);
        assert!(results.is_empty());
    }

    // ---- Frame heuristic ----

    #[test]
    fn test_frame_match() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.frame_id = Some("frame-abc".to_string());

        let mut trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);
        trace.frame_id = Some("frame-abc".to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.frame_match);
        assert!(results[0].confidence >= TIMING_WEIGHT + FRAME_WEIGHT);
    }

    #[test]
    fn test_frame_no_match() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.frame_id = Some("frame-abc".to_string());

        let mut trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);
        trace.frame_id = Some("frame-xyz".to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        // Timing + XHR bonus only, no frame match.
        assert_eq!(results.len(), 1);
        assert!(!results[0].signals.frame_match);
    }

    // ---- Initiator heuristic ----

    #[test]
    fn test_initiator_url_match() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.page_url = Some("https://example.com/dashboard".to_string());

        let mut trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);
        trace.initiator = RequestInitiator {
            initiator_type: "script".to_string(),
            stack: Some(vec![StackFrame {
                function_name: "fetchData".to_string(),
                script_id: "1".to_string(),
                url: "https://example.com/static/app.js".to_string(),
                line_number: 42,
                column_number: 10,
            }]),
            url: Some("https://example.com/dashboard".to_string()),
        };

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.initiator_match);
    }

    #[test]
    fn test_initiator_stack_frame_match() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.page_url = Some("https://app.example.com/inbox".to_string());

        let mut trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);
        trace.initiator = RequestInitiator {
            initiator_type: "script".to_string(),
            stack: Some(vec![StackFrame {
                function_name: "fetchData".to_string(),
                script_id: "1".to_string(),
                url: "https://app.example.com/js/bundle.js".to_string(),
                line_number: 100,
                column_number: 5,
            }]),
            url: None,
        };

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.initiator_match);
    }

    // ---- Value heuristic ----

    #[test]
    fn test_value_in_request_body() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.action_type = "Type".to_string();
        action.user_values = vec!["alice@example.com".to_string()];

        let mut trace = make_trace("r1", "https://api.example.com/login", "POST", 1500);
        trace.request_body = Some(r#"{"email":"alice@example.com","password":"***"}"#.to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.value_match);
        assert!(results[0].confidence >= TIMING_WEIGHT + VALUE_WEIGHT);
    }

    #[test]
    fn test_value_in_url_query() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.user_values = vec!["rust programming".to_string()];

        let trace = make_trace(
            "r1",
            "https://api.example.com/search?q=rust+programming&page=1",
            "GET",
            1200,
        );

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.value_match);
    }

    #[test]
    fn test_value_case_insensitive() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.user_values = vec!["Hello World".to_string()];

        let mut trace = make_trace("r1", "https://api.example.com/post", "POST", 1500);
        trace.request_body = Some(r#"{"title":"hello world"}"#.to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(results[0].signals.value_match);
    }

    #[test]
    fn test_short_value_ignored() {
        let correlator = Correlator::default();
        let mut action = make_action("a1", 1000);
        action.user_values = vec!["a".to_string()]; // too short

        let mut trace = make_trace("r1", "https://api.example.com/data", "GET", 1200);
        trace.request_body = Some("something with a in it".to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        // Value match should NOT fire for single-char values.
        assert!(results.iter().all(|r| !r.signals.value_match));
    }

    // ---- XHR bonus ----

    #[test]
    fn test_xhr_bonus() {
        let correlator = Correlator::default();
        let action = make_action("a1", 1000);
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);

        let results = correlator.correlate_action(&action, &[trace]);
        assert!(results[0].signals.xhr_or_fetch);
    }

    #[test]
    fn test_no_xhr_bonus_for_document() {
        let correlator = Correlator::default();
        let action = make_action("a1", 1000);
        let mut trace = make_trace("r1", "https://example.com/page", "GET", 1500);
        trace.resource_type = Some("Document".to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);
        assert!(!results[0].signals.xhr_or_fetch);
    }

    // ---- Multi-action correlation ----

    #[test]
    fn test_correlate_all_deduplication() {
        let correlator = Correlator::default();

        // Two actions close in time.
        let a1 = make_action("a1", 1000);
        let a2 = make_action("a2", 1500);

        // One trace falls in both windows.
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 1600);

        let results = correlator.correlate_all(&[a1, a2], &[trace]);

        // Should deduplicate — only one pairing kept (highest confidence).
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_correlate_all_multiple_traces() {
        let correlator = Correlator::default();

        let a1 = make_action("a1", 1000);
        let a2 = make_action("a2", 5000);

        let t1 = make_trace("r1", "https://api.example.com/list", "GET", 1200);
        let t2 = make_trace("r2", "https://api.example.com/detail", "GET", 5300);

        let results = correlator.correlate_all(&[a1, a2], &[t1, t2]);

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .any(|r| r.action_id == "a1" && r.trace.request_id == "r1"));
        assert!(results
            .iter()
            .any(|r| r.action_id == "a2" && r.trace.request_id == "r2"));
    }

    // ---- Combined heuristics ----

    #[test]
    fn test_high_confidence_all_signals() {
        let correlator = Correlator::default();

        let mut action = make_action("a1", 1000);
        action.frame_id = Some("main-frame".to_string());
        action.user_values = vec!["test query".to_string()];

        let mut trace = make_trace(
            "r1",
            "https://api.example.com/search?q=test+query",
            "GET",
            1100,
        );
        trace.frame_id = Some("main-frame".to_string());

        let results = correlator.correlate_action(&action, &[trace]);
        assert_eq!(results.len(), 1);

        let r = &results[0];
        assert!(r.signals.timing);
        assert!(r.signals.timing_close);
        assert!(r.signals.frame_match);
        assert!(r.signals.value_match);
        assert!(r.signals.xhr_or_fetch);
        // All signals: 0.30 + 0.10 + 0.30 + 0.40 + 0.05 = 1.15 → clamped to 1.0.
        assert!((r.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_below_threshold_filtered() {
        let config = CorrelatorConfig {
            min_confidence: 0.5,
            ..Default::default()
        };
        let correlator = Correlator::new(config);

        let action = make_action("a1", 1000);
        // Timing-only match gives 0.30 + 0.05 (XHR) = 0.35 < 0.5.
        let trace = make_trace("r1", "https://api.example.com/data", "GET", 1500);

        let results = correlator.correlate_action(&action, &[trace]);
        assert!(results.is_empty());
    }

    // ---- Sorting & max_per_action ----

    #[test]
    fn test_sorted_by_confidence() {
        let correlator = Correlator::default();

        let mut action = make_action("a1", 1000);
        action.user_values = vec!["findme".to_string()];

        // t1 has value match (higher confidence), t2 does not.
        let mut t1 = make_trace("r1", "https://api.example.com/search?q=findme", "GET", 1200);
        t1.request_body = Some("findme data".to_string());
        let t2 = make_trace("r2", "https://api.example.com/other", "GET", 1300);

        let results = correlator.correlate_action(&action, &[t2, t1]);

        assert_eq!(results.len(), 2);
        assert!(results[0].confidence >= results[1].confidence);
        assert_eq!(results[0].trace.request_id, "r1");
    }

    #[test]
    fn test_max_per_action_limit() {
        let config = CorrelatorConfig {
            max_per_action: 2,
            ..Default::default()
        };
        let correlator = Correlator::new(config);

        let action = make_action("a1", 1000);
        let traces: Vec<_> = (0..5)
            .map(|i| {
                make_trace(
                    &format!("r{}", i),
                    &format!("https://api.example.com/ep{}", i),
                    "GET",
                    1100 + i * 100,
                )
            })
            .collect();

        let results = correlator.correlate_action(&action, &traces);
        assert_eq!(results.len(), 2);
    }

    // ---- URL origin matching helper ----

    #[test]
    fn test_urls_same_origin() {
        assert!(urls_same_origin(
            "https://example.com/page",
            "https://example.com/other"
        ));
        assert!(!urls_same_origin(
            "https://example.com",
            "https://other.com"
        ));
        assert!(!urls_same_origin(
            "https://example.com",
            "http://example.com"
        ));
        assert!(urls_same_origin(
            "https://api.example.com:443/path",
            "https://api.example.com/other"
        ));
    }

    // ---- Custom window ----

    #[test]
    fn test_custom_window() {
        let config = CorrelatorConfig {
            window_ms: 500,
            ..Default::default()
        };
        let correlator = Correlator::new(config);

        let action = make_action("a1", 1000);
        // At 1300ms → within 500ms window.
        let t1 = make_trace("r1", "https://api.example.com/a", "GET", 1300);
        // At 1800ms → outside 500ms window.
        let t2 = make_trace("r2", "https://api.example.com/b", "GET", 1800);

        let results = correlator.correlate_action(&action, &[t1, t2]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].trace.request_id, "r1");
    }
}
