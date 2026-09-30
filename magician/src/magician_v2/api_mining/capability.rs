//! Core ApiCapability type and confidence promotion logic
//!
//! An ApiCapability represents a reverse-engineered API endpoint that was learned
//! from observing network traces during browser automation. Capabilities progress
//! through confidence levels as they are validated through replay.

use super::action_binding::{merge_action_bindings, ActionBinding};
use super::relevance::Relevance;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Schema version for forward compatibility
pub const SCHEMA_VERSION: &str = "1.0.0";

/// Minimum samples required to promote from Observed → Candidate
const MIN_SAMPLES_FOR_CANDIDATE: usize = 3;

/// Successful replays required to promote from Candidate → Validated
const REPLAYS_FOR_VALIDATED: usize = 3;

/// Total successful replays required to promote from Validated → Trusted
const REPLAYS_FOR_TRUSTED: usize = 5;

/// Consecutive failures to trigger demotion
const CONSECUTIVE_FAILURES_FOR_DEMOTION: usize = 3;

/// Failure rate threshold for demotion (50%)
const FAILURE_RATE_FOR_DEMOTION: f64 = 0.5;

/// Maximum number of trace IDs to retain per capability (prevents unbounded growth)
const MAX_TRACE_IDS: usize = 100;

/// A reverse-engineered API endpoint learned from network traces
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCapability {
    /// Schema version for forward compatibility
    pub schema_version: String,

    /// Unique capability identifier
    pub id: String,

    /// Human-readable name (auto-generated from URL path)
    pub name: String,

    /// Origin URL this capability belongs to
    pub origin: String,

    /// HTTP method (GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS)
    pub method: String,

    /// Parameterized URL with `{param}` placeholders
    pub url_template: String,

    /// Required headers with template values
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers_template: HashMap<String, String>,

    /// Session context needed for replay
    pub auth_requirements: AuthRequirements,

    /// Description of variable input parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,

    /// Response structure sketch for verification
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<serde_json::Value>,

    /// Side effect classification
    pub side_effects: SideEffects,

    /// Current confidence level
    pub confidence: ConfidenceLevel,

    /// Number of trace samples backing this capability
    pub sample_count: usize,

    /// Successful replay count
    pub replay_success_count: usize,

    /// Failed replay count
    pub replay_failure_count: usize,

    /// Consecutive failures (reset on success)
    #[serde(default)]
    pub consecutive_failures: usize,

    /// Epoch seconds of last successful validation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_validated: Option<i64>,

    /// Epoch seconds when capability was first created
    pub created_at: i64,

    /// Epoch seconds of last update
    pub updated_at: i64,

    /// Request IDs of contributing traces
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace_ids: Vec<String>,

    /// GraphQL operation name when this capability represents a GraphQL POST.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_operation: Option<String>,

    /// GraphQL operation kind when this capability represents a GraphQL request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_operation_kind: Option<GraphqlOperationKind>,

    /// Persisted query hash when this capability represents an APQ/hash-keyed GraphQL request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_persisted_query_sha256: Option<String>,

    /// Body structure fingerprint for body-aware capability splitting.
    /// When present, this capability represents a specific operation variant
    /// on a shared endpoint (e.g., "mark-as-unread" vs "archive" on Gmail sync).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_fingerprint: Option<String>,

    /// Request body template for active replay of mutating endpoints.
    /// Uses typed placeholders like `{{string:body_variables_id}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_template: Option<String>,

    /// Learned UI action signatures that can be rewritten to this capability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_bindings: Vec<ActionBinding>,

    /// Origin of the page that triggered requests to this API origin.
    /// Derived from the Referer header at mining time.
    /// Used for active auth recovery on API-only sub-origins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_origin: Option<String>,

    /// Count of auth-related failures (401/403). Observability only — does NOT
    /// affect confidence or trigger demotion.
    #[serde(default)]
    pub auth_failure_count: usize,

    /// How this endpoint relates to task outcomes. Backward-compatible records
    /// start Unclassified until mining or a recipe compile updates them.
    #[serde(default)]
    pub relevance: Relevance,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relevance_reason: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by_recipe_ids: Vec<String>,

    /// Retention is non-destructive: unused capabilities age out of default
    /// views but remain recoverable and can become visible again on use.
    #[serde(default)]
    pub hidden: bool,
}

