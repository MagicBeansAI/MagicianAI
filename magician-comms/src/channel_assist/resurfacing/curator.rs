//! Curation for the Proactive Resurfacing Engine.
//!
//! [`run_curation_pass_with_attention`] is the DETERMINISTIC timer-free unit the
//! worker drives after a scoring pass: it scans top-ranked candidate rows and
//! only marks candidates surfaced after the shared attention router accepts them
//! for Worth a look. The store still owns state/cooldown eligibility, so
//! cooling, surfaced, acted, and dismissed rows are never re-surfaced.
//!
//! [`run_curation_pass_llm_with_attention`] is the Phase-2 LLM-curated path
//! layered on top: it hands the top-`cap` candidates' titles + digests to an
//! operation-routed model that PICKS the few most worth resurfacing and authors
//! a one-line surface `line` + a short `why_now` for each. The
//! `resurfacing_curate` operation is IDLE-UNTIL-BOUND (mirrors
//! `channel_pattern_synthesis`): with no router, no binding, an LLM error, or
//! an unparseable reply it falls back to the deterministic pick — it never
//! fails or surfaces more than `cap`. The chosen phrasing lands in a separate
//! `resurfacing_phrasing` table (never on the `Candidate`), which the `today`
//! read prefers over the generic phrase.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

use crate::channel_assist::channel::{
    derive_channel_required_action, ChannelAnnotation, ChannelAnnotationState, ChannelAssistStore,
    ChannelFollowUpHint, ChannelMessageAttentionHints, ChannelMessageMeta, ChannelRequiredAction,
    ChannelRequiredActionKind, RequiredActionAnnotationDisposition, RequiredActionAnnotationResult,
    CHANNEL_ASSIST_SCHEMA_VERSION,
};
use magician::magician_v2::analytics::operation_llm_telemetry::{
    OperationLlmCallAttribution, OperationLlmTelemetryContext,
};
use magician::magician_v2::attention_funnel::{
    route_attention_candidate, AttentionAction, AttentionActionKind, AttentionCandidate,
    AttentionFunnelStage, AttentionLane, AttentionRouteContext, AttentionRouteEvent,
    AttentionScope, AttentionSource, AttentionSourceFamily, AttentionSourceKind,
    AttentionTraceStatus, AttentionUrgency, DropReason, RouteOutcome, RoutePriority, RouteReason,
};
use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;
use magician::magician_v2::prompts::{
    names as prompt_names, rendered_prompt, versions as prompt_versions,
};
use magician::magician_v2::query_analysis::operation_llm_router::{
    LLMOperation, OperationLlmRouter,
};

use super::interaction::{metadata_capabilities, ResurfacingActionCapability};
use magician::magician_v2::attention::resurfacing::interaction::{
    ResurfacingActionKind, ResurfacingRecommendation, ResurfacingRecommendationSource,
};
use magician::magician_v2::attention::resurfacing::source_refs::{
    parse_comm_source_ref, CommSourceRef,
};
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician::magician_v2::attention::resurfacing::types::{Candidate, CandidateState, SourceKind};
use uuid::Uuid;

const LOG_TARGET: &str = "resurfacing::curator";

/// Operation key — bind it in `operation_mapping` (local OR remote) to run the
/// LLM curator; leave it unbound to keep the pass on the deterministic pick.
pub const RESURFACING_CURATE_OPERATION: &str = "resurfacing_curate";

/// Cap the per-candidate digest excerpt so the prompt stays compact regardless
/// of how long a candidate's stored `content_digest` is.
const CANDIDATE_DIGEST_MAX_CHARS: usize = 480;
const CURATION_DIGEST_MAX_CHARS: usize = 25_000;
const CURATION_REVIEW_MAX_CANDIDATES: usize = 50;
const CURATION_LINE_MAX_CHARS: usize = 240;
const CURATION_WHY_MAX_CHARS: usize = 240;
const RECOMMENDATION_LABEL_MAX_CHARS: usize = 80;
const RECOMMENDATION_RATIONALE_MAX_CHARS: usize = 240;

/// The LLM should review more candidates than it is allowed to surface. If it
/// only sees `cap`, a conservative "none of these" answer can repeatedly review
/// the same top rows and starve everything just below them.
const REVIEW_WINDOW_MULTIPLIER: usize = 5;
const ROUTING_SCAN_MULTIPLIER: usize = 16;
const ROUTING_SCAN_MIN: usize = 64;
const ROUTING_SCAN_MAX: usize = 512;

/// Minimum meaningful signal outside pure recency before a candidate is worth
/// occupying the serendipitous Worth a look surface. Recency alone is useful for
/// ordering, not sufficient as the reason to interrupt the owner.
const MIN_NON_RECENCY_SIGNAL: f32 = 0.12;

/// Cooldown for candidates the LLM explicitly reviewed but declined to surface.
/// This keeps "not worth resurfacing right now" out of the immediate queue long
/// enough for the next curation pass to rotate through lower-ranked candidates.
const REVIEW_REJECT_COOLDOWN_SECS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy)]
pub struct CurationRecommendationPolicy {
    pub generate: bool,
    pub contextual_actions_enabled: bool,
    pub min_confidence: f32,
}

impl CurationRecommendationPolicy {
    pub const fn disabled() -> Self {
        Self {
            generate: false,
            contextual_actions_enabled: false,
            min_confidence: 1.0,
        }
    }

    pub fn enabled(contextual_actions_enabled: bool, min_confidence: f32) -> Self {
        Self {
            generate: true,
            contextual_actions_enabled,
            min_confidence: min_confidence.clamp(0.0, 1.0),
        }
    }
}

