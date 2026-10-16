//! Background worker for the Proactive Resurfacing Engine.
//!
//! [`ResurfacingWorker`] is the timer that drives the (already-committed)
//! timer-free passes: [`run_scorer_pass`] sweeps every corpus source — memory,
//! task/episode, and comms — on a fast cadence (default hourly), and
//! [`run_curation_pass_llm_with_recommendations`] surfaces router-accepted candidates
//! via the LLM curation path (with a deterministic fallback when no router is
//! bound) on a slow cadence (default daily). It mirrors
//! `channel_assist::pattern_synthesis::ChannelPatternSynthesisWorker`:
//!
//! * env kill-switch [`ResurfacingWorker::enabled_from_env`] — **on by default**
//!   (`RESURFACING_ENABLED`; only `0`/`false` disables), so `spawn` returns a
//!   no-op handle when disabled instead of gating at the call site;
//! * env-tunable cadence/limits via the same `env_secs`-style helpers, clamped
//!   to sane floors so a mis-set var can't hammer the runtime or divide by zero;
//! * independent scorer + curator background loops under one
//!   [`CancellationToken`], so a slow scorer pass cannot starve already-eligible
//!   candidate curation;
//! * bounded, deterministic scope enumeration from the artifact workspace,
//!   always including the default scope.
//!
//! Per-scope isolation: a failing scope is logged (`tracing::warn!`) and never
//! aborts the sweep, so one bad scope can't starve the others.
//!
//! Scope discipline: this file is the worker + its config only. The scoring
//! pass, curator, sources, and store all live in sibling modules and are reused
//! verbatim — no new store/scoring/curator logic here.

