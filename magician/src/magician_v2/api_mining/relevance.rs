//! Relevance of mined endpoints to the results tasks actually produce.
//!
//! Recipe use outranks structure; structure outranks endpoint names. Telemetry
//! is retained in redacted traces for aggregate diagnostics but is never
//! promoted into a capability.

use super::noise_filter::{has_meaningful_json_body, is_beacon_like, NoiseFilter};
use super::types::NetworkTraceEvent;
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum Relevance {
    #[default]
    Unclassified,
    Telemetry,
    ThirdPartyApi,
    FirstPartyApi,
    Dependency,
    AnswerBearing,
}

impl Relevance {
    pub fn shown_by_default(self) -> bool {
        matches!(
            self,
            Self::AnswerBearing | Self::Dependency | Self::FirstPartyApi
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Telemetry => "telemetry",
            Self::ThirdPartyApi => "third_party_api",
            Self::FirstPartyApi => "first_party_api",
            Self::Dependency => "dependency",
            Self::AnswerBearing => "answer_bearing",
        }
    }
}

impl std::str::FromStr for Relevance {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unclassified" => Ok(Self::Unclassified),
            "telemetry" => Ok(Self::Telemetry),
            "third_party_api" => Ok(Self::ThirdPartyApi),
            "first_party_api" => Ok(Self::FirstPartyApi),
            "dependency" => Ok(Self::Dependency),
            "answer_bearing" => Ok(Self::AnswerBearing),
            other => Err(format!("unknown API relevance `{other}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRole {
    Answer,
    Dependency,
}

#[derive(Debug, Clone)]
pub struct Classification {
    pub relevance: Relevance,
    pub reason: String,
}

const ANALYTICS_INITIATOR_HOSTS: &[&str] = &[
    "googletagmanager.com",
    "google-analytics.com",
    "segment.com",
    "segment.io",
    "posthog.com",
    "hotjar.com",
    "clarity.ms",
    "mixpanel.com",
    "amplitude.com",
    "sentry.io",
    "datadoghq.com",
    "newrelic.com",
    "hubspot.com",
    "hs-scripts.com",
    "intercom.io",
    "fullstory.com",
    "doubleclick.net",
    "facebook.net",
    "ads.",
];

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(|host| host.to_ascii_lowercase())
}

fn registrable_domain(host: &str) -> String {
    let labels: Vec<_> = host.split('.').collect();
    let count = labels.len();
    if count <= 2 {
        return host.to_owned();
    }
    let two_level_suffix = matches!(
        labels[count - 2],
        "co" | "com" | "org" | "net" | "gov" | "ac" | "edu"
    ) && labels[count - 1].len() == 2;
    let take = if two_level_suffix { 3 } else { 2 };
    labels[count - take..].join(".")
}

pub fn page_origin_for_trace(trace: &NetworkTraceEvent) -> String {
    let header_origin = trace
        .request_headers
        .iter()
        .find(|(name, _)| {
            name.eq_ignore_ascii_case("referer") || name.eq_ignore_ascii_case("origin")
        })
        .map(|(_, value)| value.as_str());
    header_origin
        .or(trace.initiator.url.as_deref())
        .and_then(|value| {
            let parsed = url::Url::parse(value).ok()?;
            Some(format!(
                "{}://{}{}",
                parsed.scheme(),
                parsed.host_str()?,
                parsed
                    .port()
                    .map(|port| format!(":{port}"))
                    .unwrap_or_default()
            ))
        })
        .unwrap_or_else(|| super::router::extract_origin(&trace.url))
}

pub fn classify(
    trace: &NetworkTraceEvent,
    page_origin: &str,
    parent_origin: Option<&str>,
    noise: &NoiseFilter,
) -> Classification {
    let content_type = trace
        .response_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.to_ascii_lowercase())
        .unwrap_or_default();
    let resource_type = trace
        .resource_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let body_len = trace
        .response_body
        .as_ref()
        .map_or(trace.response_size, |body| body.len() as u64);

    if matches!(resource_type.as_str(), "ping" | "beacon") {
        return telemetry(format!("resource type {resource_type}"));
    }
    if content_type.starts_with("image/") || trace.url.contains(".gif?") {
        return telemetry("pixel response");
    }
    if is_beacon_like(&trace.method, trace.status, body_len) {
        return telemetry(format!(
            "{} {} with an empty response",
            trace.method, trace.status
        ));
    }
    if let Some(initiator) = trace.initiator.url.as_deref().and_then(host_of) {
        if ANALYTICS_INITIATOR_HOSTS
            .iter()
            .any(|host| initiator.contains(host))
        {
            return telemetry(format!("initiated by analytics script {initiator}"));
        }
    }

    let meaningful_json = has_meaningful_json_body(trace);
    if noise.is_noise_host(&trace.url) {
        return telemetry("telemetry host");
    }
    if noise.is_noise_vendor_path(&trace.url) {
        return telemetry("vendor SDK path");
    }
    if !meaningful_json && noise.is_noise_path(&trace.url) {
        return telemetry("telemetry path without a meaningful JSON response");
    }
    if noise.is_noise_for(&trace.url, meaningful_json) {
        return telemetry("operator or managed noise-filter match");
    }
    if !meaningful_json {
        return Classification {
            relevance: Relevance::Unclassified,
            reason: "no meaningful JSON response".into(),
        };
    }