/// Session context needed for API replay
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthRequirements {
    /// Cookie names required (e.g., ["SID", "GMAIL_AT"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cookies: Vec<String>,

    /// Header names required (e.g., ["authorization"])
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,

    /// Query parameter names that carry auth material (e.g.,
    /// `x-algolia-api-key`, `api_key`). Persisted trace URLs keep these
    /// redacted; replay resolves the values from SessionContext.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_params: Vec<String>,

    /// LocalStorage keys required
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_storage_keys: Vec<String>,

    /// SessionStorage keys required
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_storage_keys: Vec<String>,
}

/// Side effect classification for an API capability
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SideEffects {
    /// Safe to replay (GET, HEAD, OPTIONS, GraphQL queries)
    ReadOnly,
    /// Unknown side effects — do not replay in v1
    Unknown,
    /// Has side effects (POST, PUT, PATCH, DELETE, GraphQL mutations) — replay requires elevated confidence
    Write,
}

/// GraphQL operation kind extracted from the request document.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlOperationKind {
    Query,
    Mutation,
    Subscription,
    Unknown,
}

impl GraphqlOperationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
            Self::Subscription => "subscription",
            Self::Unknown => "unknown",
        }
    }
}

/// Confidence level for a learned API capability
///
/// Progression: Observed → Candidate → Validated → Trusted
/// Demotion occurs on consecutive failures or high failure rate
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    /// 1+ sample seen, no replay attempted
    Observed,
    /// 3+ samples with stable parameters, replay is cautious
    Candidate,
    /// 3 successful replays, replay with verification
    Validated,
    /// 5+ successful replays, preferred over UI
    Trusted,
}

