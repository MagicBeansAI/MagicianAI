//! Generic tier → evidence distillation (WEG Phase 4 — the scalable connector substrate).
//!
//! ONE distiller for every producer whose writer drops per-`(key, day)` roll-up
//! rows into a user-memory tier: `meeting`, `email`, `calendar`, and any future
//! lane (`slack`, `git`, `tickets`, …). A producer is a declarative
//! [`TierProducerSpec`] — its tier, `source_type` filter, id prefix, and a domain
//! hint for the prompt — so **adding a connector is a registry entry plus a
//! writer, not a new module / handler / prompt.** The cluster → salience → distil
//! (one generic prompt) → stamp path, the producer tag, and the idempotent
//! `evidence_id` all come from the spec; everything downstream (entity resolution,
//! memory bridge, compaction, dashboard, claims, views) is the shared substrate.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::tier_contracts::{
    CALENDAR_EVIDENCE_TIER, CHAT_EVIDENCE_TIER, EMAIL_EVIDENCE_TIER, RESEARCH_FINDINGS_TIER,
    WORK_EVIDENCE_TIER,
};
use super::{
    normalize_sensitivity, parse_evidence_proposal, EvidenceProposal, EvidenceRecord,
    EvidenceStatus, Facet,
};
use crate::magician_v2::agents::AgentMemoryService;
use crate::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

/// Declarative description of a tier-backed evidence producer. Add a connector by
/// adding one arm to [`producer_spec`] (and a writer that fills its tier).
#[derive(Debug, Clone)]
pub struct TierProducerSpec {
    /// Producer tag stamped on records (`"meeting"` | `"email"` | `"calendar"` | …).
    pub producer: &'static str,
    /// User-memory tier the writer drops roll-up rows into.
    pub tier: &'static str,
    /// The row `source_type` this producer owns (rows of other types are skipped).
    pub source_type: &'static str,
    /// `evidence_id` prefix (`"evd:meet"` | `"evd:email"` | `"evd:cal"`).
    pub id_prefix: &'static str,
    /// Short domain hint fed to the generic distil prompt (`"meeting"` | `"email day"` | …).
    pub domain: &'static str,
    /// Default `evidence_kind` when the model omits one.
    pub default_kind: &'static str,
    pub salience_threshold: f64,
}

/// The producer registry — the single place a new tier connector is declared.
pub fn producer_spec(name: &str) -> Option<TierProducerSpec> {
    Some(match name.trim() {
        "meeting" => TierProducerSpec {
            producer: "meeting",
            tier: RESEARCH_FINDINGS_TIER,
            source_type: "meeting_capture",
            id_prefix: "evd:meet",
            domain: "meeting",
            default_kind: "meeting",
            salience_threshold: 0.4,
        },
        "email" => TierProducerSpec {
            producer: "email",
            tier: EMAIL_EVIDENCE_TIER,
            source_type: "email_capture",
            id_prefix: "evd:email",
            domain: "email day",
            default_kind: "email",
            salience_threshold: 0.4,
        },
        "calendar" => TierProducerSpec {
            producer: "calendar",
            tier: CALENDAR_EVIDENCE_TIER,
            source_type: "calendar_capture",
            id_prefix: "evd:cal",
            domain: "calendar day",
            default_kind: "calendar",
            salience_threshold: 0.4,
        },
        // Chat (WhatsApp — user + Presto/Kapso). Fed by the channel-assist
        // evidence bridge from locally-distilled chat rows (U1).
        "chat" => TierProducerSpec {
            producer: "chat",
            tier: CHAT_EVIDENCE_TIER,
            source_type: "chat_capture",
            id_prefix: "evd:chat",
            domain: "chat day",
            default_kind: "chat",
            salience_threshold: 0.4,
        },
        // The work-ledger lane is not tier-distilled — records are stamped
        // deterministically from a completed run via
        // `EvidenceRecord::from_work_outcome`. This registry entry exists so the
        // producer is recognized downstream (reviews / dashboard / views); the
        // `tier` / `source_type` fields are inert for this producer.
        "work_outcome" => TierProducerSpec {
            producer: "work_outcome",
            tier: WORK_EVIDENCE_TIER,
            source_type: "work_outcome",
            id_prefix: "evd:run",
            domain: "completed work run",
            default_kind: "work_outcome",
            salience_threshold: 0.4,
        },
        _ => return None,
    })
}

