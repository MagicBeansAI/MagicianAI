//! Embedding-backed centrality provider for the Proactive Resurfacing Engine.
//!
//! Centrality — the "you keep circling this" signal — asks: how densely does the
//! rest of the scope's salient context cluster around a given item? An item that
//! sits in a tight neighborhood of related memory/tasks/comms is more worth
//! resurfacing than an isolated one.
//!
//! **The async/sync bridge.** The scorer's
//! [`ScoreCtx::neighbor_density`](super::scoring::ScoreCtx) is SYNC and called
//! inside the sync `score_item`, but embedding a text is async and (relatively)
//! expensive. So the embed/query cannot happen live inside `neighbor_density`.
//! Instead [`run_scorer_pass`](super::scorer::run_scorer_pass) resolves each
//! item's centrality UP FRONT through the async [`CentralityProvider`], then
//! feeds the already-computed value into `score_item` via a precomputed ctx.
//! Scoring itself stays synchronous and the [`ScoreCtx`] trait is untouched.
//!
//! [`EmbeddingCentrality`] reuses the runtime's shared Ollama embedder
//! (the configured model via `magician-vector-index`'s `OllamaEmbedder`) plus a
//! pre-embedded in-memory reference set (the scope's recent corpus). The
//! reference is embedded ONCE at construction; each per-item query then embeds
//! only the query text and scores it against the cached reference vectors — no
//! per-call re-embedding of the corpus, and no persistent vector index to stand
//! up. This mirrors the stateless embed-then-cosine technique
//! `vector_toolkit::VectorTable::rank` already uses.
//!
//! [`NoCentrality`] is the deterministic no-op (always `None`) used by the
//! Phase-1 path and every existing test, so anything that does not opt in keeps
//! a `0.0` centrality contribution exactly as before.

pub use crate::magician_v2::resurfacing_seam::{cosine, normalize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use magician_vector_index::OllamaEmbedder;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const LOG_TARGET: &str = "resurfacing::centrality";

/// How many nearest neighbors define the "neighborhood" whose mean similarity
/// becomes the density. Deliberately small so the signal reflects a tight
/// cluster ("circling the same few things") rather than diffuse global
/// similarity.
const DEFAULT_TOP_K: usize = 8;

/// Cosine similarity at/above which a reference vector is treated as the item's
/// OWN embedding and excluded, so an item never counts itself as a neighbor.
/// (L2-normalized embeddings of identical text score ~1.0.)
const SELF_MATCH_CEIL: f32 = 0.9995;

/// Hard per-scope/pass cap on cache-miss query embeddings. Reference-corpus
/// hits do not consume this budget. Centrality is an optional scoring signal,
/// so after the cap it degrades to `None` while the deterministic scorer still
/// processes every item and advances its durable watermark.
const MAX_PROVIDER_QUERY_EMBEDDINGS_PER_PASS: usize = 64;
/// Maximum NEW reference embeddings per scope/pass. All 512 logical references
/// remain eligible; digest-matched durable vectors are reused and the deferred
/// miss suffix becomes the next pass's prefix after newly generated vectors are
/// persisted.
pub const MAX_NEW_REFERENCE_EMBEDDINGS_PER_PASS: usize = 128;

/// Bounded embedding budgets for optional resurfacing centrality. Reference
/// construction gets a larger total budget because it includes cold model load
/// and several bounded batches; each item query remains independently bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CentralityEmbeddingConfig {
    pub reference_timeout: Duration,
    pub query_timeout: Duration,
}