impl ApiCapability {
    /// Create a new capability from initial trace data
    pub fn new(name: String, origin: String, method: String, url_template: String) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            id: Uuid::new_v4().to_string(),
            name,
            origin,
            method: method.to_uppercase(),
            url_template,
            headers_template: HashMap::new(),
            auth_requirements: AuthRequirements::default(),
            input_schema: None,
            response_schema: None,
            side_effects: Self::classify_side_effects_by_method(&method),
            confidence: ConfidenceLevel::Observed,
            sample_count: 1,
            replay_success_count: 0,
            replay_failure_count: 0,
            consecutive_failures: 0,
            last_validated: None,
            created_at: now,
            updated_at: now,
            trace_ids: Vec::new(),
            graphql_operation: None,
            graphql_operation_kind: None,
            graphql_persisted_query_sha256: None,
            body_fingerprint: None,
            body_template: None,
            action_bindings: Vec::new(),
            parent_origin: None,
            auth_failure_count: 0,
            relevance: Relevance::Unclassified,
            relevance_reason: None,
            used_by_recipe_ids: Vec::new(),
            hidden: false,
        }
    }

    /// Classify side effects based on HTTP method alone.
    fn classify_side_effects_by_method(method: &str) -> SideEffects {
        match method.to_uppercase().as_str() {
            "GET" | "HEAD" | "OPTIONS" => SideEffects::ReadOnly,
            "POST" | "PUT" | "PATCH" | "DELETE" => SideEffects::Write,
            _ => SideEffects::Unknown,
        }
    }

    /// Recompute the stored side-effect tier from the current request metadata.
    pub fn refresh_side_effects(&mut self) {
        self.side_effects = classify_side_effects_for_request(
            &self.method,
            &self.url_template,
            self.body_template.as_deref(),
            self.graphql_operation_kind,
        );
    }

    /// Effective side-effect tier used by replay and validation policy.
    pub fn effective_side_effects(&self) -> SideEffects {
        effective_side_effects_for_request(
            &self.side_effects,
            &self.method,
            &self.url_template,
            self.body_template.as_deref(),
            self.graphql_operation_kind,
        )
    }

    /// Minimum confidence required for replay, based on method and side-effect tier.
    ///
    /// | Side Effects | Method         | Min Confidence |
    /// |-------------|----------------|----------------|
    /// | ReadOnly    | GET/HEAD/OPTIONS/GraphQL query | Candidate |
    /// | Write       | POST/PUT/PATCH/GraphQL mutation | Validated |
    /// | Write       | DELETE                         | Trusted   |
    /// | Unknown     | *                              | None (blocked) |
    pub fn min_replay_confidence(&self) -> Option<ConfidenceLevel> {
        min_replay_confidence_for(&self.effective_side_effects(), &self.method)
    }

    /// Whether this capability is eligible for replay at its current confidence.
    pub fn is_replayable(&self) -> bool {
        self.min_replay_confidence()
            .map(|min| self.confidence >= min)
            .unwrap_or(false)
    }

    /// Whether this capability is eligible for passive validation.
    /// Candidate+ capabilities are validatable when their effective side effects are known.
    pub fn is_validatable(&self) -> bool {
        self.confidence >= ConfidenceLevel::Candidate
            && self.effective_side_effects() != SideEffects::Unknown
    }

    /// Whether this capability should be preferred over UI automation
    pub fn is_preferred(&self) -> bool {
        self.confidence == ConfidenceLevel::Trusted && self.min_replay_confidence().is_some()
    }

    /// Add a new trace sample and attempt promotion
    pub fn add_sample(&mut self, trace_id: String) {
        self.sample_count += 1;
        self.trace_ids.push(trace_id);
        // Keep only the most recent trace IDs to prevent unbounded memory growth
        if self.trace_ids.len() > MAX_TRACE_IDS {
            let excess = self.trace_ids.len() - MAX_TRACE_IDS;
            self.trace_ids.drain(..excess);
        }
        self.updated_at = chrono::Utc::now().timestamp();
        self.try_promote();
    }

    /// Record a successful replay and attempt promotion
    pub fn record_replay_success(&mut self) {
        self.replay_success_count += 1;
        self.consecutive_failures = 0;
        self.last_validated = Some(chrono::Utc::now().timestamp());
        self.updated_at = chrono::Utc::now().timestamp();
        self.try_promote();
    }

    /// Record a failed replay and attempt demotion
    pub fn record_replay_failure(&mut self) {
        self.replay_failure_count += 1;
        self.consecutive_failures += 1;
        self.updated_at = chrono::Utc::now().timestamp();
        self.try_demote();
    }

    /// Record an auth-related replay failure (401/403). Does NOT affect
    /// confidence or trigger demotion — only increments a separate counter.
    pub fn record_auth_failure(&mut self) {
        self.auth_failure_count += 1;
        self.updated_at = chrono::Utc::now().timestamp();
    }

    /// Record or update a learned UI action binding for this capability.
    pub fn record_action_binding(&mut self, binding: ActionBinding) {
        self.action_bindings = merge_action_bindings(&self.action_bindings, &[binding]);
        self.updated_at = chrono::Utc::now().timestamp();
    }

    /// Attempt to promote the capability based on current metrics.
    /// Uses the legacy `MIN_SAMPLES_FOR_CANDIDATE` constant for the
    /// Observed → Candidate transition. Callers that have access to
    /// the `ApiMiningConfig`'s per-method takeover thresholds should
    /// prefer [`try_promote_with_threshold`] so GET / HEAD endpoints
    /// can engage on the second visit.
    pub fn try_promote(&mut self) {
        self.try_promote_with_threshold(MIN_SAMPLES_FOR_CANDIDATE);
    }

    /// Variant of `try_promote` that takes the Observed → Candidate
    /// sample threshold explicitly. PL Task 7: the mining pipeline
    /// resolves the per-method value from
    /// `ApiMiningConfig::takeover_min_samples(self.method)` and passes
    /// it here so read-only verbs (GET / HEAD) can engage on the
    /// second visit (threshold 1) while side-effecting verbs stay
    /// conservative (threshold 3).
    pub fn try_promote_with_threshold(&mut self, candidate_min_samples: usize) {
        match self.confidence {
            ConfidenceLevel::Observed => {
                if self.sample_count >= candidate_min_samples {
                    self.confidence = ConfidenceLevel::Candidate;
                }
            },
            ConfidenceLevel::Candidate => {
                if self.replay_success_count >= REPLAYS_FOR_VALIDATED {
                    self.confidence = ConfidenceLevel::Validated;
                }
            },
            ConfidenceLevel::Validated => {
                if self.replay_success_count >= REPLAYS_FOR_TRUSTED {
                    self.confidence = ConfidenceLevel::Trusted;
                }
            },
            ConfidenceLevel::Trusted => {
                // Already at max confidence
            },
        }
    }

    /// Attempt to demote the capability based on failure metrics
    fn try_demote(&mut self) {
        if self.consecutive_failures >= CONSECUTIVE_FAILURES_FOR_DEMOTION {
            self.demote();
            return;
        }

        let total_replays = self.replay_success_count + self.replay_failure_count;
        if total_replays > 0 {
            let failure_rate = self.replay_failure_count as f64 / total_replays as f64;
            if failure_rate > FAILURE_RATE_FOR_DEMOTION && total_replays >= 4 {
                self.demote();
            }
        }
    }

    /// Demote by one level
    fn demote(&mut self) {
        self.confidence = match self.confidence {
            ConfidenceLevel::Trusted => ConfidenceLevel::Validated,
            ConfidenceLevel::Validated => ConfidenceLevel::Candidate,
            ConfidenceLevel::Candidate => ConfidenceLevel::Observed,
            ConfidenceLevel::Observed => ConfidenceLevel::Observed,
        };
        self.consecutive_failures = 0;
    }

    /// Generate a lightweight summary for the registry index
    pub fn to_summary(&self) -> CapabilitySummary {
        CapabilitySummary {
            id: self.id.clone(),
            name: self.name.clone(),
            method: self.method.clone(),
            url_template: self.url_template.clone(),
            confidence: self.confidence.clone(),
            side_effects: self.effective_side_effects(),
            sample_count: self.sample_count,
            updated_at: self.updated_at,
            graphql_operation: self.graphql_operation.clone(),
            graphql_operation_kind: self.graphql_operation_kind,
            graphql_persisted_query_sha256: self.graphql_persisted_query_sha256.clone(),
            body_fingerprint: self.body_fingerprint.clone(),
            action_bindings: self.action_bindings.clone(),
            parent_origin: self.parent_origin.clone(),
            relevance: self.relevance,
            used_by_recipe_ids: self.used_by_recipe_ids.clone(),
            hidden: self.hidden,
        }
    }
}

