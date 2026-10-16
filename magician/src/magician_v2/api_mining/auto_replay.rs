//! Auto-replay validation pipeline — closes the cold-start gap.
//!
//! After mining clusters traces into Candidate capabilities, this module
//! fires a small number of safety-gated replays per capability so the
//! attempt counters needed for Candidate→Validated promotion accumulate
//! without operator intervention.
//!
//! ## Safety model (conservative by default)
//!
//! Every replay must clear three gates in order:
//!
//! 1. **Origin policy** — `OriginPolicyStore::allow_replay_for_origin` must
//!    return `true`. Origins default to `false`; operators opt in.
//! 2. **`SafetyFilter`** — HTTP method must be in the allowlist (GET/HEAD
//!    by default), URL must not match the denylist (auth, payment, admin,
//!    etc.), and the capability must not carry an `Idempotency-Key`
//!    header (a strong signal the caller's dedup design is fragile under
//!    replay).
//! 3. **Auth posture** — if the capability declares auth requirements and
//!    the captured `SessionContext` cannot satisfy them, the replay is
//!    skipped (not fired-and-failed) so a stale-creds 401 does not tar
//!    an otherwise-fine capability.
//!
//! `dry_run` mode (default `true` for the first release) runs every gate
//! and increments the would-fire counters but never emits an HTTP
//! request. Operators flip it off after at least one release of clean
//! dry-run data.

use super::capability::{ApiCapability, ConfidenceLevel};
use super::replay::ApiRunner;
use super::types::SessionContext;
use std::collections::{HashMap, HashSet};

/// Default URL-substring denylist. Matches case-insensitively against
/// the lowercased URL template (path + query). These are the floor;
/// operators add more via `AutoReplayConfig::url_denylist_substrings`.
pub const DEFAULT_URL_DENYLIST: &[&str] = &[
    "/auth",
    "/login",
    "/logout",
    "/signin",
    "/signout",
    "/oauth",
    "/token",
    "/password",
    "/payment",
    "/billing",
    "/charge",
    "/refund",
    "/invoice",
    "/transfer",
    "/checkout",
    "/order",
    "/subscribe",
    "/cancel",
    "/account/delete",
    "/permissions",
    "/admin",
    "/api-keys",
    "/webhooks",
];

/// Default method allowlist for inline auto-replay. POST/PUT/PATCH/DELETE
/// require explicit opt-in per origin via
/// `AutoReplayConfig::opt_in_post_origins`.
pub const DEFAULT_ALLOWED_METHODS: &[&str] = &["GET", "HEAD"];

/// Outcome of running `SafetyFilter::accepts` against a single capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyDecision {
    /// Capability passes every gate. Safe to replay.
    Accept,
    /// Capability is blocked. The contained string is a stable,
    /// operator-readable reason suitable for tracing logs and Forge
    /// surfaces.
    Reject(String),
}

impl SafetyDecision {
    pub fn is_accept(&self) -> bool {
        matches!(self, SafetyDecision::Accept)
    }
}

/// Conservative-by-default safety filter for inline auto-replay.
///
/// Construct via `SafetyFilter::from_config` so the allowlists and
/// denylists are sourced from `AutoReplayConfig`. The default `new()`
/// constructor uses the module constants and is intended for tests.
#[derive(Debug, Clone)]
pub struct SafetyFilter {
    methods_allowed: HashSet<String>,
    url_denylist_substrings: Vec<String>,
    opt_in_post_origins: HashSet<String>,
}

impl Default for SafetyFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl SafetyFilter {
    pub fn new() -> Self {
        Self {
            methods_allowed: DEFAULT_ALLOWED_METHODS
                .iter()
                .map(|m| m.to_ascii_uppercase())
                .collect(),
            url_denylist_substrings: DEFAULT_URL_DENYLIST
                .iter()
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            opt_in_post_origins: HashSet::new(),
        }
    }