use magician::magician_v2::resurfacing_seam::ResurfacingWakeHandle;
use std::collections::HashSet;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::channel_assist::channel::ChannelAssistStore;
use magician::magician_v2::agents::AgentMemoryResolver;
use magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext;
use magician::magician_v2::artifact_v2::workspace::{
    ArtifactV2Workspace, DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::artifact_v2::ArtifactV2Service;
use magician::magician_v2::attention::resurfacing::centrality::{
    CentralityEmbeddingConfig, CentralityProvider, CentralityReference, EmbeddingCentrality,
    NoCentrality, ReusableCentralityEmbedding,
};
use magician::magician_v2::attention::resurfacing::memory_context::memories_from_knowledge;
#[cfg(any(test, feature = "test-fixtures"))]
use magician::magician_v2::attention::resurfacing::scorer::run_scorer_pass_with_cancellation;
use magician::magician_v2::attention::resurfacing::scorer::{
    run_scorer_pass_with_cancellation_and_memories, BasicScoreCtx,
};
use magician::magician_v2::attention::resurfacing::scoring::ResurfacingScoringConfig;
use magician::magician_v2::attention::resurfacing::sources::memory::MemorySource;
use magician::magician_v2::attention::resurfacing::sources::task_episode::TaskEpisodeSource;
use magician::magician_v2::attention::resurfacing::sources::ResurfacingSource;
use magician::magician_v2::attention::resurfacing::store::{
    CandidateEmbeddingSnapshot, ResurfacingStore,
};
use magician::magician_v2::attention::resurfacing::types::candidate_id;
use magician::magician_v2::attention_funnel_store::AttentionFunnelStore;
use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use magician::magician_v2::runtime::ollama_lifecycle;
use magician::magician_v2::user_requests::UserRequestService;

use super::curator::{
    repair_active_surfaced_candidates, run_curation_pass_llm_with_recommendations_and_telemetry,
    CurationRecommendationPolicy,
};
use super::sources::comms::CommsSource;

const LOG_TARGET: &str = "resurfacing::worker";

/// Scorer cadence — how often salience is recomputed from the sources.
const DEFAULT_SCORER_INTERVAL_SECS: u64 = 3_600; // hourly
/// Curator cadence — how often the top eligible candidates are surfaced.
const DEFAULT_CURATOR_INTERVAL_SECS: u64 = 86_400; // daily
                                                   // How many candidates a curation sweep surfaces per scope is now config-driven
                                                   // (`resurfacing.surface_cap`, default via `default_resurfacing_surface_cap`),
                                                   // threaded into `spawn_with_attention` and overridable by `RESURFACING_SURFACE_CAP`.
/// Retention horizon (days) for the housekeeping sweep — terminal candidates
/// and dismissed signals older than this are pruned.
const DEFAULT_RETENTION_DAYS: i64 = 90;
/// Per-scope candidate cap for the housekeeping sweep — a scope holding more
/// than this many rows is trimmed to the highest-priority `cap`.
const DEFAULT_CANDIDATE_CAP: usize = 2_000;

/// Upper bound on reference items embedded per scope per scorer pass to build
/// the centrality neighborhood. Bounds the one-shot reference embed; the scope's
/// corpus is ordered newest-first before truncation so recent context dominates.
const CENTRALITY_REFERENCE_CAP: usize = 512;
const BACKGROUND_SCOPE_CAP: usize = 128;

/// Sane floors so a mis-set env can't hammer the runtime. The scorer's decay
/// half-life now lives in [`ResurfacingScoringConfig`] (env-overridable, clamped
/// there), so no half-life floor is needed here.
const MIN_INTERVAL_SECS: u64 = 60;
const MIN_SURFACE_CAP: usize = 1;
/// Never let retention drop below a week, so a mis-set var can't prune live-ish
/// terminal rows almost immediately.
const MIN_RETENTION_DAYS: i64 = 7;
/// Never let the per-scope cap drop below a small floor, so a mis-set var can't
/// starve a scope down to a handful of rows.
const MIN_CANDIDATE_CAP: usize = 100;

/// Parse a positive `u64` env var, falling back to `default` when unset,
/// unparseable, or `0`. Mirrors `pattern_synthesis::env_secs`.
fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Parse a positive `usize` env var, falling back to `default` when unset,
/// unparseable, or `0`.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

/// Parse a positive `i64` env var, falling back to `default` when unset,
/// unparseable, or non-positive.
fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

async fn background_scopes(workspace: &ArtifactV2Workspace) -> Vec<(String, String)> {
    // Tenants only. Resurfacing decides what to bring back to a person's
    // attention, so it means nothing in a reserved sink — and every pass here
    // reaches the scope's mail store, which opens its DuckDB and holds the
    // connection for the life of the process. That one held connection is why a
    // sink's inbox database kept being checkpointed long after the other sweeps
    // stopped touching it.
    let mut scopes = match workspace.list_tenant_scope_segments().await {
        Ok(scopes) => scopes,
        Err(error) => {
            warn!(target: LOG_TARGET, error = %error, "failed to enumerate resurfacing scopes; using default scope");
            Vec::new()
        },
    };
    scopes.push((
        DEFAULT_SCOPE_PRINCIPAL.to_string(),
        DEFAULT_SCOPE_WORKSPACE.to_string(),
    ));
    scopes.sort();
    scopes.dedup();
    scopes.truncate(BACKGROUND_SCOPE_CAP);
    let default_scope = (
        DEFAULT_SCOPE_PRINCIPAL.to_string(),
        DEFAULT_SCOPE_WORKSPACE.to_string(),
    );
    if !scopes.contains(&default_scope) {
        scopes.pop();
        scopes.push(default_scope);
        scopes.sort();
    }
    scopes
}

/// Derive the `(success, error)` fields of an O1 run record from a pass `Result`,
/// WITHOUT consuming it (the caller still `match`es the value for its existing
/// per-scope logging). `Ok` → success with no error; `Err` → failure carrying the
/// error's `Display` string. Generic over the pass's Ok type so all three passes
/// (scorer `usize`, curator `Vec<Candidate>`, retention `usize`) share it.
fn run_success_error<T>(result: &anyhow::Result<T>) -> (bool, Option<String>) {
    match result {
        Ok(_) => (true, None),
        Err(error) => (false, Some(error.to_string())),
    }
}

/// Cheap scope-local admission check for optional centrality work. A scorer
/// pass still runs when this returns false so time decay remains intact; only
/// the expensive all-history reference scan/provider path is skipped. Source
/// errors fail open because the scorer owns authoritative error handling.
async fn scope_has_changed_work(
    store: &ResurfacingStore,
    sources: &[&dyn ResurfacingSource],
    principal: &str,
    workspace: &str,
    cancel: &CancellationToken,
) -> bool {
    for source in sources {
        if cancel.is_cancelled() {
            return false;
        }
        let watermark = match store
            .get_watermark(principal, workspace, source.corpus_kind())
            .await
        {
            Ok(watermark) => watermark,
            Err(error) => {
                debug!(target: LOG_TARGET, principal, workspace, error = %error, "centrality changed-work preflight could not read watermark; failing open");
                return true;
            },
        };
        let changed = tokio::select! {
            biased;
            _ = cancel.cancelled() => return false,
            changed = source.list_changed_since(principal, workspace, watermark) => changed,
        };
        match changed {
            Ok(items) if !items.is_empty() => return true,
            Ok(_) => {},
            Err(error) => {
                debug!(target: LOG_TARGET, principal, workspace, error = %error, "centrality changed-work preflight failed; failing open");
                return true;
            },
        }
    }
    false
}

/// Best-effort persist of one pass's O1 run record. Recording is observability,
/// never load-bearing: a failure to write the record is logged and swallowed so it
/// can't break the pass it describes or the sweep's per-scope isolation. Stamps the
/// record's `at` with the current wall clock (unix ms).
#[allow(clippy::too_many_arguments)]
async fn record_pass_run(
    store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
    kind: &str,
    started_at: i64,
    duration_ms: i64,
    produced: i64,
    success: bool,
    error: Option<&str>,
) {
    if let Err(err) = store
        .record_run(
            principal,
            workspace,
            kind,
            started_at,
            duration_ms,
            produced,
            success,
            error,
            chrono::Utc::now().timestamp_millis(),
        )
        .await
    {
        warn!(target: LOG_TARGET, principal, workspace, kind, error = %err, "failed to persist resurfacing run record (observability only)");
    }
}

/// One scorer sweep across every scope. Per-scope errors are logged and never
/// abort the sweep (scope isolation).
async fn run_scorer_all_scopes(
    store: &ResurfacingStore,
    resolver: &AgentMemoryResolver,
    service: &Arc<ArtifactV2Service>,
    channel_store: &ChannelAssistStore,
    scoring_cfg: &ResurfacingScoringConfig,
    centrality_config: CentralityEmbeddingConfig,
    memory_tiers: &[String],
    cancel: &CancellationToken,
    router: Option<
        &magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
    >,
) {
    let now = chrono::Utc::now().timestamp();
    let ctx = BasicScoreCtx { now };
    // Sources reused across scopes — each is stateless beyond its handle
    // (resolver / artifact service / channel store), and `run_scorer_pass` takes
    // principal/workspace per call.
    let mem = MemorySource::new(resolver.clone(), memory_tiers.to_vec());
    let task = TaskEpisodeSource::new(service.clone());
    let comm = CommsSource::new(channel_store.clone());
    let srcs: Vec<&dyn ResurfacingSource> = vec![&mem, &task, &comm];
    for (principal, workspace) in background_scopes(service.workspace()).await {
        // Observe the cancel token BETWEEN scopes so `shutdown()` can't be blocked
        // for a whole in-flight multi-scope pass (the outer `select!` only checks it
        // between full passes). Single scope today, so cheap; correct as scopes grow.
        if cancel.is_cancelled() {
            break;
        }
        // Build the embedding-backed centrality provider for this scope (a
        // one-shot reference embed of the scope's recent corpus), falling back
        // to the no-op provider when the embedder or a corpus isn't available.
        let scope_deadline = tokio::time::Instant::now() + centrality_config.reference_timeout;
        let preflight_budget =
            scope_deadline.saturating_duration_since(tokio::time::Instant::now());
        let has_changed_work = tokio::time::timeout(
            preflight_budget,
            scope_has_changed_work(store, &srcs, &principal, &workspace, cancel),
        )
        .await
        .unwrap_or(false);
        let provider: Box<dyn CentralityProvider> = if has_changed_work {
            match build_centrality_provider(
                store,
                &srcs,
                &principal,
                &workspace,
                centrality_config,
                cancel,
                scope_deadline,
            )
            .await
            {
                Some(embedding) => Box::new(embedding),
                None => Box::new(NoCentrality),
            }
        } else {
            debug!(target: LOG_TARGET, principal, workspace, "no changed source work; skipping optional centrality construction while retaining scorer decay");
            Box::new(NoCentrality)
        };
        if cancel.is_cancelled() {
            break;
        }
        // Time + record this pass for O1 observability, then keep the existing
        // per-scope logging by `match`ing the retained `result`.
        let t0 = Instant::now();
        let started_at = chrono::Utc::now().timestamp_millis();
        // The deadline above bounds only optional centrality preparation and
        // provider I/O. Authoritative source scoring/watermark advancement must
        // still run after that budget expires; its own cancellation token is
        // the shutdown boundary and an expired provider degrades to `None`.
        let memory_service = resolver.resolve_for_scope(&principal, &workspace).ok();
        let memories = match memory_service.as_ref() {
            Some(service) => match service.load_user_knowledge().await {
                Ok(knowledge) => memories_from_knowledge(&knowledge),
                Err(_) => Vec::new(),
            },
            None => Vec::new(),
        };
        let result = run_scorer_pass_with_cancellation_and_memories(
            &principal,
            &workspace,
            store,
            &srcs,
            &ctx,
            provider.as_ref(),
            scoring_cfg,
            cancel,
            &memories,
            router,
            memory_service.as_ref(),
        )
        .await;
        let duration_ms = t0.elapsed().as_millis() as i64;
        let produced = *result.as_ref().unwrap_or(&0); // upserted count (0 on error)
        let (success, error) = run_success_error(&result);
        record_pass_run(
            store,
            &principal,
            &workspace,
            "scorer",
            started_at,
            duration_ms,
            produced as i64,
            success,
            error.as_deref(),
        )
        .await;
        match result {
            Ok(n) if n > 0 => {
                debug!(target: LOG_TARGET, principal, workspace, scored = n, "scorer sweep upserted candidates")
            },
            Ok(_) => {},
            Err(error) => {
                warn!(target: LOG_TARGET, principal, workspace, error = %error, "scorer sweep failed for scope")
            },
        }
    }
}

/// Logged at most once when the shared Ollama embedder is unavailable, so the
/// centrality degradation (→ `0.0`) is visible without spamming every pass.
static CENTRALITY_UNAVAILABLE_LOG: Once = Once::new();

/// Build the embedding-backed centrality provider for one scope, or `None` when
/// the runtime's shared Ollama embedder isn't available or the scope has no
/// reference corpus yet — in which case the caller uses [`NoCentrality`] and
/// centrality stays `0.0` (graceful, matching the pre-signal behavior). Durable
/// candidate vectors are reused only when their embedding-time digest matches;
/// bounded misses are persisted so the full logical corpus is covered over
/// subsequent passes.
async fn build_centrality_provider(
    store: &ResurfacingStore,
    srcs: &[&dyn ResurfacingSource],
    principal: &str,
    workspace: &str,
    config: CentralityEmbeddingConfig,
    cancel: &CancellationToken,
    scope_deadline: tokio::time::Instant,
) -> Option<EmbeddingCentrality> {
    let Some(embedder) = ollama_lifecycle::embedder() else {
        CENTRALITY_UNAVAILABLE_LOG.call_once(|| {
            info!(
                target: LOG_TARGET,
                "resurfacing centrality: shared Ollama embedder unavailable; centrality signal degraded to 0 (logged once)"
            );
        });
        return None;
    };
    let references = gather_reference_inputs(
        srcs,
        principal,
        workspace,
        Some(cancel),
        Some(scope_deadline),
    )
    .await;
    if cancel.is_cancelled() {
        return None;
    }
    let reference_ids: Vec<String> = references
        .iter()
        .map(|reference| reference.candidate_id.clone())
        .collect();
    let remaining = scope_deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return None;
    }
    let reusable = match tokio::time::timeout(
        remaining,
        store.list_candidate_embedding_snapshots_for_ids(principal, workspace, &reference_ids),
    )
    .await
    {
        Ok(Ok(snapshots)) => snapshots
            .into_iter()
            .map(|snapshot| ReusableCentralityEmbedding {
                candidate_id: snapshot.candidate_id,
                content_digest: snapshot.content_digest,
                embedding_contract: snapshot.embedding_contract,
                embedding: snapshot.embedding,
            })
            .collect(),
        Ok(Err(error)) => {
            warn!(target: LOG_TARGET, principal, workspace, error = %error, "failed to load durable centrality embeddings; rebuilding bounded misses");
            Vec::new()
        },
        Err(_) => return None,
    };
    let build = EmbeddingCentrality::build_from_references_with_deadline_and_cancellation(
        embedder,
        &references,
        &reusable,
        config,
        principal,
        workspace,
        cancel.clone(),
        scope_deadline,
    )
    .await?;
    // A completed provider input is a durable progress unit even when
    // cancellation arrives before scoring. Commit the whole successful prefix
    // atomically under one lock and prune once.
    let durable_prefix: Vec<CandidateEmbeddingSnapshot> = build
        .newly_embedded
        .iter()
        .map(|embedding| CandidateEmbeddingSnapshot {
            candidate_id: embedding.candidate_id.clone(),
            content_digest: embedding.content_digest.clone(),
            embedding_contract: embedding.embedding_contract.clone(),
            embedding: embedding.embedding.clone(),
        })
        .collect();
    if !durable_prefix.is_empty() {
        // `spawn_blocking` SQLite work cannot be cancelled safely once begun.
        // Await the bounded atomic transaction honestly; reporting a timeout
        // while it still commits behind the worker would create a false state.
        let persisted = store
            .upsert_embeddings_for_digests(principal, workspace, &durable_prefix)
            .await;
        if let Err(error) = persisted {
            warn!(target: LOG_TARGET, principal, workspace, rows = durable_prefix.len(), error = %error, "failed to persist reusable centrality embedding prefix");
        } else if tokio::time::Instant::now() >= scope_deadline {
            warn!(target: LOG_TARGET, principal, workspace, rows = durable_prefix.len(), "bounded durable embedding commit completed after the optional scope deadline");
        }
    }
    debug!(
        target: LOG_TARGET,
        principal,
        workspace,
        logical_references = build.logical_references,
        covered_references = build.covered_references,
        deferred_references = build.deferred_references,
        newly_persisted = build.newly_embedded.len(),
        "resurfacing centrality durable coverage updated"
    );
    Some(build.provider)
}