/// Derive the side-effect tier from request metadata alone.
pub fn classify_side_effects_for(
    method: &str,
    graphql_operation_kind: Option<GraphqlOperationKind>,
) -> SideEffects {
    classify_side_effects_for_request(method, "", None, graphql_operation_kind)
}

/// Derive the side-effect tier from method, URL, body, and GraphQL metadata.
///
/// POST is not automatically mutating on modern web apps: search/query APIs
/// often use POST for payload size, privacy, or vendor SDK conventions
/// (Algolia is a common example). This heuristic keeps those read-like APIs
/// eligible for early replay while preserving conservative write detection for
/// checkout/order/payment/cart/admin-style endpoints.
pub fn classify_side_effects_for_request(
    method: &str,
    url_template: &str,
    body_template: Option<&str>,
    graphql_operation_kind: Option<GraphqlOperationKind>,
) -> SideEffects {
    match graphql_operation_kind {
        Some(GraphqlOperationKind::Query) => SideEffects::ReadOnly,
        Some(GraphqlOperationKind::Mutation) => SideEffects::Write,
        Some(GraphqlOperationKind::Subscription) => SideEffects::Unknown,
        Some(GraphqlOperationKind::Unknown) | None => {
            let method = method.to_uppercase();
            if method == "POST"
                && looks_like_read_only_post(url_template)
                && !looks_like_write_request(url_template, body_template)
            {
                return SideEffects::ReadOnly;
            }
            ApiCapability::classify_side_effects_by_method(&method)
        },
    }
}