    /// Build a filter from configured method / URL / origin lists.
    ///
    /// The hard-coded `DEFAULT_ALLOWED_METHODS` and `DEFAULT_URL_DENYLIST`
    /// are unioned with the operator-supplied lists rather than
    /// replaced. This prevents an over-eager yaml edit
    /// (`url_denylist_substrings: ['/internal']`) from silently
    /// removing the safety floor (`/auth`, `/payment`, `/admin`, …).
    /// Operators can ADD to the floor but never SUBTRACT from it.
    /// Strings are normalized so `accepts` can do case-insensitive
    /// comparisons without re-normalizing per call.
    pub fn from_lists(
        methods_allowed: impl IntoIterator<Item = String>,
        url_denylist_substrings: impl IntoIterator<Item = String>,
        opt_in_post_origins: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut methods: HashSet<String> = DEFAULT_ALLOWED_METHODS
            .iter()
            .map(|m| m.to_ascii_uppercase())
            .collect();
        methods.extend(methods_allowed.into_iter().map(|m| m.to_ascii_uppercase()));

        let mut denylist: Vec<String> = DEFAULT_URL_DENYLIST
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        for needle in url_denylist_substrings {
            let normalized = needle.to_ascii_lowercase();
            if !denylist.contains(&normalized) {
                denylist.push(normalized);
            }
        }

        Self {
            methods_allowed: methods,
            url_denylist_substrings: denylist,
            opt_in_post_origins: opt_in_post_origins.into_iter().collect(),
        }
    }

    /// Evaluate every gate against a capability. The first failing gate
    /// short-circuits with a `SafetyDecision::Reject` carrying a stable
    /// reason string. Auth-posture gating is handled separately by the
    /// caller because it needs the live `SessionContext`.
    pub fn accepts(&self, capability: &ApiCapability) -> SafetyDecision {
        let method = capability.method.to_ascii_uppercase();

        if !self.methods_allowed.contains(&method) {
            let allow_for_origin = self.opt_in_post_origins.contains(&capability.origin);
            if !allow_for_origin {
                return SafetyDecision::Reject(format!(
                    "method '{}' not in auto-replay allowlist (origin '{}' not opted in)",
                    method, capability.origin
                ));
            }
        }

        let url_lower = capability.url_template.to_ascii_lowercase();
        for needle in &self.url_denylist_substrings {
            if url_lower.contains(needle.as_str()) {
                return SafetyDecision::Reject(format!(
                    "URL template matches denylist token '{}'",
                    needle
                ));
            }
        }

        if has_idempotency_key_header(&capability.headers_template) {
            return SafetyDecision::Reject(
                "capability carries an Idempotency-Key header; skipping replay to avoid double-count"
                    .to_string(),
            );
        }

        // Auto-replay fires with no caller-supplied params. Capabilities
        // whose URL template embeds `{placeholders}` (e.g. `/users/{id}`)
        // would fail the `build_replay_request` precondition gate and
        // bloat both the failure metric and orchestrator logs. Reject
        // them here so we only replay list-style endpoints that resolve
        // with an empty param map.
        if has_url_template_placeholder(&capability.url_template) {
            return SafetyDecision::Reject(
                "url template has unfillable placeholder; auto-replay only targets endpoints \
                 that resolve with no parameters"
                    .to_string(),
            );
        }

        if has_header_template_placeholder(&capability.headers_template) {
            return SafetyDecision::Reject(
                "header template has unfillable placeholder; auto-replay cannot fill it without \
                 a sample param map"
                    .to_string(),
            );
        }

        SafetyDecision::Accept
    }
}

/// Returns `true` if the URL template (path + query) contains a
/// `{placeholder}` segment that auto-replay can't fill. Mirrors the
/// detection in `replay::contains_unresolved_placeholders` so the
/// safety filter can short-circuit before the request builder errors.
fn has_url_template_placeholder(template: &str) -> bool {
    template.split('{').skip(1).any(|segment| {
        segment
            .split('}')
            .next()
            .map(|token| !token.is_empty())
            .unwrap_or(false)
    })
}

fn has_header_template_placeholder(headers: &HashMap<String, String>) -> bool {
    headers
        .values()
        .any(|value| has_url_template_placeholder(value))
}

fn has_idempotency_key_header(headers: &HashMap<String, String>) -> bool {
    headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("idempotency-key"))
}