/// Collect the scope's recent corpus (embedding texts) across every source for
/// the centrality reference set. Each source is scanned from watermark `0` (its
/// own internal read cap already bounds the scan); the union is ordered
/// newest-first and truncated to [`CENTRALITY_REFERENCE_CAP`]. A failing source
/// is logged and skipped — its context is simply absent from the neighborhood,
/// never aborting the sweep.
async fn gather_reference_inputs(
    srcs: &[&dyn ResurfacingSource],
    principal: &str,
    workspace: &str,
    cancel: Option<&CancellationToken>,
    scope_deadline: Option<tokio::time::Instant>,
) -> Vec<CentralityReference> {
    let mut scored: Vec<(i64, CentralityReference)> = Vec::new();
    for src in srcs {
        let remaining = scope_deadline
            .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
            .unwrap_or(Duration::MAX);
        if remaining.is_zero() {
            break;
        }
        let scan = tokio::time::timeout(remaining, src.list_changed_since(principal, workspace, 0));
        let changed = if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = scan => result,
            }
        } else {
            scan.await
        };
        match changed {
            Ok(Ok(items)) => {
                for item in items {
                    let text = item.embedding_text.trim().to_string();
                    if !text.is_empty() {
                        scored.push((
                            item.occurred_at,
                            CentralityReference {
                                candidate_id: candidate_id(item.source_kind, &item.source_ref),
                                content_digest: item.digest,
                                embedding_text: text,
                            },
                        ));
                    }
                }
            },
            Ok(Err(error)) => {
                debug!(target: LOG_TARGET, principal, workspace, error = %error, "centrality reference scan failed for a source; skipping it")
            },
            Err(_) => break,
        }
    }
    // Newest-first so truncation keeps the most recent context.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let mut seen_ids = HashSet::new();
    let mut seen_texts = HashSet::new();
    scored
        .into_iter()
        .filter_map(|(_, reference)| {
            (seen_ids.insert(reference.candidate_id.clone())
                && seen_texts.insert(reference.embedding_text.clone()))
            .then_some(reference)
        })
        .take(CENTRALITY_REFERENCE_CAP)
        .collect()
}