/// A salience-scored group of roll-up rows for one `(group, day)`. Usually one row
/// (writers emit one roll-up per key/account+day); multiple are merged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierCluster {
    pub slug: String,
    pub day: String,
    pub source_keys: Vec<String>,
    pub rows: Vec<Value>,
    pub salience: f64,
}

fn field(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn slug(label: &str) -> String {
    let s: String = label
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() {
        "row".to_string()
    } else {
        s.chars().take(40).collect()
    }
}

fn day_of(date: &str) -> String {
    date.trim()
        .split(|c| c == 'T' || c == ' ')
        .next()
        .unwrap_or(date)
        .to_string()
}

/// True when a row carries substance beyond bookkeeping fields — a real summary,
/// or any non-empty array / positive count. The generic salience signal.
fn has_substance(row: &Value) -> bool {
    if field(row, "summary").is_some() {
        return true;
    }
    if let Some(obj) = row.as_object() {
        for (k, val) in obj {
            if matches!(
                k.as_str(),
                "key" | "source_type" | "account" | "day" | "date"
            ) {
                continue;
            }
            match val {
                Value::Array(a) if !a.is_empty() => return true,
                Value::Number(n) if n.as_f64().unwrap_or(0.0) > 0.0 => return true,
                _ => {},
            }
        }
    }
    false
}

/// Group this producer's tier rows by `(account-or-key, day)` and score a generic
/// salience: a real summary is strongly promotable; substantive metadata alone is
/// borderline; an empty/bookkeeping-only row is skipped. The LLM still gates
/// promote/skip — salience is just the cheap pre-filter.
pub fn cluster_tier_entries(entries: &[Value], spec: &TierProducerSpec) -> Vec<TierCluster> {
    let mut by_key: BTreeMap<String, TierCluster> = BTreeMap::new();
    for row in entries {
        if field(row, "source_type").as_deref() != Some(spec.source_type) {
            continue;
        }
        let key = field(row, "key").unwrap_or_default();
        let group = field(row, "account").unwrap_or_else(|| slug(&key));
        let day = field(row, "day")
            .or_else(|| field(row, "date"))
            .map(|d| day_of(&d))
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "undated".to_string());
        let group_key = format!("{group}\u{1f}{day}");
        let cluster = by_key.entry(group_key).or_insert_with(|| TierCluster {
            slug: slug(&group),
            day: day.clone(),
            source_keys: Vec::new(),
            rows: Vec::new(),
            salience: 0.0,
        });
        if !key.is_empty() {
            cluster.source_keys.push(key);
        }
        cluster.rows.push(row.clone());
    }
    let mut clusters: Vec<TierCluster> = by_key.into_values().collect();
    for c in &mut clusters {
        let has_summary = c.rows.iter().any(|r| field(r, "summary").is_some());
        let substantive = c.rows.iter().any(has_substance);
        c.salience = if has_summary {
            1.0
        } else if substantive {
            0.4
        } else {
            0.0
        };
    }
    clusters.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    clusters
}

pub fn is_tier_cluster_salient(cluster: &TierCluster, spec: &TierProducerSpec) -> bool {
    cluster.salience >= spec.salience_threshold
}