/// Outcome of `replay_to_validate` for a single capability.
#[derive(Debug, Clone, Default)]
pub struct ReplayValidationOutcome {
    /// Replays attempted (post-safety, post-dry-run gating).
    pub attempts: usize,
    /// Replays where verification passed (2xx + schema match).
    pub successes: usize,
    /// Replays where verification failed.
    pub failures: usize,
    /// Replays where the server returned 401/403. These are tracked
    /// separately from `failures` so they don't demote the capability.
    pub auth_failures: usize,
    /// Replays that the safety filter or auth-posture gate rejected
    /// before any HTTP request was emitted.
    pub skipped: usize,
    /// Stable reason strings — one per skipped/aborted run.
    pub skip_reasons: Vec<String>,
    /// Whether the run was a dry-run (no HTTP request actually emitted).
    pub dry_run: bool,
}

/// Fire up to `runs` inline replays for a single capability so the
/// promotion counters accumulate. The caller is responsible for the
/// origin-policy gate; this function enforces the safety filter and
/// auth-posture gate, executes the replays via the supplied `ApiRunner`,
/// and returns a summary outcome.
///
/// Returns early with `attempts = 0` if the capability is not in a
/// state that benefits from extra replays (Observed needs samples, not
/// replays; Trusted is already promoted).
pub async fn replay_to_validate(
    runner: &mut ApiRunner,
    capability: &ApiCapability,
    session: &SessionContext,
    filter: &SafetyFilter,
    runs: usize,
    dry_run: bool,
) -> ReplayValidationOutcome {
    let mut outcome = ReplayValidationOutcome {
        dry_run,
        ..Default::default()
    };

    if runs == 0 {
        return outcome;
    }

    // Only Candidate capabilities benefit from validation replays.
    // Observed capabilities haven't crossed the sample threshold yet;
    // Validated and Trusted are already promoted and the existing
    // router-replay path will keep their counters fresh.
    if !matches!(capability.confidence, ConfidenceLevel::Candidate) {
        outcome.skipped += 1;
        outcome.skip_reasons.push(format!(
            "capability confidence is {:?}; auto-replay only targets Candidate",
            capability.confidence
        ));
        return outcome;
    }

    match filter.accepts(capability) {
        SafetyDecision::Accept => {},
        SafetyDecision::Reject(reason) => {
            outcome.skipped += 1;
            outcome.skip_reasons.push(reason);
            return outcome;
        },
    }

    if let Some(reason) = auth_posture_skip_reason(capability, session) {
        outcome.skipped += 1;
        outcome.skip_reasons.push(reason);
        return outcome;
    }

    if dry_run {
        // Count what *would* fire so dashboards can spot misconfigured
        // filters before operators flip the live switch.
        outcome.attempts = runs;
        return outcome;
    }

    let empty_params: HashMap<String, String> = HashMap::new();
    for _ in 0..runs {
        outcome.attempts += 1;
        let result = runner
            .replay_with_reqwest(
                &capability.origin,
                &capability.id,
                &empty_params,
                session,
                None,
                None,
            )
            .await;
        match result {
            Ok(replay) => {
                if replay.auth_failure {
                    outcome.auth_failures += 1;
                } else if replay.success {
                    outcome.successes += 1;
                } else {
                    outcome.failures += 1;
                    if let Some(err) = replay.error {
                        outcome.skip_reasons.push(err);
                    }
                }
            },
            Err(error) => {
                outcome.failures += 1;
                outcome.skip_reasons.push(error);
            },
        }
    }

    outcome
}