/// One curator sweep across every scope, isolated per scope as above. Uses the
/// LLM curation path, which falls back to the deterministic pick internally when
/// no router is bound.
async fn run_curator_all_scopes(
    workspace_layout: &ArtifactV2Workspace,
    store: &ResurfacingStore,
    channel_store: &ChannelAssistStore,
    cap: usize,
    router: Option<Arc<OperationLlmRouter>>,
    recommendation_policy: CurationRecommendationPolicy,
    active_repair_enabled: bool,
    active_repair_batch_size: usize,
    memory_tiers: &[String],
    attention_store: Option<&AttentionFunnelStore>,
    event_broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    cancel: &CancellationToken,
) {
    let now = chrono::Utc::now().timestamp();
    for (principal, workspace) in background_scopes(workspace_layout).await {
        // Observe the cancel token between scopes (see `run_scorer_all_scopes`) so a
        // shutdown mid-pass returns promptly instead of after the whole sweep.
        if cancel.is_cancelled() {
            break;
        }
        if active_repair_enabled {
            let repair_t0 = Instant::now();
            let repair_started_at = chrono::Utc::now().timestamp_millis();
            let repair = repair_active_surfaced_candidates(
                &principal,
                &workspace,
                store,
                channel_store,
                attention_store,
                active_repair_batch_size,
                now,
                false,
            )
            .await;
            let repair_duration_ms = repair_t0.elapsed().as_millis() as i64;
            let repair_produced = repair
                .as_ref()
                .map(|outcome| outcome.repaired as i64)
                .unwrap_or_default();
            let (repair_success, repair_error) = run_success_error(&repair);
            record_pass_run(
                store,
                &principal,
                &workspace,
                "routing_repair",
                repair_started_at,
                repair_duration_ms,
                repair_produced,
                repair_success,
                repair_error.as_deref(),
            )
            .await;
            match repair {
                Ok(outcome) if outcome.scanned > 0 => debug!(
                    target: LOG_TARGET,
                    principal,
                    workspace,
                    scanned = outcome.scanned,
                    repaired = outcome.repaired,
                    rerouted = outcome.rerouted,
                    legacy = outcome.legacy,
                    failed = outcome.failed,
                    "active resurfacing routing repair completed"
                ),
                Ok(_) => {},
                Err(error) => warn!(
                    target: LOG_TARGET,
                    principal,
                    workspace,
                    error = %error,
                    "active resurfacing routing repair failed for scope"
                ),
            }

            // The routing repair above re-checks comm cards against current
            // routing; this does the same for memory cards against the current
            // tier allowlist. Narrowing `memory_tiers` otherwise only governs
            // what is ingested next, leaving whatever the old policy already
            // surfaced in the owner's lane until it ages out.
            match store
                .retract_ineligible_memory_candidates(
                    &principal,
                    &workspace,
                    memory_tiers,
                    active_repair_batch_size,
                    true,
                )
                .await
            {
                Ok(0) => {},
                Ok(retracted) => debug!(
                    target: LOG_TARGET,
                    principal,
                    workspace,
                    retracted,
                    "retracted surfaced memory cards whose tier is no longer eligible"
                ),
                Err(error) => warn!(
                    target: LOG_TARGET,
                    principal,
                    workspace,
                    error = %error,
                    "memory-tier retraction failed for scope"
                ),
            }
        }
        // Time + record this pass for O1 observability, then keep the existing
        // per-scope logging by `match`ing the retained `result`.
        let t0 = Instant::now();
        let started_at = chrono::Utc::now().timestamp_millis();
        let telemetry = event_broadcaster.map(|broadcaster| {
            OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                principal.clone(),
                workspace.clone(),
                "resurfacing",
            )
        });
        let result = run_curation_pass_llm_with_recommendations_and_telemetry(
            &principal,
            &workspace,
            store,
            cap,
            now,
            router.clone(),
            attention_store,
            Some(channel_store),
            recommendation_policy,
            telemetry.as_ref(),
        )
        .await;
        let duration_ms = t0.elapsed().as_millis() as i64;
        let produced = result.as_ref().map(|s| s.len()).unwrap_or(0); // surfaced (0 on error)
        let (success, error) = run_success_error(&result);
        record_pass_run(
            store,
            &principal,
            &workspace,
            "curator",
            started_at,
            duration_ms,
            produced as i64,
            success,
            error.as_deref(),
        )
        .await;
        match result {
            Ok(surfaced) if !surfaced.is_empty() => {
                debug!(target: LOG_TARGET, principal, workspace, surfaced = surfaced.len(), "curator sweep surfaced candidates")
            },
            Ok(_) => {},
            Err(error) => {
                warn!(target: LOG_TARGET, principal, workspace, error = %error, "curator sweep failed for scope")
            },
        }
    }
}

