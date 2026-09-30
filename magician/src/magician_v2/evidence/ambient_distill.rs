//! Ambient-signal distillation (WEG Phase 2, P2.3b).
//!
//! Windowed roll-up of raw ambient browser signals (read from the analytics raw
//! store by the API layer) into **user-owned** evidence. High-volume page
//! signals are grouped by origin/day, salience-gated deterministically, and only
//! promotable clusters are distilled (one LLM call per cluster) into an
//! [`EvidenceRecord`] tagged `producer = "ambient_browser"`.
//!
//! Idempotent: a cluster's `evidence_id` is `evd:amb:{host}:{day}`, so
//! re-running the distiller for the same window upserts rather than duplicates.

use std::collections::HashMap;

use serde::Deserialize;

use super::{normalize_sensitivity, EvidenceProposal, EvidenceRecord, EvidenceStatus, Facet};
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter, SimplifiedLLMResponse,
};
use magicllm::LLMProviderKind;

/// Minimum cluster salience to promote into evidence.
pub const AMBIENT_SALIENCE_THRESHOLD: f64 = 0.45;

/// One ambient signal read back from the analytics raw store (the payload of an
/// `event_type = "ambient_signal"` row).
#[derive(Debug, Clone, Deserialize)]
pub struct AmbientSignalRow {
    pub signal_id: String,
    pub origin: String,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub event_kind: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub safe_url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub page_key: Option<String>,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub sensitivity: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub payload_bytes: u64,
    #[serde(default)]
    pub metadata_bytes: u64,
    #[serde(default)]
    pub dom_estimated_bytes: u64,
    #[serde(default)]
    pub has_password_field: bool,
    #[serde(default)]
    pub heading_count: u64,
}

#[derive(Debug, Clone)]
pub struct AmbientDistillLlmOutcome {
    pub proposal: EvidenceProposal,
    pub response: SimplifiedLLMResponse,
}

/// A salience-scored cluster of signals for one origin within the window.
#[derive(Debug, Clone)]
pub struct SignalCluster {
    pub host: String,
    pub origin: String,
    pub signal_ids: Vec<String>,
    pub event_kinds: Vec<String>,
    pub summaries: Vec<String>,
    pub count: usize,
    pub salience: f64,
}

/// Host portion of an origin (scheme + path stripped).
fn host_of(origin: &str) -> String {
    origin
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(origin)
        .to_lowercase()
}

/// Known work surfaces get a salience boost (the design's "trusted work tools").
const WORK_HOST_HINTS: &[&str] = &[
    "github",
    "gitlab",
    "jira",
    "atlassian",
    "linear",
    "notion",
    "figma",
    "docs.google",
    "sheets.google",
    "slack",
    "confluence",
    "vercel",
    "netlify",
    "asana",
    "trello",
    "monday",
    "datadog",
    "grafana",
    "sentry",
    "pagerduty",
];

/// Higher-intent actions outrank passive views.
fn action_weight(event_kind: &str) -> f64 {
    let k = event_kind.to_lowercase();
    if [
        "submit", "send", "edit", "upload", "comment", "review", "approve", "create",
    ]
    .iter()
    .any(|a| k.contains(a))
    {
        1.0
    } else if k.contains("form") || k.contains("click") {
        0.6
    } else {
        0.2 // page_change / view / load
    }
}

/// Pure noise origins to down-rank hard.
fn is_noise_host(host: &str) -> bool {
    [
        "accounts.google",
        "login.",
        "auth.",
        "sso.",
        "localhost",
        "127.0.0.1",
    ]
    .iter()
    .any(|n| host.contains(n))
}