    let Some(host) = host_of(&trace.url) else {
        return Classification {
            relevance: Relevance::Unclassified,
            reason: "unparseable URL".into(),
        };
    };
    let page_host = host_of(page_origin)
        .or_else(|| parent_origin.and_then(host_of))
        .unwrap_or_default();
    let same_site = !page_host.is_empty()
        && (host == page_host || registrable_domain(&host) == registrable_domain(&page_host));
    let first_party_initiator = trace
        .initiator
        .url
        .as_deref()
        .and_then(host_of)
        .is_some_and(|initiator| {
            !page_host.is_empty()
                && registrable_domain(&initiator) == registrable_domain(&page_host)
        });
    if same_site {
        Classification {
            relevance: Relevance::FirstPartyApi,
            reason: format!("same site as {page_host}"),
        }
    } else {
        Classification {
            relevance: Relevance::ThirdPartyApi,
            reason: if first_party_initiator {
                format!("third-party API called by {page_host}'s script")
            } else {
                "third-party JSON API".into()
            },
        }
    }
}

fn telemetry(reason: impl Into<String>) -> Classification {
    Classification {
        relevance: Relevance::Telemetry,
        reason: reason.into(),
    }
}

/// Actual use by a recipe always wins over structural classification.
pub fn promote(current: Relevance, role: StepRole) -> Relevance {
    match role {
        StepRole::Answer => Relevance::AnswerBearing,
        StepRole::Dependency => current.max(Relevance::Dependency),
    }
}

pub const UNUSED_HIDE_AFTER_SECS: i64 = 30 * 24 * 60 * 60;

pub fn should_hide_unused(
    relevance: Relevance,
    used_by_recipe_ids: &[String],
    age_secs: i64,
) -> bool {
    should_hide_unused_after(
        relevance,
        used_by_recipe_ids,
        age_secs,
        UNUSED_HIDE_AFTER_SECS,
    )
}

pub fn should_hide_unused_after(
    _relevance: Relevance,
    used_by_recipe_ids: &[String],
    age_secs: i64,
    hide_after_secs: i64,
) -> bool {
    used_by_recipe_ids.is_empty() && age_secs >= hide_after_secs.max(0)
}

/// Promote every capability linked by the current recipe version and maintain
/// the reverse "used by" relation consumed by registry/API views.
pub fn link_recipe_capabilities(
    mining_base: &std::path::Path,
    recipe: &super::recipe::TaskRecipe,
) -> Result<usize, String> {
    use super::registry::CapabilityRegistry;
    use std::collections::HashSet;

    let Some(version) = recipe.current() else {
        return Ok(0);
    };
    let answer_steps: HashSet<_> = version
        .answer_spec
        .iter()
        .map(|answer| answer.step_id.as_str())
        .collect();
    let mut seen = HashSet::new();
    let mut registry = CapabilityRegistry::with_base_path(mining_base)?;
    let mut linked = 0;
    for step in &version.steps {
        let Some(capability_id) = step.capability_id.as_deref() else {
            continue;
        };
        if !seen.insert((step.origin.as_str(), capability_id)) {
            continue;
        }
        let mut capability = match registry.get_capability(&step.origin, capability_id) {
            Ok(capability) => capability,
            Err(error) => {
                tracing::warn!(
                    recipe_id = %recipe.id,
                    origin = %step.origin,
                    capability_id,
                    %error,
                    "recipe capability relevance link could not load capability"
                );
                continue;
            },
        };
        let role = if answer_steps.contains(step.id.as_str()) {
            StepRole::Answer
        } else {
            StepRole::Dependency
        };
        capability.relevance = promote(capability.relevance, role);
        capability.relevance_reason = Some(match role {
            StepRole::Answer => format!("answer-bearing step in recipe {}", recipe.id),
            StepRole::Dependency => format!("dependency step in recipe {}", recipe.id),
        });
        if !capability
            .used_by_recipe_ids
            .iter()
            .any(|id| id == &recipe.id)
        {
            capability.used_by_recipe_ids.push(recipe.id.clone());
            capability.used_by_recipe_ids.sort_unstable();
        }
        capability.hidden = false;
        registry.register(&capability)?;
        linked += 1;
    }
    Ok(linked)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn default_view_is_use_first_and_first_party_only() {
        assert!(Relevance::AnswerBearing.shown_by_default());
        assert!(Relevance::Dependency.shown_by_default());
        assert!(Relevance::FirstPartyApi.shown_by_default());
        assert!(!Relevance::ThirdPartyApi.shown_by_default());
        assert!(!Relevance::Telemetry.shown_by_default());
        assert!(!Relevance::Unclassified.shown_by_default());
        assert_eq!(
            promote(Relevance::Telemetry, StepRole::Answer),
            Relevance::AnswerBearing
        );
    }

    #[test]
    fn unused_hide_rule_is_class_agnostic() {
        assert!(should_hide_unused(
            Relevance::FirstPartyApi,
            &[],
            UNUSED_HIDE_AFTER_SECS
        ));
        assert!(!should_hide_unused(
            Relevance::ThirdPartyApi,
            &["rcp_1".into()],
            UNUSED_HIDE_AFTER_SECS * 2
        ));
        assert!(should_hide_unused_after(
            Relevance::Unclassified,
            &[],
            7 * 24 * 60 * 60,
            7 * 24 * 60 * 60,
        ));
    }
}
