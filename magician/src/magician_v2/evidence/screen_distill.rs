//! Desktop screen-observation distillation (WEG Phase 4 — desktop connector).
//!
//! The screen-observe rail (`media_rails::screen_observe`) writes one rolled-up
//! provenance entry per observation SESSION into the `user.screen_observations`
//! memory tier (`key = observe:<id>`, with `purpose` / `mode` / `date`, activity
//! counts, and an optional `summary`). This module is the generic, user-owned
//! analogue of [`super::ambient_distill`]: it groups those session entries by
//! `(purpose, day)`, salience-gates them deterministically, and distils each
//! promotable cluster (one LLM call) into an [`EvidenceRecord`] tagged
//! `producer = "screen_observation"`.
//!
//! Idempotent: a cluster's `evidence_id` is `evd:scr:{purpose-slug}:{day}`, so
//! re-running the distiller over the same window upserts rather than duplicates.
//! Facet-agnostic — the domain (work/personal/…) is a derived facet, never a
//! hardcoded filter.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    normalize_sensitivity, parse_evidence_proposal, EvidenceProposal, EvidenceRecord,
    EvidenceStatus, Facet,
};
use crate::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use crate::magician_v2::prompts::PromptManager;
use crate::magician_v2::query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter};

/// Minimum cluster salience to promote into evidence.
pub const SCREEN_SALIENCE_THRESHOLD: f64 = 0.4;

/// One screen-observation session entry read back from the
/// `user.screen_observations` tier. Lenient — older/newer writers may omit
/// fields, and idle sessions carry no `summary`.
#[derive(Debug, Clone, Deserialize)]
pub struct ScreenObservationRow {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub notes: Option<u64>,
    #[serde(default)]
    pub alerts: Option<u64>,
}

/// A salience-scored cluster of observation sessions for one `(purpose, day)`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScreenObservationCluster {
    /// Human-readable purpose label (the cluster key, pre-slug).
    pub purpose: String,
    pub day: String,
    /// `observe:<id>` keys that fed this cluster (become `source_refs`).
    pub observe_ids: Vec<String>,
    pub modes: Vec<String>,
    pub summaries: Vec<String>,
    pub note_total: u64,
    pub alert_total: u64,
    pub count: usize,
    pub salience: f64,
}

/// Lowercase a-z0-9 slug of a purpose label for a stable `evidence_id` segment.
fn slug(label: &str) -> String {
    let s: String = label
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "session".to_string()
    } else {
        s.split('-')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }
}

/// Day portion of an RFC-ish date/datetime string (`YYYY-MM-DD…` → `YYYY-MM-DD`).
fn day_of(date: &str) -> String {
    date.trim()
        .split(|c| c == 'T' || c == ' ')
        .next()
        .unwrap_or(date)
        .to_string()
}