fn looks_like_read_only_post(url_template: &str) -> bool {
    let path = url::Url::parse(url_template)
        .ok()
        .map(|url| url.path().to_ascii_lowercase())
        .unwrap_or_else(|| url_template.to_ascii_lowercase());

    path == "/query"
        || path.ends_with("/query")
        || path.contains("/query/")
        || path == "/search"
        || path.ends_with("/search")
        || path.contains("/search/")
        || path.ends_with("/lookup")
        || path.contains("/lookup/")
        || path.ends_with("/suggest")
        || path.contains("/suggest/")
        || path.ends_with("/autocomplete")
        || path.contains("/autocomplete/")
        || (path.contains("/indexes/") && path.ends_with("/query"))
}

fn looks_like_write_request(url_template: &str, body_template: Option<&str>) -> bool {
    let lower_url = url_template.to_ascii_lowercase();
    let write_path_markers = [
        "/cart",
        "/checkout",
        "/payment",
        "/purchase",
        "/booking",
        "/order",
        "/orders",
        "/create",
        "/update",
        "/delete",
        "/remove",
        "/save",
        "/submit",
        "/mutation",
        "/admin",
    ];
    if write_path_markers
        .iter()
        .any(|marker| lower_url.contains(marker))
    {
        return true;
    }

    let Some(body) = body_template else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    let write_body_markers = [
        "\"mutation",
        "\"action\":\"add",
        "\"action\":\"create",
        "\"action\":\"update",
        "\"action\":\"delete",
        "\"action\":\"remove",
        "\"intent\":\"checkout",
        "\"operation\":\"checkout",
        "\"operation\":\"purchase",
        "\"operation\":\"payment",
    ];
    write_body_markers
        .iter()
        .any(|marker| body.replace(' ', "").contains(marker))
}

/// Resolve the effective side-effect tier used by routing and validation.
///
/// `stored_side_effects` preserves backwards compatibility for capabilities mined before
/// GraphQL operation kind was persisted.
pub fn effective_side_effects_for(
    stored_side_effects: &SideEffects,
    method: &str,
    graphql_operation_kind: Option<GraphqlOperationKind>,
) -> SideEffects {
    effective_side_effects_for_request(
        stored_side_effects,
        method,
        "",
        None,
        graphql_operation_kind,
    )
}

pub fn effective_side_effects_for_request(
    stored_side_effects: &SideEffects,
    method: &str,
    url_template: &str,
    body_template: Option<&str>,
    graphql_operation_kind: Option<GraphqlOperationKind>,
) -> SideEffects {
    match graphql_operation_kind {
        Some(kind) => {
            classify_side_effects_for_request(method, url_template, body_template, Some(kind))
        },
        None => {
            let inferred =
                classify_side_effects_for_request(method, url_template, body_template, None);
            if inferred == SideEffects::ReadOnly && stored_side_effects == &SideEffects::Write {
                inferred
            } else {
                stored_side_effects.clone()
            }
        },
    }
}