/// Deterministic Phase-1 curation: surface the top `cap` eligible candidates.
/// (The LLM-phrased path is Task 15 / Phase 2; this needs no LLM.)
///
/// Reads the top-`cap` eligible candidates (highest salience first), marks them
/// surfaced in the store, and returns them with their in-memory state advanced
/// to `Surfaced` (and `surface_count`/`last_surfaced_at` bumped) so the caller
/// sees the post-transition rows without a re-fetch. Returns an empty vec when
/// nothing is eligible.
pub async fn run_curation_pass_with_attention(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    cap: usize,
    now: i64,
    attention_store: Option<&AttentionFunnelStore>,
    channel_store: Option<&ChannelAssistStore>,
) -> Result<Vec<Candidate>> {
    run_deterministic_curation_pass(
        principal,
        workspace,
        store,
        cap,
        now,
        attention_store,
        channel_store,
        None,
        CurationRecommendationPolicy::disabled(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_deterministic_curation_pass(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    cap: usize,
    now: i64,
    attention_store: Option<&AttentionFunnelStore>,
    channel_store: Option<&ChannelAssistStore>,
    router: Option<&OperationLlmRouter>,
    recommendation_policy: CurationRecommendationPolicy,
) -> Result<Vec<Candidate>> {
    let mut top = list_router_accepted_resurfacing_candidates(
        principal,
        workspace,
        store,
        cap,
        now,
        attention_store,
        channel_store,
        false,
    )
    .await?;
    if top.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<String> = top.iter().map(|c| c.candidate_id.clone()).collect();
    let surfaced_ids = store.mark_surfaced(principal, workspace, &ids, now).await?;
    if surfaced_ids.is_empty() {
        return Ok(Vec::new());
    }
    let surfaced_id_set = surfaced_ids.into_iter().collect::<HashSet<_>>();
    top.retain(|candidate| surfaced_id_set.contains(&candidate.candidate_id));

    // Mirror the persisted transition into the returned copies (the store just
    // applied these same mutations) so the caller reflects Surfaced state.
    for c in &mut top {
        c.state = CandidateState::Surfaced;
        c.last_surfaced_at = Some(now);
        c.surface_count += 1;
    }
    if recommendation_policy.generate {
        for candidate in &top {
            let allowed = metadata_capabilities(
                candidate,
                router,
                recommendation_policy.contextual_actions_enabled,
            );
            let Some(recommendation) = deterministic_recommendation(
                candidate,
                &allowed,
                recommendation_policy.min_confidence,
                now,
            ) else {
                continue;
            };
            if let Err(error) = store
                .upsert_recommendation(
                    principal,
                    workspace,
                    &candidate.candidate_id,
                    &recommendation,
                    now,
                )
                .await
            {
                debug!(
                    target: LOG_TARGET,
                    candidate_id = %candidate.candidate_id,
                    error = %error,
                    "deterministic resurfacing recommendation write failed"
                );
            }
        }
    }
    record_resurfacing_routed_events(attention_store, principal, workspace, &top, now).await;
    write_memory_why_now_phrasing(principal, workspace, store, &top, now).await;
    Ok(top)
}

async fn write_memory_why_now_phrasing(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    surfaced: &[Candidate],
    now: i64,
) {
    use magician::magician_v2::attention::resurfacing::memory_context::why_now_with_memory;
    for candidate in surfaced {
        let revision = candidate
            .content_revision
            .as_deref()
            .unwrap_or(candidate.content_digest.as_str());
        let Ok(Some(judgement)) = store
            .get_memory_applications_for_content_revision(
                principal,
                workspace,
                &candidate.candidate_id,
                revision,
            )
            .await
        else {
            continue;
        };
        if judgement.applications.would_apply.is_empty() {
            continue;
        }
        let (why, _, _) = why_now_with_memory(
            "Currently surfaced by the Worth-a-look curator",
            &judgement.applications,
            false,
        );
        if let Err(error) = store
            .upsert_phrasing(
                principal,
                workspace,
                &candidate.candidate_id,
                &candidate.title,
                &why,
                candidate.content_revision.as_deref(),
                now,
            )
            .await
        {
            debug!(
                target: LOG_TARGET,
                candidate_id = %candidate.candidate_id,
                error = %error,
                "memory why-now phrasing write failed"
            );
        }
    }
}

/// One item the curator chose to resurface, as parsed from the LLM reply.
/// `index` is a signed integer so a malformed (negative / huge) index parses
/// rather than failing the whole array; [`resolve_selections`] filters it.
#[derive(Debug, Clone, Deserialize)]
struct CurationSelection {
    index: i64,
    #[serde(default)]
    line: String,
    #[serde(default)]
    why_now: String,
    #[serde(default)]
    recommended_action: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelRecommendation {
    kind: ResurfacingActionKind,
    label: String,
    rationale: String,
    confidence: f32,
}

/// A selection resolved against the actual candidate list: an in-range index
/// plus non-blank phrasing, ready to persist.
#[derive(Debug, Clone, PartialEq)]
struct ResolvedSelection {
    index: usize,
    line: String,
    why: String,
    recommendation: Option<ResurfacingRecommendation>,
}

/// Fold the review window into bounded, one-line semantic briefs. The action
/// suffix is preserved even when content must be clipped, so the model always
/// sees the complete server-owned enum allow-list.
fn build_curation_digest(
    candidates: &[Candidate],
    capabilities: &[Vec<ResurfacingActionCapability>],
) -> String {
    let digest = candidates
        .iter()
        .take(CURATION_REVIEW_MAX_CANDIDATES)
        .enumerate()
        .map(|(index, candidate)| {
            candidate_digest_line(
                index,
                candidate,
                capabilities.get(index).map(Vec::as_slice).unwrap_or(&[]),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    debug_assert!(digest.chars().count() <= CURATION_DIGEST_MAX_CHARS);
    digest
}

fn candidate_digest_line(
    index: usize,
    candidate: &Candidate,
    capabilities: &[ResurfacingActionCapability],
) -> String {
    let details = candidate.content_details.as_ref();
    let detail_status = details
        .map(|details| match details.detail_status {
            magician::magician_v2::attention::resurfacing::types::ResurfacingDetailStatus::Complete => "complete",
            magician::magician_v2::attention::resurfacing::types::ResurfacingDetailStatus::Partial => "partial",
            magician::magician_v2::attention::resurfacing::types::ResurfacingDetailStatus::SourceOmitsDetails => "source_omits_details",
        })
        .unwrap_or("legacy");
    let allowed_actions = capabilities
        .iter()
        .map(|capability| capability.kind.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let head = format!("{index}. source={}; ", candidate.source_kind.as_str());
    let tail = format!(
        "; detail_status={detail_status}; salience={:.2}; signals={{r:{:.2},f:{:.2},c:{:.2},o:{:.2},t:{:.2},d:{:.2},s:{:.2}}}; allowed_actions=[{allowed_actions}]",
        candidate.salience_score,
        candidate.signals.recency,
        candidate.signals.frequency,
        candidate.signals.centrality,
        candidate.signals.cooccurrence,
        candidate.signals.temporal_anchor,
        candidate.signals.dormancy,
        candidate.signals.source_affinity,
    );
    let facts = details
        .map(|details| bounded_join(&details.key_facts, 3, 90))
        .unwrap_or_default();
    let changes = details
        .map(|details| {
            details
                .changes
                .iter()
                .take(2)
                .map(|change| {
                    bounded_prompt_text(
                        &format!(
                            "{}:{}>{}{}",
                            change.aspect,
                            change.before.as_deref().unwrap_or("unknown"),
                            change.after.as_deref().unwrap_or("unknown"),
                            change
                                .effective_text
                                .as_deref()
                                .map(|value| format!("@{value}"))
                                .unwrap_or_default()
                        ),
                        120,
                    )
                })
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default();
    let temporal = details
        .map(|details| {
            details
                .temporal_facts
                .iter()
                .take(2)
                .map(|fact| {
                    bounded_prompt_text(
                        &format!(
                            "{}:{}{}",
                            fact.kind,
                            fact.text,
                            fact.at_ms
                                .map(|value| format!("#{value}"))
                                .unwrap_or_default()
                        ),
                        100,
                    )
                })
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_default();
    let missing = details
        .map(|details| bounded_join(&details.missing_details, 2, 90))
        .unwrap_or_default();
    let semantic = format!(
        "title={}; summary={}; facts=[{}]; changes=[{}]; dates=[{}]; missing=[{}]",
        bounded_prompt_text(&candidate.title, 120),
        bounded_prompt_text(&candidate.content_digest, 220),
        facts,
        changes,
        temporal,
        missing,
    );
    let fixed_chars = head.chars().count().saturating_add(tail.chars().count());
    let semantic_budget = CANDIDATE_DIGEST_MAX_CHARS.saturating_sub(fixed_chars);
    format!(
        "{head}{}{tail}",
        semantic.chars().take(semantic_budget).collect::<String>()
    )
}

fn bounded_join(values: &[String], max_items: usize, max_chars_each: usize) -> String {
    values
        .iter()
        .take(max_items)
        .map(|value| bounded_prompt_text(value, max_chars_each))
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn bounded_prompt_text(value: &str, max_chars: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

/// Pure: tolerantly parse the LLM reply into a list of selections. Strips a
/// code fence, then slices the first `[` .. last `]` and parses that as the
/// JSON array. Returns `None` only on genuine parse failure (the caller then
/// falls back to the deterministic pick); a valid-but-empty array parses to
/// `Some(vec![])` — the curator legitimately choosing to resurface nothing.
fn parse_curation_reply(raw: &str) -> Option<Vec<CurationSelection>> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let start = trimmed.find('[')?;
    let end = trimmed.rfind(']')?;
    // `get` (not `&trimmed[start..=end]`) so a reversed bracket order
    // (`end < start`, e.g. the model emits "] [") yields `None` instead of a
    // slice-index PANIC. This runs in the worker's async context (NOT
    // spawn_blocking), so a panic here would unwind and kill the whole
    // resurfacing task — the exact garbled-reply case the fallback must tolerate.
    trimmed
        .get(start..=end)
        .and_then(|json| serde_json::from_str(json).ok())
}

/// Pure: resolve parsed selections against a candidate list of `count` rows.
/// Ignores out-of-range/negative indices and duplicate indices, drops any
/// selection missing a non-blank `line` or `why_now`, and never returns more
/// than `cap` (first-wins on order). This is where "NEVER surface more than
/// cap; ignore out-of-range indices" is enforced.
fn resolve_selections(
    candidates: &[Candidate],
    cap: usize,
    selections: &[CurationSelection],
    capabilities: &[Vec<ResurfacingActionCapability>],
    recommendation_policy: CurationRecommendationPolicy,
    now: i64,
) -> Vec<ResolvedSelection> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for sel in selections {
        if out.len() >= cap {
            break;
        }
        if sel.index < 0 {
            continue;
        }
        let idx = sel.index as usize;
        if idx >= candidates.len() {
            continue;
        }
        if !seen.insert(idx) {
            continue;
        }
        let line = bounded_prompt_text(&sel.line, CURATION_LINE_MAX_CHARS);
        let why = bounded_prompt_text(&sel.why_now, CURATION_WHY_MAX_CHARS);
        if line.is_empty() || why.is_empty() {
            continue;
        }
        let recommendation = if recommendation_policy.generate {
            resolve_model_or_fallback_recommendation(
                sel.recommended_action.as_ref(),
                &candidates[idx],
                capabilities.get(idx).map(Vec::as_slice).unwrap_or(&[]),
                recommendation_policy.min_confidence,
                now,
            )
        } else {
            None
        };
        out.push(ResolvedSelection {
            index: idx,
            line,
            why,
            recommendation,
        });
    }
    out
}

fn resolve_model_or_fallback_recommendation(
    raw: Option<&Value>,
    candidate: &Candidate,
    capabilities: &[ResurfacingActionCapability],
    min_confidence: f32,
    now: i64,
) -> Option<ResurfacingRecommendation> {
    let model = raw
        .and_then(|value| serde_json::from_value::<ModelRecommendation>(value.clone()).ok())
        .and_then(|recommendation| {
            let label = bounded_prompt_text(&recommendation.label, RECOMMENDATION_LABEL_MAX_CHARS);
            let rationale = bounded_prompt_text(
                &recommendation.rationale,
                RECOMMENDATION_RATIONALE_MAX_CHARS,
            );
            let allowed = capabilities
                .iter()
                .any(|capability| capability.kind == recommendation.kind);
            if !allowed
                || label.is_empty()
                || rationale.is_empty()
                || !recommendation.confidence.is_finite()
                || !(0.0..=1.0).contains(&recommendation.confidence)
                || recommendation.confidence < min_confidence
            {
                return None;
            }
            Some(ResurfacingRecommendation {
                kind: recommendation.kind,
                label,
                rationale,
                confidence: recommendation.confidence,
                content_revision: candidate.content_revision.clone(),
                source: ResurfacingRecommendationSource::Curator,
            })
        });
    model.or_else(|| deterministic_recommendation(candidate, capabilities, min_confidence, now))
}

fn deterministic_recommendation(
    candidate: &Candidate,
    capabilities: &[ResurfacingActionCapability],
    min_confidence: f32,
    now: i64,
) -> Option<ResurfacingRecommendation> {
    let details = candidate.content_details.as_ref();
    let source_omits_details = details.is_some_and(|details| {
        details.detail_status == magician::magician_v2::attention::resurfacing::types::ResurfacingDetailStatus::SourceOmitsDetails
    });
    let future_date = details.is_some_and(|details| {
        let now_ms = now.saturating_mul(1_000);
        details.temporal_facts.iter().any(|fact| {
            matches!(
                fact.kind.trim().to_ascii_lowercase().as_str(),
                "due" | "expiry" | "effective" | "scheduled" | "period_start" | "period_end"
            ) && fact.at_ms.is_some_and(|at_ms| at_ms > now_ms)
        })
    });
    let durable_change = details.is_some_and(|details| !details.changes.is_empty());

    let candidates: &[(&[ResurfacingActionKind], &str, &str, f32)] = if source_omits_details {
        &[(
            &[
                ResurfacingActionKind::OpenSource,
                ResurfacingActionKind::ShowOriginal,
                ResurfacingActionKind::ViewDetails,
            ],
            "Review the exact details",
            "The source summary says the exact details were not included.",
            0.9,
        )]
    } else if future_date {
        &[(
            &[ResurfacingActionKind::CreateReminder],
            "Set a reminder",
            "The brief contains a specific future date worth revisiting.",
            0.82,
        )]
    } else if durable_change {
        &[(
            &[ResurfacingActionKind::SaveToMemory],
            "Keep this change",
            "The brief contains a durable change that may matter later.",
            0.78,
        )]
    } else {
        &[]
    };

    for (preferred, label, rationale, confidence) in candidates {
        if *confidence < min_confidence {
            continue;
        }
        if let Some(kind) = preferred.iter().copied().find(|kind| {
            capabilities
                .iter()
                .any(|capability| capability.kind == *kind)
        }) {
            return Some(deterministic_recommendation_value(
                candidate,
                kind,
                label,
                rationale,
                *confidence,
            ));
        }
    }

    if min_confidence <= 1.0 {
        [
            ResurfacingActionKind::ViewDetails,
            ResurfacingActionKind::OpenSource,
            ResurfacingActionKind::ShowOriginal,
        ]
        .into_iter()
        .find(|kind| {
            capabilities
                .iter()
                .any(|capability| capability.kind == *kind)
        })
        .map(|kind| {
            deterministic_recommendation_value(
                candidate,
                kind,
                "Review details",
                "The current brief is the safest next place to review this item.",
                1.0,
            )
        })
    } else {
        None
    }
}

fn deterministic_recommendation_value(
    candidate: &Candidate,
    kind: ResurfacingActionKind,
    label: &str,
    rationale: &str,
    confidence: f32,
) -> ResurfacingRecommendation {
    ResurfacingRecommendation {
        kind,
        label: label.to_string(),
        rationale: rationale.to_string(),
        confidence,
        content_revision: candidate.content_revision.clone(),
        source: ResurfacingRecommendationSource::Deterministic,
    }
}

fn review_limit_for_cap(cap: usize) -> usize {
    cap.saturating_mul(REVIEW_WINDOW_MULTIPLIER)
        .max(cap)
        .min(CURATION_REVIEW_MAX_CANDIDATES)
}

fn strongest_non_recency_signal(candidate: &Candidate) -> f32 {
    let s = &candidate.signals;
    [
        s.centrality,
        s.temporal_anchor,
        s.dormancy,
        s.frequency,
        s.cooccurrence,
        s.source_affinity,
    ]
    .into_iter()
    .fold(0.0_f32, f32::max)
}

fn candidate_has_non_recency_signal(candidate: &Candidate) -> bool {
    strongest_non_recency_signal(candidate) >= MIN_NON_RECENCY_SIGNAL
}

fn content_details_are_substantive(candidate: &Candidate) -> bool {
    candidate.content_details.as_ref().is_some_and(|details| {
        details.key_facts.iter().any(|fact| !fact.trim().is_empty())
            || details.changes.iter().any(|change| {
                !change.aspect.trim().is_empty()
                    || change
                        .before
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                    || change
                        .after
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
            })
            || details
                .temporal_facts
                .iter()
                .any(|fact| !fact.text.trim().is_empty())
    })
}

fn candidate_has_model_reviewable_content(candidate: &Candidate) -> bool {
    !candidate.title.trim().is_empty()
        || !candidate.content_digest.trim().is_empty()
        || content_details_are_substantive(candidate)
}

fn candidate_prefilter_drop_reason(candidate: &Candidate) -> Option<DropReason> {
    if candidate.source_kind == SourceKind::Comm && candidate.content_revision.is_none() {
        return Some(DropReason::StaleContentRevision);
    }
    if !candidate_has_model_reviewable_content(candidate) {
        return Some(DropReason::MissingSafeSummary);
    }
    if !candidate.salience_score.is_finite() || candidate.salience_score <= 0.0 {
        return Some(DropReason::WeakSignal);
    }
    None
}

fn semantic_quality_drop_can_be_deferred_to_model(
    candidate: &Candidate,
    outcome: &RouteOutcome,
) -> bool {
    candidate_has_model_reviewable_content(candidate)
        && matches!(
            outcome,
            RouteOutcome::Dropped {
                reason: DropReason::RecencyOnly | DropReason::MissingSafeSummary
            }
        )
}

fn routing_scan_limit(desired: usize) -> usize {
    desired
        .saturating_mul(ROUTING_SCAN_MULTIPLIER)
        .max(ROUTING_SCAN_MIN)
        .min(ROUTING_SCAN_MAX)
}

async fn list_router_accepted_resurfacing_candidates(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    desired: usize,
    now: i64,
    attention_store: Option<&AttentionFunnelStore>,
    channel_store: Option<&ChannelAssistStore>,
    defer_semantic_quality_to_model: bool,
) -> Result<Vec<Candidate>> {
    if desired == 0 {
        return Ok(Vec::new());
    }

    // Candidate-state/cooldown eligibility and salience ordering live in the
    // store query. Fetch one bounded review window so routing work is independent
    // of corpus size and never re-reads progressively larger prefixes.
    let scan_limit = routing_scan_limit(desired);
    let candidates = store
        .list_top_candidates(principal, workspace, now, scan_limit)
        .await?;
    let mut eligible = Vec::new();
    for candidate in candidates {
        if let Some(reason) = candidate_prefilter_drop_reason(&candidate) {
            let attention_candidate = attention_candidate_from_resurfacing(&candidate);
            let outcome = RouteOutcome::Dropped { reason };
            record_resurfacing_route_event(
                attention_store,
                principal,
                workspace,
                &candidate,
                &attention_candidate,
                &outcome,
                now,
            )
            .await;
            continue;
        }
        let (accepted, outcome, attention_candidate) = route_resurfacing_candidate(
            principal,
            workspace,
            &candidate,
            now,
            channel_store,
            true,
            false,
        )
        .await?;
        if accepted
            || (defer_semantic_quality_to_model
                && semantic_quality_drop_can_be_deferred_to_model(&candidate, &outcome))
        {
            eligible.push(candidate);
            if eligible.len() >= desired {
                break;
            }
        } else {
            record_resurfacing_route_event(
                attention_store,
                principal,
                workspace,
                &candidate,
                &attention_candidate,
                &outcome,
                now,
            )
            .await;
        }
    }
    eligible.truncate(desired);
    Ok(eligible)
}

async fn route_resurfacing_candidate(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    now: i64,
    channel_store: Option<&ChannelAssistStore>,
    materialize_required_actions: bool,
    strict_source_reads: bool,
) -> Result<(bool, RouteOutcome, AttentionCandidate)> {
    let mut attention_candidate = attention_candidate_from_resurfacing(candidate);
    let comm_evidence = enrich_attention_candidate_from_comm_evidence(
        principal,
        workspace,
        candidate,
        &mut attention_candidate,
        channel_store,
        strict_source_reads,
    )
    .await?;
    let mut context = route_context_for_resurfacing(
        principal,
        workspace,
        candidate,
        now,
        channel_store,
        strict_source_reads,
    )
    .await?;
    if let Some(evidence) = comm_evidence.as_ref() {
        context.sensitive_or_suppressed = evidence.message.as_ref().is_some_and(|message| {
            message.sensitive_suppressed
                || message.distill_state != crate::channel_assist::channel::DistillState::Done
        });
        context.stale_content_revision = evidence.stale_content_revision;
    }
    if candidate.source_kind == SourceKind::Comm
        && comm_evidence
            .as_ref()
            .and_then(|evidence| evidence.message.as_ref())
            .is_none()
    {
        context.stale_content_revision = true;
    }
    let mut outcome = route_attention_candidate(&attention_candidate, &context);

    if materialize_required_actions {
        if let (Some(channel_store), Some(_evidence), Some(required_action), Some(message)) = (
            channel_store,
            comm_evidence.as_ref(),
            comm_evidence
                .as_ref()
                .and_then(|evidence| evidence.required_action),
            comm_evidence
                .as_ref()
                .and_then(|evidence| evidence.message.as_ref()),
        ) {
            match materialize_required_action_annotation(
                principal,
                workspace,
                channel_store,
                candidate,
                message,
                required_action,
                now,
            )
            .await
            {
                Ok(RequiredActionAnnotationResult::Applied {
                    annotation,
                    disposition,
                }) => {
                    attention_candidate.metadata["follow_up_annotation_id"] =
                        serde_json::Value::String(annotation.id);
                    attention_candidate.metadata["follow_up_materialization"] =
                        serde_json::Value::String(
                            required_action_annotation_disposition_key(disposition).to_string(),
                        );
                    match disposition {
                        RequiredActionAnnotationDisposition::PreservedDismissal => {
                            context.owner_dismissed = true;
                            outcome = route_attention_candidate(&attention_candidate, &context);
                        },
                        RequiredActionAnnotationDisposition::PreservedLifecycle => {
                            context.action_already_handled = true;
                            outcome = route_attention_candidate(&attention_candidate, &context);
                        },
                        RequiredActionAnnotationDisposition::Created
                        | RequiredActionAnnotationDisposition::PromotedPassive
                        | RequiredActionAnnotationDisposition::RefreshedNeedsApproval => {},
                    }
                },
                Ok(RequiredActionAnnotationResult::StaleInput { current_revision }) => {
                    context.stale_content_revision = true;
                    attention_candidate.metadata["follow_up_materialization"] =
                        serde_json::Value::String("stale_input".to_string());
                    attention_candidate.metadata["latest_distill_revision"] =
                        current_revision.map_or(serde_json::Value::Null, serde_json::Value::from);
                    outcome = route_attention_candidate(&attention_candidate, &context);
                },
                Err(error) => {
                    warn!(
                        target: LOG_TARGET,
                        source_ref = candidate.source_ref.as_str(),
                        error = %error,
                        "failed to materialize required comm action before Worth-a-look routing"
                    );
                    context.action_materialization_failed = true;
                    attention_candidate.metadata["follow_up_materialization"] =
                        serde_json::Value::String("failed".to_string());
                    attention_candidate.metadata["follow_up_materialization_error"] =
                        serde_json::Value::String(error.to_string());
                    outcome = route_attention_candidate(&attention_candidate, &context);
                },
            }
        }
    }
    Ok((
        routed_to_worth_a_look(&outcome),
        outcome,
        attention_candidate,
    ))
}

#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct ActiveRoutingRepairOutcome {
    pub scanned: u64,
    pub repaired: u64,
    pub accepted: u64,
    pub rerouted: u64,
    pub legacy: u64,
    pub skipped_concurrent: u64,
    pub failed: u64,
}

/// Re-evaluate already-surfaced communication cards against the current
/// deterministic attention contract. Work is bounded, sequential, and
/// receipt-keyed by content revision. Required-action materialization uses the
/// existing idempotent annotation path; a dry run performs neither writes nor
/// telemetry.
pub async fn repair_active_surfaced_candidates(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    channel_store: &ChannelAssistStore,
    attention_store: Option<&AttentionFunnelStore>,
    limit: usize,
    now: i64,
    dry_run: bool,
) -> Result<ActiveRoutingRepairOutcome> {
    const REROUTED_COOLDOWN_SECS: i64 = 30 * 86_400;
    let candidates = store
        .list_active_repair_candidates(principal, workspace, limit)
        .await?;
    let mut result = ActiveRoutingRepairOutcome::default();
    for candidate in candidates {
        result.scanned = result.scanned.saturating_add(1);
        if candidate.content_details.is_none() {
            result.legacy = result.legacy.saturating_add(1);
        }
        let routed = route_resurfacing_candidate(
            principal,
            workspace,
            &candidate,
            now,
            Some(channel_store),
            !dry_run,
            true,
        )
        .await;
        let (accepted, outcome, attention_candidate) = match routed {
            Ok(value) => value,
            Err(error) => {
                result.failed = result.failed.saturating_add(1);
                warn!(
                    target: LOG_TARGET,
                    candidate_id = %candidate.candidate_id,
                    error = %error,
                    "active resurfacing routing repair failed"
                );
                continue;
            },
        };
        if accepted {
            result.accepted = result.accepted.saturating_add(1);
        } else {
            result.rerouted = result.rerouted.saturating_add(1);
        }
        if dry_run {
            continue;
        }

        let outcome_key = if accepted {
            "accepted_worth_a_look"
        } else {
            "rerouted_or_withheld"
        };
        match store
            .complete_active_repair(
                principal,
                workspace,
                &candidate.candidate_id,
                candidate.content_revision.as_deref(),
                outcome_key,
                !accepted,
                now.saturating_add(REROUTED_COOLDOWN_SECS),
                now,
            )
            .await
        {
            Ok(true) => {
                result.repaired = result.repaired.saturating_add(1);
                record_resurfacing_route_event(
                    attention_store,
                    principal,
                    workspace,
                    &candidate,
                    &attention_candidate,
                    &outcome,
                    now,
                )
                .await;
            },
            Ok(false) => result.skipped_concurrent = result.skipped_concurrent.saturating_add(1),
            Err(error) => {
                result.failed = result.failed.saturating_add(1);
                warn!(
                    target: LOG_TARGET,
                    candidate_id = %candidate.candidate_id,
                    error = %error,
                    "failed to commit active resurfacing routing repair"
                );
            },
        }
    }
    if result.failed > 0 {
        anyhow::bail!(
            "active routing repair partially failed: scanned={}, repaired={}, failed={}",
            result.scanned,
            result.repaired,
            result.failed
        );
    }
    Ok(result)
}

#[derive(Debug, Clone)]
struct CommRoutingEvidence {
    message: Option<ChannelMessageMeta>,
    required_action: Option<ChannelRequiredAction>,
    stale_content_revision: bool,
}

pub fn attention_candidate_from_resurfacing(candidate: &Candidate) -> AttentionCandidate {
    let comm_ref = (candidate.source_kind == SourceKind::Comm)
        .then(|| parse_comm_source_ref(&candidate.source_ref))
        .flatten();
    AttentionCandidate {
        candidate_key: candidate.candidate_id.clone(),
        source: AttentionSource {
            kind: attention_source_kind(candidate.source_kind),
            source_ref: candidate.source_ref.clone(),
            provider: comm_ref.as_ref().map(|parsed| parsed.provider.clone()),
            account_alias: comm_ref.as_ref().map(|parsed| parsed.account_alias.clone()),
        },
        source_family: AttentionSourceFamily::Resurfacing,
        evidence_refs: vec![candidate.source_ref.clone()],
        title: candidate.title.clone(),
        summary: candidate.content_digest.clone(),
        action: None,
        urgency: AttentionUrgency::Low,
        confidence: salience_confidence(candidate.salience_score),
        metadata: serde_json::json!({
            "source_kind": candidate.source_kind.as_str(),
            "salience_score": candidate.salience_score,
            "signals": {
                "recency": candidate.signals.recency,
                "frequency": candidate.signals.frequency,
                "centrality": candidate.signals.centrality,
                "cooccurrence": candidate.signals.cooccurrence,
                "temporal_anchor": candidate.signals.temporal_anchor,
                "dormancy": candidate.signals.dormancy,
                "source_affinity": candidate.signals.source_affinity,
            },
            "surface_count": candidate.surface_count,
            "dismiss_count": candidate.dismiss_count,
        }),
    }
}

fn attention_source_kind(kind: SourceKind) -> AttentionSourceKind {
    match kind {
        SourceKind::Memory => AttentionSourceKind::Memory,
        SourceKind::Task => AttentionSourceKind::Task,
        SourceKind::Episode => AttentionSourceKind::Episode,
        SourceKind::Comm => AttentionSourceKind::Comm,
        SourceKind::Calendar => AttentionSourceKind::Calendar,
        SourceKind::Note => AttentionSourceKind::Note,
        SourceKind::Web => AttentionSourceKind::Web,
    }
}

async fn enrich_attention_candidate_from_comm_evidence(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    attention_candidate: &mut AttentionCandidate,
    channel_store: Option<&ChannelAssistStore>,
    strict_source_reads: bool,
) -> Result<Option<CommRoutingEvidence>> {
    if candidate.source_kind != SourceKind::Comm {
        return Ok(None);
    }
    let Some(channel_store) = channel_store else {
        return Ok(None);
    };
    let Some(parsed) = parse_comm_source_ref(&candidate.source_ref) else {
        attention_candidate.metadata["exact_message_evidence"] =
            serde_json::Value::String("invalid_identity".to_string());
        return Ok(Some(CommRoutingEvidence {
            message: None,
            required_action: None,
            stale_content_revision: true,
        }));
    };
    match channel_store
        .get_message(
            principal,
            workspace,
            &parsed.provider,
            &parsed.account_alias,
            &parsed.message_id,
        )
        .await
    {
        Ok(Some(message))
            if message.thread_id == parsed.thread_id
                && message.internal_date == parsed.internal_date =>
        {
            let required_action = derive_channel_required_action(
                message.intent.as_deref(),
                message.needs_reply_hint,
                message.follow_up_hint.as_ref(),
                message.distill_brief.as_ref(),
            );
            apply_comm_message_evidence(attention_candidate, &message, required_action);
            let candidate_revision = candidate
                .content_revision
                .as_deref()
                .and_then(|revision| revision.parse::<i64>().ok());
            let stale_content_revision = match (candidate_revision, message.distill_revision) {
                (Some(candidate), Some(latest)) => candidate != latest,
                _ => true,
            };
            attention_candidate.metadata["candidate_content_revision"] =
                candidate_revision.map_or(serde_json::Value::Null, serde_json::Value::from);
            attention_candidate.metadata["latest_distill_revision"] = message
                .distill_revision
                .map_or(serde_json::Value::Null, serde_json::Value::from);
            attention_candidate.metadata["stale_content_revision"] =
                serde_json::Value::Bool(stale_content_revision);
            Ok(Some(CommRoutingEvidence {
                message: Some(message),
                required_action,
                stale_content_revision,
            }))
        },
        Ok(Some(_)) | Ok(None) => {
            attention_candidate.metadata["exact_message_evidence"] =
                serde_json::Value::String("missing".to_string());
            Ok(Some(CommRoutingEvidence {
                message: None,
                required_action: None,
                stale_content_revision: true,
            }))
        },
        Err(error) => {
            if strict_source_reads {
                return Err(error).context("loading exact comm evidence for active repair");
            }
            warn!(
                target: LOG_TARGET,
                source_ref = candidate.source_ref.as_str(),
                message_id = parsed.message_id.as_str(),
                error = %error,
                "failed to load distilled comm attention hints for resurfacing candidate"
            );
            attention_candidate.metadata["exact_message_evidence"] =
                serde_json::Value::String("load_failed".to_string());
            Ok(Some(CommRoutingEvidence {
                message: None,
                required_action: None,
                stale_content_revision: true,
            }))
        },
    }
}

fn apply_comm_message_evidence(
    attention_candidate: &mut AttentionCandidate,
    message: &ChannelMessageMeta,
    required_action: Option<ChannelRequiredAction>,
) {
    let hints = ChannelMessageAttentionHints {
        intent: message.intent.clone(),
        needs_reply_hint: message.needs_reply_hint,
        follow_up_hint: message.follow_up_hint.clone(),
    };
    apply_comm_attention_hints(attention_candidate, &hints);
    if let Some(required_action) = required_action {
        let kind = match required_action.kind {
            ChannelRequiredActionKind::Reply => AttentionActionKind::Reply,
            ChannelRequiredActionKind::FollowUp => AttentionActionKind::FollowUp,
            ChannelRequiredActionKind::Schedule => AttentionActionKind::Schedule,
        };
        if matches!(
            required_action.kind,
            ChannelRequiredActionKind::FollowUp | ChannelRequiredActionKind::Schedule
        ) {
            attention_candidate.source_family = AttentionSourceFamily::Promise;
        }
        attention_candidate.action = Some(AttentionAction {
            kind,
            label: kind.as_str().to_string(),
            payload: serde_json::json!({
                "attention_hint_source": required_action.source.as_str(),
                "required_action": required_action.kind.as_str(),
                "distill_revision": message.distill_revision,
            }),
        });
        attention_candidate.metadata["required_action"] =
            serde_json::Value::String(required_action.kind.as_str().to_string());
        attention_candidate.metadata["required_action_source"] =
            serde_json::Value::String(required_action.source.as_str().to_string());
    }
}

async fn materialize_required_action_annotation(
    principal: &str,
    workspace: &str,
    channel_store: &ChannelAssistStore,
    candidate: &Candidate,
    message: &ChannelMessageMeta,
    required_action: ChannelRequiredAction,
    now: i64,
) -> Result<RequiredActionAnnotationResult> {
    let Some(distill_revision) = message.distill_revision else {
        return Ok(RequiredActionAnnotationResult::StaleInput {
            current_revision: None,
        });
    };
    let follow_up_kind = match required_action.kind {
        ChannelRequiredActionKind::Reply => "needs_reply",
        ChannelRequiredActionKind::FollowUp => message
            .follow_up_hint
            .as_ref()
            .map(|hint| hint.kind.as_str())
            .filter(|kind| !kind.trim().is_empty())
            .unwrap_or("owner_owes"),
        ChannelRequiredActionKind::Schedule => "schedule",
    };
    let reason = message
        .follow_up_hint
        .as_ref()
        .and_then(|hint| hint.rationale.clone())
        .or_else(|| {
            message
                .distill_brief
                .as_ref()
                .and_then(|brief| brief.stated_action.clone())
        })
        .or_else(|| message.summary.clone())
        .unwrap_or_else(|| "The local distillation contains a required action.".to_string());
    let due_text = message
        .follow_up_hint
        .as_ref()
        .and_then(|hint| hint.due_text.clone())
        .or_else(|| {
            message.distill_brief.as_ref().and_then(|brief| {
                brief
                    .temporal_facts
                    .iter()
                    .find(|fact| {
                        matches!(
                            fact.kind,
                            crate::channel_assist::channel::ChannelTemporalKind::Due
                                | crate::channel_assist::channel::ChannelTemporalKind::Expiry
                                | crate::channel_assist::channel::ChannelTemporalKind::Scheduled
                        )
                    })
                    .map(|fact| fact.text.clone())
            })
        });
    let action_owner = message
        .follow_up_hint
        .as_ref()
        .and_then(|hint| hint.actor.as_deref())
        .unwrap_or("owner");
    let urgency = message
        .follow_up_hint
        .as_ref()
        .and_then(|hint| hint.urgency.as_deref())
        .unwrap_or("normal");
    let mut key_details = message
        .follow_up_hint
        .as_ref()
        .map(|hint| hint.key_details.clone())
        .unwrap_or_default();
    if let Some(brief) = message.distill_brief.as_ref() {
        for fact in &brief.key_facts {
            if key_details.len() >= 8 {
                break;
            }
            if !key_details
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(fact))
            {
                key_details.push(fact.clone());
            }
        }
    }
    let now_ms = now.saturating_mul(1000);
    let annotation = ChannelAnnotation {
        schema_version: CHANNEL_ASSIST_SCHEMA_VERSION,
        id: Uuid::new_v4().to_string(),
        provider: message.provider.clone(),
        account_alias: message.account_alias.clone(),
        thread_id: message.thread_id.clone(),
        lane: Default::default(),
        state: ChannelAnnotationState::NeedsApproval,
        label: Some(
            match required_action.kind {
                ChannelRequiredActionKind::Reply => "needs_reply",
                ChannelRequiredActionKind::FollowUp | ChannelRequiredActionKind::Schedule => {
                    "follow_up"
                },
            }
            .to_string(),
        ),
        confidence: Some(1.0),
        reason: Some(reason),
        evidence_refs: vec![
            format!("thread:{}", message.thread_id),
            format!("message:{}", message.message_id),
            candidate.source_ref.clone(),
        ],
        evidence_message_id: Some(message.message_id.clone()),
        evidence_message_at: Some(message.internal_date),
        classification_input_revision: None,
        semantic_features: None,
        proposed_action: Some(serde_json::json!({
            "follow_up_kind": follow_up_kind,
            "action_owner": action_owner,
            "due_text": due_text,
            "urgency": urgency,
            "key_details": key_details,
            "required_action": required_action.kind.as_str(),
            "required_action_source": required_action.source.as_str(),
            "distill_revision": distill_revision,
            "attention_lane": "follow_up",
            "attention_route_reason": "actionable_communication",
            "provisional": true,
        })),
        provenance: Some("resurfacing_required_action:v1".to_string()),
        created_at: now_ms,
        updated_at: now_ms,
    };
    channel_store
        .ensure_required_action_annotation(principal, workspace, annotation, distill_revision)
        .await
}

fn required_action_annotation_disposition_key(
    disposition: RequiredActionAnnotationDisposition,
) -> &'static str {
    match disposition {
        RequiredActionAnnotationDisposition::Created => "created",
        RequiredActionAnnotationDisposition::PromotedPassive => "promoted_passive",
        RequiredActionAnnotationDisposition::RefreshedNeedsApproval => "refreshed_needs_approval",
        RequiredActionAnnotationDisposition::PreservedLifecycle => "preserved_lifecycle",
        RequiredActionAnnotationDisposition::PreservedDismissal => "preserved_dismissal",
    }
}

fn apply_comm_attention_hints(
    attention_candidate: &mut AttentionCandidate,
    hints: &ChannelMessageAttentionHints,
) -> bool {
    let Some(kind) = comm_attention_action_kind(hints) else {
        return false;
    };
    attention_candidate.source_family = comm_attention_source_family(hints);
    attention_candidate.urgency = comm_attention_urgency(hints).unwrap_or(AttentionUrgency::Normal);
    attention_candidate.action = Some(AttentionAction {
        kind,
        label: kind.as_str().to_string(),
        payload: serde_json::json!({
            "attention_hint_source": "distilled_message",
            "intent": hints.intent.as_deref(),
            "needs_reply_hint": hints.needs_reply_hint,
            "follow_up_hint": hints.follow_up_hint.as_ref(),
        }),
    });
    attention_candidate.metadata["attention_hint_source"] =
        serde_json::Value::String("distilled_message".to_string());
    true
}

fn comm_attention_action_kind(hints: &ChannelMessageAttentionHints) -> Option<AttentionActionKind> {
    if hints.needs_reply_hint {
        return Some(AttentionActionKind::Reply);
    }
    if let Some(hint) = hints.follow_up_hint.as_ref() {
        let kind = hint.kind.trim().to_ascii_lowercase();
        match kind.as_str() {
            "" | "none" | "fyi" => {},
            "needs_reply" | "reply" => return Some(AttentionActionKind::Reply),
            "schedule" | "meeting" => return Some(AttentionActionKind::Schedule),
            "owner_owes" | "other_owes" | "waiting_on" | "check_back" | "follow_up" => {
                return Some(AttentionActionKind::FollowUp);
            },
            _ => return Some(AttentionActionKind::FollowUp),
        }
    }
    match hints
        .intent
        .as_deref()
        .map(|intent| intent.trim().to_ascii_lowercase())
    {
        Some(intent) if intent == "needs_reply" || intent == "reply" => {
            Some(AttentionActionKind::Reply)
        },
        Some(intent)
            if intent == "follow_up"
                || intent == "action_request"
                || intent == "owner_owes"
                || intent == "waiting_on" =>
        {
            Some(AttentionActionKind::FollowUp)
        },
        _ => None,
    }
}

fn comm_attention_source_family(hints: &ChannelMessageAttentionHints) -> AttentionSourceFamily {
    if hints
        .follow_up_hint
        .as_ref()
        .map(follow_up_hint_is_promise_like)
        .unwrap_or(false)
    {
        return AttentionSourceFamily::Promise;
    }
    match hints
        .intent
        .as_deref()
        .map(|intent| intent.trim().to_ascii_lowercase())
    {
        Some(intent)
            if intent == "follow_up"
                || intent == "owner_owes"
                || intent == "other_owes"
                || intent == "waiting_on"
                || intent == "check_back" =>
        {
            AttentionSourceFamily::Promise
        },
        _ => AttentionSourceFamily::CommsIngest,
    }
}

fn follow_up_hint_is_promise_like(hint: &ChannelFollowUpHint) -> bool {
    matches!(
        hint.kind.trim().to_ascii_lowercase().as_str(),
        "owner_owes" | "other_owes" | "waiting_on" | "check_back" | "schedule" | "follow_up"
    )
}

fn comm_attention_urgency(hints: &ChannelMessageAttentionHints) -> Option<AttentionUrgency> {
    let raw = hints.follow_up_hint.as_ref()?.urgency.as_deref()?.trim();
    match raw.to_ascii_lowercase().as_str() {
        "critical" => Some(AttentionUrgency::Critical),
        "high" | "urgent" => Some(AttentionUrgency::High),
        "low" => Some(AttentionUrgency::Low),
        "normal" | "medium" => Some(AttentionUrgency::Normal),
        _ => None,
    }
}

async fn route_context_for_resurfacing(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    now: i64,
    channel_store: Option<&ChannelAssistStore>,
    strict_source_reads: bool,
) -> Result<AttentionRouteContext> {
    Ok(AttentionRouteContext {
        active_follow_up_exists: active_follow_up_exists_for_candidate(
            principal,
            workspace,
            candidate,
            channel_store,
            strict_source_reads,
        )
        .await?,
        recency_only: !candidate_has_non_recency_signal(candidate),
        missing_safe_summary: candidate.content_digest.trim().is_empty(),
        cooldown_active: candidate.cooldown_until > now,
        weak_signal: !candidate.salience_score.is_finite() || candidate.salience_score <= 0.0,
        non_actionable_useful_context: true,
        ..Default::default()
    })
}

async fn active_follow_up_exists_for_candidate(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    channel_store: Option<&ChannelAssistStore>,
    strict_source_reads: bool,
) -> Result<bool> {
    if candidate.source_kind != SourceKind::Comm {
        return Ok(false);
    }
    let Some(channel_store) = channel_store else {
        return Ok(false);
    };
    let Some(parsed) = parse_comm_source_ref(&candidate.source_ref) else {
        return Ok(false);
    };
    let thread_ids = vec![parsed.thread_id.clone()];
    let exact_thread_match = match channel_store
        .list_annotations_by_thread_ids(
            principal,
            workspace,
            &parsed.provider,
            &parsed.account_alias,
            &thread_ids,
        )
        .await
    {
        Ok(annotations) => annotations.iter().any(annotation_is_active_follow_up),
        Err(error) => {
            if strict_source_reads {
                return Err(error).context("checking active comm follow-up for active repair");
            }
            warn!(
                target: LOG_TARGET,
                source_ref = candidate.source_ref.as_str(),
                error = %error,
                "failed to check active follow-up overlap for resurfacing candidate"
            );
            false
        },
    };
    if exact_thread_match {
        return Ok(true);
    }
    active_follow_up_exists_for_message_ref(
        principal,
        workspace,
        channel_store,
        candidate,
        &parsed,
        strict_source_reads,
    )
    .await
}

async fn active_follow_up_exists_for_message_ref(
    principal: &str,
    workspace: &str,
    channel_store: &ChannelAssistStore,
    candidate: &Candidate,
    parsed: &CommSourceRef,
    strict_source_reads: bool,
) -> Result<bool> {
    match channel_store
        .list_annotations_by_evidence_message_id(
            principal,
            workspace,
            &parsed.provider,
            &parsed.message_id,
        )
        .await
    {
        Ok(annotations) => Ok(annotations.iter().any(annotation_is_active_follow_up)),
        Err(error) => {
            if strict_source_reads {
                return Err(error)
                    .context("checking active comm evidence overlap for active repair");
            }
            warn!(
                target: LOG_TARGET,
                source_ref = candidate.source_ref.as_str(),
                message_id = parsed.message_id.as_str(),
                error = %error,
                "failed to check active follow-up overlap by resurfacing evidence message"
            );
            Ok(false)
        },
    }
}

fn annotation_is_active_follow_up(annotation: &ChannelAnnotation) -> bool {
    matches!(
        annotation.state,
        ChannelAnnotationState::NeedsApproval
            | ChannelAnnotationState::Approved
            | ChannelAnnotationState::Scheduled
            | ChannelAnnotationState::DraftRequested
            | ChannelAnnotationState::DraftReady
            | ChannelAnnotationState::Inserted
            | ChannelAnnotationState::SentDetected
    )
}

fn routed_to_worth_a_look(outcome: &RouteOutcome) -> bool {
    matches!(
        outcome,
        RouteOutcome::Routed {
            lane: AttentionLane::WorthALook,
            ..
        }
    )
}

fn salience_confidence(score: f32) -> Option<f32> {
    if score.is_finite() {
        Some(score.clamp(0.0, 1.0))
    } else {
        None
    }
}

async fn record_resurfacing_route_event(
    attention_store: Option<&AttentionFunnelStore>,
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    attention_candidate: &AttentionCandidate,
    outcome: &RouteOutcome,
    now: i64,
) {
    let Some(attention_store) = attention_store else {
        return;
    };
    let stage = match outcome {
        RouteOutcome::Routed { .. } => AttentionFunnelStage::Routed,
        RouteOutcome::Dropped { .. } => AttentionFunnelStage::Dropped,
        RouteOutcome::Traced { .. } => AttentionFunnelStage::Filtered,
    };
    let now_ms = now.saturating_mul(1000);
    let filtered_status = match outcome {
        RouteOutcome::Routed { .. } => AttentionTraceStatus::Succeeded,
        RouteOutcome::Dropped { .. } | RouteOutcome::Traced { .. } => AttentionTraceStatus::Skipped,
    };
    let mut events = vec![
        resurfacing_trace_event(
            principal,
            workspace,
            candidate,
            attention_candidate,
            AttentionFunnelStage::Ingested,
            AttentionTraceStatus::Succeeded,
            now_ms,
            serde_json::json!({ "trace": "candidate_loaded_for_curation" }),
        ),
        resurfacing_trace_event(
            principal,
            workspace,
            candidate,
            attention_candidate,
            AttentionFunnelStage::Extracted,
            AttentionTraceStatus::Succeeded,
            now_ms,
            serde_json::json!({ "trace": "candidate_normalized_for_router" }),
        ),
        resurfacing_trace_event(
            principal,
            workspace,
            candidate,
            attention_candidate,
            AttentionFunnelStage::Filtered,
            filtered_status,
            now_ms,
            serde_json::json!({
                "trace": "router_filter_evaluated",
                "route_outcome": route_outcome_key(outcome),
            }),
        ),
        AttentionRouteEvent {
            event_id: resurfacing_route_event_id(principal, workspace, candidate, outcome),
            scope: AttentionScope {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
            },
            source: attention_candidate.source.clone(),
            source_family: attention_candidate.source_family,
            candidate_key: attention_candidate.candidate_key.clone(),
            stage,
            outcome: outcome.clone(),
            occurred_at: candidate.last_scored_at.saturating_mul(1000),
            created_at: now_ms,
            confidence: attention_candidate.confidence,
            metadata: resurfacing_event_metadata(
                candidate,
                attention_candidate,
                outcome,
                serde_json::json!({ "trace": "terminal_route_decision" }),
            ),
        },
    ];
    if routed_to_worth_a_look(outcome) {
        events.push(resurfacing_trace_event(
            principal,
            workspace,
            candidate,
            attention_candidate,
            AttentionFunnelStage::Surfaced,
            AttentionTraceStatus::Succeeded,
            now_ms,
            serde_json::json!({ "trace": "candidate_marked_surfaced" }),
        ));
    }
    for event in events {
        let event_id = event.event_id.clone();
        if let Err(error) = attention_store.append_event(event).await {
            warn!(
                target: LOG_TARGET,
                candidate_id = candidate.candidate_id.as_str(),
                event_id = %event_id,
                error = %error,
                "failed to record attention funnel event for resurfacing candidate"
            );
        }
    }
}

fn resurfacing_trace_event(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    attention_candidate: &AttentionCandidate,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
    now_ms: i64,
    detail: serde_json::Value,
) -> AttentionRouteEvent {
    AttentionRouteEvent {
        event_id: resurfacing_trace_event_id(principal, workspace, candidate, stage, status),
        scope: AttentionScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        },
        source: attention_candidate.source.clone(),
        source_family: attention_candidate.source_family,
        candidate_key: attention_candidate.candidate_key.clone(),
        stage,
        outcome: RouteOutcome::Traced { status },
        occurred_at: candidate.last_scored_at.saturating_mul(1000),
        created_at: now_ms,
        confidence: attention_candidate.confidence,
        metadata: resurfacing_event_metadata(
            candidate,
            attention_candidate,
            &RouteOutcome::Traced { status },
            detail,
        ),
    }
}

fn resurfacing_event_metadata(
    candidate: &Candidate,
    attention_candidate: &AttentionCandidate,
    outcome: &RouteOutcome,
    detail: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "producer": "resurfacing_curator",
        "source_kind": candidate.source_kind.as_str(),
        "candidate_state": candidate.state.as_str(),
        "salience_score": candidate.salience_score,
        "surface_count": candidate.surface_count,
        "dismiss_count": candidate.dismiss_count,
        "cooldown_until": candidate.cooldown_until,
        "first_seen_at": candidate.first_seen_at,
        "last_scored_at": candidate.last_scored_at,
        "last_surfaced_at": candidate.last_surfaced_at,
        "content_revision": candidate.content_revision,
        "routing_evidence": attention_candidate.metadata,
        "surface_eligible": routed_to_worth_a_look(outcome),
        "detail": detail,
    })
}

fn resurfacing_trace_event_id(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    stage: AttentionFunnelStage,
    status: AttentionTraceStatus,
) -> String {
    let raw = format!(
        "resurfacing_trace\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        candidate.candidate_id,
        candidate.content_revision.as_deref().unwrap_or(""),
        candidate.last_scored_at,
        candidate.state.as_str(),
        stage.as_str(),
        status.as_str(),
    );
    format!(
        "resurfacing-trace:{}",
        blake3::hash(raw.as_bytes()).to_hex()
    )
}

fn resurfacing_route_event_id(
    principal: &str,
    workspace: &str,
    candidate: &Candidate,
    outcome: &RouteOutcome,
) -> String {
    let raw = format!(
        "resurfacing_route\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}\x1f{}",
        principal,
        workspace,
        candidate.candidate_id,
        candidate.content_revision.as_deref().unwrap_or(""),
        candidate.last_scored_at,
        candidate.state.as_str(),
        route_outcome_key(outcome),
    );
    format!(
        "resurfacing-route:{}",
        blake3::hash(raw.as_bytes()).to_hex()
    )
}

fn route_outcome_key(outcome: &RouteOutcome) -> String {
    match outcome {
        RouteOutcome::Routed {
            lane,
            reason,
            priority,
        } => format!(
            "routed:{}:{}:{}",
            lane.as_str(),
            reason.as_str(),
            priority.as_str()
        ),
        RouteOutcome::Dropped { reason } => format!("dropped:{}", reason.as_str()),
        RouteOutcome::Traced { status } => format!("traced:{}", status.as_str()),
    }
}

fn rejected_candidate_ids(candidates: &[Candidate], resolved: &[ResolvedSelection]) -> Vec<String> {
    let selected: std::collections::HashSet<usize> = resolved.iter().map(|r| r.index).collect();
    candidates
        .iter()
        .enumerate()
        .filter(|(idx, _)| !selected.contains(idx))
        .map(|(_, c)| c.candidate_id.clone())
        .collect()
}

/// LLM-curated Phase-2 surfacing (idle-until-bound). Picks a top-ranked
/// shortlist of eligible candidates, then asks the `resurfacing_curate`
/// operation to choose the few most worth resurfacing and phrase each. The LLM reviews a wider
/// shortlist than `cap`; any reviewed candidate it declines is temporarily
/// cooled down so future passes rotate through the pool instead of reconsidering
/// the same rejected top rows forever. Falls back to the
/// deterministic [`run_curation_pass_with_attention`] whenever the LLM lane is
/// unavailable — `router` is `None`, the op is unbound, the call errors, or the
/// reply is unparseable — so the pass never fails and is a strict superset of
/// the deterministic behavior. Chosen rows are marked surfaced and their
/// phrasing is written to `resurfacing_phrasing`; the returned candidates mirror
/// the Surfaced transition. Never surfaces more than `cap`.
pub async fn run_curation_pass_llm_with_attention(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    cap: usize,
    now: i64,
    router: Option<Arc<OperationLlmRouter>>,
    attention_store: Option<&AttentionFunnelStore>,
    channel_store: Option<&ChannelAssistStore>,
) -> Result<Vec<Candidate>> {
    run_curation_pass_llm_with_recommendations(
        principal,
        workspace,
        store,
        cap,
        now,
        router,
        attention_store,
        channel_store,
        CurationRecommendationPolicy::disabled(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_curation_pass_llm_with_recommendations(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    cap: usize,
    now: i64,
    router: Option<Arc<OperationLlmRouter>>,
    attention_store: Option<&AttentionFunnelStore>,
    channel_store: Option<&ChannelAssistStore>,
    recommendation_policy: CurationRecommendationPolicy,
) -> Result<Vec<Candidate>> {
    run_curation_pass_llm_with_recommendations_and_telemetry(
        principal,
        workspace,
        store,
        cap,
        now,
        router,
        attention_store,
        channel_store,
        recommendation_policy,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_curation_pass_llm_with_recommendations_and_telemetry(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    cap: usize,
    now: i64,
    router: Option<Arc<OperationLlmRouter>>,
    attention_store: Option<&AttentionFunnelStore>,
    channel_store: Option<&ChannelAssistStore>,
    recommendation_policy: CurationRecommendationPolicy,
    telemetry: Option<&OperationLlmTelemetryContext>,
) -> Result<Vec<Candidate>> {
    // No router or unbound op ⇒ deterministic pick (no default-remote surprise).
    let Some(router) = router.as_ref() else {
        debug!(target: LOG_TARGET, "no router — deterministic curation");
        return run_deterministic_curation_pass(
            principal,
            workspace,
            store,
            cap,
            now,
            attention_store,
            channel_store,
            None,
            recommendation_policy,
        )
        .await;
    };
    if router
        .explicit_binding_for_operation(RESURFACING_CURATE_OPERATION)
        .is_none()
    {
        debug!(target: LOG_TARGET, "resurfacing_curate unbound — deterministic curation");
        return run_deterministic_curation_pass(
            principal,
            workspace,
            store,
            cap,
            now,
            attention_store,
            channel_store,
            Some(router.as_ref()),
            recommendation_policy,
        )
        .await;
    }

    let review_limit = review_limit_for_cap(cap);
    let top = list_router_accepted_resurfacing_candidates(
        principal,
        workspace,
        store,
        review_limit,
        now,
        attention_store,
        channel_store,
        true,
    )
    .await?;
    if top.is_empty() {
        return Ok(Vec::new());
    }

    let capabilities = top
        .iter()
        .map(|candidate| {
            metadata_capabilities(
                candidate,
                Some(router.as_ref()),
                recommendation_policy.contextual_actions_enabled,
            )
        })
        .collect::<Vec<_>>();
    let digest = build_curation_digest(&top, &capabilities);
    let mut vars = HashMap::new();
    vars.insert("candidates".to_string(), digest);
    let prompts = async {
        let system = rendered_prompt(
            prompt_names::RESURFACING_CURATE_SYSTEM,
            prompt_versions::RESURFACING_CURATE,
            HashMap::new(),
        )
        .await
        .context("rendering managed resurfacing curator system prompt")?;
        let user = rendered_prompt(
            prompt_names::RESURFACING_CURATE_USER,
            prompt_versions::RESURFACING_CURATE,
            vars,
        )
        .await
        .context("rendering managed resurfacing curator user prompt")?;
        Ok::<_, anyhow::Error>((system, user))
    }
    .await;
    let (system, user) = match prompts {
        Ok(prompts) => prompts,
        Err(error) => {
            warn!(target: LOG_TARGET, error = %error, "managed resurfacing curator prompt unavailable; using deterministic curation");
            return run_deterministic_curation_pass(
                principal,
                workspace,
                store,
                cap,
                now,
                attention_store,
                channel_store,
                Some(router.as_ref()),
                recommendation_policy,
            )
            .await;
        },
    };

    let operation = LLMOperation::Other(RESURFACING_CURATE_OPERATION.to_string());
    let llm_started = std::time::Instant::now();
    let scoped_router =
        router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)));
    let response = match scoped_router
        .generate_for_operation_with_system(&operation, Some(&system), &user)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            debug!(target: LOG_TARGET, error = %error, "resurfacing_curate LLM call failed — deterministic fallback");
            return run_deterministic_curation_pass(
                principal,
                workspace,
                store,
                cap,
                now,
                attention_store,
                channel_store,
                Some(router.as_ref()),
                recommendation_policy,
            )
            .await;
        },
    };
    let parsed = parse_curation_reply(&response.content);
    if let Some(telemetry) = telemetry {
        let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        match parsed.as_ref() {
            Some(_) => telemetry.emit_validated_success(
                RESURFACING_CURATE_OPERATION,
                &response,
                latency_ms,
                OperationLlmCallAttribution::default(),
                "resurfacing_curation_json",
            ),
            None => telemetry.emit_validation_failure(
                RESURFACING_CURATE_OPERATION,
                &response,
                latency_ms,
                OperationLlmCallAttribution::default(),
                "resurfacing_curation_json",
                "resurfacing curation reply was not parseable JSON",
            ),
        }
    }

    let Some(selections) = parsed else {
        debug!(target: LOG_TARGET, "resurfacing_curate reply not parseable — deterministic fallback");
        return run_deterministic_curation_pass(
            principal,
            workspace,
            store,
            cap,
            now,
            attention_store,
            channel_store,
            Some(router.as_ref()),
            recommendation_policy,
        )
        .await;
    };

    // Resolve against the actual list (out-of-range/blank/dupes dropped, capped).
    // An empty resolution is the curator legitimately choosing to surface
    // nothing — respect it rather than falling back to the deterministic pick.
    let resolved = resolve_selections(
        &top,
        cap,
        &selections,
        &capabilities,
        recommendation_policy,
        now,
    );
    let rejected_ids = rejected_candidate_ids(&top, &resolved);
    if !rejected_ids.is_empty() {
        let deferred_ids = store
            .defer_candidates(
                principal,
                workspace,
                &rejected_ids,
                now + REVIEW_REJECT_COOLDOWN_SECS,
            )
            .await?;
        debug!(
            target: LOG_TARGET,
            reviewed = top.len(),
            selected = resolved.len(),
            deferred = deferred_ids.len(),
            "resurfacing_curate deferred reviewed-but-unselected candidates",
        );
        let deferred_id_set = deferred_ids.into_iter().collect::<HashSet<_>>();
        let rejected = rejected_ids
            .iter()
            .filter(|id| deferred_id_set.contains(*id))
            .filter_map(|id| top.iter().find(|candidate| candidate.candidate_id == *id))
            .cloned()
            .collect::<Vec<_>>();
        record_resurfacing_drop_events(
            attention_store,
            principal,
            workspace,
            &rejected,
            now,
            DropReason::CuratorDeferred,
        )
        .await;
    }
    if resolved.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<String> = resolved
        .iter()
        .map(|r| top[r.index].candidate_id.clone())
        .collect();
    let surfaced_ids = store.mark_surfaced(principal, workspace, &ids, now).await?;
    if surfaced_ids.is_empty() {
        return Ok(Vec::new());
    }
    let surfaced_id_set = surfaced_ids.into_iter().collect::<HashSet<_>>();
    let selected_resolved = resolved
        .iter()
        .filter(|r| surfaced_id_set.contains(&top[r.index].candidate_id))
        .collect::<Vec<_>>();
    // Phrasing is BEST-EFFORT: the rows are already durably surfaced, and the
    // `today` read falls back to the generic signal-derived `why_now` when a
    // phrasing row is absent. A phrasing write failure must NOT make this pass
    // return Err (which the worker would record as a failed run despite the
    // surfacing having happened) — log and continue.
    for r in &selected_resolved {
        if let Err(error) = store
            .upsert_phrasing(
                principal,
                workspace,
                &top[r.index].candidate_id,
                &r.line,
                &r.why,
                top[r.index].content_revision.as_deref(),
                now,
            )
            .await
        {
            debug!(
                target: LOG_TARGET,
                candidate_id = %top[r.index].candidate_id,
                error = %error,
                "resurfacing_curate: phrasing write failed (surfaced anyway; today falls back)"
            );
        }
        if let Some(recommendation) = r.recommendation.as_ref() {
            if let Err(error) = store
                .upsert_recommendation(
                    principal,
                    workspace,
                    &top[r.index].candidate_id,
                    recommendation,
                    now,
                )
                .await
            {
                debug!(
                    target: LOG_TARGET,
                    candidate_id = %top[r.index].candidate_id,
                    error = %error,
                    "resurfacing_curate: recommendation write failed (surfaced anyway)"
                );
            }
        }
    }

    // Mirror the persisted Surfaced transition into the returned copies.
    let mut out = Vec::with_capacity(selected_resolved.len());
    for r in &selected_resolved {
        let mut c = top[r.index].clone();
        c.state = CandidateState::Surfaced;
        c.last_surfaced_at = Some(now);
        c.surface_count += 1;
        out.push(c);
    }
    record_resurfacing_routed_events(attention_store, principal, workspace, &out, now).await;
    Ok(out)
}

async fn record_resurfacing_routed_events(
    attention_store: Option<&AttentionFunnelStore>,
    principal: &str,
    workspace: &str,
    candidates: &[Candidate],
    now: i64,
) {
    for candidate in candidates {
        let attention_candidate = attention_candidate_from_resurfacing(candidate);
        let outcome = RouteOutcome::Routed {
            lane: AttentionLane::WorthALook,
            reason: RouteReason::NonActionableUsefulContext,
            priority: RoutePriority::Low,
        };
        record_resurfacing_route_event(
            attention_store,
            principal,
            workspace,
            candidate,
            &attention_candidate,
            &outcome,
            now,
        )
        .await;
    }
}

async fn record_resurfacing_drop_events(
    attention_store: Option<&AttentionFunnelStore>,
    principal: &str,
    workspace: &str,
    candidates: &[Candidate],
    now: i64,
    reason: DropReason,
) {
    for candidate in candidates {
        let attention_candidate = attention_candidate_from_resurfacing(candidate);
        let outcome = RouteOutcome::Dropped { reason };
        record_resurfacing_route_event(
            attention_store,
            principal,
            workspace,
            candidate,
            &attention_candidate,
            &outcome,
            now,
        )
        .await;
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::channel_assist::channel::{
        ChannelDetailStatus, ChannelInformationBrief, ChannelInformationType, ChannelLane,
        ChannelRecordOrigin, ChannelTemporalFact, ChannelTemporalKind, ChannelThreadRecord,
        DistillState, MessageDirection,
    };
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician::magician_v2::attention::resurfacing::types::{
        candidate_id, FeedbackAction, ResurfacingChangeFact, ResurfacingContentDetails,
        ResurfacingDetailStatus, ResurfacingTemporalFact, SalienceSignals, SourceKind,
    };
    use magician::magician_v2::attention_funnel::DropReason;
    use tempfile::TempDir;

    #[test]
    fn routing_scan_window_is_corpus_independent_and_bounded() {
        assert_eq!(routing_scan_limit(1), ROUTING_SCAN_MIN);
        assert_eq!(routing_scan_limit(10), 160);
        assert_eq!(routing_scan_limit(usize::MAX), ROUTING_SCAN_MAX);
    }

    #[test]
    fn curation_prefilter_defers_semantic_quality_to_the_model() {
        let mut title_only = sample_candidate("title-only", 0.8);
        title_only.title = "Project Atlas".to_string();
        title_only.content_digest = "Project Atlas".to_string();
        assert_eq!(candidate_prefilter_drop_reason(&title_only), None);

        let mut activity_only = sample_candidate("activity-only", 0.8);
        activity_only.content_digest = "Recently active group chat".to_string();
        assert_eq!(candidate_prefilter_drop_reason(&activity_only), None);

        let mut recency_only = sample_candidate("recency-only", 0.8);
        recency_only.content_digest = "A concrete project decision".to_string();
        recency_only.signals = SalienceSignals {
            recency: 0.9,
            ..Default::default()
        };
        assert_eq!(candidate_prefilter_drop_reason(&recency_only), None);

        let mut empty = sample_candidate("empty", 0.8);
        empty.title.clear();
        empty.content_digest.clear();
        empty.content_details = None;
        assert_eq!(
            candidate_prefilter_drop_reason(&empty),
            Some(DropReason::MissingSafeSummary)
        );
    }

    #[test]
    fn model_review_may_defer_only_semantic_quality_drops() {
        let candidate = sample_candidate("reviewable", 0.8);
        assert!(semantic_quality_drop_can_be_deferred_to_model(
            &candidate,
            &RouteOutcome::Dropped {
                reason: DropReason::RecencyOnly,
            }
        ));
        assert!(semantic_quality_drop_can_be_deferred_to_model(
            &candidate,
            &RouteOutcome::Dropped {
                reason: DropReason::MissingSafeSummary,
            }
        ));
        assert!(!semantic_quality_drop_can_be_deferred_to_model(
            &candidate,
            &RouteOutcome::Dropped {
                reason: DropReason::StaleContentRevision,
            }
        ));
        assert!(!semantic_quality_drop_can_be_deferred_to_model(
            &candidate,
            &RouteOutcome::Dropped {
                reason: DropReason::WeakSignal,
            }
        ));
    }

    #[test]
    fn curation_prefilter_preserves_concise_candidates_with_substantive_details() {
        let mut candidate = sample_candidate("concise-detail", 0.8);
        candidate.title = "Policy".to_string();
        candidate.content_digest = "Policy".to_string();
        candidate.content_details = Some(details(
            ResurfacingDetailStatus::Complete,
            Vec::new(),
            Vec::new(),
        ));

        assert_eq!(candidate_prefilter_drop_reason(&candidate), None);
    }

    #[test]
    fn curation_prefilter_rejects_revisionless_comm_candidate_as_stale() {
        let mut candidate = sample_candidate("legacy-comm", 0.8);
        candidate.source_kind = SourceKind::Comm;
        candidate.content_revision = None;

        assert_eq!(
            candidate_prefilter_drop_reason(&candidate),
            Some(DropReason::StaleContentRevision)
        );
    }

    #[tokio::test]
    async fn malformed_comm_identity_fails_closed_from_worth_a_look() {
        let mut candidate = sample_candidate("not-a-comm-ref", 0.8);
        candidate.source_kind = SourceKind::Comm;
        candidate.candidate_id = candidate_id(SourceKind::Comm, &candidate.source_ref);

        let (accepted, outcome, _) =
            route_resurfacing_candidate("p", "w", &candidate, 2_000, None, false, false)
                .await
                .unwrap();

        assert!(!accepted);
        assert_eq!(
            outcome,
            RouteOutcome::Dropped {
                reason: DropReason::StaleContentRevision,
            }
        );
    }

    /// A fresh `Candidate`-state memory row with the given ref and score, no
    /// cooldown. Mirrors the store's test helper but scoped to this module.
    fn sample_candidate(source_ref: &str, score: f32) -> Candidate {
        let source_kind = SourceKind::Memory;
        Candidate {
            candidate_id: candidate_id(source_kind, source_ref),
            source_kind,
            source_ref: source_ref.to_string(),
            title: format!("title-{source_ref}"),
            content_digest: format!("digest-{source_ref}"),
            content_details: None,
            content_revision: None,
            semantic_features: None,
            salience_score: score,
            signals: SalienceSignals {
                centrality: score,
                ..SalienceSignals::default()
            },
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Candidate,
            first_seen_at: 1_000,
            last_scored_at: 1_000,
            last_surfaced_at: None,
            cooldown_until: 0,
            surface_count: 0,
            dismiss_count: 0,
        }
    }

    fn comm_candidate(source_ref: &str) -> Candidate {
        let mut candidate = sample_candidate(source_ref, 0.8);
        candidate.source_kind = SourceKind::Comm;
        candidate.source_ref = format!("gmail/business/thread/{source_ref}@1000000");
        candidate.candidate_id = candidate_id(candidate.source_kind, &candidate.source_ref);
        candidate.content_revision = Some("7".to_string());
        candidate
    }

    fn details(
        status: ResurfacingDetailStatus,
        changes: Vec<ResurfacingChangeFact>,
        temporal_facts: Vec<ResurfacingTemporalFact>,
    ) -> ResurfacingContentDetails {
        ResurfacingContentDetails {
            schema_version: 2,
            key_facts: vec!["A concrete source-supported fact".to_string()],
            changes,
            temporal_facts,
            detail_status: status,
            missing_details: if status == ResurfacingDetailStatus::SourceOmitsDetails {
                vec!["exact changed limit".to_string()]
            } else {
                Vec::new()
            },
        }
    }

    fn resolve_one_recommendation(
        candidate: Candidate,
        recommended_action: Option<Value>,
        contextual_actions_enabled: bool,
        min_confidence: f32,
        now: i64,
    ) -> ResolvedSelection {
        let capabilities = vec![metadata_capabilities(
            &candidate,
            None,
            contextual_actions_enabled,
        )];
        resolve_selections(
            &[candidate],
            1,
            &[CurationSelection {
                index: 0,
                line: "Concrete line".to_string(),
                why_now: "Useful now".to_string(),
                recommended_action,
            }],
            &capabilities,
            CurationRecommendationPolicy::enabled(contextual_actions_enabled, min_confidence),
            now,
        )
        .into_iter()
        .next()
        .expect("selection remains valid")
    }

    #[tokio::test]
    async fn router_drops_recency_only_resurfacing_candidate() {
        let mut candidate = sample_candidate("recent-only", 0.9);
        candidate.signals = SalienceSignals {
            recency: 0.9,
            ..SalienceSignals::default()
        };

        let attention_candidate = attention_candidate_from_resurfacing(&candidate);
        let context = route_context_for_resurfacing("p", "w", &candidate, 2_000, None, false)
            .await
            .unwrap();
        let outcome = route_attention_candidate(&attention_candidate, &context);

        assert_eq!(
            outcome,
            RouteOutcome::Dropped {
                reason: DropReason::RecencyOnly
            }
        );
        assert!(
            !route_resurfacing_candidate("p", "w", &candidate, 2_000, None, true, false)
                .await
                .unwrap()
                .0,
            "recency alone must not occupy Worth a look"
        );
    }

    #[tokio::test]
    async fn router_allows_non_actionable_useful_context_into_worth_a_look() {
        let candidate = sample_candidate("useful-context", 0.4);

        let attention_candidate = attention_candidate_from_resurfacing(&candidate);
        let context = route_context_for_resurfacing("p", "w", &candidate, 2_000, None, false)
            .await
            .unwrap();
        let outcome = route_attention_candidate(&attention_candidate, &context);

        assert!(matches!(
            outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::WorthALook,
                ..
            }
        ));
        assert!(
            route_resurfacing_candidate("p", "w", &candidate, 2_000, None, true, false)
                .await
                .unwrap()
                .0,
            "non-actionable useful context is eligible for Worth a look"
        );
    }

    #[test]
    fn distilled_comm_hints_route_actionable_rows_out_of_worth_a_look() {
        let mut candidate = sample_candidate("gmail/business/thread-1/msg-1@1783209600000", 0.4);
        candidate.source_kind = SourceKind::Comm;
        let mut attention_candidate = attention_candidate_from_resurfacing(&candidate);
        let applied = apply_comm_attention_hints(
            &mut attention_candidate,
            &ChannelMessageAttentionHints {
                intent: Some("follow_up".to_string()),
                needs_reply_hint: false,
                follow_up_hint: Some(ChannelFollowUpHint {
                    kind: "owner_owes".to_string(),
                    actor: Some("owner".to_string()),
                    counterparty: Some("Acme".to_string()),
                    due_text: Some("this week".to_string()),
                    urgency: Some("high".to_string()),
                    rationale: Some("The owner promised to send the invoice.".to_string()),
                    key_details: vec!["Due this week".to_string()],
                }),
            },
        );

        assert!(applied);
        assert_eq!(
            attention_candidate.source_family,
            AttentionSourceFamily::Promise
        );
        assert_eq!(attention_candidate.urgency, AttentionUrgency::High);
        let outcome = route_attention_candidate(
            &attention_candidate,
            &AttentionRouteContext {
                non_actionable_useful_context: true,
                ..Default::default()
            },
        );
        assert_eq!(
            outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::FollowUp,
                reason: RouteReason::PromiseOrObligation,
                priority: RoutePriority::High,
            }
        );
    }

    #[tokio::test]
    async fn v2_deadline_materializes_one_follow_up_and_never_surfaces_in_worth_a_look() {
        let tmp = TempDir::new().unwrap();
        let channel_store = ChannelAssistStore::open(tmp.path()).unwrap();
        let message_at = 1_783_209_600_000;
        channel_store
            .upsert_thread(
                "p",
                "w",
                ChannelThreadRecord {
                    schema_version: CHANNEL_ASSIST_SCHEMA_VERSION,
                    provider: "gmail".to_string(),
                    account_alias: "business".to_string(),
                    account_email: Some("owner@example.com".to_string()),
                    thread_id: "thread-1".to_string(),
                    lane: ChannelLane::UserAssist,
                    subject: Some("Invoice due".to_string()),
                    latest_summary: None,
                    latest_from_name: Some("Acme".to_string()),
                    latest_from_address: Some("billing@acme.example".to_string()),
                    recipient_domains: vec!["example.com".to_string()],
                    label_ids: vec!["inbox".to_string()],
                    message_count: 1,
                    last_message_at: Some(message_at),
                    provider_cursor: None,
                    sensitive_suppressed: false,
                    origin: ChannelRecordOrigin::MetadataSync,
                    first_observed_at: message_at,
                    last_observed_at: message_at,
                },
            )
            .await
            .unwrap();
        channel_store
            .append_messages(
                "p",
                "w",
                vec![ChannelMessageMeta {
                    schema_version: CHANNEL_ASSIST_SCHEMA_VERSION,
                    provider: "gmail".to_string(),
                    account_alias: "business".to_string(),
                    account_email: Some("owner@example.com".to_string()),
                    thread_id: "thread-1".to_string(),
                    message_id: "msg-1".to_string(),
                    provider_cursor: None,
                    label_ids: vec!["inbox".to_string()],
                    subject: Some("Invoice due".to_string()),
                    from_name: Some("Acme".to_string()),
                    from_address: Some("billing@acme.example".to_string()),
                    to_domains: vec!["example.com".to_string()],
                    cc_domains: Vec::new(),
                    internal_date: message_at,
                    observed_at: message_at,
                    direction: Some(MessageDirection::Inbound),
                    summary: None,
                    intent: None,
                    needs_reply_hint: false,
                    follow_up_hint: None,
                    distill_brief: None,
                    distill_contract_version: None,
                    distilled_at: None,
                    distill_revision: None,
                    distill_state: DistillState::Pending,
                    distill_attempts: 0,
                    sensitive_suppressed: false,
                    origin: ChannelRecordOrigin::MetadataSync,
                }],
            )
            .await
            .unwrap();
        let summary = "The invoice is due July 15, 2026.";
        let brief = ChannelInformationBrief {
            schema_version: 2,
            information_type: ChannelInformationType::Deadline,
            summary: summary.to_string(),
            key_facts: vec!["Amount: INR 1,250".to_string()],
            changes: Vec::new(),
            temporal_facts: vec![ChannelTemporalFact {
                kind: ChannelTemporalKind::Due,
                text: "July 15, 2026".to_string(),
                at_ms: None,
                timezone: None,
            }],
            stated_action: None,
            detail_status: ChannelDetailStatus::Complete,
            missing_details: Vec::new(),
        };
        let revision = channel_store
            .set_distill_result_with_brief_and_evidence_ids(
                "p",
                "w",
                "gmail",
                "business",
                "msg-1",
                summary,
                "fyi",
                false,
                None,
                Some(&brief),
                2,
                message_at + 1,
                &["msg-1".to_string()],
            )
            .await
            .unwrap();
        let source_ref = format!("gmail/business/thread-1/msg-1@{message_at}");
        let mut candidate = sample_candidate(&source_ref, 0.4);
        candidate.source_kind = SourceKind::Comm;
        candidate.candidate_id = candidate_id(SourceKind::Comm, &source_ref);
        candidate.content_revision = Some(revision.to_string());
        candidate.title = "Invoice due".to_string();
        candidate.content_digest = summary.to_string();

        let (accepted, outcome, _) = route_resurfacing_candidate(
            "p",
            "w",
            &candidate,
            message_at / 1000 + 10,
            Some(&channel_store),
            true,
            false,
        )
        .await
        .unwrap();
        assert!(!accepted);
        assert!(matches!(
            outcome,
            RouteOutcome::Routed {
                lane: AttentionLane::FollowUp,
                ..
            }
        ));
        let annotations = channel_store
            .list_annotations_by_thread_ids(
                "p",
                "w",
                "gmail",
                "business",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].state, ChannelAnnotationState::NeedsApproval);
        assert_eq!(annotations[0].evidence_message_id.as_deref(), Some("msg-1"));

        let mut stale_candidate = candidate.clone();
        stale_candidate.content_revision = Some(revision.saturating_sub(1).to_string());
        let (accepted, stale_outcome, _) = route_resurfacing_candidate(
            "p",
            "w",
            &stale_candidate,
            message_at / 1000 + 15,
            Some(&channel_store),
            true,
            false,
        )
        .await
        .unwrap();
        assert!(!accepted);
        assert_eq!(
            stale_outcome,
            RouteOutcome::Dropped {
                reason: DropReason::StaleContentRevision,
            }
        );

        route_resurfacing_candidate(
            "p",
            "w",
            &candidate,
            message_at / 1000 + 20,
            Some(&channel_store),
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            channel_store
                .list_annotations_by_thread_ids(
                    "p",
                    "w",
                    "gmail",
                    "business",
                    &["thread-1".to_string()],
                )
                .await
                .unwrap()
                .len(),
            1
        );

        let resurfacing_store = ResurfacingStore::open(tmp.path()).unwrap();
        candidate.state = CandidateState::Surfaced;
        candidate.last_surfaced_at = Some(message_at / 1000 + 20);
        resurfacing_store
            .upsert_candidate("p", "w", &candidate)
            .await
            .unwrap();
        let repaired = repair_active_surfaced_candidates(
            "p",
            "w",
            &resurfacing_store,
            &channel_store,
            None,
            10,
            message_at / 1000 + 30,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            (repaired.scanned, repaired.repaired, repaired.rerouted),
            (1, 1, 1)
        );
        assert_eq!(
            repair_active_surfaced_candidates(
                "p",
                "w",
                &resurfacing_store,
                &channel_store,
                None,
                10,
                message_at / 1000 + 31,
                false,
            )
            .await
            .unwrap()
            .scanned,
            0
        );
        assert_eq!(
            resurfacing_store
                .get_candidate("p", "w", &candidate.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CandidateState::Candidate
        );
        assert_eq!(
            channel_store
                .list_annotations_by_thread_ids(
                    "p",
                    "w",
                    "gmail",
                    "business",
                    &["thread-1".to_string()],
                )
                .await
                .unwrap()
                .len(),
            1,
            "active repair must reuse the existing Follow-up annotation"
        );
    }

    #[tokio::test]
    async fn active_repair_surfaces_partial_storage_failure_without_receipting_failed_row() {
        let tmp = TempDir::new().unwrap();
        let channel_store = ChannelAssistStore::open(tmp.path()).unwrap();
        // Intentionally do NOT materialize scope "p"/"w" on the channel store —
        // its DuckDB instance must never be opened, so the first read below fails
        // to open it. See the failure-injection note before the repair call.
        let resurfacing_store = ResurfacingStore::open(tmp.path()).unwrap();

        let mut malformed = comm_candidate("malformed");
        malformed.source_ref = "invalid-comm-identity".to_string();
        malformed.candidate_id = candidate_id(SourceKind::Comm, &malformed.source_ref);
        malformed.state = CandidateState::Surfaced;
        malformed.last_surfaced_at = Some(1_001);
        resurfacing_store
            .upsert_candidate("p", "w", &malformed)
            .await
            .unwrap();

        let mut unreadable = comm_candidate("unreadable");
        unreadable.state = CandidateState::Surfaced;
        unreadable.last_surfaced_at = Some(1_002);
        resurfacing_store
            .upsert_candidate("p", "w", &unreadable)
            .await
            .unwrap();

        // Make the channel-assist source store unreadable so the FIRST read of a
        // comm source (`get_message` for the "unreadable" candidate) fails,
        // simulating a transient source-store failure. Active repair must surface
        // that failure and must NOT write a repair receipt/cooldown for the
        // failed row.
        //
        // Replace the scope's DuckDB path with a directory so the lazy
        // `scope_inner` open errors on first access. The store is deliberately
        // left unmaterialized (above): reads now clone off a shared, already-open
        // instance, so deleting the file AFTER the store opened it no longer
        // breaks reads — the store must be unopenable BEFORE its first read.
        let db_path = ArtifactV2Workspace::new(tmp.path()).channel_assist_db_path("p", "w");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&db_path).unwrap();

        let error = repair_active_surfaced_candidates(
            "p",
            "w",
            &resurfacing_store,
            &channel_store,
            None,
            10,
            2_000,
            false,
        )
        .await
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("scanned=2"), "{message}");
        assert!(message.contains("repaired=1"), "{message}");
        assert!(message.contains("failed=1"), "{message}");

        let repaired = resurfacing_store
            .get_candidate("p", "w", &malformed.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(repaired.state, CandidateState::Candidate);
        assert!(repaired.cooldown_until > 2_000);

        let failed = resurfacing_store
            .get_candidate("p", "w", &unreadable.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(failed.state, CandidateState::Surfaced);
        assert_eq!(failed.cooldown_until, 0);
        let pending = resurfacing_store
            .list_active_repair_candidates("p", "w", 10)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].candidate_id, unreadable.candidate_id);
    }

    #[test]
    fn parse_comm_source_ref_extracts_annotation_lookup_keys() {
        assert_eq!(
            parse_comm_source_ref("gmail/business/thread-1/msg-1@1783209600000"),
            Some(CommSourceRef {
                provider: "gmail".to_string(),
                account_alias: "business".to_string(),
                thread_id: "thread-1".to_string(),
                message_id: "msg-1".to_string(),
                internal_date: 1_783_209_600_000,
            })
        );
        assert_eq!(
            parse_comm_source_ref("whatsapp/my%2Faccount/group%2Fthread/msg%2F1@1783209600000"),
            Some(CommSourceRef {
                provider: "whatsapp".to_string(),
                account_alias: "my/account".to_string(),
                thread_id: "group/thread".to_string(),
                message_id: "msg/1".to_string(),
                internal_date: 1_783_209_600_000,
            })
        );
        assert_eq!(
            parse_comm_source_ref("whatsapp/my/account/group/thread/msg-raw@1783209600000"),
            None
        );
        assert_eq!(parse_comm_source_ref("gmail/business"), None);
    }

    #[tokio::test]
    async fn deterministic_curator_surfaces_top_k_capped() {
        let store = ResurfacingStore::open_in_temp();
        // 8 eligible candidates, scores 0.1 .. 0.8.
        for i in 1..=8u32 {
            let c = sample_candidate(&format!("k{i}"), i as f32 / 10.0);
            store.upsert_candidate("p", "w", &c).await.unwrap();
        }

        let surfaced = run_curation_pass_with_attention("p", "w", &store, 5, 2_000, None, None)
            .await
            .unwrap();

        // Capped at 5, and the 5 highest scores (k8..k4) in descending order.
        assert_eq!(surfaced.len(), 5);
        let refs: Vec<&str> = surfaced.iter().map(|c| c.source_ref.as_str()).collect();
        assert_eq!(refs, vec!["k8", "k7", "k6", "k5", "k4"]);

        // Returned copies reflect the Surfaced transition.
        assert!(surfaced.iter().all(|c| c.state == CandidateState::Surfaced));
        assert!(surfaced.iter().all(|c| c.surface_count == 1));

        // The store persisted the transition on the top row.
        let top = store
            .get_candidate("p", "w", &candidate_id(SourceKind::Memory, "k8"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(top.state, CandidateState::Surfaced);
        assert_eq!(top.surface_count, 1);
        assert_eq!(top.last_surfaced_at, Some(2_000));

        // Surfaced rows drop out of the `candidate` eligibility filter, so only
        // the three never-surfaced rows (k1..k3) remain eligible.
        let remaining = store
            .list_top_candidates("p", "w", 2_000, 10)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 3);
    }

    #[tokio::test]
    async fn memory_why_now_uses_judgement_for_this_content_revision() {
        use magician::magician_v2::attention::resurfacing::memory_context::{
            MemoryApplication, MemoryApplicationRecord,
        };
        use magician::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement;

        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("rev-bound", 0.8);
        candidate.content_revision = Some("rev-this".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();

        let this_revision = MemoryJudgement {
            applications: MemoryApplicationRecord {
                would_apply: vec![MemoryApplication {
                    memory_key: "preferences: this".to_string(),
                    memory_revision: Some("1".to_string()),
                    direction: "explain".to_string(),
                    rationale: "you said this revision".to_string(),
                    strength: None,
                }],
            },
            ..Default::default()
        };
        let other_revision = MemoryJudgement {
            applications: MemoryApplicationRecord {
                would_apply: vec![MemoryApplication {
                    memory_key: "preferences: other".to_string(),
                    memory_revision: Some("2".to_string()),
                    direction: "explain".to_string(),
                    rationale: "you said other revision".to_string(),
                    strength: None,
                }],
            },
            ..Default::default()
        };
        store
            .put_memory_applications(
                "p",
                "w",
                &candidate.candidate_id,
                "rev-this",
                "mem-old",
                &this_revision,
                10,
            )
            .await
            .unwrap();
        store
            .put_memory_applications(
                "p",
                "w",
                &candidate.candidate_id,
                "rev-other",
                "mem-new",
                &other_revision,
                99,
            )
            .await
            .unwrap();

        run_curation_pass_with_attention("p", "w", &store, 1, 2_000, None, None)
            .await
            .unwrap();

        let phrasing = store
            .get_phrasing("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .expect("memory why-now");
        assert_eq!(phrasing.1, "you said this revision");
        assert!(
            !phrasing.1.contains("other revision"),
            "why-now must not use a newer judgement for a different content revision"
        );
    }

    #[tokio::test]
    async fn deterministic_curator_skips_recency_only_candidates() {
        let store = ResurfacingStore::open_in_temp();
        let mut recent_only = sample_candidate("recent-only", 0.99);
        recent_only.signals = SalienceSignals {
            recency: 1.0,
            ..SalienceSignals::default()
        };
        let mut connected = sample_candidate("connected", 0.7);
        connected.signals = SalienceSignals {
            centrality: MIN_NON_RECENCY_SIGNAL,
            ..SalienceSignals::default()
        };
        store
            .upsert_candidate("p", "w", &recent_only)
            .await
            .unwrap();
        store.upsert_candidate("p", "w", &connected).await.unwrap();

        let surfaced = run_curation_pass_with_attention("p", "w", &store, 5, 2_000, None, None)
            .await
            .unwrap();

        let refs: Vec<&str> = surfaced.iter().map(|c| c.source_ref.as_str()).collect();
        assert_eq!(refs, vec!["connected"]);
        assert_eq!(
            store
                .get_candidate("p", "w", &recent_only.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CandidateState::Candidate
        );
    }

    #[tokio::test]
    async fn model_shortlist_reviews_recency_only_candidate_before_final_selection() {
        let store = ResurfacingStore::open_in_temp();
        let mut recent_only = sample_candidate("recent-only-model-review", 0.99);
        recent_only.signals = SalienceSignals {
            recency: 1.0,
            ..SalienceSignals::default()
        };
        store
            .upsert_candidate("p", "w", &recent_only)
            .await
            .unwrap();

        let deterministic = list_router_accepted_resurfacing_candidates(
            "p", "w", &store, 1, 2_000, None, None, false,
        )
        .await
        .unwrap();
        let model_review = list_router_accepted_resurfacing_candidates(
            "p", "w", &store, 1, 2_000, None, None, true,
        )
        .await
        .unwrap();

        assert!(deterministic.is_empty());
        assert_eq!(model_review.len(), 1);
        assert_eq!(model_review[0].candidate_id, recent_only.candidate_id);
    }

    #[tokio::test]
    async fn deterministic_curator_scans_past_recency_only_frontier() {
        let store = ResurfacingStore::open_in_temp();
        for i in 0..12 {
            let mut recent_only =
                sample_candidate(&format!("recent-{i}"), 1.0 - (i as f32 / 100.0));
            recent_only.signals = SalienceSignals {
                recency: 1.0,
                ..SalienceSignals::default()
            };
            store
                .upsert_candidate("p", "w", &recent_only)
                .await
                .unwrap();
        }
        let mut connected = sample_candidate("connected-low-score", 0.1);
        connected.signals = SalienceSignals {
            centrality: MIN_NON_RECENCY_SIGNAL,
            ..SalienceSignals::default()
        };
        store.upsert_candidate("p", "w", &connected).await.unwrap();

        let surfaced = run_curation_pass_with_attention("p", "w", &store, 1, 2_000, None, None)
            .await
            .unwrap();

        let refs: Vec<&str> = surfaced.iter().map(|c| c.source_ref.as_str()).collect();
        assert_eq!(refs, vec!["connected-low-score"]);
    }

    #[tokio::test]
    async fn dismiss_sets_cooldown_and_marks_dismissed() {
        let store = ResurfacingStore::open_in_temp();
        let c = sample_candidate("d", 0.9);
        store.upsert_candidate("p", "w", &c).await.unwrap();

        // Surface it, then dismiss (dismiss cooldown 100, ack cooldown 1000).
        run_curation_pass_with_attention("p", "w", &store, 5, 1_000, None, None)
            .await
            .unwrap();
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                5_000,
                100,
                1_000,
            )
            .await
            .unwrap();

        let got = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.state, CandidateState::Dismissed);
        assert_eq!(got.cooldown_until, 5_100); // now + dismiss_cooldown_secs
        assert_eq!(got.dismiss_count, 1);

        // Dismissed → excluded from re-surfacing even once the cooldown lapses.
        let top = store
            .list_top_candidates("p", "w", 5_050, 10)
            .await
            .unwrap();
        assert!(top.is_empty());
    }

    #[tokio::test]
    async fn acknowledge_marks_acted_with_longer_cooldown() {
        let store = ResurfacingStore::open_in_temp();
        let c = sample_candidate("a", 0.9);
        store.upsert_candidate("p", "w", &c).await.unwrap();

        // Acknowledge uses the longer ack cooldown (1000 vs the 100 dismiss).
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Acknowledge,
                5_000,
                100,
                1_000,
            )
            .await
            .unwrap();

        let got = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.state, CandidateState::Acted);
        assert_eq!(got.cooldown_until, 6_000); // now + ack_cooldown_secs
    }

    #[tokio::test]
    async fn curator_llm_falls_back_to_deterministic_when_router_absent() {
        let store = ResurfacingStore::open_in_temp();
        for i in 1..=8u32 {
            let c = sample_candidate(&format!("k{i}"), i as f32 / 10.0);
            store.upsert_candidate("p", "w", &c).await.unwrap();
        }

        // router = None ⇒ identical to the deterministic path: top-5 by score.
        let surfaced =
            run_curation_pass_llm_with_attention("p", "w", &store, 5, 2_000, None, None, None)
                .await
                .unwrap();
        let refs: Vec<&str> = surfaced.iter().map(|c| c.source_ref.as_str()).collect();
        assert_eq!(refs, vec!["k8", "k7", "k6", "k5", "k4"]);
        assert!(surfaced.iter().all(|c| c.state == CandidateState::Surfaced));

        // The deterministic fallback writes NO phrasing rows — the `today` read
        // must fall back to the title + signal-derived phrase for these.
        for c in &surfaced {
            assert!(store
                .get_phrasing("p", "w", &c.candidate_id)
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn unbound_curator_persists_deterministic_recommendation_when_enabled() {
        let store = ResurfacingStore::open_in_temp();
        let candidate = sample_candidate("user.knowledge#fallback", 0.8);
        store.upsert_candidate("p", "w", &candidate).await.unwrap();

        let surfaced = run_curation_pass_llm_with_recommendations(
            "p",
            "w",
            &store,
            1,
            2_000,
            None,
            None,
            None,
            CurationRecommendationPolicy::enabled(false, 0.65),
        )
        .await
        .unwrap();
        assert_eq!(surfaced.len(), 1);
        let recommendation = store
            .get_recommendation("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recommendation.kind, ResurfacingActionKind::ViewDetails);
        assert_eq!(
            recommendation.source,
            ResurfacingRecommendationSource::Deterministic
        );
    }

    #[test]
    fn build_curation_digest_numbers_and_caps_excerpt() {
        let mut a = sample_candidate("a", 0.9);
        a.title = "Tokyo trip".to_string();
        a.content_digest = "x".repeat(CANDIDATE_DIGEST_MAX_CHARS + 40);
        let mut b = sample_candidate("b", 0.8);
        b.title = "Renew passport".to_string();
        b.content_digest = "  ".to_string();

        let candidates = vec![a, b];
        let capabilities = candidates
            .iter()
            .map(|candidate| metadata_capabilities(candidate, None, true))
            .collect::<Vec<_>>();
        let digest = build_curation_digest(&candidates, &capabilities);
        let lines: Vec<&str> = digest.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("0. source=memory; title=Tokyo trip"));
        assert!(lines[0].contains("salience=0.90"));
        assert!(lines[0].contains("signals={r:"));
        assert!(lines[0].contains("allowed_actions=[view_details]"));
        assert!(lines[1].contains("title=Renew passport"));
        assert!(lines
            .iter()
            .all(|line| line.chars().count() <= CANDIDATE_DIGEST_MAX_CHARS));
    }

    #[test]
    fn parse_curation_reply_tolerates_fence_and_prose() {
        let raw = "Sure!\n```json\n[{\"index\":0,\"line\":\"Circle back\",\"why_now\":\"date near\"}]\n```";
        let sels = parse_curation_reply(raw).expect("parses");
        assert_eq!(sels.len(), 1);
        assert_eq!(sels[0].index, 0);
        assert_eq!(sels[0].line, "Circle back");
        assert_eq!(sels[0].why_now, "date near");

        // A valid-but-empty array parses to an empty selection list, NOT None.
        assert_eq!(parse_curation_reply("[]").expect("parses empty").len(), 0);
        // Genuine garbage → None (caller then falls back to deterministic).
        assert!(parse_curation_reply("not json at all").is_none());
        // REVERSED brackets (last `]` before first `[`) must NOT panic — this
        // once sliced `&s[start..=end]` with start > end and unwound the worker.
        assert!(parse_curation_reply("]  [").is_none());
        assert!(parse_curation_reply("nope] then [nope").is_none());
        assert!(parse_curation_reply("]").is_none());
    }

    #[test]
    fn resolve_selections_ignores_out_of_range_dupes_blanks_and_caps() {
        let sels = vec![
            CurationSelection {
                index: 0,
                line: "l0".into(),
                why_now: "w0".into(),
                recommended_action: None,
            },
            CurationSelection {
                index: 9,
                line: "oob".into(),
                why_now: "oob".into(),
                recommended_action: None,
            }, // out of range
            CurationSelection {
                index: -1,
                line: "neg".into(),
                why_now: "neg".into(),
                recommended_action: None,
            }, // negative
            CurationSelection {
                index: 0,
                line: "dup".into(),
                why_now: "dup".into(),
                recommended_action: None,
            }, // duplicate index
            CurationSelection {
                index: 1,
                line: "  ".into(),
                why_now: "w1".into(),
                recommended_action: None,
            }, // blank line
            CurationSelection {
                index: 2,
                line: "l2".into(),
                why_now: "  ".into(),
                recommended_action: None,
            }, // blank why
            CurationSelection {
                index: 3,
                line: "l3".into(),
                why_now: "w3".into(),
                recommended_action: None,
            },
            CurationSelection {
                index: 4,
                line: "l4".into(),
                why_now: "w4".into(),
                recommended_action: None,
            },
        ];
        // count = 5 candidates, cap = 2.
        let candidates = (0..5)
            .map(|index| sample_candidate(&format!("k{index}"), 0.5))
            .collect::<Vec<_>>();
        let capabilities = candidates
            .iter()
            .map(|candidate| metadata_capabilities(candidate, None, false))
            .collect::<Vec<_>>();
        let resolved = resolve_selections(
            &candidates,
            2,
            &sels,
            &capabilities,
            CurationRecommendationPolicy::disabled(),
            0,
        );
        assert_eq!(
            resolved,
            vec![
                ResolvedSelection {
                    index: 0,
                    line: "l0".into(),
                    why: "w0".into(),
                    recommendation: None,
                },
                ResolvedSelection {
                    index: 3,
                    line: "l3".into(),
                    why: "w3".into(),
                    recommendation: None,
                },
            ]
        );
    }

    #[test]
    fn malformed_or_payload_bearing_recommendation_does_not_drop_selection() {
        let candidate = comm_candidate("msg-malformed");
        let malformed = resolve_one_recommendation(
            candidate.clone(),
            Some(serde_json::json!("create_task")),
            true,
            0.65,
            1_000,
        );
        assert_eq!(malformed.index, 0);
        assert_eq!(
            malformed.recommendation.unwrap().kind,
            ResurfacingActionKind::ViewDetails
        );

        let with_payload = resolve_one_recommendation(
            candidate,
            Some(serde_json::json!({
                "kind": "create_task",
                "label": "Do it",
                "rationale": "Useful",
                "confidence": 0.99,
                "input": {"instruction": "model-authored payload"}
            })),
            true,
            0.65,
            1_000,
        );
        let recommendation = with_payload.recommendation.unwrap();
        assert_eq!(recommendation.kind, ResurfacingActionKind::ViewDetails);
        assert_eq!(
            recommendation.source,
            ResurfacingRecommendationSource::Deterministic
        );
    }

    #[test]
    fn model_recommendation_requires_allowed_id_and_confidence() {
        let candidate = comm_candidate("msg-model");
        let invalid_kind = resolve_one_recommendation(
            candidate.clone(),
            Some(serde_json::json!({
                "kind": "not_an_action",
                "label": "Do it",
                "rationale": "Useful",
                "confidence": 0.99
            })),
            true,
            0.65,
            1_000,
        );
        assert_eq!(
            invalid_kind.recommendation.unwrap().source,
            ResurfacingRecommendationSource::Deterministic
        );

        let accepted = resolve_one_recommendation(
            candidate.clone(),
            Some(serde_json::json!({
                "kind": "create_task",
                "label": "Create a follow-up task",
                "rationale": "This may need work later.",
                "confidence": 0.82
            })),
            true,
            0.65,
            1_000,
        )
        .recommendation
        .unwrap();
        assert_eq!(accepted.kind, ResurfacingActionKind::CreateTask);
        assert_eq!(accepted.source, ResurfacingRecommendationSource::Curator);
        assert_eq!(accepted.content_revision.as_deref(), Some("7"));

        let low_confidence = resolve_one_recommendation(
            candidate,
            Some(serde_json::json!({
                "kind": "create_task",
                "label": "Create a follow-up task",
                "rationale": "This may need work later.",
                "confidence": 0.2
            })),
            true,
            0.65,
            1_000,
        )
        .recommendation
        .unwrap();
        assert_eq!(low_confidence.kind, ResurfacingActionKind::ViewDetails);
        assert_eq!(
            low_confidence.source,
            ResurfacingRecommendationSource::Deterministic
        );
    }

    #[test]
    fn deterministic_recommendation_uses_semantic_fallback_order() {
        let mut omitted = comm_candidate("msg-omitted");
        omitted.content_details = Some(details(
            ResurfacingDetailStatus::SourceOmitsDetails,
            Vec::new(),
            Vec::new(),
        ));
        let omitted = resolve_one_recommendation(omitted, None, true, 0.65, 1_000)
            .recommendation
            .unwrap();
        assert_eq!(omitted.kind, ResurfacingActionKind::OpenSource);

        let mut future = comm_candidate("msg-future");
        future.content_details = Some(details(
            ResurfacingDetailStatus::Complete,
            Vec::new(),
            vec![ResurfacingTemporalFact {
                kind: "effective".to_string(),
                text: "July 20".to_string(),
                at_ms: Some(2_000_000),
                timezone: Some("UTC".to_string()),
            }],
        ));
        let future = resolve_one_recommendation(future, None, true, 0.65, 1_000)
            .recommendation
            .unwrap();
        assert_eq!(future.kind, ResurfacingActionKind::CreateReminder);

        let mut changed = comm_candidate("msg-change");
        changed.content_details = Some(details(
            ResurfacingDetailStatus::Complete,
            vec![ResurfacingChangeFact {
                aspect: "reward cap".to_string(),
                before: Some("old cap".to_string()),
                after: Some("new cap".to_string()),
                effective_text: Some("July 1".to_string()),
            }],
            Vec::new(),
        ));
        let changed = resolve_one_recommendation(changed, None, true, 0.65, 1_000)
            .recommendation
            .unwrap();
        assert_eq!(changed.kind, ResurfacingActionKind::SaveToMemory);
    }

    #[test]
    fn unsupported_semantic_action_falls_back_to_advertised_details() {
        let mut future = comm_candidate("msg-read-only");
        future.content_details = Some(details(
            ResurfacingDetailStatus::Complete,
            Vec::new(),
            vec![ResurfacingTemporalFact {
                kind: "effective".to_string(),
                text: "July 20".to_string(),
                at_ms: Some(2_000_000),
                timezone: None,
            }],
        ));
        let recommendation = resolve_one_recommendation(future, None, false, 0.65, 1_000)
            .recommendation
            .unwrap();
        assert_eq!(recommendation.kind, ResurfacingActionKind::ViewDetails);
        assert_eq!(recommendation.label, "Review details");
    }

    #[test]
    fn rich_curation_digest_has_per_item_and_total_caps() {
        let candidates = (0..100)
            .map(|index| {
                let mut candidate = comm_candidate(&format!("msg-{index}"));
                candidate.title = "t".repeat(500);
                candidate.content_digest = "d".repeat(2_000);
                candidate.content_details = Some(details(
                    ResurfacingDetailStatus::Partial,
                    Vec::new(),
                    Vec::new(),
                ));
                candidate
            })
            .collect::<Vec<_>>();
        let capabilities = candidates
            .iter()
            .map(|candidate| metadata_capabilities(candidate, None, true))
            .collect::<Vec<_>>();
        let digest = build_curation_digest(&candidates, &capabilities);
        assert_eq!(digest.lines().count(), CURATION_REVIEW_MAX_CANDIDATES);
        assert!(digest.chars().count() <= CURATION_DIGEST_MAX_CHARS);
        assert!(digest
            .lines()
            .all(|line| line.chars().count() <= CANDIDATE_DIGEST_MAX_CHARS));
        assert!(digest
            .lines()
            .all(|line| line.contains("allowed_actions=[")));
    }

    #[tokio::test]
    async fn recommendation_store_ignores_stale_revision() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = comm_candidate("msg-revision");
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        let recommendation = deterministic_recommendation(
            &candidate,
            &metadata_capabilities(&candidate, None, true),
            0.65,
            1_000,
        )
        .unwrap();
        store
            .upsert_recommendation("p", "w", &candidate.candidate_id, &recommendation, 1_000)
            .await
            .unwrap();
        assert!(store
            .get_recommendation("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .is_some());

        candidate.content_revision = Some("8".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        assert!(store
            .get_recommendation("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .is_none());
        store
            .upsert_recommendation("p", "w", &candidate.candidate_id, &recommendation, 1_001)
            .await
            .unwrap();
        assert!(store
            .get_recommendation("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn review_window_and_rejected_ids_rotate_unselected_candidates() {
        assert_eq!(review_limit_for_cap(5), 25);
        assert_eq!(review_limit_for_cap(0), 0);

        let candidates = vec![
            sample_candidate("a", 0.9),
            sample_candidate("b", 0.8),
            sample_candidate("c", 0.7),
        ];
        let selected = vec![ResolvedSelection {
            index: 1,
            line: "line".to_string(),
            why: "why".to_string(),
            recommendation: None,
        }];
        let rejected = rejected_candidate_ids(&candidates, &selected);
        assert_eq!(
            rejected,
            vec![
                candidates[0].candidate_id.clone(),
                candidates[2].candidate_id.clone()
            ]
        );
    }
}