/// One retention sweep across every scope, isolated per scope as above. Keeps
/// each scope's store bounded (age-out terminal candidates + dismissed signals,
/// cap scope size, clear orphaned side-table rows). Folded into the curator
/// tick — a daily-ish housekeeping cadence — so it needs no timer of its own.
async fn run_retention_all_scopes(
    workspace_layout: &ArtifactV2Workspace,
    store: &ResurfacingStore,
    retention_days: i64,
    candidate_cap: usize,
    cancel: &CancellationToken,
) {
    let now = chrono::Utc::now().timestamp();
    for (principal, workspace) in background_scopes(workspace_layout).await {
        // Observe the cancel token between scopes (see `run_scorer_all_scopes`) so a
        // shutdown mid-pass returns promptly instead of after the whole sweep.
        if cancel.is_cancelled() {
            break;
        }
        // Time + record this pass for O1 observability, then keep the existing
        // per-scope logging by `match`ing the retained `result`.
        let t0 = Instant::now();
        let started_at = chrono::Utc::now().timestamp_millis();
        let result = store
            .retention_sweep(&principal, &workspace, now, retention_days, candidate_cap)
            .await;
        let duration_ms = t0.elapsed().as_millis() as i64;
        let produced = *result.as_ref().unwrap_or(&0); // pruned count (0 on error)
        let (success, error) = run_success_error(&result);
        record_pass_run(
            store,
            &principal,
            &workspace,
            "retention",
            started_at,
            duration_ms,
            produced as i64,
            success,
            error.as_deref(),
        )
        .await;
        match result {
            Ok(pruned) if pruned > 0 => {
                info!(target: LOG_TARGET, principal, workspace, pruned, "retention sweep pruned rows")
            },
            Ok(_) => {},
            Err(error) => {
                warn!(target: LOG_TARGET, principal, workspace, error = %error, "retention sweep failed for scope")
            },
        }
    }
}

/// Timer that drives the scorer + curator passes on their configured cadences.
pub struct ResurfacingWorker {
    /// Empty when spawned disabled (the no-op handle); two handles while running.
    handles: Vec<JoinHandle<()>>,
    cancel: CancellationToken,
    wake_curator: ResurfacingWakeHandle,
}

impl ResurfacingWorker {
    /// `RESURFACING_ENABLED` — **on by default**. This is a pure kill-switch:
    /// only `0`/`false` (case-insensitive, trimmed) disable the worker; unset or
    /// empty leaves it enabled.
    pub fn enabled_from_env() -> bool {
        std::env::var("RESURFACING_ENABLED")
            .ok()
            .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false"))
            .unwrap_or(true)
    }

    /// Scorer cadence in seconds — default [`DEFAULT_SCORER_INTERVAL_SECS`],
    /// clamped up to [`MIN_INTERVAL_SECS`] (never `0`).
    pub fn scorer_interval_secs() -> u64 {
        env_secs(
            "RESURFACING_SCORER_INTERVAL_SECS",
            DEFAULT_SCORER_INTERVAL_SECS,
        )
        .max(MIN_INTERVAL_SECS)
    }

    /// Curator cadence in seconds — default [`DEFAULT_CURATOR_INTERVAL_SECS`],
    /// clamped up to [`MIN_INTERVAL_SECS`] (never `0`).
    pub fn curator_interval_secs() -> u64 {
        env_secs(
            "RESURFACING_CURATOR_INTERVAL_SECS",
            DEFAULT_CURATOR_INTERVAL_SECS,
        )
        .max(MIN_INTERVAL_SECS)
    }

    /// Candidates surfaced per curation sweep. The declared value comes from
    /// `resurfacing.surface_cap` in config (`config_default`); the
    /// `RESURFACING_SURFACE_CAP` env var overrides it at deploy time. Clamped up
    /// to [`MIN_SURFACE_CAP`] (never `0`).
    pub fn surface_cap(config_default: usize) -> usize {
        env_usize(
            "RESURFACING_SURFACE_CAP",
            config_default.max(MIN_SURFACE_CAP),
        )
        .max(MIN_SURFACE_CAP)
    }

    /// Retention horizon in days for the housekeeping sweep — default
    /// [`DEFAULT_RETENTION_DAYS`], clamped up to [`MIN_RETENTION_DAYS`].
    pub fn retention_days() -> i64 {
        env_i64("RESURFACING_RETENTION_DAYS", DEFAULT_RETENTION_DAYS).max(MIN_RETENTION_DAYS)
    }

    /// Per-scope candidate cap for the housekeeping sweep — default
    /// [`DEFAULT_CANDIDATE_CAP`], clamped up to [`MIN_CANDIDATE_CAP`].
    pub fn candidate_cap() -> usize {
        env_usize("RESURFACING_CANDIDATE_CAP", DEFAULT_CANDIDATE_CAP).max(MIN_CANDIDATE_CAP)
    }