/// Compute the minimum replay confidence for a given side-effect + method pair.
///
/// This is the single source of truth for the tiered replay policy.
///
/// **History:** before v0.6.514 this tiered Write methods to Validated
/// (POST/PUT/PATCH) and Trusted (DELETE), so the router would only
/// replay non-read-only capabilities after multiple successful
/// replays had accumulated. v0.6.514 lowered every Write method to
/// Candidate so mining-captured capabilities can engage on visit 2,
/// matching the takeover-sample-threshold change in `config.rs`.
/// Safety floors that remain:
///   - `is_replayable()` still gates Unknown side-effects regardless.
///   - The router only routes when the request shape (URL template +
///     body fingerprint + headers) matches the captured one.
///   - `auto_replay_validation.url_denylist_substrings` blocks
///     auth / payment / admin paths even at the Candidate tier.
///   - Per-origin opt-in via `OriginPolicyStore::allow_replay_for_origin`
///     remains in force for orchestrator-driven auto-replay.
///
/// Operators who want the historical conservative policy back can
/// fork this function or guard it behind a config flag in their
/// deployment.
pub fn min_replay_confidence_for(
    side_effects: &SideEffects,
    method: &str,
) -> Option<ConfidenceLevel> {
    let method = method.to_uppercase();
    match (side_effects, method.as_str()) {
        (SideEffects::ReadOnly, _) => Some(ConfidenceLevel::Candidate),
        // v0.6.514: lowered from `Trusted`/`Validated` → `Candidate`.
        // Side-effecting methods now replay-eligible at Candidate so
        // the visit-2 takeover policy holds for POST/PUT/PATCH/DELETE
        // the same way it does for GET/HEAD. See function docs above.
        (SideEffects::Write, _) => Some(ConfidenceLevel::Candidate),
        (SideEffects::Unknown, _) => None,
    }
}

/// Lightweight capability summary for the registry index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySummary {
    pub id: String,
    pub name: String,
    pub method: String,
    pub url_template: String,
    pub confidence: ConfidenceLevel,
    pub side_effects: SideEffects,
    pub sample_count: usize,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_operation_kind: Option<GraphqlOperationKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql_persisted_query_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub action_bindings: Vec<ActionBinding>,
    /// Page/site origin that taught this API origin. Persisting it in the
    /// lightweight index lets dashboards group by site without rereading each
    /// full capability file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_origin: Option<String>,
    #[serde(default)]
    pub relevance: Relevance,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by_recipe_ids: Vec<String>,
    #[serde(default)]
    pub hidden: bool,
}

impl CapabilitySummary {
    pub fn effective_side_effects(&self) -> SideEffects {
        effective_side_effects_for(
            &self.side_effects,
            &self.method,
            self.graphql_operation_kind,
        )
    }

