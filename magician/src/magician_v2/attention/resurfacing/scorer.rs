//! Per-scope scoring pass for the Proactive Resurfacing Engine.
//!
//! [`run_scorer_pass`] is the pure, timer-free unit of work the (later) worker
//! (Task 9) drives on an interval: for each corpus source it reads the source's
//! watermark, pulls the items changed since, scores each into a [`Candidate`],
//! and upserts it — advancing the watermark to the newest source cursor **only
//! after every upsert for that source has succeeded**. A time-decay sweep runs
//! once at the end so idle scopes bleed salience between passes.
//!
//! Two invariants make the pass safe to re-run:
//!
//! * **Idempotent** — the candidate id is a stable hash of
//!   `(source_kind, source_ref)` and the upsert is idempotent, so re-scoring the
//!   same item overwrites the same row. Because the watermark advances only on
//!   full success, a mid-pass failure leaves the watermark where it was and the
//!   next run reprocesses the unfinished items rather than skipping them.
//! * **Feedback-preserving** — when a row already exists we carry its lifecycle
//!   forward (`state`, `cooldown_until`, `first_seen_at`, `surface_count`,
//!   `dismiss_count`, `last_surfaced_at`) and only refresh the score-derived
//!   fields. A `Dismissed`/`Acted`/`Snoozed` (or still-cooling) row is therefore
//!   never resurrected back to `Candidate` by a re-scan — the owner's feedback
//!   sticks.
//!
//! Scope discipline: this file is the scoring *pass* only — the interval loop
//! and the curator live in sibling modules. Embedding centrality is supplied by
//! an injected [`CentralityProvider`](super::centrality::CentralityProvider)
//! (resolved async, up front, per item); [`BasicScoreCtx`] remains the clock/
//! access-stat carrier and [`NoCentrality`](super::centrality::NoCentrality) is
//! the deterministic no-op default.

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use tracing::debug;

use super::centrality::{normalize, CentralityProvider};
use super::scoring::{
    affinity_boost, dismissal_penalty, matches_dismissed_series, score_item_with_temporal_anchor,
    utility_multiplier_for_item, ResurfacingScoringConfig, ScoreCtx, DISMISSED_SERIES_THRESHOLD,
};
use super::sources::ResurfacingSource;

const LOG_TARGET: &str = "magician::resurfacing::scorer";
use super::store::ResurfacingStore;
use super::types::{candidate_id, Candidate, CandidateState, SourceKind};

/// Run one scoring pass over `sources` for a single `principal`/`workspace`
/// scope, returning the number of candidates scored/upserted.
///
/// For each source: read its watermark, list items changed since, score and
/// upsert each (preserving any existing row's lifecycle — see the module docs),
/// then advance the watermark to the max `watermark_cursor` seen **only if** every
/// upsert for that source succeeded. If an upsert errors, the error propagates
/// and the watermark is left unadvanced so the next run reprocesses the item
/// (safe because the candidate id is stable and the upsert is idempotent).
///
/// After all sources, a single time-decay sweep runs over the scope (its
/// half-life comes from `cfg.decay_halflife_days`).
///
/// `cfg` supplies the tunable scoring parameters — signal weights, the recency/
/// temporal half-lives, and the dismissal/affinity feedback thresholds+factors.
/// With `&ResurfacingScoringConfig::default()` the pass is byte-identical to the
/// pre-config behavior; the worker builds it once via
/// [`ResurfacingScoringConfig::from_env`].
///
/// `ctx` supplies the synchronous signals (clock + access stats). Embedding
/// centrality can't be resolved inside the sync `score_item`, so `centrality`
/// resolves each item's [`CentralityOutcome`](super::centrality::CentralityOutcome)
/// **async, up front**; its density is threaded into a per-item
/// [`PrecomputedCtx`] whose `neighbor_density` returns it synchronously, and its
/// embedding is persisted (via [`ResurfacingStore::upsert_embedding`]) so a
/// later dismiss can suppress the item's semantic neighbors. Pass
/// [`NoCentrality`](super::centrality::NoCentrality) for the deterministic path
/// (`None` outcome → centrality degrades to `0.0` via `score_item`'s graceful
/// branch and no embedding is stored, exactly as before this signal existed).
///
/// P3a/P4a cross-session feedback learning: the pass loads the scope's DURABLE
/// dismissed-signal vectors (via [`ResurfacingStore::list_dismissed_signals`])
/// AND its affinity-signal vectors (via
/// [`ResurfacingStore::list_affinity_signals`]) once, and for each item that has
/// an embedding applies BOTH a POST-score dismissal penalty
/// ([`dismissal_penalty`](super::scoring::dismissal_penalty)) and an affinity
/// boost ([`affinity_boost`](super::scoring::affinity_boost)) to the final
/// `salience_score`. They're feedback overrides on the stored salience, NOT
/// `score_item` signals, so the auditable signal bundle stays a pure function of
/// the item's own content. An item near a past dismissal is penalized, near a
/// past positive is boosted, and near BOTH they partially cancel (net =
/// penalty × boost). When nothing has been dismissed/engaged-with in the scope
/// both multipliers are `1.0` and the pass is identical to the pre-P3a behavior.
///
/// P4c per-lane utility learning: the pass ALSO loads the scope's per-lane
/// engagement tallies once (via [`ResurfacingStore::kind_engagement`]) and
/// multiplies each item's salience by
/// [`utility_multiplier_for_item`](super::scoring::utility_multiplier_for_item)
/// — down-weighting lanes the owner keeps dismissing, up-weighting lanes they
/// engage with. The two halves are asymmetric: the PENALTY is lane-based and
/// embedding-independent, so it applies to every item; the BOOST is scaled by
/// the item's own centrality, so a popular lane cannot lift an item unconnected
/// to current context (and an item with no embedding gets no boost at all).
/// That asymmetry is deliberate — see the function's docs. The full multiplier
/// chain is
/// `score × penalty × boost × utility`; a lane with no recorded engagement is
/// neutral (`×1.0`), keeping [`ResurfacingScoringConfig::default`] byte-identical
/// to the pre-P4c behavior.
pub async fn run_scorer_pass(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    sources: &[&dyn ResurfacingSource],
    ctx: &dyn ScoreCtx,
    centrality: &dyn CentralityProvider,
    cfg: &ResurfacingScoringConfig,
) -> Result<usize> {
    run_scorer_pass_inner(
        principal,
        workspace,
        store,
        sources,
        ctx,
        centrality,
        cfg,
        None,
        &[],
        None,
        None,
    )
    .await
}

/// Same as [`run_scorer_pass`] but records shadow memory applications without
/// changing the stored salience.
pub async fn run_scorer_pass_with_memories(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    sources: &[&dyn ResurfacingSource],
    ctx: &dyn ScoreCtx,
    centrality: &dyn CentralityProvider,
    cfg: &ResurfacingScoringConfig,
    memories: &[super::memory_context::ScopedMemory],
) -> Result<usize> {
    run_scorer_pass_inner(
        principal, workspace, store, sources, ctx, centrality, cfg, None, memories, None, None,
    )
    .await
}