/// Group signals by host and score salience. Salience blends the strongest
/// observed action, engagement (signal count), and a work-tool boost, minus a
/// noise penalty — so a single form submit on a work tool beats dozens of
/// idle page loads, and login/SSO churn is suppressed.
pub fn cluster_signals(rows: &[AmbientSignalRow]) -> Vec<SignalCluster> {
    let mut by_host: HashMap<String, SignalCluster> = HashMap::new();
    for row in rows {
        let host = host_of(&row.origin);
        if host.is_empty() {
            continue;
        }
        let cluster = by_host
            .entry(host.clone())
            .or_insert_with(|| SignalCluster {
                host: host.clone(),
                origin: row.origin.clone(),
                signal_ids: Vec::new(),
                event_kinds: Vec::new(),
                summaries: Vec::new(),
                count: 0,
                salience: 0.0,
            });
        cluster.signal_ids.push(row.signal_id.clone());
        if let Some(kind) = &row.event_kind {
            cluster.event_kinds.push(kind.clone());
        }
        if let Some(summary) = &row.summary {
            if !summary.trim().is_empty() {
                cluster.summaries.push(summary.clone());
            }
        }
        cluster.count += 1;
    }

    let mut clusters: Vec<SignalCluster> = by_host.into_values().collect();
    for c in &mut clusters {
        let max_action = c
            .event_kinds
            .iter()
            .map(|k| action_weight(k))
            .fold(0.2_f64, f64::max);
        let engagement = ((c.count as f64).min(10.0) / 10.0) * 0.4;
        let work_boost = if WORK_HOST_HINTS.iter().any(|h| c.host.contains(h)) {
            0.25
        } else {
            0.0
        };
        let noise_penalty = if is_noise_host(&c.host) { 0.6 } else { 0.0 };
        c.salience = (max_action + engagement + work_boost - noise_penalty).clamp(0.0, 1.0);
    }
    clusters.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    clusters
}

pub fn is_cluster_salient(cluster: &SignalCluster) -> bool {
    cluster.salience >= AMBIENT_SALIENCE_THRESHOLD
}