    /// Minimum confidence required to replay this capability.
    pub fn min_replay_confidence(&self) -> Option<ConfidenceLevel> {
        min_replay_confidence_for(&self.effective_side_effects(), &self.method)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn create_test_capability() -> ApiCapability {
        ApiCapability::new(
            "gmail_inbox_sync".to_string(),
            "https://mail.google.com".to_string(),
            "GET".to_string(),
            "https://mail.google.com/sync/u/0/i/s?hl=en&c={cursor}".to_string(),
        )
    }

    #[test]
    fn test_new_capability_defaults() {
        let cap = create_test_capability();
        assert_eq!(cap.confidence, ConfidenceLevel::Observed);
        assert_eq!(cap.side_effects, SideEffects::ReadOnly);
        assert_eq!(cap.sample_count, 1);
        assert_eq!(cap.replay_success_count, 0);
        assert!(!cap.is_replayable()); // Observed is not replayable
    }

    #[test]
    fn test_post_classified_as_write() {
        let cap = ApiCapability::new(
            "submit_form".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/submit".to_string(),
        );
        assert_eq!(cap.side_effects, SideEffects::Write);
        assert!(!cap.is_replayable());
    }

    #[test]
    fn test_read_like_post_query_endpoint_is_read_only() {
        let mut cap = ApiCapability::new(
            "algolia_query".to_string(),
            "https://search.example.com".to_string(),
            "POST".to_string(),
            "https://search.example.com/1/indexes/Item_dev/query".to_string(),
        );
        cap.body_template = Some(r#"{"query":"rust","hitsPerPage":10}"#.to_string());
        cap.refresh_side_effects();

        assert_eq!(cap.side_effects, SideEffects::ReadOnly);
    }

    #[test]
    fn test_write_like_post_query_endpoint_remains_write() {
        let mut cap = ApiCapability::new(
            "checkout_search".to_string(),
            "https://shop.example.com".to_string(),
            "POST".to_string(),
            "https://shop.example.com/api/checkout/query".to_string(),
        );
        cap.body_template = Some(r#"{"action":"add","sku":"abc"}"#.to_string());
        cap.refresh_side_effects();

        assert_eq!(cap.side_effects, SideEffects::Write);
    }

    #[test]
    fn test_effective_side_effects_corrects_older_read_like_post_write() {
        let mut cap = ApiCapability::new(
            "search_query".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/search".to_string(),
        );
        cap.side_effects = SideEffects::Write;
        cap.body_template = Some(r#"{"query":"hello"}"#.to_string());

        assert_eq!(cap.effective_side_effects(), SideEffects::ReadOnly);
    }

    #[test]
    fn test_graphql_query_post_is_treated_as_read_only() {
        let mut cap = ApiCapability::new(
            "graphql_get_user".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/graphql".to_string(),
        );
        cap.graphql_operation = Some("GetUser".to_string());
        cap.graphql_operation_kind = Some(GraphqlOperationKind::Query);

        assert_eq!(cap.effective_side_effects(), SideEffects::ReadOnly);

        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);
        assert!(cap.is_replayable());
    }

    #[test]
    fn test_graphql_mutation_post_remains_write_like_v_0_6_514() {
        let mut cap = ApiCapability::new(
            "graphql_archive_email".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/graphql".to_string(),
        );
        cap.graphql_operation = Some("ArchiveEmail".to_string());
        cap.graphql_operation_kind = Some(GraphqlOperationKind::Mutation);

        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        assert_eq!(cap.effective_side_effects(), SideEffects::Write);
        // v0.6.514: Write at Candidate is now replayable
        assert!(cap.is_replayable());

        cap.record_replay_success();
        cap.record_replay_success();
        cap.record_replay_success();
        assert_eq!(cap.confidence, ConfidenceLevel::Validated);
        assert!(cap.is_replayable());
    }

    #[test]
    fn test_graphql_subscription_is_blocked_from_replay_and_validation() {
        let mut cap = ApiCapability::new(
            "graphql_live_feed".to_string(),
            "https://api.example.com".to_string(),
            "POST".to_string(),
            "https://api.example.com/graphql".to_string(),
        );
        cap.graphql_operation = Some("LiveFeed".to_string());
        cap.graphql_operation_kind = Some(GraphqlOperationKind::Subscription);

        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());

        assert_eq!(cap.effective_side_effects(), SideEffects::Unknown);
        assert!(!cap.is_replayable());
        assert!(!cap.is_validatable());
    }

    #[test]
    fn test_promotion_observed_to_candidate() {
        let mut cap = create_test_capability();
        assert_eq!(cap.confidence, ConfidenceLevel::Observed);

        cap.add_sample("req-2".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Observed);

        cap.add_sample("req-3".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);
        assert!(cap.is_replayable());
    }

    #[test]
    fn test_promotion_candidate_to_validated() {
        let mut cap = create_test_capability();
        // Promote to candidate
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);

        // 3 successful replays → validated
        cap.record_replay_success();
        cap.record_replay_success();
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);
        cap.record_replay_success();
        assert_eq!(cap.confidence, ConfidenceLevel::Validated);
    }