/// Group session entries by `(purpose, day)` and score salience. Salience blends
/// summary presence (real narrative beats counts), alert volume (flagged moments
/// are noteworthy), and note engagement — so an observation session that actually
/// produced a summary or raised alerts beats an idle watch that logged nothing.
pub fn cluster_screen_observations(rows: &[ScreenObservationRow]) -> Vec<ScreenObservationCluster> {
    let mut by_key: HashMap<String, ScreenObservationCluster> = HashMap::new();
    for row in rows {
        let purpose = row
            .purpose
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("screen activity")
            .to_string();
        let day = row
            .date
            .as_deref()
            .map(day_of)
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "undated".to_string());
        let group_key = format!("{purpose}\u{1f}{day}");
        let cluster = by_key
            .entry(group_key)
            .or_insert_with(|| ScreenObservationCluster {
                purpose: purpose.clone(),
                day: day.clone(),
                observe_ids: Vec::new(),
                modes: Vec::new(),
                summaries: Vec::new(),
                note_total: 0,
                alert_total: 0,
                count: 0,
                salience: 0.0,
            });
        if !row.key.trim().is_empty() {
            cluster.observe_ids.push(row.key.clone());
        }
        if let Some(mode) = &row.mode {
            if !mode.trim().is_empty() && !cluster.modes.iter().any(|m| m == mode) {
                cluster.modes.push(mode.clone());
            }
        }
        if let Some(summary) = &row.summary {
            if !summary.trim().is_empty() {
                cluster.summaries.push(summary.clone());
            }
        }
        cluster.note_total += row.notes.unwrap_or(0);
        cluster.alert_total += row.alerts.unwrap_or(0);
        cluster.count += 1;
    }

    let mut clusters: Vec<ScreenObservationCluster> = by_key.into_values().collect();
    for c in &mut clusters {
        let summary_weight = if c.summaries.is_empty() { 0.0 } else { 0.5 };
        let alert_weight = ((c.alert_total as f64).min(4.0) / 4.0) * 0.35;
        let note_engagement = ((c.note_total as f64).min(20.0) / 20.0) * 0.25;
        c.salience = (summary_weight + alert_weight + note_engagement).clamp(0.0, 1.0);
    }
    clusters.sort_by(|a, b| {
        b.salience
            .partial_cmp(&a.salience)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    clusters
}

pub fn is_screen_cluster_salient(cluster: &ScreenObservationCluster) -> bool {
    cluster.salience >= SCREEN_SALIENCE_THRESHOLD
}

/// Distill one promotable observation cluster into an evidence proposal via the
/// `screen_evidence_distill` op. Reuses the shared [`EvidenceProposal`] contract.
pub async fn distill_screen_cluster(
    cluster: &ScreenObservationCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
) -> anyhow::Result<EvidenceProposal> {
    distill_screen_cluster_with_telemetry(cluster, router, prompt_manager, None).await
}

pub async fn distill_screen_cluster_with_telemetry(
    cluster: &ScreenObservationCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> anyhow::Result<EvidenceProposal> {
    distill_screen_cluster_inner(cluster, router, prompt_manager, telemetry, None).await
}

/// The tier owner supplies its scoped store so observation replay can reject a
/// changed or deleted source before and after the reference call.
pub async fn distill_screen_cluster_source_bound(
    cluster: &ScreenObservationCluster,
    router: &OperationLlmRouter,
    prompt_manager: &std::sync::Arc<PromptManager>,
    memory: &crate::magician_v2::agents::AgentMemoryService,
    source_tier: &Value,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> anyhow::Result<EvidenceProposal> {
    let source_digest = super::decision::screen_tier_digest(source_tier)
        .ok_or_else(|| anyhow::anyhow!("screen observation source tier is missing"))?;
    distill_screen_cluster_inner(
        cluster,
        router,
        prompt_manager,
        telemetry,
        Some((memory, prompt_manager, &source_digest)),
    )
    .await
}

async fn distill_screen_cluster_inner(
    cluster: &ScreenObservationCluster,
    router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    telemetry: Option<&OperationLlmTelemetryContext>,
    replay_source: Option<(
        &crate::magician_v2::agents::AgentMemoryService,
        &std::sync::Arc<PromptManager>,
        &str,
    )>,
) -> anyhow::Result<EvidenceProposal> {
    let (system, user) = render_screen_prompt(cluster, prompt_manager).await?;
    let operation = LLMOperation::Other("screen_evidence_distill".to_string());
    let reviewed = super::decision::review(
        router,
        "screen_evidence_distill",
        &system,
        &user,
        false,
        replay_source.map(
            |(service, prompts, source_digest)| super::decision::EvidenceReplaySource::Screen {
                service,
                cluster,
                prompts,
                source_digest,
            },
        ),
        || async {
            let started = std::time::Instant::now();
            let response = router
                .generate_for_operation_with_system(&operation, Some(&system), &user)
                .await?;
            if let Some(telemetry) = telemetry {
                let elapsed = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                match parse_evidence_proposal(&response.content) {
                    Ok(_) => telemetry.emit_validated_success(
                        "screen_evidence_distill",
                        &response,
                        elapsed,
                        OperationLlmCallAttribution::default(),
                        "screen_evidence_proposal",
                    ),
                    Err(error) => telemetry.emit_validation_failure(
                        "screen_evidence_distill",
                        &response,
                        elapsed,
                        OperationLlmCallAttribution::default(),
                        "screen_evidence_proposal",
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

pub(super) async fn render_screen_prompt(
    cluster: &ScreenObservationCluster,
    prompt_manager: &PromptManager,
) -> anyhow::Result<(String, String)> {
    let packet = serde_json::json!({
        "purpose": cluster.purpose,
        "day": cluster.day,
        "session_count": cluster.count,
        "observed_modes": cluster.modes,
        "session_summaries": cluster.summaries,
        "total_notes": cluster.note_total,
        "total_alerts": cluster.alert_total,
    });
    let mut vars = HashMap::new();
    vars.insert(
        "observation_cluster_json".to_string(),
        serde_json::to_string_pretty(&packet)?,
    );
    let system = prompt_manager
        .get_rendered_prompt("screen_evidence_distill_system", "1.0.0", HashMap::new())
        .await?;
    let user = prompt_manager
        .get_rendered_prompt("screen_evidence_distill_user", "1.0.0", vars)
        .await?;
    Ok((system, user))
}

/// Stamp a distilled observation cluster into a user-owned evidence record.
/// `evidence_id` is deterministic per `(purpose, day)` for idempotent re-runs.
pub fn stamp_screen_evidence(
    proposal: &EvidenceProposal,
    cluster: &ScreenObservationCluster,
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
        .observe_ids
        .iter()
        .map(|id| id.to_string())
        .collect();
    Some(EvidenceRecord {
        evidence_id: format!("evd:scr:{}:{}", slug(&cluster.purpose), cluster.day),
        summary,
        evidence_kind: proposal
            .evidence_kind
            .clone()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| "screen_activity".to_string()),
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
        producer: "screen_observation".to_string(),
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

    fn row(
        key: &str,
        purpose: &str,
        date: &str,
        summary: Option<&str>,
        notes: u64,
        alerts: u64,
    ) -> ScreenObservationRow {
        ScreenObservationRow {
            key: key.to_string(),
            purpose: Some(purpose.to_string()),
            mode: Some("watch".to_string()),
            date: Some(date.to_string()),
            summary: summary.map(ToString::to_string),
            notes: Some(notes),
            alerts: Some(alerts),
        }
    }

    #[test]
    fn clusters_by_purpose_and_day() {
        let rows = vec![
            row(
                "observe:1",
                "Monitor the deploy",
                "2026-06-14",
                Some("watched CI"),
                3,
                1,
            ),
            row(
                "observe:2",
                "Monitor the deploy",
                "2026-06-14",
                Some("deploy went green"),
                2,
                0,
            ),
            row(
                "observe:3",
                "Monitor the deploy",
                "2026-06-13",
                Some("rollback"),
                1,
                2,
            ),
        ];
        let clusters = cluster_screen_observations(&rows);
        // Two days for the same purpose → two clusters.
        assert_eq!(clusters.len(), 2);
        let today = clusters.iter().find(|c| c.day == "2026-06-14").unwrap();
        assert_eq!(today.count, 2);
        assert_eq!(today.observe_ids.len(), 2);
        assert_eq!(today.note_total, 5);
        assert_eq!(today.alert_total, 1);
    }

    #[test]
    fn idle_session_without_summary_or_activity_is_not_salient() {
        let rows = vec![row(
            "observe:idle",
            "Background watch",
            "2026-06-14",
            None,
            0,
            0,
        )];
        let clusters = cluster_screen_observations(&rows);
        assert_eq!(clusters.len(), 1);
        assert!(!is_screen_cluster_salient(&clusters[0]));
    }

    #[test]
    fn session_with_summary_is_salient() {
        let rows = vec![row(
            "observe:s",
            "Review PRs",
            "2026-06-14",
            Some("reviewed 3 PRs"),
            0,
            0,
        )];
        let clusters = cluster_screen_observations(&rows);
        assert!(is_screen_cluster_salient(&clusters[0]));
    }

    #[test]
    fn stamp_builds_deterministic_idempotent_id() {
        let cluster = ScreenObservationCluster {
            purpose: "Monitor the Deploy!".to_string(),
            day: "2026-06-14".to_string(),
            observe_ids: vec!["observe:1".to_string(), "observe:2".to_string()],
            modes: vec!["watch".to_string()],
            summaries: vec!["watched CI".to_string()],
            note_total: 5,
            alert_total: 1,
            count: 2,
            salience: 0.7,
        };
        let proposal = EvidenceProposal {
            decision_origin: None,
            decision_guard: None,
            promote: true,
            skip_reason: None,
            summary: Some("Monitored the deploy; CI went green.".to_string()),
            evidence_kind: None,
            observed_actions: vec!["monitored".to_string()],
            entity_keys: vec!["project:checkout".to_string()],
            people_keys: vec![],
            entities: vec![],
            facets: vec![],
            importance: Some(0.6),
            confidence: Some(0.7),
            sensitivity: None,
        };
        let record = stamp_screen_evidence(&proposal, &cluster, "2026-06-14T10:00:00Z").unwrap();
        assert_eq!(record.evidence_id, "evd:scr:monitor-the-deploy:2026-06-14");
        assert_eq!(record.producer, "screen_observation");
        assert_eq!(record.evidence_kind, "screen_activity");
        assert_eq!(record.source_refs, vec!["observe:1", "observe:2"]);
    }

    #[test]
    fn non_promote_proposal_yields_no_record() {
        let cluster = ScreenObservationCluster {
            purpose: "x".to_string(),
            day: "2026-06-14".to_string(),
            observe_ids: vec![],
            modes: vec![],
            summaries: vec![],
            note_total: 0,
            alert_total: 0,
            count: 0,
            salience: 0.0,
        };
        let proposal = EvidenceProposal {
            decision_origin: None,
            decision_guard: None,
            promote: false,
            skip_reason: Some("idle".to_string()),
            summary: None,
            evidence_kind: None,
            observed_actions: vec![],
            entity_keys: vec![],
            people_keys: vec![],
            entities: vec![],
            facets: vec![],
            importance: None,
            confidence: None,
            sensitivity: None,
        };
        assert!(stamp_screen_evidence(&proposal, &cluster, "2026-06-14T10:00:00Z").is_none());
    }
}