impl CentralityEmbeddingConfig {
    /// Build timeout policy. The third argument remains only for backward
    /// compatibility with shipped configuration; reference work now commits
    /// one logical input at a time so a later stall cannot erase a completed
    /// durable prefix.
    pub fn from_seconds(
        reference_timeout_secs: u64,
        query_timeout_secs: u64,
        _legacy_reference_batch_size: usize,
    ) -> Self {
        Self {
            reference_timeout: Duration::from_secs(reference_timeout_secs.max(1)),
            query_timeout: Duration::from_secs(query_timeout_secs.max(1)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CentralityReference {
    pub candidate_id: String,
    pub content_digest: String,
    pub embedding_text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReusableCentralityEmbedding {
    pub candidate_id: String,
    pub content_digest: String,
    pub embedding_contract: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewCentralityEmbedding {
    pub candidate_id: String,
    pub content_digest: String,
    pub embedding_contract: String,
    pub embedding: Vec<f32>,
}

pub struct EmbeddingCentralityBuild {
    pub provider: EmbeddingCentrality,
    pub newly_embedded: Vec<NewCentralityEmbedding>,
    pub logical_references: usize,
    pub covered_references: usize,
    pub deferred_references: usize,
}

/// The per-item centrality signal PLUS the item's own embedding vector.
///
/// [`EmbeddingCentrality::evaluate`] embeds the item's text ONCE to compute the
/// neighborhood `density`; that same vector is surfaced here as `embedding` so
/// the scorer can persist it (via the store) for dismiss neighbor-suppression
/// without a second embed call. `density` keeps the identical `[0,1]` meaning
/// the old `centrality()` returned.
pub struct CentralityOutcome {
    /// Neighborhood density in `[0,1]` — "how much other salient context
    /// clusters around this item".
    pub density: f32,
    /// The item's raw (un-normalized) embedding vector, ready to persist.
    pub embedding: Vec<f32>,
    /// Exact persisted-embedding compatibility contract. `None` is reserved
    /// for deterministic/test providers that do not produce Ollama vectors.
    pub embedding_contract: Option<String>,
}

/// Async source of the embedding-centrality signal, kept separate from the sync
/// [`ScoreCtx`](super::scoring::ScoreCtx) so scoring stays synchronous: the
/// scorer calls this UP FRONT per item and threads the result into a precomputed
/// ctx.
#[async_trait]
pub trait CentralityProvider: Send + Sync {
    /// Semantic-space identity for any embeddings returned by this provider.
    /// Feedback vectors from a different or legacy-unknown space must remain
    /// neutral rather than participate in cosine comparisons.
    fn embedding_contract(&self) -> Option<&str> {
        None
    }

    fn embedding_dimensions(&self) -> Option<usize> {
        None
    }

    /// The neighborhood [`CentralityOutcome`] (density in `[0,1]` + the item's
    /// own embedding) for `embedding_text`, or `None` when no signal is
    /// available (embedder unavailable, empty reference, embed error, blank
    /// text, or no neighbor to form a density from). `None` degrades to a `0.0`
    /// centrality contribution via the scorer's graceful path — it must never
    /// abort a scoring pass — and simply persists no embedding for that item.
    async fn evaluate(&self, embedding_text: &str) -> Option<CentralityOutcome>;
}

/// Deterministic no-op provider: always `None`. The Phase-1 default and what
/// every non-opted-in caller/test uses, so their centrality stays exactly
/// `0.0` — identical to the pre-existing degraded behavior.
pub struct NoCentrality;

#[async_trait]
impl CentralityProvider for NoCentrality {
    async fn evaluate(&self, _embedding_text: &str) -> Option<CentralityOutcome> {
        None
    }
}

/// Embedding-backed centrality over a pre-embedded reference corpus.
///
/// Construction embeds the reference texts ONCE in bounded batches. Each
/// [`evaluate`](CentralityProvider::evaluate) then embeds only the query text
/// and scores it against the cached reference vectors in memory — no per-call
/// corpus re-embedding, no persistent index.
pub struct EmbeddingCentrality {
    embedder: OllamaEmbedder,
    /// L2-normalized reference vectors (the scope's recent corpus).
    reference: Vec<Vec<f32>>,
    /// Exact trimmed reference text -> raw embedding. Scorer items normally
    /// come from this same corpus, so this avoids embedding them a second time.
    reference_by_text: HashMap<String, Vec<f32>>,
    top_k: usize,
    query_timeout: Duration,
    scope_deadline: tokio::time::Instant,
    embedding_contract: String,
    remaining_provider_queries: AtomicUsize,
    disabled: AtomicBool,
    cancel: Option<CancellationToken>,
}

impl EmbeddingCentrality {
    /// Build over the shared embedder and a reference corpus, embedding the
    /// reference once. Returns `None` when there is nothing to form a
    /// neighborhood from (all-blank/empty reference) or the reference embed
    /// fails (e.g. Ollama down) — the caller then falls back to
    /// [`NoCentrality`].
    pub async fn build(
        embedder: OllamaEmbedder,
        reference_texts: &[String],
        config: CentralityEmbeddingConfig,
        principal: &str,
        workspace: &str,
    ) -> Option<Self> {
        Self::build_inner(
            embedder,
            reference_texts,
            config,
            principal,
            workspace,
            None,
        )
        .await
    }

    /// Cancellable worker-facing constructor. Cancelling drops the in-flight
    /// HTTP future (and therefore its admission permit) instead of forcing
    /// shutdown to wait for the whole reference corpus.
    pub async fn build_with_cancellation(
        embedder: OllamaEmbedder,
        reference_texts: &[String],
        config: CentralityEmbeddingConfig,
        principal: &str,
        workspace: &str,
        cancel: CancellationToken,
    ) -> Option<Self> {
        Self::build_inner(
            embedder,
            reference_texts,
            config,
            principal,
            workspace,
            Some(cancel),
        )
        .await
    }

    /// Build from stable candidate identities with digest-validated durable
    /// reuse. Only cache misses consume provider work, bounded to
    /// [`MAX_NEW_REFERENCE_EMBEDDINGS_PER_PASS`]. The returned new vectors are
    /// persisted by the worker before scoring, so deferred misses make
    /// deterministic progress on subsequent passes without shrinking the
    /// 512-item logical corpus.
    pub async fn build_from_references_with_cancellation(
        embedder: OllamaEmbedder,
        references: &[CentralityReference],
        reusable: &[ReusableCentralityEmbedding],
        config: CentralityEmbeddingConfig,
        principal: &str,
        workspace: &str,
        cancel: CancellationToken,
    ) -> Option<EmbeddingCentralityBuild> {
        Self::build_references_inner(
            embedder,
            references,
            reusable,
            config,
            principal,
            workspace,
            MAX_NEW_REFERENCE_EMBEDDINGS_PER_PASS,
            Some(cancel),
            None,
        )
        .await
    }

    pub async fn build_from_references_with_deadline_and_cancellation(
        embedder: OllamaEmbedder,
        references: &[CentralityReference],
        reusable: &[ReusableCentralityEmbedding],
        config: CentralityEmbeddingConfig,
        principal: &str,
        workspace: &str,
        cancel: CancellationToken,
        scope_deadline: tokio::time::Instant,
    ) -> Option<EmbeddingCentralityBuild> {
        Self::build_references_inner(
            embedder,
            references,
            reusable,
            config,
            principal,
            workspace,
            MAX_NEW_REFERENCE_EMBEDDINGS_PER_PASS,
            Some(cancel),
            Some(scope_deadline),
        )
        .await
    }

    async fn build_inner(
        embedder: OllamaEmbedder,
        reference_texts: &[String],
        config: CentralityEmbeddingConfig,
        principal: &str,
        workspace: &str,
        cancel: Option<CancellationToken>,
    ) -> Option<Self> {
        let references: Vec<CentralityReference> = reference_texts
            .iter()
            .enumerate()
            .map(|(index, text)| CentralityReference {
                candidate_id: format!("legacy-reference-{index}"),
                content_digest: blake3::hash(text.as_bytes()).to_hex().to_string(),
                embedding_text: text.clone(),
            })
            .collect();
        Self::build_references_inner(
            embedder,
            &references,
            &[],
            config,
            principal,
            workspace,
            usize::MAX,
            cancel,
            None,
        )
        .await
        .map(|build| build.provider)
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_references_inner(
        embedder: OllamaEmbedder,
        references: &[CentralityReference],
        reusable: &[ReusableCentralityEmbedding],
        config: CentralityEmbeddingConfig,
        principal: &str,
        workspace: &str,
        max_new_references: usize,
        cancel: Option<CancellationToken>,
        inherited_scope_deadline: Option<tokio::time::Instant>,
    ) -> Option<EmbeddingCentralityBuild> {
        // One absolute budget covers reference recovery, new reference work,
        // and every later cache-miss item query for this scope/pass.
        let scope_deadline = inherited_scope_deadline
            .unwrap_or_else(|| tokio::time::Instant::now() + config.reference_timeout);
        let embedding_contract = embedder.embedding_contract_id();
        let mut seen_ids = HashSet::new();
        let mut seen_texts = HashSet::new();
        let references: Vec<CentralityReference> = references
            .iter()
            .filter_map(|reference| {
                let text = reference.embedding_text.trim();
                (!text.is_empty()
                    && seen_ids.insert(reference.candidate_id.clone())
                    && seen_texts.insert(text.to_string()))
                .then(|| {
                    let mut reference = reference.clone();
                    reference.embedding_text = text.to_string();
                    reference
                })
            })
            .collect();
        if references.is_empty() {
            return None;
        }

        let reusable: HashMap<(&str, &str, &str), &ReusableCentralityEmbedding> = reusable
            .iter()
            .map(|embedding| {
                (
                    (
                        embedding.candidate_id.as_str(),
                        embedding.content_digest.as_str(),
                        embedding.embedding_contract.as_str(),
                    ),
                    embedding,
                )
            })
            .collect();
        let mut raw_by_id: HashMap<String, Vec<f32>> = HashMap::new();
        let mut misses = Vec::new();
        for reference in &references {
            match reusable.get(&(
                reference.candidate_id.as_str(),
                reference.content_digest.as_str(),
                embedding_contract.as_str(),
            )) {
                Some(cached)
                    if cached.content_digest == reference.content_digest
                        && cached.embedding_contract == embedding_contract
                        && valid_reusable_embedding(&cached.embedding, embedder.config().dims) =>
                {
                    raw_by_id.insert(reference.candidate_id.clone(), cached.embedding.clone());
                },
                _ => misses.push(reference.clone()),
            }
        }
        let reused_count = raw_by_id.len();
        let selected: Vec<CentralityReference> =
            misses.iter().take(max_new_references).cloned().collect();
        let mut newly_embedded = Vec::new();

        // Record each embed batch into the separate `llm_embeddings` dataset.
        // Best-effort/non-blocking: recording cannot affect the embed. The
        // total deadline is shared across batches; cancellation or timeout
        // returns the already-covered subset instead of discarding useful
        // durable vectors.
        let embed_model = embedder.config().model.clone();
        'references: for reference in &selected {
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                break 'references;
            }
            let remaining = scope_deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break 'references;
            }
            // A successful input is a persistence unit. Never put several
            // logical references behind one outer timeout: if a later HTTP
            // call stalls, the completed prefix still advances durably.
            let texts = vec![reference.embedding_text.clone()];
            let started = std::time::Instant::now();
            let embed = timeout(remaining, embedder.embed_documents_background(&texts));
            let result = if let Some(cancel) = cancel.as_ref() {
                tokio::select! {
                biased;
                    _ = cancel.cancelled() => break 'references,
                    result = embed => result,
                }
            } else {
                embed.await
            };
            let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let succeeded = matches!(&result, Ok(Ok(_)));
            crate::magician_v2::analytics::llm_embeddings_sink::record_embedding_batch(
                principal,
                workspace,
                &embed_model,
                "resurfacing",
                1,
                crate::magician_v2::analytics::llm_embeddings_sink::estimate_input_tokens(&texts),
                latency_ms,
                succeeded,
            );
            match result {
                Ok(Ok(vectors)) => {
                    for embedding in vectors {
                        raw_by_id.insert(reference.candidate_id.clone(), embedding.clone());
                        newly_embedded.push(NewCentralityEmbedding {
                            candidate_id: reference.candidate_id.clone(),
                            content_digest: reference.content_digest.clone(),
                            embedding_contract: embedding_contract.clone(),
                            embedding,
                        });
                    }
                },
                Ok(Err(error)) => {
                    debug!(target: LOG_TARGET, error = %error, "reference embedding batch failed; using durable partial coverage");
                    break 'references;
                },
                Err(_) => {
                    warn!(
                        target: LOG_TARGET,
                        timeout_ms = config.reference_timeout.as_millis() as u64,
                        logical_reference_count = references.len(),
                        covered_reference_count = raw_by_id.len(),
                        "reference embedding budget elapsed; using durable partial coverage"
                    );
                    break 'references;
                },
            }
        }

        let reference_by_text: HashMap<String, Vec<f32>> = references
            .iter()
            .filter_map(|reference| {
                raw_by_id
                    .get(&reference.candidate_id)
                    .cloned()
                    .map(|embedding| (reference.embedding_text.clone(), embedding))
            })
            .collect();
        let reference: Vec<Vec<f32>> = references
            .iter()
            .filter_map(|reference| raw_by_id.get(&reference.candidate_id).cloned())
            .map(normalize)
            .collect();
        if reference.is_empty() {
            return None;
        }
        let logical_references = references.len();
        let covered_references = reference.len();
        let deferred_references = logical_references.saturating_sub(covered_references);
        if deferred_references > 0 {
            info!(
                target: LOG_TARGET,
                logical_references,
                reused_references = reused_count,
                newly_embedded_references = newly_embedded.len(),
                covered_references,
                deferred_references,
                "resurfacing centrality is using partial durable reference coverage; deferred misses remain eligible next pass"
            );
        } else {
            debug!(
                target: LOG_TARGET,
                logical_references,
                reused_references = reused_count,
                newly_embedded_references = newly_embedded.len(),
                covered_references,
                "resurfacing centrality reference coverage complete"
            );
        }
        Some(EmbeddingCentralityBuild {
            provider: Self {
                embedder,
                reference,
                reference_by_text,
                top_k: DEFAULT_TOP_K,
                query_timeout: config.query_timeout,
                scope_deadline,
                embedding_contract,
                remaining_provider_queries: AtomicUsize::new(
                    MAX_PROVIDER_QUERY_EMBEDDINGS_PER_PASS,
                ),
                disabled: AtomicBool::new(false),
                cancel,
            },
            newly_embedded,
            logical_references,
            covered_references,
            deferred_references,
        })
    }
}

#[async_trait]
impl CentralityProvider for EmbeddingCentrality {
    fn embedding_contract(&self) -> Option<&str> {
        Some(&self.embedding_contract)
    }

    fn embedding_dimensions(&self) -> Option<usize> {
        Some(self.embedder.config().dims)
    }

    async fn evaluate(&self, embedding_text: &str) -> Option<CentralityOutcome> {
        if self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return None;
        }
        let text = embedding_text.trim();
        if text.is_empty() {
            return None;
        }
        if tokio::time::Instant::now() >= self.scope_deadline {
            return None;
        }
        let embedded = if let Some(reference) = self.reference_by_text.get(text) {
            reference.clone()
        } else {
            if self.disabled.load(Ordering::Relaxed) {
                return None;
            }
            let remaining_scope = self
                .scope_deadline
                .saturating_duration_since(tokio::time::Instant::now());
            if remaining_scope.is_zero() {
                return None;
            }
            let reserved = self.remaining_provider_queries.fetch_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |remaining| remaining.checked_sub(1),
            );
            if reserved.is_err() {
                return None;
            }
            let query_budget = self.query_timeout.min(remaining_scope);
            let query = timeout(query_budget, self.embedder.embed_background_query(text));
            let query_result = if let Some(cancel) = self.cancel.as_ref() {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return None,
                    result = query => result,
                }
            } else {
                query.await
            };
            match query_result {
                Ok(Ok(embedded)) => embedded,
                Ok(Err(error)) => {
                    if !self.disabled.swap(true, Ordering::Relaxed) {
                        warn!(
                            target: LOG_TARGET,
                            error = %error,
                            "query embedding failed; centrality disabled for the rest of this pass"
                        );
                    }
                    return None;
                },
                Err(_) => {
                    if !self.disabled.swap(true, Ordering::Relaxed) {
                        warn!(
                            target: LOG_TARGET,
                            timeout_ms = query_budget.as_millis() as u64,
                            "query embedding timed out; centrality disabled for the rest of this pass"
                        );
                    }
                    return None;
                },
            }
        };
        // Keep the raw embedding to persist (for dismiss suppression); normalize
        // a copy for the density cosine below.
        let raw = embedded;
        let query = normalize(raw.clone());
        // Cosine of the (normalized) query against every reference vector,
        // dropping the item's own embedding (self-match) so it can't count
        // itself as a neighbor.
        let sims: Vec<f32> = self
            .reference
            .iter()
            .map(|r| cosine(&query, r))
            .filter(|s| *s < SELF_MATCH_CEIL)
            .collect();
        let density = density_from_similarities(&sims, self.top_k)?;
        Some(CentralityOutcome {
            density,
            embedding: raw,
            embedding_contract: Some(self.embedding_contract.clone()),
        })
    }
}

fn valid_reusable_embedding(vector: &[f32], expected_dims: usize) -> bool {
    if vector.len() != expected_dims {
        return false;
    }
    let norm_squared = vector.iter().try_fold(0.0_f64, |sum, value| {
        value
            .is_finite()
            .then_some(sum + f64::from(*value) * f64::from(*value))
    });
    norm_squared.is_some_and(|norm| norm.is_finite() && norm > f64::EPSILON)
}

/// Map a set of neighbor cosine similarities to a density in `[0,1]`: the mean
/// of the top-`k` NON-NEGATIVE similarities. A negative cosine means "unrelated"
/// and contributes nothing (clamped to `0.0`); more strong neighbors ⇒ a denser
/// neighborhood ⇒ a higher density. Returns `None` when there are no neighbors
/// to average (empty input or `k == 0`).
pub fn density_from_similarities(sims: &[f32], k: usize) -> Option<f32> {
    if sims.is_empty() || k == 0 {
        return None;
    }
    let mut clipped: Vec<f32> = sims.iter().map(|s| s.max(0.0)).collect();
    // Largest similarities first, so `take(k)` is the top-k neighborhood.
    clipped.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let take = k.min(clipped.len());
    let sum: f32 = clipped.iter().take(take).sum();
    let density = sum / take as f32;
    Some(density.clamp(0.0, 1.0))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::attention::resurfacing::store::{
        CandidateEmbeddingSnapshot, ResurfacingStore,
    };
    use magician_vector_index::{OllamaEmbedderConfig, EMBEDDING_ADMISSION_TEST_LOCK};
    use serde_json::{json, Value};
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    #[derive(Clone)]
    struct DeterministicEmbeddingResponder;

    impl Respond for DeterministicEmbeddingResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value =
                serde_json::from_slice(&request.body).expect("embedding request JSON");
            assert_eq!(body["truncate"], json!(false));
            let inputs = body["input"].as_array().expect("embedding input array");
            let embeddings: Vec<Vec<f32>> = inputs
                .iter()
                .map(|input| match input.as_str().unwrap_or_default() {
                    "alpha" => vec![1.0, 0.0],
                    "beta" => vec![0.0, 1.0],
                    _ => vec![0.6, 0.8],
                })
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({ "embeddings": embeddings }))
        }
    }

    #[derive(Clone)]
    struct FirstEmbeddingThenStall {
        calls: Arc<AtomicUsize>,
    }

    impl Respond for FirstEmbeddingThenStall {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value =
                serde_json::from_slice(&request.body).expect("embedding request JSON");
            assert_eq!(body["truncate"], json!(false));
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(200).set_body_json(json!({ "embeddings": [[1.0, 0.0]] }))
            } else {
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({ "embeddings": [[0.0, 1.0]] }))
            }
        }
    }

    async fn mock_embedder(server: &MockServer) -> OllamaEmbedder {
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(DeterministicEmbeddingResponder)
            .mount(server)
            .await;
        OllamaEmbedder::new(OllamaEmbedderConfig {
            base_url: server.uri(),
            model: "test-embed".to_string(),
            dims: 2,
            timeout: Duration::from_secs(2),
            query_timeout: Duration::from_secs(2),
            batch_size: 8,
            num_parallel: 1,
            batch_tokens: None,
            ..OllamaEmbedderConfig::default()
        })
    }

    #[test]
    fn density_is_mean_of_top_k_nonnegative_sims() {
        // Top-3 of [0.9, 0.1, 0.8, -0.2, 0.7] (relu'd) = [0.9, 0.8, 0.7] → 0.8.
        let d = density_from_similarities(&[0.9, 0.1, 0.8, -0.2, 0.7], 3).unwrap();
        assert!((d - 0.8).abs() < 1e-5, "expected ~0.8, got {d}");
    }

    #[test]
    fn density_relus_negatives_to_zero() {
        // Every neighbor is anti-correlated → nothing clusters here → 0.0.
        let d = density_from_similarities(&[-0.5, -0.9, -0.1], 3).unwrap();
        assert_eq!(d, 0.0);
    }

    #[test]
    fn density_k_larger_than_len_uses_all_neighbors() {
        let d = density_from_similarities(&[0.6, 0.4], 8).unwrap();
        assert!((d - 0.5).abs() < 1e-5, "expected ~0.5, got {d}");
    }

    #[test]
    fn density_is_clamped_to_unit_range() {
        // Similarities can nominally exceed 1 with un-normalized inputs; the
        // formula must never emit > 1.0.
        let d = density_from_similarities(&[1.4, 1.2], 2).unwrap();
        assert_eq!(d, 1.0);
    }

    #[test]
    fn density_empty_or_zero_k_is_none() {
        assert!(density_from_similarities(&[], 8).is_none());
        assert!(density_from_similarities(&[0.9], 0).is_none());
    }

    #[test]
    fn cosine_of_orthogonal_is_zero_and_identical_is_one() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        // Mismatched lengths degrade to 0.0, never panic.
        assert_eq!(cosine(&[1.0, 0.0], &[1.0]), 0.0);
    }

    #[tokio::test]
    async fn no_centrality_is_always_none() {
        assert!(NoCentrality.evaluate("anything at all").await.is_none());
        assert!(NoCentrality.evaluate("").await.is_none());
    }

    #[test]
    fn embedding_config_clamps_zero_budgets_and_accepts_legacy_batch_argument() {
        let config = CentralityEmbeddingConfig::from_seconds(0, 0, 0);
        assert_eq!(config.reference_timeout, Duration::from_secs(1));
        assert_eq!(config.query_timeout, Duration::from_secs(1));
    }

    #[tokio::test]
    async fn reference_hits_reuse_vectors_and_duplicate_text_is_embedded_once() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let embedder = mock_embedder(&server).await;
        let provider = EmbeddingCentrality::build(
            embedder,
            &[" alpha ".into(), "beta".into(), "alpha".into()],
            CentralityEmbeddingConfig::from_seconds(2, 2, 8),
            "test",
            "centrality-cache",
        )
        .await
        .expect("centrality provider");
        assert_eq!(server.received_requests().await.unwrap().len(), 2);

        let cached = provider.evaluate("alpha").await.expect("cached outcome");
        assert_eq!(cached.embedding, vec![1.0, 0.0]);
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "an exact reference hit must not issue a second provider request"
        );

        assert!(provider.evaluate("gamma").await.is_some());
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn concurrent_identical_oversized_queries_coalesce_in_both_arrival_orders() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(DeterministicEmbeddingResponder)
            .mount(&server)
            .await;
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            base_url: server.uri(),
            model: "test-embed".to_string(),
            dims: 2,
            timeout: Duration::from_secs(2),
            query_timeout: Duration::from_secs(2),
            batch_size: 8,
            num_parallel: 1,
            context_tokens: Some(72),
            batch_tokens: None,
            ..Default::default()
        });
        for (index, delayed_first) in [false, true].into_iter().enumerate() {
            let query = format!(
                "{} unique-{index}",
                "A long semantic query sentence. ".repeat(6)
            );
            let first_embedder = embedder.clone();
            let first_query = query.clone();
            let first = async move {
                if delayed_first {
                    tokio::task::yield_now().await;
                }
                first_embedder.embed_query(&first_query).await
            };
            let second_embedder = embedder.clone();
            let second_query = query.clone();
            let second = async move {
                if !delayed_first {
                    tokio::task::yield_now().await;
                }
                second_embedder.embed_query(&second_query).await
            };
            let (first, second) = tokio::join!(first, second);
            assert_eq!(first.unwrap(), second.unwrap());
        }
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "each unique oversized query makes one HTTP call; its sibling coalesces"
        );
    }

    #[tokio::test]
    async fn durable_reference_misses_progress_to_full_coverage_without_reembedding() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let references: Vec<CentralityReference> = ["alpha", "beta", "gamma", "delta", "epsilon"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| CentralityReference {
                candidate_id: format!("candidate-{index}"),
                content_digest: format!("digest-{index}"),
                embedding_text: text.to_string(),
            })
            .collect();
        let mut reusable = Vec::new();

        for (pass, expected_covered) in [2, 4, 5].into_iter().enumerate() {
            let embedder = mock_embedder(&server).await;
            let build = EmbeddingCentrality::build_references_inner(
                embedder,
                &references,
                &reusable,
                CentralityEmbeddingConfig::from_seconds(2, 2, 8),
                "test",
                "durable-progress",
                2,
                None,
                None,
            )
            .await
            .expect("partial centrality build");
            assert_eq!(build.logical_references, 5);
            assert_eq!(build.covered_references, expected_covered);
            assert_eq!(build.deferred_references, 5 - expected_covered);
            for embedded in &build.newly_embedded {
                let reference = references
                    .iter()
                    .find(|reference| reference.candidate_id == embedded.candidate_id)
                    .unwrap();
                reusable.push(ReusableCentralityEmbedding {
                    candidate_id: embedded.candidate_id.clone(),
                    content_digest: reference.content_digest.clone(),
                    embedding_contract: embedded.embedding_contract.clone(),
                    embedding: embedded.embedding.clone(),
                });
            }
            assert_eq!(
                server.received_requests().await.unwrap().len(),
                expected_covered,
                "pass {pass} must embed only the next durable miss suffix"
            );
        }
    }

    #[tokio::test]
    async fn successful_reference_before_stall_is_returned_as_durable_progress() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(FirstEmbeddingThenStall {
                calls: Arc::clone(&calls),
            })
            .mount(&server)
            .await;
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            base_url: server.uri(),
            model: "test-embed".to_string(),
            dims: 2,
            timeout: Duration::from_secs(5),
            query_timeout: Duration::from_secs(1),
            batch_size: 8,
            num_parallel: 1,
            context_tokens: None,
            batch_tokens: None,
            ..Default::default()
        });
        let references = vec![
            CentralityReference {
                candidate_id: "first".to_string(),
                content_digest: "d1".to_string(),
                embedding_text: "alpha".to_string(),
            },
            CentralityReference {
                candidate_id: "second".to_string(),
                content_digest: "d2".to_string(),
                embedding_text: "beta".to_string(),
            },
        ];
        let build = EmbeddingCentrality::build_references_inner(
            embedder,
            &references,
            &[],
            CentralityEmbeddingConfig {
                reference_timeout: Duration::from_millis(75),
                query_timeout: Duration::from_millis(50),
            },
            "test",
            "partial-prefix",
            2,
            None,
            None,
        )
        .await
        .expect("the successful prefix still forms a provider");
        assert_eq!(build.newly_embedded.len(), 1);
        assert_eq!(build.newly_embedded[0].candidate_id, "first");
        assert_eq!(build.covered_references, 1);
        assert_eq!(build.deferred_references, 1);
        let store = ResurfacingStore::open_in_temp();
        let durable = build
            .newly_embedded
            .iter()
            .map(|embedding| CandidateEmbeddingSnapshot {
                candidate_id: embedding.candidate_id.clone(),
                content_digest: embedding.content_digest.clone(),
                embedding_contract: embedding.embedding_contract.clone(),
                embedding: embedding.embedding.clone(),
            })
            .collect::<Vec<_>>();
        store
            .upsert_embeddings_for_digests("test", "partial-prefix", &durable)
            .await
            .unwrap();
        let reloaded = store
            .list_candidate_embedding_snapshots_for_ids(
                "test",
                "partial-prefix",
                &["first".to_string(), "second".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            reloaded, durable,
            "successful prefix survives exhausted deadline"
        );
    }

    #[tokio::test]
    async fn stale_digest_wrong_dimension_or_invalid_norm_is_never_reused() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let embedder = mock_embedder(&server).await;
        let current_contract = embedder.embedding_contract_id();
        let references = vec![
            CentralityReference {
                candidate_id: "stale".to_string(),
                content_digest: "digest-v2".to_string(),
                embedding_text: "alpha".to_string(),
            },
            CentralityReference {
                candidate_id: "wrong-dims".to_string(),
                content_digest: "digest-current".to_string(),
                embedding_text: "beta".to_string(),
            },
            CentralityReference {
                candidate_id: "zero".to_string(),
                content_digest: "digest-zero".to_string(),
                embedding_text: "gamma".to_string(),
            },
            CentralityReference {
                candidate_id: "nonfinite".to_string(),
                content_digest: "digest-nonfinite".to_string(),
                embedding_text: "delta".to_string(),
            },
        ];
        let reusable = vec![
            ReusableCentralityEmbedding {
                candidate_id: "stale".to_string(),
                content_digest: "digest-v1".to_string(),
                embedding_contract: current_contract.clone(),
                embedding: vec![9.0, 9.0],
            },
            ReusableCentralityEmbedding {
                candidate_id: "wrong-dims".to_string(),
                content_digest: "digest-current".to_string(),
                embedding_contract: current_contract.clone(),
                embedding: vec![9.0],
            },
            ReusableCentralityEmbedding {
                candidate_id: "zero".to_string(),
                content_digest: "digest-zero".to_string(),
                embedding_contract: current_contract.clone(),
                embedding: vec![0.0, 0.0],
            },
            ReusableCentralityEmbedding {
                candidate_id: "nonfinite".to_string(),
                content_digest: "digest-nonfinite".to_string(),
                embedding_contract: current_contract,
                embedding: vec![f32::INFINITY, 1.0],
            },
        ];

        let build = EmbeddingCentrality::build_references_inner(
            embedder,
            &references,
            &reusable,
            CentralityEmbeddingConfig::from_seconds(2, 2, 8),
            "test",
            "durable-invalidation",
            4,
            None,
            None,
        )
        .await
        .expect("rebuilt centrality");
        assert_eq!(build.newly_embedded.len(), 4);
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn exact_version_cache_hit_is_order_independent_for_same_candidate() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let embedder = mock_embedder(&server).await;
        let contract = embedder.embedding_contract_id();
        let reference = CentralityReference {
            candidate_id: "same-card".to_string(),
            content_digest: "digest-current".to_string(),
            embedding_text: "alpha".to_string(),
        };
        let stale = ReusableCentralityEmbedding {
            candidate_id: reference.candidate_id.clone(),
            content_digest: "digest-old".to_string(),
            embedding_contract: contract.clone(),
            embedding: vec![0.0, 1.0],
        };
        let current = ReusableCentralityEmbedding {
            candidate_id: reference.candidate_id.clone(),
            content_digest: reference.content_digest.clone(),
            embedding_contract: contract,
            embedding: vec![1.0, 0.0],
        };
        for reusable in [
            vec![stale.clone(), current.clone()],
            vec![current.clone(), stale.clone()],
        ] {
            let build = EmbeddingCentrality::build_references_inner(
                embedder.clone(),
                std::slice::from_ref(&reference),
                &reusable,
                CentralityEmbeddingConfig::from_seconds(2, 2, 8),
                "test",
                "version-order",
                1,
                None,
                None,
            )
            .await
            .expect("exact version should form provider");
            assert!(build.newly_embedded.is_empty());
            assert_eq!(build.covered_references, 1);
        }
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "neither cache ordering may trigger re-embedding"
        );
    }

    #[tokio::test]
    async fn expired_scope_provider_degrades_even_for_an_exact_reference_hit() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let embedder = mock_embedder(&server).await;
        let contract = embedder.embedding_contract_id();
        let reference = CentralityReference {
            candidate_id: "expired-card".to_string(),
            content_digest: "digest".to_string(),
            embedding_text: "alpha".to_string(),
        };
        let reusable = ReusableCentralityEmbedding {
            candidate_id: reference.candidate_id.clone(),
            content_digest: reference.content_digest.clone(),
            embedding_contract: contract,
            embedding: vec![1.0, 0.0],
        };
        let build = EmbeddingCentrality::build_references_inner(
            embedder,
            std::slice::from_ref(&reference),
            std::slice::from_ref(&reusable),
            CentralityEmbeddingConfig::from_seconds(2, 2, 8),
            "test",
            "expired-provider",
            1,
            None,
            Some(tokio::time::Instant::now()),
        )
        .await
        .expect("durable reference can still construct a provider");
        assert!(build.provider.evaluate("alpha").await.is_none());
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn same_dimension_model_rotation_invalidates_durable_vectors() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let original = mock_embedder(&server).await;
        let original_contract = original.embedding_contract_id();
        let rotated = OllamaEmbedder::new(OllamaEmbedderConfig {
            model: "rotated-model".to_string(),
            ..original.config().clone()
        });
        let references = vec![CentralityReference {
            candidate_id: "same-id".to_string(),
            content_digest: "same-digest".to_string(),
            embedding_text: "alpha".to_string(),
        }];
        let reusable = vec![ReusableCentralityEmbedding {
            candidate_id: "same-id".to_string(),
            content_digest: "same-digest".to_string(),
            embedding_contract: original_contract,
            embedding: vec![1.0, 0.0],
        }];
        let build = EmbeddingCentrality::build_references_inner(
            rotated,
            &references,
            &reusable,
            CentralityEmbeddingConfig::from_seconds(2, 2, 8),
            "test",
            "model-rotation",
            1,
            None,
            None,
        )
        .await
        .expect("rotated model rebuild");
        assert_eq!(build.newly_embedded.len(), 1);
        assert_ne!(
            build.newly_embedded[0].embedding_contract,
            reusable[0].embedding_contract
        );
    }

    #[tokio::test]
    async fn cache_miss_query_work_is_hard_bounded_per_pass() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        let embedder = mock_embedder(&server).await;
        let provider = EmbeddingCentrality::build(
            embedder,
            &["alpha".into(), "beta".into()],
            CentralityEmbeddingConfig::from_seconds(2, 2, 8),
            "test",
            "centrality-budget",
        )
        .await
        .expect("centrality provider");
        provider
            .remaining_provider_queries
            .store(2, Ordering::Relaxed);

        assert!(provider.evaluate("gamma-one").await.is_some());
        assert!(provider.evaluate("gamma-two").await.is_some());
        assert!(provider.evaluate("gamma-three").await.is_none());
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            4,
            "two reference calls plus exactly two budgeted cache misses"
        );
    }

    #[tokio::test]
    async fn cancellation_aborts_reference_construction_without_waiting_for_provider_timeout() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(json!({ "embeddings": [[1.0, 0.0]] })),
            )
            .mount(&server)
            .await;
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            base_url: server.uri(),
            model: "test-embed".to_string(),
            dims: 2,
            timeout: Duration::from_secs(30),
            query_timeout: Duration::from_secs(2),
            num_parallel: 1,
            batch_tokens: None,
            ..OllamaEmbedderConfig::default()
        });
        let cancel = CancellationToken::new();
        let cancel_after_admission = cancel.clone();
        let texts = ["alpha".into()];
        let build = EmbeddingCentrality::build_with_cancellation(
            embedder,
            &texts,
            CentralityEmbeddingConfig::from_seconds(30, 2, 8),
            "test",
            "centrality-cancel",
            cancel,
        );
        let cancel_soon = async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            cancel_after_admission.cancel();
        };

        let (provider, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(build, cancel_soon)
        })
        .await
        .expect("cancellation must not wait for the delayed provider response");
        assert!(provider.is_none());
    }
}