    #[test]
    fn test_promotion_validated_to_trusted() {
        let mut cap = create_test_capability();
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        for _ in 0..5 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);
        assert!(cap.is_preferred());
    }

    #[test]
    fn test_demotion_on_consecutive_failures() {
        let mut cap = create_test_capability();
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        for _ in 0..5 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);

        // 3 consecutive failures → demote one level
        cap.record_replay_failure();
        cap.record_replay_failure();
        cap.record_replay_failure();
        assert_eq!(cap.confidence, ConfidenceLevel::Validated);
    }

    #[test]
    fn test_success_resets_consecutive_failures() {
        let mut cap = create_test_capability();
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        for _ in 0..5 {
            cap.record_replay_success();
        }

        // 2 failures then a success — should NOT demote
        cap.record_replay_failure();
        cap.record_replay_failure();
        cap.record_replay_success();
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);
        assert_eq!(cap.consecutive_failures, 0);
    }

    #[test]
    fn test_summary_generation() {
        let cap = create_test_capability();
        let summary = cap.to_summary();
        assert_eq!(summary.id, cap.id);
        assert_eq!(summary.name, "gmail_inbox_sync");
        assert_eq!(summary.method, "GET");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let cap = create_test_capability();
        let json = serde_json::to_string_pretty(&cap).unwrap();
        let deserialized: ApiCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, cap.id);
        assert_eq!(deserialized.name, cap.name);
        assert_eq!(deserialized.confidence, cap.confidence);
    }

    #[test]
    fn test_post_candidate_is_validatable_v_0_6_514() {
        let mut cap = ApiCapability::new(
            "sync_action".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/sync".to_string(),
        );
        // Promote to Candidate
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);
        // v0.6.514: POST at Candidate is now replayable AND validatable
        assert!(cap.is_replayable());
        assert!(cap.is_validatable());
    }

    #[test]
    fn test_observed_not_validatable() {
        let cap = create_test_capability();
        assert_eq!(cap.confidence, ConfidenceLevel::Observed);
        assert!(!cap.is_validatable());
    }

    #[test]
    fn test_post_candidate_is_replayable_v_0_6_514() {
        // v0.6.514 lowered the Write replay tier from Validated to
        // Candidate so POST/PUT/PATCH capabilities can replay on
        // visit 2 (matching the takeover-sample-threshold change).
        // Was: `test_post_validated_is_replayable` — required
        // Validated; now Candidate is enough.
        let mut cap = ApiCapability::new(
            "submit_form".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/submit".to_string(),
        );
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);
        assert!(cap.is_replayable(), "POST at Candidate now replayable");

        cap.record_replay_success();
        cap.record_replay_success();
        cap.record_replay_success();
        assert_eq!(cap.confidence, ConfidenceLevel::Validated);
        assert!(cap.is_replayable());
    }

    #[test]
    fn test_delete_candidate_is_replayable_v_0_6_514() {
        // v0.6.514 lowered DELETE's replay tier from Trusted to
        // Candidate. Was: `test_delete_needs_trusted`. URL denylist
        // (`/account/delete`, etc.) still blocks the obviously-
        // dangerous paths.
        let mut cap = ApiCapability::new(
            "delete_item".to_string(),
            "https://example.com".to_string(),
            "DELETE".to_string(),
            "https://example.com/api/items/{id}".to_string(),
        );
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        assert_eq!(cap.confidence, ConfidenceLevel::Candidate);
        assert!(cap.is_replayable(), "DELETE at Candidate now replayable");
    }

    #[test]
    fn test_write_trusted_is_preferred() {
        let mut cap = ApiCapability::new(
            "submit_form".to_string(),
            "https://example.com".to_string(),
            "POST".to_string(),
            "https://example.com/api/submit".to_string(),
        );
        // Promote through all levels to Trusted
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        for _ in 0..5 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);
        assert!(cap.is_preferred()); // Write at Trusted is preferred
    }

    #[test]
    fn record_auth_failure_does_not_affect_confidence() {
        let mut cap = create_test_capability();
        cap.add_sample("req-2".to_string());
        cap.add_sample("req-3".to_string());
        for _ in 0..5 {
            cap.record_replay_success();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);

        for _ in 0..5 {
            cap.record_auth_failure();
        }
        assert_eq!(cap.confidence, ConfidenceLevel::Trusted);
        assert_eq!(cap.consecutive_failures, 0);
        assert_eq!(cap.auth_failure_count, 5);
    }

    #[test]
    fn parent_origin_serialization_roundtrip() {
        let mut cap = create_test_capability();
        cap.parent_origin = Some("https://mail.google.com".to_string());
        let json = serde_json::to_string(&cap).unwrap();
        let deser: ApiCapability = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deser.parent_origin,
            Some("https://mail.google.com".to_string())
        );
        assert_eq!(
            deser.to_summary().parent_origin.as_deref(),
            Some("https://mail.google.com")
        );
    }

    #[test]
    fn parent_origin_none_not_serialized() {
        let cap = create_test_capability();
        let json = serde_json::to_string(&cap).unwrap();
        assert!(!json.contains("parent_origin"));
    }
}