/// Run `replay_to_validate` against every Candidate capability the
/// caller passes in, enforcing a per-pipeline replay budget. Returns
/// the aggregate count of capabilities touched plus the total replays
/// fired (or would-fire under `dry_run`). The caller is responsible for
/// the origin-policy gate before adding a capability to `candidates`.
pub async fn run_pipeline_auto_replay(
    registry_base: &std::path::Path,
    candidates: &[ApiCapability],
    session_lookup: impl Fn(&str) -> Option<SessionContext>,
    filter: &SafetyFilter,
    runs_per_capability: usize,
    max_per_pipeline: usize,
    dry_run: bool,
) -> PipelineAutoReplaySummary {
    let mut summary = PipelineAutoReplaySummary {
        dry_run,
        ..Default::default()
    };

    if candidates.is_empty() || runs_per_capability == 0 || max_per_pipeline == 0 {
        return summary;
    }

    let mut runner = match ApiRunner::with_base_path(registry_base) {
        Ok(runner) => runner,
        Err(error) => {
            summary.error = Some(format!("failed to open ApiRunner: {}", error));
            return summary;
        },
    };

    let mut remaining_budget = max_per_pipeline;
    for cap in candidates {
        if remaining_budget == 0 {
            summary.budget_exhausted = true;
            break;
        }

        let session = session_lookup(&cap.origin).unwrap_or_default();

        let cap_runs = runs_per_capability.min(remaining_budget);
        let outcome =
            replay_to_validate(&mut runner, cap, &session, filter, cap_runs, dry_run).await;

        summary.capabilities_touched += 1;
        summary.attempts += outcome.attempts;
        summary.successes += outcome.successes;
        summary.failures += outcome.failures;
        summary.auth_failures += outcome.auth_failures;
        summary.skipped += outcome.skipped;

        if outcome.attempts > 0 {
            remaining_budget = remaining_budget.saturating_sub(outcome.attempts);
        }
    }

    summary
}

/// Aggregate result for a whole-pipeline auto-replay pass. Suitable for
/// tracing + metrics emission at the orchestrator call site.
#[derive(Debug, Clone, Default)]
pub struct PipelineAutoReplaySummary {
    pub capabilities_touched: usize,
    pub attempts: usize,
    pub successes: usize,
    pub failures: usize,
    pub auth_failures: usize,
    pub skipped: usize,
    pub budget_exhausted: bool,
    pub dry_run: bool,
    pub error: Option<String>,
}