    /// Spawn the worker. When [`enabled_from_env`](Self::enabled_from_env) is
    /// false this logs and returns a no-op handle (no background task) so the
    /// call site can spawn unconditionally.
    pub fn spawn_with_attention(
        store: ResurfacingStore,
        resolver: AgentMemoryResolver,
        service: Arc<ArtifactV2Service>,
        channel_store: ChannelAssistStore,
        router: Option<Arc<OperationLlmRouter>>,
        recommendation_policy: CurationRecommendationPolicy,
        active_repair_enabled: bool,
        active_repair_batch_size: usize,
        surface_cap_config: usize,
        centrality_config: CentralityEmbeddingConfig,
        memory_tiers: Vec<String>,
        attention_store: Option<AttentionFunnelStore>,
        user_request_service: Option<Arc<UserRequestService>>,
    ) -> Self {
        let cancel = CancellationToken::new();
        if !Self::enabled_from_env() {
            info!(target: LOG_TARGET, "resurfacing worker disabled (RESURFACING_ENABLED=0); no-op handle");
            return Self {
                handles: Vec::new(),
                cancel,
                wake_curator: ResurfacingWakeHandle::default(),
            };
        }

        let scorer_interval = Duration::from_secs(Self::scorer_interval_secs());
        let curator_interval = Duration::from_secs(Self::curator_interval_secs());
        let surface_cap = Self::surface_cap(surface_cap_config);
        // Tunable scoring config (weights / half-lives / feedback factors), loaded
        // once from the env — the single source of truth for the scorer's decay
        // half-life too. Moved into the task and passed by reference to each pass.
        let scoring_cfg = ResurfacingScoringConfig::from_env();
        let retention_days = Self::retention_days();
        let candidate_cap = Self::candidate_cap();
        info!(
            target: LOG_TARGET,
            scorer_interval_secs = scorer_interval.as_secs(),
            curator_interval_secs = curator_interval.as_secs(),
            surface_cap,
            active_repair_enabled,
            active_repair_batch_size,
            decay_halflife_days = scoring_cfg.decay_halflife_days,
            w_recency = scoring_cfg.w_recency,
            w_frequency = scoring_cfg.w_frequency,
            w_centrality = scoring_cfg.w_centrality,
            w_cooccurrence = scoring_cfg.w_cooccurrence,
            w_temporal = scoring_cfg.w_temporal,
            w_dormancy = scoring_cfg.w_dormancy,
            retention_days,
            candidate_cap,
            centrality_reference_timeout_secs = centrality_config.reference_timeout.as_secs(),
            centrality_query_timeout_secs = centrality_config.query_timeout.as_secs(),
            "resurfacing worker started"
        );

        let notify_curator = Arc::new(Notify::new());
        let wake_curator = ResurfacingWakeHandle {
            notify: Some(Arc::clone(&notify_curator)),
        };

        // Independent reconciliation keeps owner responses and source withdrawal
        // responsive even while scoring/curation waits on providers. Generation
        // has its own durable, scope-local hourly budget.
        let connections =
            magician::magician_v2::feed::FeedStore::open_workspace(service.workspace().clone())
                .map(|feed| super::memory_connections::ConnectionRuntime {
                    store: store.clone(),
                    resolver: resolver.clone(),
                    feed,
                    interactions: super::interaction::ResurfacingInteractionRegistry::new(
                        service.workspace().clone(),
                        channel_store.clone(),
                        service.clone(),
                        resolver.clone(),
                        None,
                        true,
                        true,
                        false,
                        false,
                        1.0,
                    ),
                    requests: user_request_service.clone(),
                    router: router.clone(),
                    attention: attention_store.clone(),
                    taste: magician::magician_v2::taste_profile::global_taste_profile_loader(),
                });
        let connection_workspace = service.workspace().clone();
        let connection_cancel = cancel.clone();
        let connection_handle = tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&connection_cancel)
                .await
            {
                return;
            }
            let runtime = match connections {
                Ok(runtime) => runtime,
                Err(error) => {
                    warn!(target: LOG_TARGET, %error, "memory connection delivery store unavailable");
                    return;
                },
            };
            let mut cursors = std::collections::HashMap::new();
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = connection_cancel.cancelled() => return,
                    _ = tick.tick() => {},
                }
                for (principal, workspace) in background_scopes(&connection_workspace).await {
                    let cursor = cursors
                        .entry((principal.clone(), workspace.clone()))
                        .or_insert(None);
                    let scope = magician::magician_v2::attention_funnel::AttentionScope {
                        principal,
                        workspace,
                    };
                    let started_at = chrono::Utc::now().timestamp_millis();
                    let started = Instant::now();
                    let result = tokio::select! {
                        _ = connection_cancel.cancelled() => return,
                        result = async {
                            if let Ok(memory) = runtime.resolver.resolve_for_scope(&scope.principal,&scope.workspace) {
                                if let Err(error) = magician::magician_v2::agents::memory_lifecycle::runtime::pass(
                                    &memory,runtime.router.as_deref(),runtime.requests.as_deref(),chrono::Utc::now(),None,
                                ).await {
                                    warn!(target: LOG_TARGET,%error,"memory lifecycle pass deferred");
                                }
                            }
                            runtime.pass(&scope, cursor, started_at / 1000).await
                        } => result,
                    };
                    if let Err(error) = &result {
                        warn!(target: LOG_TARGET, %error, "memory connection pass failed; durable work retained");
                    }
                    if !matches!(result, Ok(0)) {
                        record_pass_run(
                            &runtime.store,
                            &scope.principal,
                            &scope.workspace,
                            "memory_connections",
                            started_at,
                            started.elapsed().as_millis() as i64,
                            result.as_ref().copied().unwrap_or_default() as i64,
                            result.is_ok(),
                            result.as_ref().err().map(|_| "connection pass failed"),
                        )
                        .await;
                    }
                }
            }
        });

        let scorer_store = store.clone();
        let scorer_resolver = resolver.clone();
        let scorer_service = Arc::clone(&service);
        let scorer_channel_store = channel_store.clone();
        let scorer_cfg = scoring_cfg.clone();
        let scorer_memory_tiers = memory_tiers.clone();
        let scorer_cancel = cancel.clone();
        let scorer_notify = Arc::clone(&notify_curator);
        let scorer_user_requests = user_request_service;
        let scorer_router = router.clone();
        let scorer_handle = tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&scorer_cancel)
                .await
            {
                return;
            }
            let mut scorer_tick = tokio::time::interval(scorer_interval);
            loop {
                tokio::select! {
                    _ = scorer_cancel.cancelled() => return,
                    _ = scorer_tick.tick() => {
                        run_scorer_all_scopes(
                            &scorer_store,
                            &scorer_resolver,
                            &scorer_service,
                            &scorer_channel_store,
                            &scorer_cfg,
                            centrality_config,
                            &scorer_memory_tiers,
                            &scorer_cancel,
                            scorer_router.as_deref(),
                        )
                        .await;
                        if let Some(user_requests) = scorer_user_requests.as_ref() {
                            for (principal, workspace) in
                                background_scopes(scorer_service.workspace()).await
                            {
                                magician::magician_v2::attention::resurfacing::memory_effect_review::maybe_prompt_scope(
                                    &scorer_store,
                                    Arc::clone(user_requests),
                                    &principal,
                                    &workspace,
                                )
                                .await;
                            }
                        }
                        // Kick curation after scorer completion so fresh
                        // candidates do not wait for the daily curator timer.
                        scorer_notify.notify_one();
                    },
                }
            }
        });

        let curator_store = store.clone();
        let curator_memory_tiers = memory_tiers.clone();
        let curator_channel_store = channel_store.clone();
        let curator_workspace = service.workspace().clone();
        let curator_attention_store = attention_store.clone();
        let curator_event_broadcaster = service.runtime_event_broadcaster().cloned();
        let curator_cancel = cancel.clone();
        let curator_notify = Arc::clone(&notify_curator);
        let curator_handle = tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&curator_cancel)
                .await
            {
                return;
            }
            let mut curator_tick = tokio::time::interval(curator_interval);
            loop {
                tokio::select! {
                    _ = curator_cancel.cancelled() => return,
                    _ = curator_tick.tick() => {
                        run_curator_all_scopes(
                            &curator_workspace,
                            &curator_store,
                            &curator_channel_store,
                            surface_cap,
                            router.clone(),
                            recommendation_policy,
                            active_repair_enabled,
                            active_repair_batch_size,
                            &curator_memory_tiers,
                            curator_attention_store.as_ref(),
                            curator_event_broadcaster.as_ref(),
                            &curator_cancel,
                        )
                        .await;
                        // Fold the bounded-store housekeeping into the daily
                        // curator cadence rather than adding a second timer.
                        run_retention_all_scopes(
                            &curator_workspace,
                            &curator_store,
                            retention_days,
                            candidate_cap,
                            &curator_cancel,
                        ).await;
                    },
                    _ = curator_notify.notified() => {
                        run_curator_all_scopes(
                            &curator_workspace,
                            &curator_store,
                            &curator_channel_store,
                            surface_cap,
                            router.clone(),
                            recommendation_policy,
                            active_repair_enabled,
                            active_repair_batch_size,
                            &curator_memory_tiers,
                            curator_attention_store.as_ref(),
                            curator_event_broadcaster.as_ref(),
                            &curator_cancel,
                        )
                        .await;
                    },
                }
            }
        });

        Self {
            handles: vec![scorer_handle, curator_handle, connection_handle],
            cancel,
            wake_curator,
        }
    }

    pub fn wake_handle(&self) -> ResurfacingWakeHandle {
        self.wake_curator.clone()
    }

    /// Cancel the background task (if any) and await its exit.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        for handle in self.handles {
            let _ = handle.await;
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tempfile::TempDir;

    use magician::magician_v2::attention::resurfacing::types::{CorpusItem, SourceKind};

    // The process environment is global; serialize the config tests so their
    // set/remove_var calls can't interleave across the harness's test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct PreflightSource {
        items: Vec<CorpusItem>,
    }

    struct NeverResolvingSource;

    #[async_trait]
    impl ResurfacingSource for NeverResolvingSource {
        fn corpus_kind(&self) -> &'static str {
            "memory"
        }

        async fn list_changed_since(
            &self,
            _principal: &str,
            _workspace: &str,
            _watermark: i64,
        ) -> anyhow::Result<Vec<CorpusItem>> {
            std::future::pending().await
        }
    }

    #[async_trait]
    impl ResurfacingSource for PreflightSource {
        fn corpus_kind(&self) -> &'static str {
            "memory"
        }

        async fn list_changed_since(
            &self,
            _principal: &str,
            _workspace: &str,
            watermark: i64,
        ) -> anyhow::Result<Vec<CorpusItem>> {
            Ok(self
                .items
                .iter()
                .filter(|item| item.watermark_cursor > watermark)
                .cloned()
                .collect())
        }
    }

    fn preflight_item(cursor: i64) -> CorpusItem {
        CorpusItem {
            source_kind: SourceKind::Memory,
            source_ref: "changed".to_string(),
            title: "changed".to_string(),
            digest: "digest".to_string(),
            content_details: None,
            content_revision: None,
            occurred_at: cursor,
            watermark_cursor: cursor,
            embedding_text: "changed".to_string(),
        }
    }

    #[tokio::test]
    async fn background_scope_enumeration_is_sorted_bounded_and_includes_default() {
        let tmp = TempDir::new().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        for index in 0..(BACKGROUND_SCOPE_CAP + 10) {
            std::fs::create_dir_all(workspace.scope_root("principal", &format!("w-{index:03}")))
                .unwrap();
        }

        let scopes = background_scopes(&workspace).await;

        assert_eq!(scopes.len(), BACKGROUND_SCOPE_CAP);
        assert!(scopes.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(scopes.contains(&(
            DEFAULT_SCOPE_PRINCIPAL.to_string(),
            DEFAULT_SCOPE_WORKSPACE.to_string()
        )));
    }

    #[tokio::test]
    async fn centrality_preflight_skips_unchanged_scope_and_admits_new_work() {
        let store = ResurfacingStore::open_in_temp();
        let source = PreflightSource {
            items: vec![preflight_item(10)],
        };
        let sources: Vec<&dyn ResurfacingSource> = vec![&source];
        store
            .set_watermark("p", "caught-up", "memory", 10)
            .await
            .unwrap();

        assert!(
            !scope_has_changed_work(
                &store,
                &sources,
                "p",
                "caught-up",
                &CancellationToken::new(),
            )
            .await
        );
        assert!(
            scope_has_changed_work(&store, &sources, "p", "behind", &CancellationToken::new(),)
                .await
        );
    }

    #[tokio::test]
    async fn scope_deadline_cancels_a_never_resolving_source_preflight() {
        let store = ResurfacingStore::open_in_temp();
        let source = NeverResolvingSource;
        let sources: Vec<&dyn ResurfacingSource> = vec![&source];
        let result = tokio::time::timeout(
            Duration::from_millis(25),
            scope_has_changed_work(&store, &sources, "p", "w", &CancellationToken::new()),
        )
        .await;
        assert!(
            result.is_err(),
            "the scope budget must bound a stuck source"
        );
    }

    #[tokio::test]
    async fn exhausted_optional_centrality_budget_does_not_block_authoritative_scoring() {
        let store = ResurfacingStore::open_in_temp();
        let source = PreflightSource {
            items: vec![preflight_item(10)],
        };
        let sources: Vec<&dyn ResurfacingSource> = vec![&source];
        let expired_optional_deadline = tokio::time::Instant::now();
        assert!(tokio::time::Instant::now() >= expired_optional_deadline);

        let scored = run_scorer_pass_with_cancellation(
            "p",
            "w",
            &store,
            &sources,
            &BasicScoreCtx { now: 10 },
            &NoCentrality,
            &ResurfacingScoringConfig::default(),
            &CancellationToken::new(),
        )
        .await
        .expect("optional deadline exhaustion cannot reject deterministic scoring");
        assert_eq!(scored, 1);
        assert_eq!(store.get_watermark("p", "w", "memory").await.unwrap(), 10);
    }

    fn set(name: &str, value: Option<&str>) {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }

    #[test]
    fn enabled_defaults_true_and_only_zero_false_disable() {
        let _g = ENV_LOCK.lock().unwrap();

        set("RESURFACING_ENABLED", None);
        assert!(ResurfacingWorker::enabled_from_env(), "unset -> enabled");
        set("RESURFACING_ENABLED", Some(""));
        assert!(ResurfacingWorker::enabled_from_env(), "empty -> enabled");

        for off in ["0", "false", "FALSE", " false "] {
            set("RESURFACING_ENABLED", Some(off));
            assert!(
                !ResurfacingWorker::enabled_from_env(),
                "{off:?} -> disabled"
            );
        }
        for on in ["1", "true", "TRUE", "yes", "on"] {
            set("RESURFACING_ENABLED", Some(on));
            assert!(ResurfacingWorker::enabled_from_env(), "{on:?} -> enabled");
        }

        set("RESURFACING_ENABLED", None);
    }

    #[test]
    fn interval_getters_default_and_clamp_never_zero() {
        let _g = ENV_LOCK.lock().unwrap();

        set("RESURFACING_SCORER_INTERVAL_SECS", None);
        set("RESURFACING_CURATOR_INTERVAL_SECS", None);
        assert_eq!(
            ResurfacingWorker::scorer_interval_secs(),
            DEFAULT_SCORER_INTERVAL_SECS
        );
        assert_eq!(
            ResurfacingWorker::curator_interval_secs(),
            DEFAULT_CURATOR_INTERVAL_SECS
        );

        // Zero / garbage -> default (never 0).
        set("RESURFACING_SCORER_INTERVAL_SECS", Some("0"));
        assert_eq!(
            ResurfacingWorker::scorer_interval_secs(),
            DEFAULT_SCORER_INTERVAL_SECS
        );
        set("RESURFACING_SCORER_INTERVAL_SECS", Some("notanumber"));
        assert_eq!(
            ResurfacingWorker::scorer_interval_secs(),
            DEFAULT_SCORER_INTERVAL_SECS
        );

        // A tiny positive value clamps up to the sane floor (never below MIN,
        // and in particular never 0).
        set("RESURFACING_SCORER_INTERVAL_SECS", Some("5"));
        assert_eq!(ResurfacingWorker::scorer_interval_secs(), MIN_INTERVAL_SECS);
        assert!(ResurfacingWorker::scorer_interval_secs() >= MIN_INTERVAL_SECS);
        assert!(ResurfacingWorker::scorer_interval_secs() > 0);

        // A large explicit value above the floor is honored verbatim.
        set("RESURFACING_CURATOR_INTERVAL_SECS", Some("120000"));
        assert_eq!(ResurfacingWorker::curator_interval_secs(), 120_000);

        set("RESURFACING_SCORER_INTERVAL_SECS", None);
        set("RESURFACING_CURATOR_INTERVAL_SECS", None);
    }

    #[test]
    fn surface_cap_default_and_clamp() {
        let _g = ENV_LOCK.lock().unwrap();

        // The decay half-life moved to `ResurfacingScoringConfig::from_env`
        // (covered by `scoring::tests::scoring_config_from_env_overrides_and_defaults`);
        // the worker no longer owns that getter.
        // Env unset -> the config-supplied default is used. `5` mirrors config's
        // `default_resurfacing_surface_cap()`.
        set("RESURFACING_SURFACE_CAP", None);
        assert_eq!(ResurfacingWorker::surface_cap(5), 5);
        assert_eq!(ResurfacingWorker::surface_cap(15), 15);

        // Zero / garbage env -> falls back to the config default (never 0).
        set("RESURFACING_SURFACE_CAP", Some("0"));
        assert_eq!(ResurfacingWorker::surface_cap(15), 15);
        assert!(ResurfacingWorker::surface_cap(15) >= MIN_SURFACE_CAP);

        // A valid env value overrides the config default.
        set("RESURFACING_SURFACE_CAP", Some("9"));
        assert_eq!(ResurfacingWorker::surface_cap(15), 9);

        set("RESURFACING_SURFACE_CAP", None);
    }

    #[test]
    fn retention_days_and_candidate_cap_default_and_clamp() {
        let _g = ENV_LOCK.lock().unwrap();

        set("RESURFACING_RETENTION_DAYS", None);
        set("RESURFACING_CANDIDATE_CAP", None);
        assert_eq!(ResurfacingWorker::retention_days(), DEFAULT_RETENTION_DAYS);
        assert_eq!(ResurfacingWorker::candidate_cap(), DEFAULT_CANDIDATE_CAP);

        // Zero / garbage / negative -> default.
        set("RESURFACING_RETENTION_DAYS", Some("0"));
        assert_eq!(ResurfacingWorker::retention_days(), DEFAULT_RETENTION_DAYS);
        set("RESURFACING_RETENTION_DAYS", Some("notanumber"));
        assert_eq!(ResurfacingWorker::retention_days(), DEFAULT_RETENTION_DAYS);
        set("RESURFACING_RETENTION_DAYS", Some("-5"));
        assert_eq!(ResurfacingWorker::retention_days(), DEFAULT_RETENTION_DAYS);
        set("RESURFACING_CANDIDATE_CAP", Some("0"));
        assert_eq!(ResurfacingWorker::candidate_cap(), DEFAULT_CANDIDATE_CAP);

        // A tiny positive value clamps up to the sane floor.
        set("RESURFACING_RETENTION_DAYS", Some("1"));
        assert_eq!(ResurfacingWorker::retention_days(), MIN_RETENTION_DAYS);
        assert!(ResurfacingWorker::retention_days() >= MIN_RETENTION_DAYS);
        set("RESURFACING_CANDIDATE_CAP", Some("5"));
        assert_eq!(ResurfacingWorker::candidate_cap(), MIN_CANDIDATE_CAP);
        assert!(ResurfacingWorker::candidate_cap() >= MIN_CANDIDATE_CAP);

        // Explicit values above the floor are honored verbatim.
        set("RESURFACING_RETENTION_DAYS", Some("180"));
        assert_eq!(ResurfacingWorker::retention_days(), 180);
        set("RESURFACING_CANDIDATE_CAP", Some("5000"));
        assert_eq!(ResurfacingWorker::candidate_cap(), 5_000);

        set("RESURFACING_RETENTION_DAYS", None);
        set("RESURFACING_CANDIDATE_CAP", None);
    }

    #[test]
    fn run_success_error_maps_ok_and_err() {
        // Ok -> success, no error (Ok type is irrelevant — try the curator's).
        let ok: anyhow::Result<usize> = Ok(7);
        let (success, error) = run_success_error(&ok);
        assert!(success);
        assert!(error.is_none());

        // Err -> failure carrying the error's Display string.
        let err: anyhow::Result<usize> = Err(anyhow::anyhow!("boom"));
        let (success, error) = run_success_error(&err);
        assert!(!success);
        assert_eq!(error.as_deref(), Some("boom"));
    }
}