/// Distil one cluster into an evidence proposal via the single generic
/// `tier_evidence_distill` op (parameterized by the spec's `domain`).
pub async fn distill_tier_cluster(
    cluster: &TierCluster,
    spec: &TierProducerSpec,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<EvidenceProposal> {
    distill_tier_cluster_with_telemetry(cluster, spec, router, prompt_manager, None).await
}

pub async fn distill_tier_cluster_with_telemetry(
    cluster: &TierCluster,
    spec: &TierProducerSpec,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> anyhow::Result<EvidenceProposal> {
    distill_tier_cluster_with_source(cluster, spec, router, prompt_manager, telemetry, None).await
}

pub(super) async fn distill_tier_cluster_with_source(
    cluster: &TierCluster,
    spec: &TierProducerSpec,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    telemetry: Option<&OperationLlmTelemetryContext>,
    source: Option<(&AgentMemoryService, &Arc<PromptManager>)>,
) -> anyhow::Result<EvidenceProposal> {
    let payload = if cluster.rows.len() == 1 {
        cluster.rows[0].clone()
    } else {
        Value::Array(cluster.rows.clone())
    };
    let mut vars = HashMap::new();
    vars.insert("domain".to_string(), spec.domain.to_string());
    vars.insert(
        "record_json".to_string(),
        serde_json::to_string_pretty(&payload)?,
    );
    let system = prompt_manager
        .get_rendered_prompt("tier_evidence_distill_system", "1.0.0", HashMap::new())
        .await?;
    let user = prompt_manager
        .get_rendered_prompt("tier_evidence_distill_user", "1.0.0", vars)
        .await?;
    let operation = LLMOperation::Other("tier_evidence_distill".to_string());
    let reviewed = super::decision::review(
        router,
        "tier_evidence_distill",
        &system,
        &user,
        false,
        source.map(
            |(service, prompts)| super::decision::EvidenceReplaySource::Tier {
                service,
                cluster,
                producer: spec.producer,
                prompts,
            },
        ),
        || async {
            let started = std::time::Instant::now();
            let response = router
                .generate_for_operation_with_system(&operation, Some(&system), &user)
                .await?;
            if telemetry.is_none() {
                if let Some(scope) = router.authoritative_trace_scope() {
                    crate::magician_v2::decisions::reference::record_response(
                        &response,
                        &scope.principal,
                        &scope.workspace,
                        "tier_evidence_distill",
                        started.elapsed(),
                    );
                }
            }
            if let Some(telemetry) = telemetry {
                let elapsed = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                match parse_evidence_proposal(&response.content) {
                    Ok(_) => telemetry.emit_validated_success(
                        "tier_evidence_distill",
                        &response,
                        elapsed,
                        OperationLlmCallAttribution::default(),
                        "tier_evidence_proposal",
                    ),
                    Err(error) => telemetry.emit_validation_failure(
                        "tier_evidence_distill",
                        &response,
                        elapsed,
                        OperationLlmCallAttribution::default(),
                        "tier_evidence_proposal",
                        &error.to_string(),
                    ),
                }
            }
            Ok(response)
        },
    )
    .await?;
    Ok(reviewed.proposal)
}

/// Stamp a distilled cluster into a user-owned evidence record. `evidence_id` is
/// deterministic per `(producer, group, day)` for idempotent re-runs.
pub fn stamp_tier_evidence(
    proposal: &EvidenceProposal,
    cluster: &TierCluster,
    spec: &TierProducerSpec,
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
    Some(EvidenceRecord {
        evidence_id: format!("{}:{}:{}", spec.id_prefix, cluster.slug, cluster.day),
        summary,
        evidence_kind: proposal
            .evidence_kind
            .clone()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| spec.default_kind.to_string()),
        observed_actions: proposal.observed_actions.clone(),
        entity_keys: proposal.entity_keys.clone(),
        people_keys: proposal.people_keys.clone(),
        artifact_refs: Vec::new(),
        source_refs: cluster.source_keys.clone(),
        facets,
        importance: proposal.importance.unwrap_or(0.5).clamp(0.0, 1.0),
        confidence: proposal.confidence.unwrap_or(0.5).clamp(0.0, 1.0),
        sensitivity: normalize_sensitivity(proposal.sensitivity.as_deref()),
        first_seen_at: now_rfc3339.to_string(),
        last_seen_at: now_rfc3339.to_string(),
        status: EvidenceStatus::Active,
        last_corrected_at: None,
        producer: spec.producer.to_string(),
        metadata: proposal
            .decision_origin
            .as_ref()
            .map(|origin| serde_json::json!({"decision_origin": origin}))
            .unwrap_or(serde_json::Value::Null),
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registry_covers_the_built_lanes() {
        for (name, producer, prefix) in [
            ("meeting", "meeting", "evd:meet"),
            ("email", "email", "evd:email"),
            ("calendar", "calendar", "evd:cal"),
        ] {
            let spec = producer_spec(name).unwrap();
            assert_eq!(spec.producer, producer);
            assert_eq!(spec.id_prefix, prefix);
        }
        assert!(producer_spec("nope").is_none());
    }

    #[test]
    fn clusters_by_account_day_and_skips_foreign_source_types() {
        let spec = producer_spec("email").unwrap();
        let entries = vec![
            json!({"key":"email:work:2026-06-20","source_type":"email_capture","account":"work","day":"2026-06-20","summary":"replied on launch","subjects":["Launch"],"thread_count":3}),
            json!({"key":"email:personal:2026-06-20","source_type":"email_capture","account":"personal","day":"2026-06-20","summary":"dinner plans"}),
            // foreign source_type in the same tier is ignored
            json!({"key":"other","source_type":"meeting_capture","account":"work","day":"2026-06-20","summary":"x"}),
        ];
        let clusters = cluster_tier_entries(&entries, &spec);
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|c| is_tier_cluster_salient(c, &spec)));
    }

    #[test]
    fn empty_row_is_not_salient_but_metadata_is() {
        let spec = producer_spec("calendar").unwrap();
        let entries = vec![
            json!({"key":"cal:work:2026-06-20","source_type":"calendar_capture","account":"work","day":"2026-06-20"}),
            json!({"key":"cal:biz:2026-06-20","source_type":"calendar_capture","account":"biz","day":"2026-06-20","event_titles":["Roadmap"],"event_count":2}),
        ];
        let clusters = cluster_tier_entries(&entries, &spec);
        let work = clusters.iter().find(|c| c.slug == "work").unwrap();
        let biz = clusters.iter().find(|c| c.slug == "biz").unwrap();
        assert!(!is_tier_cluster_salient(work, &spec));
        assert!(is_tier_cluster_salient(biz, &spec));
    }

    #[test]
    fn stamp_uses_spec_prefix_producer_and_kind() {
        let spec = producer_spec("meeting").unwrap();
        let cluster = TierCluster {
            slug: "thread-abc".to_string(),
            day: "2026-06-20".to_string(),
            source_keys: vec!["meeting:thread-abc".to_string()],
            rows: vec![json!({"summary":"x"})],
            salience: 1.0,
        };
        let proposal = EvidenceProposal {
            decision_origin: None,
            decision_guard: None,
            promote: true,
            skip_reason: None,
            summary: Some("Aligned on the roadmap.".to_string()),
            evidence_kind: None,
            observed_actions: vec![],
            entity_keys: vec![],
            people_keys: vec![],
            entities: vec![],
            facets: vec![],
            importance: Some(0.6),
            confidence: Some(0.6),
            sensitivity: None,
        };
        let record =
            stamp_tier_evidence(&proposal, &cluster, &spec, "2026-06-20T10:00:00Z").unwrap();
        assert_eq!(record.evidence_id, "evd:meet:thread-abc:2026-06-20");
        assert_eq!(record.producer, "meeting");
        assert_eq!(record.evidence_kind, "meeting");
        assert_eq!(record.source_refs, vec!["meeting:thread-abc"]);
    }
}