/// Distill one promotable cluster into an evidence proposal via the
/// `ambient_distill` op. Shared shape with episode distillation (reuses
/// [`EvidenceProposal`]).
pub async fn distill_ambient_cluster(
    cluster: &SignalCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<EvidenceProposal> {
    distill_ambient_cluster_with_profile(cluster, router, prompt_manager, None, None).await
}

/// Same distillation contract as [`distill_ambient_cluster`], but pins dispatch
/// to a previously verified local profile. This is used by the Observe-tabs
/// lifecycle worker and endpoint so ambient browsing content never falls through
/// to a remote/default profile after the locality guard has passed.
pub async fn distill_ambient_cluster_pinned(
    cluster: &SignalCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    pinned_profile: &str,
    required_provider_kind: LLMProviderKind,
) -> anyhow::Result<EvidenceProposal> {
    Ok(distill_ambient_cluster_pinned_with_response(
        cluster,
        router,
        prompt_manager,
        pinned_profile,
        required_provider_kind,
    )
    .await?
    .proposal)
}

pub async fn distill_ambient_cluster_pinned_with_response(
    cluster: &SignalCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    pinned_profile: &str,
    required_provider_kind: LLMProviderKind,
) -> anyhow::Result<AmbientDistillLlmOutcome> {
    distill_ambient_cluster_with_profile_and_response(
        cluster,
        router,
        prompt_manager,
        Some(pinned_profile),
        Some(required_provider_kind),
    )
    .await
}

async fn distill_ambient_cluster_with_profile(
    cluster: &SignalCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    pinned_profile: Option<&str>,
    required_provider_kind: Option<LLMProviderKind>,
) -> anyhow::Result<EvidenceProposal> {
    Ok(distill_ambient_cluster_with_profile_and_response(
        cluster,
        router,
        prompt_manager,
        pinned_profile,
        required_provider_kind,
    )
    .await?
    .proposal)
}

async fn distill_ambient_cluster_with_profile_and_response(
    cluster: &SignalCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    pinned_profile: Option<&str>,
    required_provider_kind: Option<LLMProviderKind>,
) -> anyhow::Result<AmbientDistillLlmOutcome> {
    let packet = serde_json::json!({
        "host": cluster.host,
        "origin": cluster.origin,
        "signal_count": cluster.count,
        "observed_event_kinds": cluster.event_kinds,
        "page_summaries": cluster.summaries,
    });
    let mut vars = HashMap::new();
    vars.insert(
        "signal_cluster_json".to_string(),
        serde_json::to_string_pretty(&packet)?,
    );
    let system = prompt_manager
        .get_rendered_prompt("ambient_distill_system", "1.0.0", HashMap::new())
        .await?;
    let user = prompt_manager
        .get_rendered_prompt("ambient_distill_user", "1.0.0", vars)
        .await?;
    let operation = LLMOperation::Other("ambient_distill".to_string());
    // Evidence proposals are strict JSON: the local arm gets provider-enforced
    // JSON from `metadata.format: json`; a remote `when_cloud` arm needs the
    // format on the request itself.
    let pinned_remote_json = required_provider_kind
        .as_ref()
        .is_some_and(|kind| *kind != LLMProviderKind::Ollama);
    let reviewed = super::decision::review(
        router,
        "ambient_distill",
        &system,
        &user,
        pinned_profile.is_some(),
        None,
        || async {
            match pinned_profile {
                Some(profile) if pinned_remote_json => {
                    router
                        .generate_for_operation_with_system_pinned_and_response_format(
                            &operation,
                            Some(&system),
                            &user,
                            profile,
                            required_provider_kind.clone(),
                            magicllm::LLMResponseFormat::JsonObject,
                        )
                        .await
                },
                Some(profile) => {
                    router
                        .generate_for_operation_with_system_pinned(
                            &operation,
                            Some(&system),
                            &user,
                            profile,
                            required_provider_kind.clone(),
                        )
                        .await
                },
                None => {
                    router
                        .generate_for_operation_with_system(&operation, Some(&system), &user)
                        .await
                },
            }
        },
    )
    .await?;
    Ok(AmbientDistillLlmOutcome {
        proposal: reviewed.proposal,
        response: reviewed.response,
    })
}

/// Stamp a distilled ambient cluster into a user-owned evidence record.
/// `evidence_id` is deterministic per (host, day) for idempotent re-runs.
pub fn stamp_ambient_evidence(
    proposal: &EvidenceProposal,
    cluster: &SignalCluster,
    day_key: &str,
    now_rfc3339: &str,
) -> Option<EvidenceRecord> {
    if !proposal.promote || !proposal.decision_is_current() {
        return None;
    }
    let summary = proposal.summary.as_ref()?.trim().to_string();
    if summary.is_empty() {
        return None;
    }
    let facets = proposal
        .facets
        .iter()
        .filter(|f| !f.label.trim().is_empty())
        .map(|f| Facet {
            label: f.label.trim().to_lowercase(),
            confidence: f.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
            assigned_by: "llm".to_string(),
        })
        .collect();
    let source_refs = cluster
        .signal_ids
        .iter()
        .map(|id| format!("signal:{id}"))
        .collect();
    Some(EvidenceRecord {
        evidence_id: format!("evd:amb:{}:{}", cluster.host, day_key),
        summary,
        evidence_kind: proposal
            .evidence_kind
            .clone()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| "browsing".to_string()),
        observed_actions: proposal.observed_actions.clone(),
        entity_keys: proposal.entity_keys.clone(),
        people_keys: proposal.people_keys.clone(),
        artifact_refs: Vec::new(),
        source_refs,
        facets,
        importance: proposal.importance.unwrap_or(0.5).clamp(0.0, 1.0),
        confidence: proposal.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
        sensitivity: normalize_sensitivity(proposal.sensitivity.as_deref()),
        first_seen_at: now_rfc3339.to_string(),
        last_seen_at: now_rfc3339.to_string(),
        status: EvidenceStatus::Active,
        last_corrected_at: None,
        producer: "ambient_browser".to_string(),
        metadata: proposal
            .decision_origin
            .as_ref()
            .map(|origin| serde_json::json!({"decision_origin": origin}))
            .unwrap_or(serde_json::Value::Null),
    })
}
