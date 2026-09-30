//! Passive validation of browser-observed XHR/Fetch traces.
//!
//! This pass never replaces browser execution. It routes already-captured
//! XHR/Fetch traffic through the relaxed validation router, replays matching
//! requests in the background, and compares the replay response with the
//! browser-observed response. Successful matches count as replay evidence for
//! capability promotion; mismatches count as replay failures.

use std::collections::HashMap;
use std::path::Path;

use url::Url;

use crate::config::ApiMiningConfig;

use super::capability::ConfidenceLevel;
use super::metrics::PassiveValidationMetrics;
use super::miner::{compute_body_fingerprint_standalone, detect_graphql_request_info_standalone};
use super::origin_policy::OriginPolicyStore;
use super::replay::{ApiRunner, ReplayRequest};
use super::router::{extract_origin, ApiRouter, ValidationDecision};
use super::types::{NetworkTraceEvent, SessionContext};

#[derive(Debug, Clone, Default)]
pub struct PassiveValidationSummary {
    pub observed_xhr_fetch: usize,
    pub matched_capabilities: usize,
    pub validations_fired: usize,
    pub validations_passed: usize,
    pub validations_failed: usize,
    pub skipped_non_xhr_fetch: usize,
    pub skipped_not_first_party: usize,
    pub skipped_no_match: usize,
    pub skipped_policy: usize,
    pub skipped_missing_response: usize,
    pub errors: usize,
    pub promoted_to_validated: usize,
    pub promoted_to_trusted: usize,
    pub budget_exhausted: usize,
    pub touched_capabilities: Vec<PassiveValidationTouchedCapability>,
    pub details: Vec<PassiveValidationDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassiveValidationTouchedCapability {
    pub origin: String,
    pub capability_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassiveValidationDetail {
    pub trace_id: String,
    pub origin: String,
    pub capability_id: String,
    pub matched: bool,
    pub api_status: u16,
    pub browser_status: u16,
    pub schema_match: bool,
    pub timing_ms: u64,
    pub detail: String,
}

/// Run a bounded passive validation pass over browser-observed XHR/Fetch traces.
pub async fn run_passive_validation<F>(
    base_path: &Path,
    config: &ApiMiningConfig,
    traces: &[NetworkTraceEvent],
    origin_policy: &OriginPolicyStore,
    mut session_lookup: F,
    metrics: &PassiveValidationMetrics,
) -> PassiveValidationSummary
where
    F: FnMut(&str, &str) -> Option<SessionContext>,
{
    let mut summary = PassiveValidationSummary::default();

    if !config.enable_xhr_validation {
        return summary;
    }

    let mut router = ApiRouter::with_base_path(config, base_path);
    router.refresh_registry();

    let mut runner = match ApiRunner::with_base_path(base_path) {
        Ok(runner) => runner,
        Err(error) => {
            metrics.record_error();
            summary.errors += 1;
            tracing::warn!(
                target: "magician::api_mining",
                error = %error,
                "api_mining.passive_validation.runner_open_failed"
            );
            return summary;
        },
    };

    let budget = config.xhr_validation_max_per_pipeline;

    for trace in traces {
        if !is_xhr_fetch_trace(trace) {
            metrics.record_skipped_non_xhr_fetch();
            summary.skipped_non_xhr_fetch += 1;
            continue;
        }

        metrics.record_observed_xhr_fetch();
        summary.observed_xhr_fetch += 1;

        if !is_first_party_trace(trace) {
            metrics.record_skipped_not_first_party();
            summary.skipped_not_first_party += 1;
            continue;
        }

        if trace.status == 0 {
            metrics.record_skipped_missing_response();
            summary.skipped_missing_response += 1;
            continue;
        }

        let origin = extract_origin(&trace.url);
        let content_type = header_value(&trace.request_headers, "content-type");
        let graphql_info =
            detect_graphql_request_info_standalone(trace.request_body.as_deref(), content_type);
        let body_fingerprint = compute_body_fingerprint_standalone(
            trace.request_body.as_deref(),
            content_type,
            graphql_info.is_some(),
        );

        let session = session_lookup(&origin, &trace.url).unwrap_or_default();
        let decision = router.route_xhr_for_validation_with_context(
            &trace.method,
            &trace.url,
            body_fingerprint.as_deref(),
            graphql_info
                .as_ref()
                .map(|info| info.operation_name.as_str()),
            graphql_info.as_ref().map(|info| info.operation_kind),
            &session,
        );

        let ValidationDecision::Validate {
            mut request,
            capability_id,
            origin,
            confidence,
        } = decision
        else {
            metrics.record_skipped_no_match();
            summary.skipped_no_match += 1;
            continue;
        };

        metrics.record_matched_capability();
        summary.matched_capabilities += 1;

        let _capability = match runner.registry().get_capability(&origin, &capability_id) {
            Ok(capability) => capability,
            Err(error) => {
                metrics.record_error();
                summary.errors += 1;
                tracing::warn!(
                    target: "magician::api_mining",
                    origin = %origin,
                    capability_id = %capability_id,
                    error = %error,
                    "api_mining.passive_validation.capability_load_failed"
                );
                continue;
            },
        };

        if !origin_policy.allows_passive_validation_for_origin(&origin) {
            metrics.record_skipped_policy();
            summary.skipped_policy += 1;
            continue;
        }

        if summary.validations_fired >= budget {
            metrics.record_budget_exhausted();
            summary.budget_exhausted += 1;
            break;
        }

        let before_confidence = confidence;
        attach_observed_request_body(&mut request, trace);

        metrics.record_validation_fired();
        summary.validations_fired += 1;

        match runner
            .validate_with_reqwest(
                &origin,
                &capability_id,
                &request,
                trace.status,
                trace.response_body.as_deref(),
            )
            .await
        {
            Ok(result) => {
                if result.matched {
                    metrics.record_validation_passed();
                    summary.validations_passed += 1;
                } else {
                    metrics.record_validation_failed();
                    summary.validations_failed += 1;
                }

                if let Ok(refreshed) = runner.registry().get_capability(&origin, &capability_id) {
                    if before_confidence < ConfidenceLevel::Validated
                        && refreshed.confidence >= ConfidenceLevel::Validated
                    {
                        metrics.record_promoted_to_validated();
                        summary.promoted_to_validated += 1;
                    }
                    if before_confidence < ConfidenceLevel::Trusted
                        && refreshed.confidence >= ConfidenceLevel::Trusted
                    {
                        metrics.record_promoted_to_trusted();
                        summary.promoted_to_trusted += 1;
                    }
                }

                summary
                    .touched_capabilities
                    .push(PassiveValidationTouchedCapability {
                        origin: origin.clone(),
                        capability_id: capability_id.clone(),
                    });
                summary.details.push(PassiveValidationDetail {
                    trace_id: trace.request_id.clone(),
                    origin: origin.clone(),
                    capability_id: capability_id.clone(),
                    matched: result.matched,
                    api_status: result.api_status,
                    browser_status: result.browser_status,
                    schema_match: result.schema_match,
                    timing_ms: result.timing_ms,
                    detail: result.detail.clone(),
                });

                tracing::info!(
                    target: "magician::api_mining",
                    trace_id = %trace.request_id,
                    origin = %origin,
                    capability_id = %capability_id,
                    matched = result.matched,
                    api_status = result.api_status,
                    browser_status = result.browser_status,
                    schema_match = result.schema_match,
                    timing_ms = result.timing_ms,
                    detail = %result.detail,
                    "api_mining.passive_validation.result"
                );
            },
            Err(error) => {
                metrics.record_error();
                summary.errors += 1;
                tracing::warn!(
                    target: "magician::api_mining",
                    trace_id = %trace.request_id,
                    origin = %origin,
                    capability_id = %capability_id,
                    error = %error,
                    "api_mining.passive_validation.failed"
                );
            },
        }
    }

    dedupe_touched_capabilities(&mut summary.touched_capabilities);
    summary
}

fn attach_observed_request_body(request: &mut ReplayRequest, trace: &NetworkTraceEvent) {
    if method_allows_body(&trace.method) {
        request.body = trace.request_body.clone();
    }
}

fn method_allows_body(method: &str) -> bool {
    !matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD")
}

fn is_xhr_fetch_trace(trace: &NetworkTraceEvent) -> bool {
    trace
        .resource_type
        .as_deref()
        .map(|kind| kind.eq_ignore_ascii_case("xhr") || kind.eq_ignore_ascii_case("fetch"))
        .unwrap_or(false)
}

fn is_first_party_trace(trace: &NetworkTraceEvent) -> bool {
    let Some(request_url) = Url::parse(&trace.url).ok() else {
        return false;
    };
    let Some(request_host) = request_url.host_str() else {
        return false;
    };

    let mut evidence_hosts = Vec::new();
    if let Some(origin) = header_value(&trace.request_headers, "origin") {
        if let Some(host) = parse_host(origin) {
            evidence_hosts.push(host);
        }
    }
    if let Some(referer) = header_value(&trace.request_headers, "referer") {
        if let Some(host) = parse_host(referer) {
            evidence_hosts.push(host);
        }
    }
    if let Some(initiator_url) = trace.initiator.url.as_deref() {
        if let Some(host) = parse_host(initiator_url) {
            evidence_hosts.push(host);
        }
    }

    if evidence_hosts.is_empty() {
        return true;
    }

    evidence_hosts
        .iter()
        .any(|host| host_is_same_party(request_host, host))
}

fn parse_host(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn host_is_same_party(left: &str, right: &str) -> bool {
    let left = left.trim_end_matches('.').to_ascii_lowercase();
    let right = right.trim_end_matches('.').to_ascii_lowercase();
    if left == right {
        return true;
    }
    if left.ends_with(&format!(".{right}")) || right.ends_with(&format!(".{left}")) {
        return true;
    }
    same_registrable_suffix(&left, &right)
}

fn same_registrable_suffix(left: &str, right: &str) -> bool {
    if shared_suffix(left, 3)
        .zip(shared_suffix(right, 3))
        .map(|(left_suffix, right_suffix)| left_suffix == right_suffix)
        .unwrap_or(false)
    {
        return true;
    }

    let Some(left_suffix) = shared_suffix(left, 2) else {
        return false;
    };
    let Some(right_suffix) = shared_suffix(right, 2) else {
        return false;
    };
    if left_suffix != right_suffix {
        return false;
    }

    let first_label = left_suffix.split('.').next().unwrap_or_default();
    !matches!(
        first_label,
        "ac" | "co" | "com" | "edu" | "gov" | "net" | "org"
    )
}

fn shared_suffix(host: &str, labels: usize) -> Option<String> {
    let parts: Vec<&str> = host.split('.').filter(|part| !part.is_empty()).collect();
    if parts.len() < labels {
        return None;
    }
    Some(parts[parts.len() - labels..].join("."))
}

fn dedupe_touched_capabilities(touched: &mut Vec<PassiveValidationTouchedCapability>) {
    touched.sort_by(|left, right| {
        left.origin
            .cmp(&right.origin)
            .then(left.capability_id.cmp(&right.capability_id))
    });
    touched.dedup_by(|left, right| {
        left.origin == right.origin && left.capability_id == right.capability_id
    });
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::types::{RequestInitiator, RequestTiming};
    use super::*;

    fn trace_with_resource(resource_type: Option<&str>) -> NetworkTraceEvent {
        NetworkTraceEvent {
            request_id: "req-1".to_string(),
            method: "GET".to_string(),
            url: "https://api.example.com/v1/search".to_string(),
            resource_type: resource_type.map(str::to_string),
            frame_id: None,
            tab_id: None,
            thread_id: None,
            request_headers: HashMap::new(),
            request_body: None,
            response_headers: HashMap::new(),
            response_body: Some(r#"{"ok":true}"#.to_string()),
            body_unavailable_reason: None,
            failure_error_text: None,
            failure_blocked_reason: None,
            failure_canceled: None,
            status: 200,
            timing: RequestTiming {
                request_time: 0.0,
                dns_duration: None,
                connect_duration: None,
                ssl_duration: None,
                ttfb: None,
                total_duration: 1.0,
            },
            initiator: RequestInitiator {
                initiator_type: "script".to_string(),
                stack: None,
                url: None,
            },
            timestamp: 0,
            request_size: 0,
            response_size: 11,
            capture_source: Some("cdp_proxy".to_string()),
        }
    }

    #[test]
    fn passive_validation_only_accepts_xhr_fetch_resources() {
        assert!(is_xhr_fetch_trace(&trace_with_resource(Some("XHR"))));
        assert!(is_xhr_fetch_trace(&trace_with_resource(Some("fetch"))));
        assert!(!is_xhr_fetch_trace(&trace_with_resource(Some("Document"))));
        assert!(!is_xhr_fetch_trace(&trace_with_resource(None)));
    }

    #[test]
    fn first_party_guard_accepts_related_subdomains_and_rejects_unrelated_hosts() {
        let mut same_party = trace_with_resource(Some("XHR"));
        same_party.request_headers.insert(
            "Referer".to_string(),
            "https://app.example.com/page".to_string(),
        );
        assert!(is_first_party_trace(&same_party));

        let mut country_suffix = trace_with_resource(Some("XHR"));
        country_suffix.url = "https://api.zepto.co.in/search".to_string();
        country_suffix
            .request_headers
            .insert("Origin".to_string(), "https://app.zepto.co.in".to_string());
        assert!(is_first_party_trace(&country_suffix));

        let mut third_party = trace_with_resource(Some("XHR"));
        third_party.request_headers.insert(
            "Referer".to_string(),
            "https://analytics-vendor.example.net/pixel".to_string(),
        );
        assert!(!is_first_party_trace(&third_party));
    }
}