/// Worker-facing cancellable variant. Cancellation returns the number already
/// persisted, but deliberately leaves the current source watermark unadvanced
/// so the next pass safely retries its idempotent upserts.
#[allow(clippy::too_many_arguments)]
pub async fn run_scorer_pass_with_cancellation(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    sources: &[&dyn ResurfacingSource],
    ctx: &dyn ScoreCtx,
    centrality: &dyn CentralityProvider,
    cfg: &ResurfacingScoringConfig,
    cancel: &CancellationToken,
) -> Result<usize> {
    run_scorer_pass_inner(
        principal,
        workspace,
        store,
        sources,
        ctx,
        centrality,
        cfg,
        Some(cancel),
        &[],
        None,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_scorer_pass_with_cancellation_and_memories(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    sources: &[&dyn ResurfacingSource],
    ctx: &dyn ScoreCtx,
    centrality: &dyn CentralityProvider,
    cfg: &ResurfacingScoringConfig,
    cancel: &CancellationToken,
    memories: &[super::memory_context::ScopedMemory],
    router: Option<&crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
    memory_service: Option<&crate::magician_v2::agents::AgentMemoryService>,
) -> Result<usize> {
    run_scorer_pass_inner(
        principal,
        workspace,
        store,
        sources,
        ctx,
        centrality,
        cfg,
        Some(cancel),
        memories,
        router,
        memory_service,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_scorer_pass_inner(
    principal: &str,
    workspace: &str,
    store: &ResurfacingStore,
    sources: &[&dyn ResurfacingSource],
    ctx: &dyn ScoreCtx,
    centrality: &dyn CentralityProvider,
    cfg: &ResurfacingScoringConfig,
    cancel: Option<&CancellationToken>,
    memories: &[super::memory_context::ScopedMemory],
    router: Option<&crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter>,
    memory_service: Option<&crate::magician_v2::agents::AgentMemoryService>,
) -> Result<usize> {
    let mut upserted = 0usize;
    let mut stage2_budget = super::memory_stage2::Stage2Budget::per_pass();
    // Candidates never created because they repeat an already-dismissed series.
    // Counted and logged rather than dropped quietly: silent suppression is
    // indistinguishable from a broken source.
    let mut suppressed = 0usize;
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Ok(upserted);
    }

    // P3a cross-session dismissal learning: load this scope's DURABLE dismissed-
    // signal vectors ONCE and normalize each up front (the pure penalty helper
    // compares an item against these pre-normalized vectors). Empty when nothing
    // has ever been dismissed in the scope -> the penalty is a no-op and the pass
    // is byte-identical to the pre-P3a behavior every existing test relies on.
    let dismissed = if let Some(contract) = centrality.embedding_contract() {
        store
            .list_dismissed_signals_for_contract(principal, workspace, contract)
            .await?
    } else {
        store.list_dismissed_signals(principal, workspace).await?
    };
    let feedback_dims = centrality.embedding_dimensions();
    let dismissed_norm: Vec<Vec<f32>> = dismissed
        .into_iter()
        .filter(|vector| feedback_dims.is_none_or(|dims| vector.len() == dims))
        .map(normalize)
        .collect();

    // P4a cross-session affinity learning: the positive mirror — load this
    // scope's DURABLE affinity-signal vectors ONCE and normalize each up front.
    // Empty when nothing has ever been opened/acknowledged in the scope -> the
    // boost is a no-op and the pass is byte-identical to the pre-P4a behavior.
    let affinity = if let Some(contract) = centrality.embedding_contract() {
        store
            .list_affinity_signals_for_contract(principal, workspace, contract)
            .await?
    } else {
        store.list_affinity_signals(principal, workspace).await?
    };
    let affinity_norm: Vec<Vec<f32>> = affinity
        .into_iter()
        .filter(|vector| feedback_dims.is_none_or(|dims| vector.len() == dims))
        .map(normalize)
        .collect();

    // P4c per-lane utility signal: load this scope's engagement tallies ONCE as a
    // small lane -> (positive, negative) lookup (at most one row per SourceKind,
    // so a linear scan is trivial and avoids a Hash bound on the enum). A lane
    // with no recorded engagement is absent here and treated as neutral (0, 0) ->
    // a ×1.0 multiplier, so the pass is byte-identical to the pre-P4c behavior
    // until real feedback accrues. This signal is lane-based and embedding-
    // INDEPENDENT (it needs no vector), so it is applied to EVERY item below
    // regardless of whether centrality produced an embedding.
    let engagement: Vec<(SourceKind, (u64, u64))> = store
        .kind_engagement(principal, workspace)
        .await?
        .into_iter()
        .map(|(kind, positive, negative)| (kind, (positive, negative)))
        .collect();

    for source in sources {
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Ok(upserted);
        }
        let corpus_kind = source.corpus_kind();
        let watermark = store
            .get_watermark(principal, workspace, corpus_kind)
            .await?;
        let items = source
            .list_changed_since(principal, workspace, watermark)
            .await?;

        // No new items -> leave the watermark untouched (nothing to advance to).
        if items.is_empty() {
            continue;
        }

        // Track the newest source cursor so we can advance the watermark past
        // it, but only after all upserts for this source have succeeded.
        let mut max_watermark_cursor = watermark;
        for item in &items {
            if cancel.is_some_and(CancellationToken::is_cancelled) {
                // Do not advance this source watermark: every upsert is
                // idempotent, so a later pass can safely replay the prefix.
                return Ok(upserted);
            }
            // Resolve embedding centrality async, up front, then hand it to the
            // sync scorer through a precomputed ctx (see the module docs). A
            // `None` outcome degrades to a `0.0` centrality contribution; a
            // `Some` outcome ALSO carries the item's embedding, persisted below
            // so a later dismiss can suppress this item's semantic neighbors.
            let outcome = centrality.evaluate(&item.embedding_text).await;

            // Series-level dismissal. A per-item penalty cannot stop a recurring
            // digest: tomorrow's copy is a new candidate scoring on its own
            // freshness, so it surfaces before yesterday's dismissal has been
            // re-scored onto it, and the owner dismisses the same mail daily.
            // Suppressing here makes one dismissal end the series. The watermark
            // still advances — the item was handled, just not kept.
            if let Some(o) = &outcome {
                if matches_dismissed_series(
                    &o.embedding,
                    &dismissed_norm,
                    DISMISSED_SERIES_THRESHOLD,
                ) {
                    suppressed += 1;
                    debug!(
                        target: LOG_TARGET,
                        source_ref = item.source_ref.as_str(),
                        "skipped a candidate matching an already-dismissed series"
                    );
                    if item.watermark_cursor > max_watermark_cursor {
                        max_watermark_cursor = item.watermark_cursor;
                    }
                    continue;
                }
            }

            let density = outcome.as_ref().map(|o| o.density);
            let item_ctx = PrecomputedCtx { base: ctx, density };
            let (score, signals, temporal_anchor_at) =
                score_item_with_temporal_anchor(item, &item_ctx, cfg);

            // P3a/P4a cross-session feedback OVERRIDES — post-score multipliers,
            // NOT `score_item` signals (so the auditable signal bundle stays a
            // pure function of the item's own content). When this item has an
            // embedding (`Some` outcome) we apply BOTH:
            //   * dismissal PENALTY (`DISMISS_PENALTY_FACTOR`, < 1) when it
            //     resembles a past dismissal — "you keep dismissing this kind".
            //   * affinity BOOST (`AFFINITY_BOOST_FACTOR`, > 1) when it resembles
            //     a past open/acknowledge — "you keep engaging with this kind".
            // The two are independent multiplicative gates: an item near a past
            // dismissal is penalized, near a past positive is boosted, and near
            // BOTH they partially cancel (net = penalty × boost) — a sensible,
            // symmetric combination that never lets one feedback silently erase
            // the other. `None` outcome (no embedding) or empty signal sets ->
            // both multipliers 1.0, identical to before these signals existed.
            let feedback_score = match &outcome {
                Some(o) => {
                    let penalty = dismissal_penalty(
                        &o.embedding,
                        &dismissed_norm,
                        cfg.dismiss_penalty_threshold,
                        cfg.dismiss_penalty_factor,
                    );
                    let affinity = if super::memory_effects::serendipity_floor_holds(score) {
                        affinity_boost(
                            &o.embedding,
                            &affinity_norm,
                            cfg.affinity_threshold,
                            cfg.affinity_boost_factor,
                        )
                    } else {
                        1.0
                    };
                    score * penalty * affinity
                },
                None => score,
            };

            // P4c per-lane utility multiplier. Unlike the dismissal/affinity
            // gates above (which need the item's embedding), utility is lane-based
            // and embedding-INDEPENDENT, so it applies to EVERY item — including
            // those with no embedding (`None` outcome). A lane with no recorded
            // engagement is neutral (0, 0) -> ×1.0. The full multiplier chain is
            // therefore `score × penalty × boost × utility`.
            let (pos, neg) = engagement
                .iter()
                .find(|(kind, _)| *kind == item.source_kind)
                .map(|(_, counts)| *counts)
                .unwrap_or((0, 0));
            // The lane boost is scaled by this item's own contextual relevance
            // (embedding centrality), so a popular lane cannot lift items that
            // have nothing to do with the current context. A dismissed-heavy
            // lane still damps unconditionally.
            let salience_score =
                feedback_score * utility_multiplier_for_item(pos, neg, signals.centrality, cfg);

            // Re-guard finiteness AFTER the post-score multiplier chain. `score_item`
            // guards its own return, but the penalty/boost/utility product can still
            // overflow to +inf under an adversarial/large env factor — and this value
            // is persisted and later `ORDER BY salience_score`ed, so a non-finite must
            // never reach the store. Map any non-finite product to 0.0.
            let mut salience_score = if salience_score.is_finite() {
                salience_score
            } else {
                0.0
            };

            let id = candidate_id(item.source_kind, &item.source_ref);

            // Preserve an existing row's lifecycle (state/cooldown/counts/
            // first_seen/last_surfaced) so owner feedback survives a re-scan and
            // a terminal or cooling row is never resurrected to `Candidate`.
            let existing = store.get_candidate(principal, workspace, &id).await?;
            let (
                first_seen_at,
                state,
                last_surfaced_at,
                cooldown_until,
                surface_count,
                dismiss_count,
            ) = match existing {
                Some(prev) => (
                    prev.first_seen_at,
                    prev.state,
                    prev.last_surfaced_at,
                    prev.cooldown_until,
                    prev.surface_count,
                    prev.dismiss_count,
                ),
                None => (ctx.now(), CandidateState::Candidate, None, 0, 0, 0),
            };

            // Judge before persist so canary salience can land on the stored
            // score. Shadow `apply_capped_salience` is a no-op, so ranking is
            // unchanged until the mode flips.
            let content_revision = item
                .content_revision
                .clone()
                .unwrap_or_else(|| item.digest.clone());
            let mut persist_judgement = None;
            if !memories.is_empty() {
                let memory_revision = super::memory_context::memory_set_revision(memories);
                let (mut judgement, cache_hit) = match store
                    .get_memory_applications(
                        principal,
                        workspace,
                        &id,
                        &content_revision,
                        &memory_revision,
                    )
                    .await?
                {
                    Some(cached) => (cached, true),
                    None => {
                        let engaged = matches!(&outcome, Some(o) if {
                            affinity_boost(
                                &o.embedding,
                                &affinity_norm,
                                cfg.affinity_threshold,
                                cfg.affinity_boost_factor,
                            ) > 1.0
                        });
                        let mut judgement =
                            super::memory_effects::evaluate_memory_effects_with_dismisses(
                                &super::memory_context::candidate_attributes_from_text(
                                    &format!("{} {}", item.title, item.digest),
                                    item.source_kind.as_str(),
                                ),
                                memories,
                                ctx.now(),
                                engaged,
                                dismiss_count,
                            );
                        if engaged {
                            for conflict in &mut judgement.conflicts {
                                if conflict.rationale.contains("recent engagement disagrees") {
                                    conflict.agree_count = dismiss_count;
                                    conflict.disagree_count = surface_count.max(1);
                                    conflict.rationale = format!(
                                        "still applying {} — {} opens / {} dismisses disagree",
                                        conflict.memory_key, surface_count, dismiss_count
                                    );
                                }
                            }
                        }
                        (judgement, false)
                    },
                };
                if !cache_hit {
                    super::memory_stage2::refine_judgement_with_llm(
                        router,
                        memory_service,
                        store,
                        principal,
                        workspace,
                        &id,
                        item.content_revision.as_deref(),
                        &item.title,
                        &item.digest,
                        item.source_kind.as_str(),
                        memories,
                        &mut judgement,
                        &mut stage2_budget,
                    )
                    .await;
                }
                salience_score = super::memory_effects::apply_capped_salience(
                    salience_score,
                    judgement.salience_delta,
                );
                if !cache_hit {
                    persist_judgement = Some((memory_revision, judgement));
                }
            }

            let candidate = Candidate {
                candidate_id: id,
                source_kind: item.source_kind,
                source_ref: item.source_ref.clone(),
                title: item.title.clone(),
                content_digest: item.digest.clone(),
                content_details: item.content_details.clone(),
                content_revision: item.content_revision.clone(),
                semantic_features: None,
                salience_score,
                signals,
                temporal_anchor_at,
                embedding_id: None,
                state,
                first_seen_at,
                last_scored_at: ctx.now(),
                last_surfaced_at,
                cooldown_until,
                surface_count,
                dismiss_count,
            };

            let content_applied = store
                .upsert_candidate(principal, workspace, &candidate)
                .await?;
            upserted += usize::from(content_applied);

            if content_applied {
                if let Some((memory_revision, judgement)) = persist_judgement {
                    let _ = store
                        .put_memory_applications(
                            principal,
                            workspace,
                            &candidate.candidate_id,
                            &content_revision,
                            &memory_revision,
                            &judgement,
                            ctx.now(),
                        )
                        .await;
                }
                // Persist the per-item embedding the provider already computed so a
                // later dismiss can down-weight this item's semantic neighbors. A
                // `None` outcome simply persists nothing (suppression won't have this
                // vector — graceful). Kept under the same advance-only-on-success
                // guard: a failure here leaves the watermark unadvanced for a retry.
                if let Some(outcome) = &outcome {
                    if let Some(contract) = outcome.embedding_contract.as_deref() {
                        store
                            .upsert_embedding_for_digest_at(
                                principal,
                                workspace,
                                &candidate.candidate_id,
                                &candidate.content_digest,
                                contract,
                                &outcome.embedding,
                                candidate.last_scored_at,
                            )
                            .await?;
                    } else {
                        store
                            .upsert_embedding(
                                principal,
                                workspace,
                                &candidate.candidate_id,
                                &outcome.embedding,
                            )
                            .await?;
                    }
                }
            }

            if item.watermark_cursor > max_watermark_cursor {
                max_watermark_cursor = item.watermark_cursor;
            }
        }

        // Reached only if every upsert above succeeded (errors return early via
        // `?`), so advancing the watermark can't skip an unpersisted item.
        store
            .set_watermark(principal, workspace, corpus_kind, max_watermark_cursor)
            .await?;
    }

    // One decay sweep per pass. Freshly upserted rows (last_scored_at == now)
    // decay by 0.5^0 == 1.0 -> no change; only rows untouched this pass bleed.
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Ok(upserted);
    }
    store
        .decay_all(principal, workspace, cfg.decay_halflife_days, ctx.now())
        .await?;

    if suppressed > 0 {
        debug!(
            target: LOG_TARGET,
            suppressed,
            "skipped candidates repeating an already-dismissed series"
        );
    }

    Ok(upserted)
}

/// Phase-1 scoring context: real clock, no vector-index/access enrichment yet.
///
/// `neighbor_density` returns `Err` (→ centrality degrades to `0.0` via
/// `score_item`'s graceful path) and `access_count` returns `0`. Later work can
/// enrich this with the vector index + access stats without touching the pass
/// logic.
pub struct BasicScoreCtx {
    pub now: i64,
}

impl ScoreCtx for BasicScoreCtx {
    fn now(&self) -> i64 {
        self.now
    }

    fn neighbor_density(&self, _embedding_text: &str) -> Result<f32> {
        anyhow::bail!("vector index not wired")
    }

    fn access_count(&self, _source_ref: &str) -> u32 {
        0
    }
}

/// Per-item scoring context that bridges the async
/// [`CentralityProvider`](super::centrality::CentralityProvider) into the sync
/// scorer. It delegates `now`/`access_count` to the pass's `base` ctx but
/// returns a PRECOMPUTED neighbor density (already resolved async by the
/// provider before scoring). A `None` density maps to `Err`, so `score_item`'s
/// graceful path zeroes centrality — identical to the pre-centrality behavior.
struct PrecomputedCtx<'a> {
    base: &'a dyn ScoreCtx,
    density: Option<f32>,
}