/// If the capability declares auth requirements that the session
/// cannot satisfy, return a stable skip reason. Returning `None` means
/// the auth posture is good enough to proceed.
fn auth_posture_skip_reason(
    capability: &ApiCapability,
    session: &SessionContext,
) -> Option<String> {
    let required_cookies = &capability.auth_requirements.cookies;
    if !required_cookies.is_empty() {
        let missing: Vec<&str> = required_cookies
            .iter()
            .filter(|c| !session.has_cookie_name(c.as_str()))
            .map(|c| c.as_str())
            .collect();
        if !missing.is_empty() {
            return Some(format!("session missing required cookies: {:?}", missing));
        }
    }

    let required_headers = &capability.auth_requirements.headers;
    if !required_headers.is_empty() {
        let missing: Vec<&str> = required_headers
            .iter()
            .filter(|h| !session.auth_headers.contains_key(h.as_str()))
            .map(|h| h.as_str())
            .collect();
        if !missing.is_empty() {
            return Some(format!(
                "session missing required auth headers: {:?}",
                missing
            ));
        }
    }

    let required_local_storage = &capability.auth_requirements.local_storage_keys;
    if !required_local_storage.is_empty() {
        let missing: Vec<&str> = required_local_storage
            .iter()
            .filter(|key| !session.has_local_storage_key(key.as_str()))
            .map(|key| key.as_str())
            .collect();
        if !missing.is_empty() {
            return Some(format!(
                "session missing required localStorage keys: {:?}",
                missing
            ));
        }
    }

    let required_session_storage = &capability.auth_requirements.session_storage_keys;
    if !required_session_storage.is_empty() {
        let missing: Vec<&str> = required_session_storage
            .iter()
            .filter(|key| !session.has_session_storage_key(key.as_str()))
            .map(|key| key.as_str())
            .collect();
        if !missing.is_empty() {
            return Some(format!(
                "session missing required sessionStorage keys: {:?}",
                missing
            ));
        }
    }

    None
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::api_mining::capability::ApiCapability;

    fn cap(method: &str, url: &str) -> ApiCapability {
        ApiCapability::new(
            "test_cap".to_string(),
            "https://api.example.com".to_string(),
            method.to_string(),
            url.to_string(),
        )
    }

    fn candidate(method: &str, url: &str) -> ApiCapability {
        let mut c = cap(method, url);
        c.add_sample("req-1".to_string());
        c.add_sample("req-2".to_string());
        c.add_sample("req-3".to_string());
        c
    }

    #[test]
    fn safety_filter_accepts_safe_get() {
        let f = SafetyFilter::new();
        let c = candidate("GET", "https://api.example.com/items");
        assert_eq!(f.accepts(&c), SafetyDecision::Accept);
    }

    #[test]
    fn safety_filter_rejects_post_by_default() {
        let f = SafetyFilter::new();
        let c = candidate("POST", "https://api.example.com/users");
        match f.accepts(&c) {
            SafetyDecision::Reject(reason) => {
                assert!(reason.contains("not in auto-replay allowlist"), "{reason}")
            },
            _ => panic!("POST should be rejected by default"),
        }
    }

    #[test]
    fn safety_filter_accepts_post_when_origin_opted_in() {
        let f = SafetyFilter::from_lists(
            ["GET".to_string()],
            DEFAULT_URL_DENYLIST.iter().map(|s| s.to_string()),
            ["https://api.example.com".to_string()],
        );
        let c = candidate("POST", "https://api.example.com/items");
        assert_eq!(f.accepts(&c), SafetyDecision::Accept);
    }

    #[test]
    fn safety_filter_rejects_denylisted_url() {
        let f = SafetyFilter::new();
        // /login is in DEFAULT_URL_DENYLIST
        let c = candidate("GET", "https://api.example.com/auth/login");
        match f.accepts(&c) {
            SafetyDecision::Reject(reason) => assert!(reason.contains("denylist"), "{reason}"),
            _ => panic!("login URL should be rejected"),
        }
    }

    #[test]
    fn safety_filter_rejects_idempotency_keyed_request() {
        let f = SafetyFilter::new();
        let mut c = candidate("GET", "https://api.example.com/items");
        c.headers_template
            .insert("Idempotency-Key".to_string(), "{x}".to_string());
        match f.accepts(&c) {
            SafetyDecision::Reject(reason) => {
                assert!(reason.contains("Idempotency-Key"), "{reason}")
            },
            _ => panic!("Idempotency-Key requests should be rejected"),
        }
    }

    #[test]
    fn safety_filter_case_insensitive_denylist_match() {
        let f = SafetyFilter::new();
        let c = candidate("GET", "https://api.example.com/ADMIN/users");
        assert!(matches!(f.accepts(&c), SafetyDecision::Reject(_)));
    }

    #[test]
    fn safety_filter_rejects_url_template_with_placeholder() {
        let f = SafetyFilter::new();
        // `/users/{id}` cannot resolve with auto-replay's empty param
        // map; reject before the request builder errors.
        let c = candidate("GET", "https://api.example.com/api/users/{id}");
        match f.accepts(&c) {
            SafetyDecision::Reject(reason) => {
                assert!(reason.contains("placeholder"), "{reason}")
            },
            _ => panic!("URL with {{placeholder}} should be rejected"),
        }
    }

    #[test]
    fn safety_filter_rejects_header_template_with_placeholder() {
        let f = SafetyFilter::new();
        let mut c = candidate("GET", "https://api.example.com/items");
        c.headers_template
            .insert("x-api-key".to_string(), "{api_key}".to_string());
        match f.accepts(&c) {
            SafetyDecision::Reject(reason) => {
                assert!(reason.contains("header"), "{reason}")
            },
            _ => panic!("header template with {{placeholder}} should be rejected"),
        }
    }

    #[test]
    fn safety_filter_accepts_url_template_without_placeholders() {
        let f = SafetyFilter::new();
        // List endpoints (no path params) are the auto-replay sweet
        // spot; this is the happy path we don't want to regress.
        let c = candidate("GET", "https://api.example.com/api/items?cursor=abc");
        assert_eq!(f.accepts(&c), SafetyDecision::Accept);
    }

    #[test]
    fn safety_filter_floor_cannot_be_subtracted_by_operator_lists() {
        // Operator passes empty lists thinking they're "disabling
        // restrictions" — the hard-coded safety floor must survive.
        let f = SafetyFilter::from_lists(
            std::iter::empty::<String>(),
            std::iter::empty::<String>(),
            std::iter::empty::<String>(),
        );
        // POST is not in DEFAULT_ALLOWED_METHODS → must be rejected.
        let post_cap = candidate("POST", "https://api.example.com/items");
        assert!(matches!(f.accepts(&post_cap), SafetyDecision::Reject(_)));

        // /payment is in DEFAULT_URL_DENYLIST → must be rejected.
        let pay_cap = candidate("GET", "https://api.example.com/payment/initiate");
        assert!(matches!(f.accepts(&pay_cap), SafetyDecision::Reject(_)));

        // Safe GET still accepted.
        let safe_cap = candidate("GET", "https://api.example.com/items");
        assert_eq!(f.accepts(&safe_cap), SafetyDecision::Accept);
    }

    #[test]
    fn safety_filter_operator_can_add_to_url_denylist() {
        // Custom needle (`/internal`) is not in DEFAULT_URL_DENYLIST.
        // Operator adds it; both the custom needle AND the defaults
        // are enforced.
        let f = SafetyFilter::from_lists(
            std::iter::empty::<String>(),
            std::iter::once("/internal".to_string()),
            std::iter::empty::<String>(),
        );
        let custom_block = candidate("GET", "https://api.example.com/internal/secrets");
        assert!(matches!(
            f.accepts(&custom_block),
            SafetyDecision::Reject(_)
        ));

        let default_block = candidate("GET", "https://api.example.com/admin/users");
        assert!(matches!(
            f.accepts(&default_block),
            SafetyDecision::Reject(_)
        ));
    }

    #[tokio::test]
    async fn replay_to_validate_dry_run_counts_attempts_without_firing() {
        // No network call — runner is unused on dry-run, but the
        // public API requires one. We use a tempdir so the registry
        // setup succeeds even though no replays will fire.
        let temp = tempfile::TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
        // Placeholder-less URL so the safety filter accepts it (path
        // params are auto-replay's biggest unfillable gap).
        let c = candidate("GET", "https://api.example.com/items");
        let outcome = replay_to_validate(
            &mut runner,
            &c,
            &SessionContext::default(),
            &SafetyFilter::new(),
            3,
            true,
        )
        .await;
        assert!(outcome.dry_run);
        assert_eq!(outcome.attempts, 3);
        assert_eq!(outcome.successes, 0);
        assert_eq!(outcome.failures, 0);
        assert_eq!(outcome.skipped, 0);
    }

    #[tokio::test]
    async fn replay_to_validate_skips_when_safety_rejects() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
        let c = candidate("POST", "https://api.example.com/users");
        let outcome = replay_to_validate(
            &mut runner,
            &c,
            &SessionContext::default(),
            &SafetyFilter::new(),
            3,
            true,
        )
        .await;
        assert_eq!(outcome.attempts, 0);
        assert_eq!(outcome.skipped, 1);
        assert_eq!(outcome.skip_reasons.len(), 1);
    }

    #[tokio::test]
    async fn replay_to_validate_skips_when_session_lacks_required_cookies() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
        let mut c = candidate("GET", "https://api.example.com/items");
        c.auth_requirements.cookies = vec!["SID".to_string()];
        let outcome = replay_to_validate(
            &mut runner,
            &c,
            &SessionContext::default(),
            &SafetyFilter::new(),
            3,
            true,
        )
        .await;
        assert_eq!(outcome.attempts, 0);
        assert_eq!(outcome.skipped, 1);
        assert!(
            outcome.skip_reasons[0].contains("missing required cookies"),
            "{:?}",
            outcome.skip_reasons
        );
    }

    #[tokio::test]
    async fn replay_to_validate_skips_when_session_lacks_local_storage() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
        let mut c = candidate("GET", "https://api.example.com/items");
        c.auth_requirements.local_storage_keys = vec!["auth-token".to_string()];
        let outcome = replay_to_validate(
            &mut runner,
            &c,
            &SessionContext::default(),
            &SafetyFilter::new(),
            3,
            true,
        )
        .await;
        assert_eq!(outcome.attempts, 0);
        assert_eq!(outcome.skipped, 1);
        assert!(
            outcome.skip_reasons[0].contains("localStorage"),
            "{:?}",
            outcome.skip_reasons
        );
    }

    #[tokio::test]
    async fn replay_to_validate_skips_observed_capabilities() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
        let c = cap("GET", "https://api.example.com/items"); // 1 sample, Observed
        let outcome = replay_to_validate(
            &mut runner,
            &c,
            &SessionContext::default(),
            &SafetyFilter::new(),
            3,
            true,
        )
        .await;
        assert_eq!(outcome.attempts, 0);
        assert_eq!(outcome.skipped, 1);
    }
}