impl ScoreCtx for PrecomputedCtx<'_> {
    fn now(&self) -> i64 {
        self.base.now()
    }

    fn neighbor_density(&self, _embedding_text: &str) -> Result<f32> {
        match self.density {
            Some(d) => Ok(d),
            None => anyhow::bail!("no centrality signal for this item"),
        }
    }

    fn access_count(&self, source_ref: &str) -> u32 {
        self.base.access_count(source_ref)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    // Only the tests compare against the unscaled lane multiplier now that the
    // scorer applies the relevance-scaled form.
    use super::super::scoring::utility_multiplier;
    use crate::magician_v2::attention::resurfacing::centrality::{CentralityOutcome, NoCentrality};
    use crate::magician_v2::attention::resurfacing::types::{
        CorpusItem, FeedbackAction, SourceKind,
    };
    use async_trait::async_trait;

    /// A fake [`CentralityProvider`] returning a fixed density (and an empty
    /// embedding) for every item — lets the pass tests exercise the centrality
    /// signal path without real embeddings.
    struct StubCentrality(f32);

    #[async_trait]
    impl CentralityProvider for StubCentrality {
        async fn evaluate(&self, _embedding_text: &str) -> Option<CentralityOutcome> {
            Some(CentralityOutcome {
                density: self.0,
                embedding: Vec::new(),
                embedding_contract: None,
            })
        }
    }

    /// A fake provider returning a fixed outcome carrying a SPECIFIC embedding,
    /// so the persist test can assert that exact vector reaches the store.
    struct StubOutcome {
        density: f32,
        embedding: Vec<f32>,
        embedding_contract: Option<String>,
    }

    #[async_trait]
    impl CentralityProvider for StubOutcome {
        fn embedding_contract(&self) -> Option<&str> {
            self.embedding_contract.as_deref()
        }

        async fn evaluate(&self, _embedding_text: &str) -> Option<CentralityOutcome> {
            Some(CentralityOutcome {
                density: self.density,
                embedding: self.embedding.clone(),
                embedding_contract: self.embedding_contract.clone(),
            })
        }
    }

    /// A provider whose per-item embedding is keyed off the item text: items
    /// whose `embedding_text` contains "near" embed PARALLEL to `[1,0]`; everyone
    /// else embeds ORTHOGONAL as `[0,1]`. Density is a flat `0.0` so only the
    /// dismissal penalty (not centrality) moves salience between the two items.
    /// Lets the P3a penalty test steer one item toward and one away from a seeded
    /// dismissed signal without real embeddings.
    struct PerTextCentrality;

    #[async_trait]
    impl CentralityProvider for PerTextCentrality {
        async fn evaluate(&self, embedding_text: &str) -> Option<CentralityOutcome> {
            let embedding = if embedding_text.contains("near") {
                vec![1.0, 0.0]
            } else {
                vec![0.0, 1.0]
            };
            Some(CentralityOutcome {
                density: 0.0,
                embedding,
                embedding_contract: None,
            })
        }
    }

    /// In-memory source holding a fixed item set. `list_changed_since` filters
    /// `watermark_cursor > watermark` (strict, matching the trait contract) so
    /// a re-run at the advanced watermark yields nothing.
    struct MockSource {
        kind: &'static str,
        items: Vec<CorpusItem>,
    }

    impl MockSource {
        fn new(kind: &'static str, items: Vec<CorpusItem>) -> Self {
            Self { kind, items }
        }
    }

    #[async_trait]
    impl ResurfacingSource for MockSource {
        fn corpus_kind(&self) -> &'static str {
            self.kind
        }

        async fn list_changed_since(
            &self,
            _principal: &str,
            _workspace: &str,
            watermark: i64,
        ) -> Result<Vec<CorpusItem>> {
            Ok(self
                .items
                .iter()
                .filter(|it| it.watermark_cursor > watermark)
                .cloned()
                .collect())
        }
    }

    /// Simple ctx: fixed clock, flat density (`Ok(0.0)`) and zero access count.
    struct TestCtx {
        now: i64,
    }

    impl ScoreCtx for TestCtx {
        fn now(&self) -> i64 {
            self.now
        }

        fn neighbor_density(&self, _embedding_text: &str) -> Result<f32> {
            Ok(0.0)
        }

        fn access_count(&self, _source_ref: &str) -> u32 {
            0
        }
    }

    /// A memory-kind corpus item at `occurred_at` with a date-free title/digest
    /// (so temporal anchoring stays out of these tests).
    fn item_at(source_ref: &str, occurred_at: i64) -> CorpusItem {
        CorpusItem {
            source_kind: SourceKind::Memory,
            source_ref: source_ref.to_string(),
            title: format!("title-{source_ref}"),
            digest: format!("digest-{source_ref}"),
            content_details: None,
            content_revision: None,
            occurred_at,
            watermark_cursor: occurred_at,
            embedding_text: format!("text-{source_ref}"),
        }
    }

    /// Like [`item_at`] but with an explicit `source_kind`, so the P4c per-lane
    /// utility test can score one item in each lane. Date-free so only lane +
    /// recency (identical across items) decide the salience.
    fn item_kind(kind: SourceKind, source_ref: &str, occurred_at: i64) -> CorpusItem {
        CorpusItem {
            source_kind: kind,
            source_ref: source_ref.to_string(),
            title: format!("title-{source_ref}"),
            digest: format!("digest-{source_ref}"),
            content_details: None,
            content_revision: None,
            occurred_at,
            watermark_cursor: occurred_at,
            embedding_text: format!("text-{source_ref}"),
        }
    }

    /// A minimal live candidate in an arbitrary lane, used only to seed the
    /// engagement counter (each `record_action` needs a candidate row to read the
    /// lane's `source_kind` from). Carries no embedding, so seeding dismisses/
    /// opens on it writes no dismissed/affinity signals — only the utility
    /// counter moves.
    fn seed_candidate(kind: SourceKind, source_ref: &str) -> Candidate {
        Candidate {
            candidate_id: candidate_id(kind, source_ref),
            source_kind: kind,
            source_ref: source_ref.to_string(),
            title: String::new(),
            content_digest: String::new(),
            content_details: None,
            content_revision: None,
            semantic_features: None,
            salience_score: 0.0,
            signals: Default::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Candidate,
            first_seen_at: 0,
            last_scored_at: 0,
            last_surfaced_at: None,
            cooldown_until: 0,
            surface_count: 0,
            dismiss_count: 0,
        }
    }

    #[tokio::test]
    async fn scorer_pass_scores_new_memory_items_and_advances_watermark() {
        let store = ResurfacingStore::open_in_temp();
        let src = MockSource::new("memory", vec![item_at("a", 100), item_at("b", 200)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];

        let n = run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(
            store
                .list_top_candidates("anonymous", "default", 1000, 10)
                .await
                .unwrap()
                .len(),
            2
        );
        // Watermark advanced to the newest item stamp.
        assert_eq!(
            store
                .get_watermark("anonymous", "default", "memory")
                .await
                .unwrap(),
            200
        );
    }

    #[tokio::test]
    async fn cancelled_scorer_pass_does_not_advance_source_watermark() {
        let store = ResurfacingStore::open_in_temp();
        let src = MockSource::new("memory", vec![item_at("a", 100), item_at("b", 200)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let cancel = CancellationToken::new();
        cancel.cancel();

        let n = run_scorer_pass_with_cancellation(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
            &cancel,
        )
        .await
        .unwrap();

        assert_eq!(n, 0);
        assert_eq!(
            store
                .get_watermark("anonymous", "default", "memory")
                .await
                .unwrap(),
            0,
            "cancellation must leave work retryable"
        );
    }

    #[tokio::test]
    async fn scorer_pass_advances_source_cursor_independent_of_scoring_time() {
        let store = ResurfacingStore::open_in_temp();
        let mut item = item_kind(SourceKind::Comm, "same-second-message", 5_000);
        item.watermark_cursor = 5_000_200;
        let src = MockSource::new("comm", vec![item]);
        let ctx = TestCtx { now: 10_000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];

        let n = run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();

        assert_eq!(n, 1);
        assert_eq!(
            store
                .get_watermark("anonymous", "default", "comm")
                .await
                .unwrap(),
            5_000_200,
            "source-native cursor advances without corrupting occurred_at"
        );

        let second = run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(second, 0, "cursor catches up even inside one second");
    }

    #[tokio::test]
    async fn scorer_pass_is_idempotent_on_rerun() {
        let store = ResurfacingStore::open_in_temp();
        let src = MockSource::new("memory", vec![item_at("a", 100), item_at("b", 200)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];

        let first = run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(first, 2);

        // Second pass: the source respects the advanced watermark, so nothing is
        // newer than 200 -> zero scored, watermark unchanged, two rows total.
        let second = run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            second, 0,
            "re-run must score nothing once the watermark caught up"
        );
        assert_eq!(
            store
                .list_top_candidates("anonymous", "default", 1000, 10)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            store
                .get_watermark("anonymous", "default", "memory")
                .await
                .unwrap(),
            200
        );
    }

    #[tokio::test]
    async fn scorer_pass_preserves_dismissed_state_and_feedback_counts() {
        let store = ResurfacingStore::open_in_temp();
        let src = MockSource::new("memory", vec![item_at("a", 100)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];

        // First pass creates the candidate in the default `Candidate` state.
        run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let id = candidate_id(SourceKind::Memory, "a");
        let c = store
            .get_candidate("anonymous", "default", &id)
            .await
            .unwrap()
            .unwrap();

        // Simulate owner feedback through the real lifecycle path: dismissed
        // with a cooldown + recorded dismissal. `upsert_candidate` deliberately
        // preserves lifecycle columns on conflict, so mutating and upserting a
        // fetched row is not a valid way to record owner feedback.
        let pinned_first_seen = c.first_seen_at;
        store
            .record_action(
                "anonymous",
                "default",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                9_000,
                999,
                86_400,
            )
            .await
            .unwrap();

        // Rewind the watermark so the same item is re-scanned, then re-run.
        store
            .set_watermark("anonymous", "default", "memory", 0)
            .await
            .unwrap();
        run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();

        // The re-score must NOT resurrect the row: state/cooldown/dismiss_count
        // and first_seen are preserved; only last_scored_at is refreshed.
        let after = store
            .get_candidate("anonymous", "default", &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, CandidateState::Dismissed);
        assert_eq!(after.cooldown_until, 9_999);
        assert_eq!(after.dismiss_count, 1);
        assert_eq!(after.first_seen_at, pinned_first_seen);
        assert_eq!(after.last_scored_at, 1000);
    }

    #[tokio::test]
    async fn centrality_provider_raises_the_centrality_signal() {
        let src = MockSource::new("memory", vec![item_at("a", 100)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let id = candidate_id(SourceKind::Memory, "a");

        // Score identical fresh corpora independently. Watermarks are
        // intentionally monotonic, so trying to "rewind" one store would not
        // execute a second pass and would only re-read the first score.
        let with_store = ResurfacingStore::open_in_temp();
        run_scorer_pass(
            "anonymous",
            "default",
            &with_store,
            sources,
            &ctx,
            &StubCentrality(0.9),
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let with = with_store
            .get_candidate("anonymous", "default", &id)
            .await
            .unwrap()
            .unwrap();

        let without_store = ResurfacingStore::open_in_temp();
        run_scorer_pass(
            "anonymous",
            "default",
            &without_store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let without = without_store
            .get_candidate("anonymous", "default", &id)
            .await
            .unwrap()
            .unwrap();

        // The provider's density flows straight into the centrality signal, and
        // lifts the blended salience above the no-centrality baseline (which
        // degrades to exactly 0.0).
        assert!((with.signals.centrality - 0.9).abs() < 1e-6);
        assert_eq!(without.signals.centrality, 0.0);
        assert!(with.signals.centrality > without.signals.centrality);
        assert!(with.salience_score > without.salience_score);
    }

    #[tokio::test]
    async fn scorer_persists_embedding_when_provider_returns_one() {
        let store = ResurfacingStore::open_in_temp();
        let src = MockSource::new("memory", vec![item_at("a", 100)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let id = candidate_id(SourceKind::Memory, "a");

        // No embedding until a scoring pass persists the provider's vector.
        assert!(store
            .get_embedding("anonymous", "default", &id)
            .await
            .unwrap()
            .is_none());

        let embedding = vec![0.1_f32, 0.2, 0.3];
        let provider = StubOutcome {
            density: 0.7,
            embedding: embedding.clone(),
            embedding_contract: None,
        };
        run_scorer_pass(
            "anonymous",
            "default",
            &store,
            sources,
            &ctx,
            &provider,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();

        // The exact vector the provider returned is now retrievable by candidate.
        assert_eq!(
            store
                .get_embedding("anonymous", "default", &id)
                .await
                .unwrap()
                .unwrap(),
            embedding
        );
    }

    #[tokio::test]
    async fn scorer_preserves_exact_contract_and_changed_content_cannot_inherit_it() {
        let store = ResurfacingStore::open_in_temp();
        let first_source = MockSource::new("memory", vec![item_at("contract-row", 100)]);
        let ctx = TestCtx { now: 1000 };
        let provider = StubOutcome {
            density: 0.5,
            embedding: vec![0.6, 0.8],
            embedding_contract: Some("model-a".to_string()),
        };
        run_scorer_pass(
            "p",
            "w",
            &store,
            &[&first_source],
            &ctx,
            &provider,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let id = candidate_id(SourceKind::Memory, "contract-row");
        let requested = vec![id.clone()];
        let first = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &requested)
            .await
            .unwrap();
        assert_eq!(first[0].embedding_contract, "model-a");

        let mut changed = item_at("contract-row", 200);
        changed.digest = "changed-digest".to_string();
        let changed_source = MockSource::new("memory", vec![changed]);
        run_scorer_pass(
            "p",
            "w",
            &store,
            &[&changed_source],
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let after = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &requested)
            .await
            .unwrap();
        assert_ne!(after[0].content_digest, "changed-digest");
        assert_eq!(after[0].embedding_contract, "model-a");
    }

    #[tokio::test]
    async fn scorer_penalizes_items_resembling_past_dismissals() {
        let ctx = TestCtx { now: 1000 };
        // Two items at the SAME occurred_at so their intrinsic scores are equal;
        // they differ only in the embedding `PerTextCentrality` assigns them.
        let src = MockSource::new("memory", vec![item_at("near", 100), item_at("far", 100)]);
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let near_id = candidate_id(SourceKind::Memory, "near");
        let far_id = candidate_id(SourceKind::Memory, "far");

        // Baseline store: NO dismissed signals, so the penalty is a no-op and we
        // capture each item's un-penalized salience.
        let base = ResurfacingStore::open_in_temp();
        run_scorer_pass(
            "p",
            "w",
            &base,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let base_near = base
            .get_candidate("p", "w", &near_id)
            .await
            .unwrap()
            .unwrap();
        let base_far = base
            .get_candidate("p", "w", &far_id)
            .await
            .unwrap()
            .unwrap();

        // Penalized store: seed ONE durable dismissed signal parallel to the
        // "near" item's embedding ([1,0]) and orthogonal to "far" ([0,1]).
        let store = ResurfacingStore::open_in_temp();
        store
            // Similar to "near" (cosine 0.9) but NOT identical: identical would
            // trip series suppression and the candidate would not exist at all,
            // which is a different behavior than the penalty under test here.
            .record_dismissed_signal("p", "w", &[0.9, 0.436], 50)
            .await
            .unwrap();
        run_scorer_pass(
            "p",
            "w",
            &store,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let pen_near = store
            .get_candidate("p", "w", &near_id)
            .await
            .unwrap()
            .unwrap();
        let pen_far = store
            .get_candidate("p", "w", &far_id)
            .await
            .unwrap()
            .unwrap();

        // The item resembling a past dismissal is cut to ~×DISMISS_PENALTY_FACTOR
        // of its un-penalized salience...
        assert!(
            (pen_near.salience_score - base_near.salience_score * 0.4).abs() < 1e-5,
            "near item should be penalized by the dismissal factor: base={} pen={}",
            base_near.salience_score,
            pen_near.salience_score,
        );
        // ...while the far item is untouched by the penalty.
        assert!(
            (pen_far.salience_score - base_far.salience_score).abs() < 1e-6,
            "far item must be unaffected: base={} pen={}",
            base_far.salience_score,
            pen_far.salience_score,
        );
        // And the penalty leaves the near item ranked below the far item, even
        // though their intrinsic scores were equal.
        assert!(
            pen_near.salience_score < pen_far.salience_score,
            "the dismissal penalty must sink the resembling item below the far one",
        );
    }

    #[tokio::test]
    async fn scorer_boosts_items_resembling_past_positive_actions() {
        let ctx = TestCtx { now: 1000 };
        // Two items at the SAME occurred_at so their intrinsic scores are equal;
        // they differ only in the embedding `PerTextCentrality` assigns them.
        let src = MockSource::new("memory", vec![item_at("near", 100), item_at("far", 100)]);
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let near_id = candidate_id(SourceKind::Memory, "near");
        let far_id = candidate_id(SourceKind::Memory, "far");

        // Baseline store: NO affinity signals, so the boost is a no-op and we
        // capture each item's un-boosted salience.
        let base = ResurfacingStore::open_in_temp();
        run_scorer_pass(
            "p",
            "w",
            &base,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let base_near = base
            .get_candidate("p", "w", &near_id)
            .await
            .unwrap()
            .unwrap();
        let base_far = base
            .get_candidate("p", "w", &far_id)
            .await
            .unwrap()
            .unwrap();

        // Boosted store: seed ONE durable affinity signal parallel to the "near"
        // item's embedding ([1,0]) and orthogonal to "far" ([0,1]).
        let store = ResurfacingStore::open_in_temp();
        store
            .record_affinity_signal("p", "w", &[1.0, 0.0], 50)
            .await
            .unwrap();
        run_scorer_pass(
            "p",
            "w",
            &store,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let boost_near = store
            .get_candidate("p", "w", &near_id)
            .await
            .unwrap()
            .unwrap();
        let boost_far = store
            .get_candidate("p", "w", &far_id)
            .await
            .unwrap()
            .unwrap();

        // The item resembling a past positive action is lifted to
        // ~×AFFINITY_BOOST_FACTOR of its un-boosted salience...
        assert!(
            (boost_near.salience_score - base_near.salience_score * 1.5).abs() < 1e-5,
            "near item should be boosted by the affinity factor: base={} boost={}",
            base_near.salience_score,
            boost_near.salience_score,
        );
        // ...while the far item is untouched by the boost.
        assert!(
            (boost_far.salience_score - base_far.salience_score).abs() < 1e-6,
            "far item must be unaffected: base={} boost={}",
            base_far.salience_score,
            boost_far.salience_score,
        );
        // And the boost leaves the near item ranked ABOVE the far item, even
        // though their intrinsic scores were equal.
        assert!(
            boost_near.salience_score > boost_far.salience_score,
            "the affinity boost must lift the resembling item above the far one",
        );
    }

    /// One dismissal must end a recurring series. A per-item penalty cannot:
    /// tomorrow's copy is a new candidate scoring on its own freshness, so it
    /// surfaces before the penalty is re-scored onto it and the owner dismisses
    /// the same digest daily.
    #[tokio::test]
    async fn scorer_never_creates_a_candidate_repeating_a_dismissed_series() {
        let ctx = TestCtx { now: 1000 };
        let src = MockSource::new("memory", vec![item_at("near", 100), item_at("far", 100)]);
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let near_id = candidate_id(SourceKind::Memory, "near");
        let far_id = candidate_id(SourceKind::Memory, "far");

        let store = ResurfacingStore::open_in_temp();
        // Exactly "near"'s embedding: the same thing again, not merely similar.
        store
            .record_dismissed_signal("p", "w", &[1.0, 0.0], 50)
            .await
            .unwrap();
        run_scorer_pass(
            "p",
            "w",
            &store,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();

        assert!(
            store
                .get_candidate("p", "w", &near_id)
                .await
                .unwrap()
                .is_none(),
            "a repeat of a dismissed series must never be created"
        );
        // Suppression is targeted, not a blanket mute of the source: an
        // unrelated item from the same pass still lands.
        assert!(
            store
                .get_candidate("p", "w", &far_id)
                .await
                .unwrap()
                .is_some(),
            "an unrelated item must still be created"
        );

        // The watermark still advanced past the suppressed item, so a later
        // pass does not re-offer it forever.
        let replay = run_scorer_pass(
            "p",
            "w",
            &store,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            replay, 0,
            "the watermark must have advanced past the suppressed item"
        );
    }

    #[tokio::test]
    async fn scorer_combines_penalty_and_boost_multiplicatively() {
        let ctx = TestCtx { now: 1000 };
        // A single item whose embedding `PerTextCentrality` sets to [1,0].
        let src = MockSource::new("memory", vec![item_at("near", 100)]);
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let near_id = candidate_id(SourceKind::Memory, "near");

        // Baseline store: NO feedback signals -> both multipliers are 1.0.
        let base = ResurfacingStore::open_in_temp();
        run_scorer_pass(
            "p",
            "w",
            &base,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let base_near = base
            .get_candidate("p", "w", &near_id)
            .await
            .unwrap()
            .unwrap();

        // Combined store: seed BOTH a dismissed signal AND an affinity signal
        // parallel to the item's embedding ([1,0]). The item resembles both, so
        // its salience is multiplied by penalty × boost (0.4 × 1.5 = 0.6) — the
        // two feedbacks partially cancel rather than either erasing the other.
        let store = ResurfacingStore::open_in_temp();
        store
            // Similar, not identical — see the note in the penalty test.
            .record_dismissed_signal("p", "w", &[0.9, 0.436], 40)
            .await
            .unwrap();
        store
            .record_affinity_signal("p", "w", &[1.0, 0.0], 50)
            .await
            .unwrap();
        run_scorer_pass(
            "p",
            "w",
            &store,
            sources,
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
        )
        .await
        .unwrap();
        let combined_near = store
            .get_candidate("p", "w", &near_id)
            .await
            .unwrap()
            .unwrap();

        assert!(
            (combined_near.salience_score - base_near.salience_score * 0.4 * 1.5).abs() < 1e-5,
            "net multiplier must be penalty × boost (0.4 × 1.5): base={} combined={}",
            base_near.salience_score,
            combined_near.salience_score,
        );
    }

    #[tokio::test]
    async fn scorer_downweights_low_engagement_lane() {
        let store = ResurfacingStore::open_in_temp();
        let ctx = TestCtx { now: 1000 };
        let cfg = ResurfacingScoringConfig::default();

        // Seed engagement: the memory lane is heavily DISMISSED, the comm lane is
        // heavily ENGAGED. Each action needs a candidate row to attribute; these
        // seed rows carry no embedding, so no dismissed/affinity signals are
        // written and ONLY the per-lane utility counter moves. Re-acting on the
        // same candidate keeps incrementing the lane counter (the row persists).
        let seed_mem = seed_candidate(SourceKind::Memory, "seed-mem");
        let seed_comm = seed_candidate(SourceKind::Comm, "seed-comm");
        store.upsert_candidate("p", "w", &seed_mem).await.unwrap();
        store.upsert_candidate("p", "w", &seed_comm).await.unwrap();
        for i in 0..20 {
            store
                .record_action(
                    "p",
                    "w",
                    &seed_mem.candidate_id,
                    FeedbackAction::Dismiss,
                    i,
                    3_600,
                    86_400,
                )
                .await
                .unwrap();
            store
                .record_action(
                    "p",
                    "w",
                    &seed_comm.candidate_id,
                    FeedbackAction::Open,
                    i,
                    3_600,
                    86_400,
                )
                .await
                .unwrap();
        }

        // Score one FRESH item per lane (distinct from the seed rows) with
        // identical intrinsic salience — a memory, a comm, and a task lane with NO
        // engagement (the neutral baseline). One mixed source holds all three.
        let src = MockSource::new(
            "mixed",
            vec![
                item_kind(SourceKind::Memory, "mem-item", 100),
                item_kind(SourceKind::Comm, "comm-item", 100),
                item_kind(SourceKind::Task, "task-item", 100),
            ],
        );
        let sources: &[&dyn ResurfacingSource] = &[&src];
        run_scorer_pass("p", "w", &store, sources, &ctx, &NoCentrality, &cfg)
            .await
            .unwrap();

        let mem = store
            .get_candidate("p", "w", &candidate_id(SourceKind::Memory, "mem-item"))
            .await
            .unwrap()
            .unwrap();
        let comm = store
            .get_candidate("p", "w", &candidate_id(SourceKind::Comm, "comm-item"))
            .await
            .unwrap()
            .unwrap();
        let task = store
            .get_candidate("p", "w", &candidate_id(SourceKind::Task, "task-item"))
            .await
            .unwrap()
            .unwrap();

        // The task lane has no engagement, so its item is unaffected (×1.0) — it
        // is the intrinsic baseline the other two are re-weighted from.
        let intrinsic = task.salience_score;
        assert!(intrinsic > 0.0, "intrinsic baseline should be non-zero");

        // Each re-weighted lane matches intrinsic × its lane's utility multiplier
        // AS APPLIED TO THIS ITEM. These fixtures carry no embedding, so
        // centrality is 0: the engaged lane's boost is fully scaled away and the
        // dismissed lane's penalty still applies in full. That asymmetry is the
        // point — a popular lane may not lift an item with no connection to the
        // current context, while a rejected lane goes quieter regardless.
        let expect_mem = intrinsic * utility_multiplier_for_item(0, 20, 0.0, &cfg);
        let expect_comm = intrinsic * utility_multiplier_for_item(20, 0, 0.0, &cfg);
        assert!(
            (mem.salience_score - expect_mem).abs() < 1e-5,
            "dismissed memory lane must be down-weighted: got {} want {}",
            mem.salience_score,
            expect_mem,
        );
        assert!(
            (comm.salience_score - expect_comm).abs() < 1e-5,
            "engaged comm lane must be up-weighted: got {} want {}",
            comm.salience_score,
            expect_comm,
        );

        // The engaged lane outscores the dismissed lane despite identical
        // intrinsic salience; both straddle the neutral baseline.
        assert!(comm.salience_score > mem.salience_score);
        assert!(
            mem.salience_score < intrinsic,
            "dismissed lane sinks below neutral"
        );
        assert!(
            (comm.salience_score - intrinsic).abs() < 1e-5,
            "an engaged lane must not lift an item with no contextual relevance"
        );
    }

    /// The lane boost has to be earned per item, or one popular lane takes over
    /// the surface and then reinforces itself by accruing more engagement.
    #[test]
    fn lane_boost_scales_with_item_relevance_while_penalty_does_not() {
        let cfg = ResurfacingScoringConfig::default();

        // A heavily-engaged lane: flat multiplier sits at the ceiling.
        let flat = utility_multiplier(738, 2, &cfg);
        assert!(
            flat > 1.4,
            "engaged lane should pin near the ceiling: {flat}"
        );

        // An item with no contextual connection gets none of that boost.
        assert!((utility_multiplier_for_item(738, 2, 0.0, &cfg) - 1.0).abs() < 1e-6);
        // A fully-connected item gets all of it.
        assert!((utility_multiplier_for_item(738, 2, 1.0, &cfg) - flat).abs() < 1e-6);
        // And relevance interpolates rather than switching.
        let half = utility_multiplier_for_item(738, 2, 0.5, &cfg);
        assert!(
            half > 1.0 && half < flat,
            "half-relevant should interpolate: {half}"
        );

        // The penalty side is unconditional: a rejected lane goes quieter even
        // for an item that looks topical.
        let penalty = utility_multiplier(2, 738, &cfg);
        assert!(penalty < 1.0);
        assert!((utility_multiplier_for_item(2, 738, 1.0, &cfg) - penalty).abs() < 1e-6);
        assert!((utility_multiplier_for_item(2, 738, 0.0, &cfg) - penalty).abs() < 1e-6);
    }

    fn stated_preference(key: &str, topics: &[&str]) -> super::super::memory_context::ScopedMemory {
        use crate::magician_v2::agents::memory_scope::MemoryScope;
        use crate::magician_v2::agents::{MemoryKind, MemoryTrust};
        super::super::memory_context::ScopedMemory {
            key: key.to_string(),
            tier: "preferences".to_string(),
            source_type: "owner_confirmed".to_string(),
            trust: MemoryTrust::Stated,
            kind: MemoryKind::Normative,
            text: key.to_string(),
            updated_at: Some("1700000000".to_string()),
            scope: MemoryScope {
                topics: topics.iter().map(|topic| (*topic).to_string()).collect(),
                entities: Vec::new(),
                applies_to: Vec::new(),
            },
            may_explain: true,
            may_suppress: true,
            may_condition: true,
            may_propose_action: false,
        }
    }

    #[tokio::test]
    async fn scorer_reuses_cached_memory_judgement_without_reevaluating() {
        let store = ResurfacingStore::open_in_temp();
        let src = MockSource::new("memory", vec![item_at("a", 100)]);
        let ctx = TestCtx { now: 1000 };
        let sources: &[&dyn ResurfacingSource] = &[&src];
        let memories = vec![stated_preference("preferences: injected", &["digest"])];
        let memory_revision = super::super::memory_context::memory_set_revision(&memories);
        let id = candidate_id(SourceKind::Memory, "a");
        let injected = super::super::memory_effects::MemoryJudgement {
            would_suppress: true,
            suppress_reason: Some("injected-cache-hit".to_string()),
            salience_delta: -0.08,
            ..Default::default()
        };
        store
            .put_memory_applications("p", "w", &id, "digest-a", &memory_revision, &injected, 50)
            .await
            .unwrap();

        run_scorer_pass_with_memories(
            "p",
            "w",
            &store,
            sources,
            &ctx,
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
            &memories,
        )
        .await
        .unwrap();

        let got = store
            .get_memory_applications("p", "w", &id, "digest-a", &memory_revision)
            .await
            .unwrap()
            .expect("cached judgement");
        assert_eq!(got.suppress_reason.as_deref(), Some("injected-cache-hit"));
        assert_eq!(got.salience_delta, -0.08);
        assert!(
            got.applications.would_apply.is_empty(),
            "a cache hit must not replace the injected judgement with a fresh evaluate"
        );
    }

    #[tokio::test]
    async fn scorer_records_surface_and_dismiss_counts_on_engagement_conflict() {
        let store = ResurfacingStore::open_in_temp();
        let ctx = TestCtx { now: 1000 };
        let mut existing = seed_candidate(SourceKind::Memory, "near");
        existing.surface_count = 4;
        existing.dismiss_count = 1;
        store.upsert_candidate("p", "w", &existing).await.unwrap();
        store
            .record_affinity_signal("p", "w", &[1.0, 0.0], 50)
            .await
            .unwrap();

        let src = MockSource::new("memory", vec![item_at("near", 100)]);
        let memories = vec![stated_preference("preferences: avoid_near", &["near"])];
        run_scorer_pass_with_memories(
            "p",
            "w",
            &store,
            &[&src],
            &ctx,
            &PerTextCentrality,
            &ResurfacingScoringConfig::default(),
            &memories,
        )
        .await
        .unwrap();

        let id = candidate_id(SourceKind::Memory, "near");
        let memory_revision = super::super::memory_context::memory_set_revision(&memories);
        let got = store
            .get_memory_applications("p", "w", &id, "digest-near", &memory_revision)
            .await
            .unwrap()
            .expect("judgement");
        assert!(got.would_suppress);
        assert!(
            got.conflicts.iter().any(|conflict| conflict
                .rationale
                .contains("4 opens / 1 dismisses disagree")),
            "engagement conflict should cite the candidate's surface/dismiss counts: {:?}",
            got.conflicts
        );
        let after = store.get_candidate("p", "w", &id).await.unwrap().unwrap();
        assert_eq!(after.surface_count, 4);
        assert_eq!(after.dismiss_count, 1);
        assert_eq!(after.state, CandidateState::Candidate);
    }
}
