//! Generic vector toolkit primitives for agent capabilities.
//!
//! Exposes:
//! - [`OllamaEmbedder`] — health-checked text embedding via the Ollama HTTP API.
//! - [`VectorTable`] — lancedb-backed table with index / search / rank semantics
//!   over arbitrary `(id, text, metadata)` triples.
//!
//! Designed to be consumed by the `vector` capability provider in the magician
//! crate without exposing memory-tier internals. The memory index uses its own
//! private path (`memory_index.rs`); this module is a parallel, generic surface
//! agents call directly.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator,
    RecordBatchReader, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures_util::TryStreamExt;
use lancedb::{
    connect,
    index::{
        scalar::{FullTextSearchQuery, MatchQuery, Operator},
        Index,
    },
    query::{ExecutableQuery, QueryBase, Select},
    Table,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::lancedb_scheduler::run_lancedb_search_at_scheduler_root;
use crate::ollama_keep_alive;
use crate::{
    acquire_embedding_permit_with_capacity, acquire_embedding_permit_with_capacity_and_wait_signal,
    embedding_scheduler::coalesce_query_embedding_until_with_reuse_signal, EmbeddingPriority,
};

/// Default dedicated embedding Ollama daemon URL.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11435";
/// Default per-request timeout for embedding calls.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Priority retrieval embedding budget; the model is expected to be resident.
pub const DEFAULT_QUERY_TIMEOUT_MS: u64 = 5_000;
/// Default per-request timeout for the health probe + model-tag check. Bumped
/// from 5s to 10s because a freshly-started Ollama daemon can take a few
/// seconds to load its index of pulled models on the first probe.
pub const DEFAULT_HEALTH_TIMEOUT_MS: u64 = 10_000;
const TABLE_NAME: &str = "items";
const FTS_COLUMN: &str = "text";
const DELETE_ID_BATCH_SIZE: usize = 100;

// =============================================================================
// OllamaEmbedder
// =============================================================================

/// Configuration for [`OllamaEmbedder`].
#[derive(Debug, Clone)]
pub struct OllamaEmbedderConfig {
    pub base_url: String,
    pub model: String,
    pub dims: usize,
    pub timeout: Duration,
    pub query_timeout: Duration,
    pub health_timeout: Duration,
    pub batch_size: usize,
    pub num_parallel: u32,
    pub max_loaded_models: u32,
    pub keep_alive: Option<String>,
    pub context_tokens: Option<u32>,
    pub batch_tokens: Option<u32>,
}

impl Default for OllamaEmbedderConfig {
    fn default() -> Self {
        Self {
            base_url: ollama_keep_alive::default_embedding_base_url()
                .unwrap_or_else(|| DEFAULT_OLLAMA_URL.to_string()),
            model: ollama_keep_alive::default_embedding_model(),
            dims: ollama_keep_alive::default_embedding_dimensions(),
            timeout: Duration::from_millis(
                ollama_keep_alive::default_embedding_write_timeout_ms()
                    .unwrap_or(DEFAULT_TIMEOUT_MS),
            ),
            query_timeout: Duration::from_millis(
                ollama_keep_alive::default_embedding_query_timeout_ms()
                    .unwrap_or(DEFAULT_QUERY_TIMEOUT_MS),
            ),
            health_timeout: Duration::from_millis(DEFAULT_HEALTH_TIMEOUT_MS),
            batch_size: ollama_keep_alive::default_embedding_batch_size(),
            num_parallel: ollama_keep_alive::default_embedding_num_parallel().unwrap_or(1),
            max_loaded_models: ollama_keep_alive::default_embedding_max_loaded_models()
                .unwrap_or(1),
            keep_alive: ollama_keep_alive::default_keep_alive(),
            context_tokens: ollama_keep_alive::default_embedding_context_tokens(),
            batch_tokens: ollama_keep_alive::default_embedding_batch_tokens(),
        }
    }
}

impl OllamaEmbedderConfig {
    /// Build from the configured embedding contract plus operational env knobs.
    ///
    /// Reads (in priority order, first wins per setting):
    ///   - URL:     `MAGICIAN_MEMORY_OLLAMA_URL` → `MAGICIAN_OLLAMA_BASE_URL`
    ///   - Timeout: `MAGICIAN_OLLAMA_EMBEDDING_TIMEOUT_MS` → `MAGICIAN_MEMORY_EMBEDDING_TIMEOUT_MS`
    ///   - Health:  `MAGICIAN_OLLAMA_HEALTH_TIMEOUT_MS`
    /// Residency is config-only and must remain pinned for the dedicated daemon.
    ///
    /// Model, dimensions, context, request batch size, and `num_batch` are
    /// installed exclusively from `runtime.ollama` in `magician-config.yaml`.
    pub fn from_env() -> Self {
        fn read_string_env(names: &[&str]) -> Option<String> {
            for name in names {
                if let Ok(v) = std::env::var(name) {
                    let trimmed = v.trim().trim_end_matches('/').to_string();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
            None
        }
        fn read_ms_env(names: &[&str]) -> Option<u64> {
            for name in names {
                if let Ok(v) = std::env::var(name) {
                    if let Ok(ms) = v.parse::<u64>() {
                        if ms > 0 {
                            return Some(ms);
                        }
                    }
                }
            }
            None
        }
        let mut cfg = Self::default();
        if let Some(url) = read_string_env(&["MAGICIAN_MEMORY_OLLAMA_URL"])
            .or_else(ollama_keep_alive::default_embedding_base_url)
            .or_else(|| read_string_env(&["MAGICIAN_OLLAMA_BASE_URL"]))
        {
            cfg.base_url = url;
        }
        if let Some(ms) = read_ms_env(&[
            "MAGICIAN_OLLAMA_EMBEDDING_TIMEOUT_MS",
            "MAGICIAN_MEMORY_EMBEDDING_TIMEOUT_MS",
        ]) {
            cfg.timeout = Duration::from_millis(ms);
        }
        if let Some(ms) = read_ms_env(&[
            "MAGICIAN_MEMORY_QUERY_EMBEDDING_TIMEOUT_MS",
            "MAGICIAN_OLLAMA_QUERY_EMBEDDING_TIMEOUT_MS",
        ]) {
            cfg.query_timeout = Duration::from_millis(ms);
        }
        if let Some(ms) = read_ms_env(&["MAGICIAN_OLLAMA_HEALTH_TIMEOUT_MS"]) {
            cfg.health_timeout = Duration::from_millis(ms);
        }
        if let Some(context_tokens) = ollama_keep_alive::default_embedding_context_tokens() {
            cfg.context_tokens = Some(context_tokens);
        }
        if let Some(batch_tokens) = ollama_keep_alive::default_embedding_batch_tokens() {
            cfg.batch_tokens = Some(batch_tokens);
        }
        cfg
    }

    /// Stable identity for persisted embeddings without constructing an HTTP
    /// client. Provider, model, dimensions, logical context, physical
    /// `num_batch` ceiling, and preprocessing version define compatibility.
    /// Parallelism, request batch size, timeouts, URL, and residency remain
    /// execution-only because they cannot alter an individual vector.
    pub fn embedding_contract_id(&self) -> String {
        let mut hasher = blake3::Hasher::new();
        for field in [
            "ollama-embedding-contract-v3".as_bytes(),
            "ollama".as_bytes(),
            self.model.as_bytes(),
            &self.dims.to_le_bytes(),
            &self.context_tokens.unwrap_or_default().to_le_bytes(),
            &self.batch_tokens.unwrap_or_default().to_le_bytes(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        hasher.finalize().to_hex().to_string()
    }
}

/// Thin HTTP client for Ollama embeddings + health checks.
///
/// Holds a shared `reqwest::Client` (built once at construction) so all
/// embed / health / tag-list calls reuse the connection pool. Per-call
/// timeouts are applied via `RequestBuilder::timeout` rather than on the
/// client itself, so a long embed call doesn't block a fast health probe.
#[derive(Debug, Clone)]
pub struct OllamaEmbedder {
    cfg: OllamaEmbedderConfig,
    client: reqwest::Client,
}

impl OllamaEmbedder {
    pub fn new(cfg: OllamaEmbedderConfig) -> Self {
        let client = reqwest::Client::builder()
            .build()
            .expect("reqwest::Client::build with default options should never fail");
        Self { cfg, client }
    }

    /// Convenience: build from environment with default fallbacks.
    pub fn from_env() -> Self {
        Self::new(OllamaEmbedderConfig::from_env())
    }

    pub fn config(&self) -> &OllamaEmbedderConfig {
        &self.cfg
    }

    /// Quick health probe: `GET /api/tags`. Per-call timeout from
    /// `cfg.health_timeout` (default 10s) on BOTH paths — the routed probe
    /// is wrapped in the same bound so boot/liveness decisions never wait on
    /// the provider's 180s default. Returns `Ok(())` if Ollama is reachable
    /// and responsive. When an embedding router is installed the probe rides
    /// the routed provider (same daemon, one chokepoint).
    pub async fn health_check(&self) -> Result<()> {
        if let Some(probe) = crate::embedding_router::routed_health_check() {
            let healthy = tokio::time::timeout(self.cfg.health_timeout, probe)
                .await
                .map_err(|_| {
                    anyhow!(
                        "routed Ollama embedding health check exceeded the {} ms budget",
                        self.cfg.health_timeout.as_millis()
                    )
                })??;
            if healthy {
                return Ok(());
            }
            return Err(anyhow!(
                "routed Ollama embedding health check reported the provider unhealthy"
            ));
        }
        let url = format!("{}/api/tags", self.cfg.base_url);
        let resp = self
            .client
            .get(&url)
            .timeout(self.cfg.health_timeout)
            .send()
            .await
            .with_context(|| format!("calling Ollama health endpoint {url}"))?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "Ollama health endpoint {url} returned HTTP {}",
                resp.status()
            ));
        }
        Ok(())
    }

    /// Check if the configured embedding model is pulled. Returns
    /// `Ok(true)` if pulled, `Ok(false)` if missing, `Err` on transport
    /// failure.
    pub async fn model_pulled(&self) -> Result<bool> {
        self.validate_embedding_contract()?;
        let url = format!("{}/api/tags", self.cfg.base_url);
        let resp: TagsResponse = self
            .client
            .get(&url)
            .timeout(self.cfg.health_timeout)
            .send()
            .await
            .with_context(|| format!("calling {url}"))?
            .error_for_status()?
            .json()
            .await
            .context("parsing Ollama /api/tags response")?;
        let model_lc = self.cfg.model.to_ascii_lowercase();
        Ok(resp.models.iter().any(|m| {
            let name = m.name.to_ascii_lowercase();
            name == model_lc || name.split(':').next().unwrap_or("") == model_lc
        }))
    }

    /// Embed a batch of texts. Returns one vector per input in order.
    /// Vectors are dimension `self.cfg.dims`.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_documents(texts).await
    }

    /// Embed source documents as yieldable write work. Only one write request
    /// is admitted at a time, and each HTTP batch releases admission so queued
    /// retrieval reads run before the next batch.
    pub async fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_priority(texts, EmbeddingPriority::Write, self.cfg.timeout)
            .await
    }

    /// Embed documents for optional background analysis rather than index
    /// construction. This shares the cooperative background lane but is
    /// distinguished in admission telemetry and can never jump ahead of a
    /// queued request-path retrieval.
    pub async fn embed_documents_background(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_with_priority_inner(
            texts,
            EmbeddingPriority::BackgroundRead,
            self.cfg.timeout,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Eval-only admission gate over the exact optional-background pipeline.
    /// The first acquired permit emits `acquired`, remains held until `release`,
    /// and only then proceeds to HTTP. It mutates no store.
    #[doc(hidden)]
    pub async fn embed_documents_background_with_admission_gate(
        &self,
        texts: &[String],
        acquired: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<Vec<Vec<f32>>> {
        self.embed_with_priority_inner(
            texts,
            EmbeddingPriority::BackgroundRead,
            self.cfg.timeout,
            None,
            Some((acquired, release)),
            None,
            None,
        )
        .await
    }

    /// Embed a retrieval query through the reserved priority-0 read lane.
    pub async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.embed_query_inner(query, None, None, None).await
    }

    /// Eval-only signal attributed to this exact coalescer key. The sender
    /// fires only if this call owns the query loader and that loader acquires a
    /// foreground provider permit; unrelated reads cannot satisfy it.
    #[doc(hidden)]
    pub async fn embed_query_with_admission_signal(
        &self,
        query: &str,
        provider_admission_started: tokio::sync::oneshot::Sender<()>,
    ) -> Result<Vec<f32>> {
        self.embed_query_inner(query, None, Some(provider_admission_started), None)
            .await
    }

    /// Two-stage eval probe for one exact unique query/coalescer loader.
    #[doc(hidden)]
    pub async fn embed_query_with_admission_probe(
        &self,
        query: &str,
        admission_wait_started: tokio::sync::oneshot::Sender<()>,
        permit_acquired: tokio::sync::oneshot::Sender<()>,
    ) -> Result<Vec<f32>> {
        self.embed_query_inner(
            query,
            Some(admission_wait_started),
            Some(permit_acquired),
            None,
        )
        .await
    }

    /// Three-stage live-eval probe bound to one exact unique query key:
    /// admission waiter registered, a sibling caller joined this coalescer
    /// entry, and the entry's loader acquired provider capacity.
    #[doc(hidden)]
    pub async fn embed_query_with_contention_probe(
        &self,
        query: &str,
        admission_wait_started: tokio::sync::oneshot::Sender<()>,
        coalesced_reuse_observed: tokio::sync::oneshot::Sender<()>,
        permit_acquired: tokio::sync::oneshot::Sender<()>,
    ) -> Result<Vec<f32>> {
        self.embed_query_inner(
            query,
            Some(admission_wait_started),
            Some(permit_acquired),
            Some(coalesced_reuse_observed),
        )
        .await
    }

    async fn embed_query_inner(
        &self,
        query: &str,
        admission_wait_started: Option<tokio::sync::oneshot::Sender<()>>,
        provider_admission_started: Option<tokio::sync::oneshot::Sender<()>>,
        coalesced_reuse_observed: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<Vec<f32>> {
        let deadline = tokio::time::Instant::now() + self.cfg.query_timeout;
        self.validate_embedding_contract()?;
        let key = self.query_embedding_key(query);
        if let Some(cached) = crate::query_vector_cache::get_query_vector(&key) {
            return Ok(cached);
        }
        let query_owned = query.to_string();
        let embedder = self.clone();
        let contract = embedder.query_embed_contract();
        let batch = crate::query_embed_batcher::query_embed_batch_enabled();
        let batch_key = key.clone();
        let vector = coalesce_query_embedding_until_with_reuse_signal(
            key.clone(),
            Some(deadline),
            coalesced_reuse_observed,
            move || {
                let query_owned = query_owned;
                let embedder = embedder;
                let contract = contract;
                let batch_key = batch_key;
                async move {
                    let vector = if batch {
                        crate::query_embed_batcher::join_query_embed_batch(
                            contract,
                            query_owned,
                            batch_key,
                            deadline,
                            move |texts, http_deadline| {
                                let embedder = embedder.clone();
                                async move {
                                    embedder
                                        .embed_query_texts_with_deadline(&texts, http_deadline)
                                        .await
                                }
                            },
                        )
                        .await?
                    } else {
                        let vectors = embedder
                            .embed_with_priority_inner(
                                &[query_owned],
                                EmbeddingPriority::Read,
                                embedder.cfg.query_timeout,
                                Some(deadline),
                                None,
                                admission_wait_started,
                                provider_admission_started,
                            )
                            .await?;
                        vectors
                            .into_iter()
                            .next()
                            .context("Ollama returned no query embedding")?
                    };
                    Ok(vector)
                }
            },
        )
        .await?;
        crate::query_vector_cache::store_query_vector(key, vector.clone());
        Ok(vector)
    }

    fn query_embed_contract(&self) -> crate::query_embed_batcher::QueryEmbedContract {
        crate::query_embed_batcher::QueryEmbedContract {
            base_url: self.cfg.base_url.clone(),
            model: self.cfg.model.clone(),
            dims: self.cfg.dims,
            context_tokens: self.cfg.context_tokens.unwrap_or_default(),
            batch_tokens: self.cfg.batch_tokens.unwrap_or_default(),
            contract_id: self.cfg.embedding_contract_id(),
        }
    }

    pub(crate) async fn embed_query_texts_with_deadline(
        &self,
        texts: &[String],
        deadline: tokio::time::Instant,
    ) -> Result<Vec<Vec<f32>>> {
        self.embed_with_priority_inner(
            texts,
            EmbeddingPriority::Read,
            self.cfg.query_timeout,
            Some(deadline),
            None,
            None,
            None,
        )
        .await
    }

    /// Embed an optional/background query without entering the foreground
    /// coalescer. Background work releases admission between provider requests
    /// and yields to every queued [`EmbeddingPriority::Read`].
    pub async fn embed_background_query(&self, query: &str) -> Result<Vec<f32>> {
        let vectors = self
            .embed_with_priority(
                &[query.to_string()],
                EmbeddingPriority::BackgroundRead,
                self.cfg.query_timeout,
            )
            .await?;
        vectors
            .into_iter()
            .next()
            .context("Ollama returned no background query embedding")
    }

    pub(crate) fn query_embedding_key(&self, query: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        for field in [
            "ollama-query-v2".as_bytes(),
            self.cfg.base_url.as_bytes(),
            self.cfg.model.as_bytes(),
            &self.cfg.dims.to_le_bytes(),
            &self.cfg.context_tokens.unwrap_or_default().to_le_bytes(),
            &self.cfg.batch_tokens.unwrap_or_default().to_le_bytes(),
            query.as_bytes(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field);
        }
        hasher.finalize().to_hex().to_string()
    }

    /// Stable identity for persisted embeddings. Provider, model, dimensions,
    /// logical context, physical `num_batch` ceiling, and preprocessing version
    /// define compatibility; execution-only parallelism, request batching,
    /// timeouts, endpoint, and residency are deliberately excluded.
    pub fn embedding_contract_id(&self) -> String {
        self.cfg.embedding_contract_id()
    }

    /// Load and pin the configured model before request-path retrieval starts.
    pub async fn prewarm(&self) -> Result<()> {
        self.embed_with_priority(
            &["Magician embedding startup".to_string()],
            EmbeddingPriority::Read,
            self.cfg.timeout,
        )
        .await
        .map(|_| ())
    }

    async fn embed_with_priority(
        &self,
        texts: &[String],
        priority: EmbeddingPriority,
        request_timeout: Duration,
    ) -> Result<Vec<Vec<f32>>> {
        self.embed_with_priority_inner(texts, priority, request_timeout, None, None, None, None)
            .await
    }

    async fn embed_with_priority_inner(
        &self,
        texts: &[String],
        priority: EmbeddingPriority,
        request_timeout: Duration,
        absolute_deadline: Option<tokio::time::Instant>,
        mut admission_gate: Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
        mut admission_wait_started: Option<tokio::sync::oneshot::Sender<()>>,
        mut provider_admission_started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<Vec<Vec<f32>>> {
        self.validate_embedding_contract()?;
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let prepared =
            prepare_embedding_inputs(texts, self.cfg.context_tokens, self.cfg.batch_tokens)?;
        let mut fragment_vectors = Vec::with_capacity(prepared.inputs.len());
        // Background requests are intentionally one sequence at a time. This
        // keeps the single non-preemptible provider request short enough for a
        // queued foreground query to take over at the next admission boundary.
        let provider_batch_size = if priority == EmbeddingPriority::Read {
            self.cfg.batch_size
        } else {
            1
        };
        for chunk in prepared.inputs.chunks(provider_batch_size) {
            // Foreground query fragmentation consumes one caller-owned SLA.
            // Yieldable document/background work instead gets a fresh bounded
            // admission+HTTP budget for every provider call, so a healthy long
            // sequence is not mistaken for one stalled request.
            let deadline = embedding_provider_deadline(
                absolute_deadline,
                tokio::time::Instant::now(),
                request_timeout,
            );
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "{priority:?} embedding exhausted the {} ms provider request budget",
                    request_timeout.as_millis()
                ));
            }
            let admission = async {
                if let Some(wait_started) = admission_wait_started.take() {
                    acquire_embedding_permit_with_capacity_and_wait_signal(
                        priority,
                        self.cfg.num_parallel.max(1) as usize,
                        wait_started,
                    )
                    .await
                } else {
                    acquire_embedding_permit_with_capacity(
                        priority,
                        self.cfg.num_parallel.max(1) as usize,
                    )
                    .await
                }
            };
            let _permit = tokio::time::timeout(remaining, admission)
            .await
            .with_context(|| {
                format!(
                    "waiting for {priority:?} embedding admission exceeded the {} ms request budget",
                    request_timeout.as_millis()
                )
            })?
            .with_context(|| format!("validating {priority:?} embedding daemon capacity"))?;
            if let Some(started) = provider_admission_started.take() {
                let _ = started.send(());
            }
            if let Some((acquired, release)) = admission_gate.take() {
                acquired
                    .send(())
                    .map_err(|_| anyhow!("embedding admission probe receiver dropped"))?;
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                tokio::time::timeout(remaining, release)
                    .await
                    .context("embedding admission probe release exceeded request deadline")?
                    .map_err(|_| anyhow!("embedding admission probe release dropped"))?;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "{priority:?} embedding admission exhausted the {} ms provider request budget",
                    request_timeout.as_millis()
                ));
            }
            // Innermost HTTP exchange routes through magicllm when a router is
            // installed (profile-bound provider identity); the fallback keeps
            // the previous direct POST byte-for-byte.
            let embeddings = crate::embedding_router::post_ollama_embed(
                if priority == EmbeddingPriority::Read {
                    crate::embedding_router::EMBED_QUERY_OPERATION
                } else {
                    crate::embedding_router::EMBED_DOCUMENTS_OPERATION
                },
                &self.cfg.base_url,
                &self.client,
                &self.cfg.model,
                chunk,
                self.cfg.keep_alive.as_deref(),
                self.cfg.context_tokens,
                self.cfg.batch_tokens,
                remaining,
            )
            .await?;
            if embeddings.len() != chunk.len() {
                return Err(anyhow!(
                    "Ollama returned {} embeddings for {} inputs",
                    embeddings.len(),
                    chunk.len()
                ));
            }
            for vec in &embeddings {
                if vec.len() != self.cfg.dims {
                    return Err(anyhow!(
                        "Ollama returned {}-dim vector; expected {}",
                        vec.len(),
                        self.cfg.dims
                    ));
                }
                if !embedding_vector_has_valid_norm(vec) {
                    return Err(anyhow!(
                        "Ollama returned an embedding with non-finite values or invalid zero norm"
                    ));
                }
            }
            fragment_vectors.extend(embeddings);
        }
        pool_embedding_fragments(
            fragment_vectors,
            &prepared.groups,
            &prepared.fragment_weights,
            self.cfg.dims,
        )
    }

    fn validate_embedding_contract(&self) -> Result<()> {
        if self.cfg.model.trim().is_empty() {
            return Err(anyhow!(
                "Ollama embedding model is not configured; set runtime.ollama.embedding_model"
            ));
        }
        if self.cfg.dims == 0 {
            return Err(anyhow!(
                "Ollama embedding dimensions are not configured; set runtime.ollama.embedding_dimensions"
            ));
        }
        if self.cfg.batch_size == 0 {
            return Err(anyhow!(
                "Ollama embedding batch size is not configured; set runtime.ollama.embedding_batch_size"
            ));
        }
        if self.cfg.context_tokens == Some(0) {
            return Err(anyhow!(
                "Ollama embedding logical context size must be greater than zero"
            ));
        }
        if self.cfg.batch_tokens == Some(0) {
            return Err(anyhow!(
                "Ollama embedding physical batch size must be greater than zero"
            ));
        }
        // Install or compare process-wide capacity only after every pure
        // contract check succeeds. A malformed first request must not poison
        // the process authority for the valid runtime configuration.
        crate::embedding_scheduler::validate_embedding_admission_capacity(
            self.cfg.num_parallel.max(1) as usize,
        )?;
        Ok(())
    }
}

fn embedding_provider_deadline(
    absolute_deadline: Option<tokio::time::Instant>,
    now: tokio::time::Instant,
    request_timeout: Duration,
) -> tokio::time::Instant {
    absolute_deadline.unwrap_or(now + request_timeout)
}

/// Reserve a handful of tokens for model-added BOS/EOS/special tokens. UTF-8
/// byte length is a deliberately conservative tokenizer-independent upper
/// bound because byte-fallback tokenizers cannot produce more content tokens
/// than input bytes. Ollama's llama.cpp embedding runner rejects an individual
/// input that exceeds either `num_ctx` or its physical `num_batch`; using the
/// smaller configured ceiling prevents both failures. The request also sends
/// `truncate: false`, so tokenizer-specific overflow fails closed instead of
/// silently dropping tail content.
const EMBEDDING_SPECIAL_TOKEN_RESERVE: usize = 8;
// Runtime configuration permits a 32-token physical batch. After the special
// token reserve that still leaves 24 conservative content bytes; keep the
// semantic splitter's fail-closed floor below that supported minimum.
const MIN_SEMANTIC_FRAGMENT_BYTES: usize = 16;
const MAX_OLLAMA_ERROR_DETAIL_CHARS: usize = 512;

#[derive(Debug)]
pub(crate) struct OllamaHttpStatusError {
    endpoint: String,
    status: reqwest::StatusCode,
    detail: String,
}

impl OllamaHttpStatusError {
    pub(crate) fn new(endpoint: String, status: reqwest::StatusCode, response_body: &[u8]) -> Self {
        Self {
            endpoint,
            status,
            detail: bounded_ollama_error_detail(response_body),
        }
    }

    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.status
    }
}

impl fmt::Display for OllamaHttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Ollama embed endpoint {} returned HTTP {}: {}",
            self.endpoint, self.status, self.detail
        )
    }
}

impl std::error::Error for OllamaHttpStatusError {}

#[derive(Debug, PartialEq, Eq)]
struct PreparedEmbeddingInputs {
    inputs: Vec<String>,
    groups: Vec<std::ops::Range<usize>>,
    /// Conservative tokenizer-independent content weight (UTF-8 bytes) for
    /// length-weighted pooling. A small tail must not count as much as a full
    /// semantic fragment.
    fragment_weights: Vec<usize>,
}

fn prepare_embedding_inputs(
    texts: &[String],
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
) -> Result<PreparedEmbeddingInputs> {
    let physical_token_ceiling = match (context_tokens, batch_tokens) {
        (Some(context), Some(batch)) => Some(context.min(batch)),
        (Some(context), None) => Some(context),
        (None, Some(batch)) => Some(batch),
        (None, None) => None,
    };
    let Some(physical_token_ceiling) = physical_token_ceiling else {
        return Ok(PreparedEmbeddingInputs {
            inputs: texts.to_vec(),
            groups: (0..texts.len()).map(|index| index..index + 1).collect(),
            fragment_weights: texts.iter().map(|text| text.len().max(1)).collect(),
        });
    };
    let fragment_bytes =
        (physical_token_ceiling as usize).saturating_sub(EMBEDDING_SPECIAL_TOKEN_RESERVE);
    if fragment_bytes < MIN_SEMANTIC_FRAGMENT_BYTES {
        return Err(anyhow!(
            "Ollama embedding physical token ceiling {physical_token_ceiling} is too small for safe semantic chunking; expected at least {}",
            MIN_SEMANTIC_FRAGMENT_BYTES + EMBEDDING_SPECIAL_TOKEN_RESERVE
        ));
    }

    let mut inputs = Vec::new();
    let mut groups = Vec::with_capacity(texts.len());
    for text in texts {
        let start = inputs.len();
        inputs.extend(split_semantic_fragments(text, fragment_bytes));
        groups.push(start..inputs.len());
    }
    let fragment_weights = inputs
        .iter()
        .map(|fragment| fragment.len().max(1))
        .collect();
    Ok(PreparedEmbeddingInputs {
        inputs,
        groups,
        fragment_weights,
    })
}

pub(crate) fn bounded_ollama_error_detail(body: &[u8]) -> String {
    let detail = String::from_utf8_lossy(body);
    let detail = detail.trim();
    if detail.is_empty() {
        return "empty response body".to_string();
    }
    let mut bounded = detail
        .chars()
        .take(MAX_OLLAMA_ERROR_DETAIL_CHARS)
        .collect::<String>();
    if detail.chars().count() > MAX_OLLAMA_ERROR_DETAIL_CHARS {
        bounded.push_str("…");
    }
    bounded
}

fn split_semantic_fragments(text: &str, max_bytes: usize) -> Vec<String> {
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }

    let mut fragments = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut hard_end = (start + max_bytes).min(text.len());
        while hard_end > start && !text.is_char_boundary(hard_end) {
            hard_end -= 1;
        }
        if hard_end == text.len() {
            let tail = text[start..].trim();
            if !tail.is_empty() {
                fragments.push(tail.to_string());
            }
            break;
        }

        // Prefer a nearby semantic boundary, but never create a pathologically
        // tiny fragment when a long identifier/URL has no whitespace.
        let soft_floor = start + (hard_end - start) / 2;
        let boundary = text[start..hard_end]
            .char_indices()
            .filter_map(|(offset, ch)| {
                let absolute = start + offset + ch.len_utf8();
                (absolute >= soft_floor
                    && (ch.is_whitespace() || matches!(ch, '.' | '!' | '?' | ';' | ':')))
                .then_some(absolute)
            })
            .last()
            .unwrap_or(hard_end);
        let fragment = text[start..boundary].trim();
        if !fragment.is_empty() {
            fragments.push(fragment.to_string());
        }
        start = boundary;
        while start < text.len() {
            let Some(ch) = text[start..].chars().next() else {
                break;
            };
            if !ch.is_whitespace() {
                break;
            }
            start += ch.len_utf8();
        }
    }

    if fragments.is_empty() {
        // Preserve the one-input/one-output contract even for whitespace-only
        // oversized input.
        vec![String::new()]
    } else {
        fragments
    }
}

fn pool_embedding_fragments(
    fragment_vectors: Vec<Vec<f32>>,
    groups: &[std::ops::Range<usize>],
    fragment_weights: &[usize],
    dims: usize,
) -> Result<Vec<Vec<f32>>> {
    if fragment_vectors.len() != fragment_weights.len() {
        return Err(anyhow!(
            "embedding fragment weights did not cover every provider vector"
        ));
    }
    let mut pooled = Vec::with_capacity(groups.len());
    for group in groups {
        let vectors = fragment_vectors
            .get(group.clone())
            .context("embedding fragment response did not cover every original input")?;
        for vector in vectors {
            if vector.len() != dims {
                return Err(anyhow!(
                    "embedding fragment had {} dimensions; expected {dims}",
                    vector.len()
                ));
            }
            if !embedding_vector_has_valid_norm(vector) {
                return Err(anyhow!(
                    "embedding fragment contained non-finite values or an invalid zero norm"
                ));
            }
        }
        if vectors.len() == 1 {
            pooled.push(vectors[0].clone());
            continue;
        }
        if vectors.is_empty() {
            return Err(anyhow!("embedding input produced no semantic fragments"));
        }
        let mut mean = vec![0.0_f32; dims];
        let weights = fragment_weights
            .get(group.clone())
            .context("embedding fragment weights did not cover every original input")?;
        let mut total_weight = 0.0_f32;
        for (vector, weight) in vectors.iter().zip(weights) {
            let normalized = normalize(vector.clone());
            let weight = *weight as f32;
            total_weight += weight;
            for (slot, value) in mean.iter_mut().zip(normalized) {
                *slot += value * weight;
            }
        }
        if total_weight <= 0.0 {
            return Err(anyhow!("embedding fragments had zero aggregate weight"));
        }
        for value in &mut mean {
            *value /= total_weight;
        }
        let mean = normalize(mean);
        if !embedding_vector_has_valid_norm(&mean) {
            return Err(anyhow!(
                "embedding fragments cancelled to an invalid zero-norm pooled vector"
            ));
        }
        pooled.push(mean);
    }
    Ok(pooled)
}

fn embedding_vector_has_valid_norm(vector: &[f32]) -> bool {
    let norm_squared = vector.iter().try_fold(0.0_f64, |sum, value| {
        value
            .is_finite()
            .then_some(sum + f64::from(*value) * f64::from(*value))
    });
    norm_squared.is_some_and(|norm| norm.is_finite() && norm > f64::EPSILON)
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TaggedModel>,
}

#[derive(Debug, Deserialize)]
struct TaggedModel {
    name: String,
}

/// Normalize a URL for cross-source dedup: lowercased host, scheme stripped,
/// trailing slash dropped, common tracking params (`utm_*`, `fbclid`, `gclid`,
/// `ref`, `ref_src`) removed. Idempotent. Used by both [`VectorTable`]
/// (transparent within-call dedup) and the `catchup_merge` capability (RRF
/// dedup across source envelopes). Single source of truth so the on-the-fly
/// callers and the persisted index normalize identically.
pub fn normalize_url(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let trimmed = url.trim();
    let parsed = match url::Url::parse(trimmed) {
        Ok(u) => u,
        Err(_) => return trimmed.to_ascii_lowercase(),
    };
    let host = parsed
        .host_str()
        .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
        .unwrap_or_default();
    let path = parsed.path().trim_end_matches('/');
    let drop_keys: &[&str] = &[
        "utm_source",
        "utm_medium",
        "utm_campaign",
        "utm_content",
        "utm_term",
        "ref",
        "ref_src",
        "fbclid",
        "gclid",
    ];
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !drop_keys.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut out = format!("{host}{path}");
    if !kept.is_empty() {
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &kept {
            serializer.append_pair(k, v);
        }
        out.push('?');
        out.push_str(&serializer.finish());
    }
    out
}

// =============================================================================
// Item / VectorTable
// =============================================================================

/// A single item to index. `text` is what gets embedded and used for FTS.
/// `metadata` is opaque JSON returned alongside hits — agents store anything
/// they want to thread through (source URL, original title, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorItem {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub metadata: Value,
}

/// One fused notes hit. `semantic` means only the vector side retrieved it.
#[derive(Debug, Clone)]
pub struct HybridAdmissionHit {
    pub id: String,
    pub score: f32,
    pub semantic: bool,
    pub text: String,
}

/// A search hit returned by [`VectorTable::search`] or [`VectorTable::rank`].
#[derive(Debug, Clone, Serialize)]
pub struct VectorHit {
    pub id: String,
    pub score: f32,
    pub text: String,
    pub metadata: Value,
}

/// Search mode for [`VectorTable::search`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// BM25 full-text only.
    Fts,
    /// Vector cosine only.
    Vector,
    /// Reciprocal-rank fusion of FTS + vector.
    Hybrid,
}

/// A lancedb table at a specific URI, scoped per agent-chosen namespace.
///
/// Holds no embedder reference — pass embeddings in to `index_with_embeddings`.
/// The high-level helpers (`index`, `search`, `rank`) take an `OllamaEmbedder`
/// so callers don't have to manage embedding-vs-storage themselves.
pub struct VectorTable {
    uri: PathBuf,
    dims: usize,
}

impl VectorTable {
    /// Open or create the table at the given directory. The directory is
    /// created if missing. The lancedb table inside is named `items`.
    pub fn at(dir: impl AsRef<Path>, dims: usize) -> Self {
        Self {
            uri: dir.as_ref().to_path_buf(),
            dims,
        }
    }

    pub fn uri(&self) -> &Path {
        &self.uri
    }

    pub fn dims(&self) -> usize {
        self.dims
    }

    /// Return whether this namespace already contains a vector table without
    /// creating the directory or table as a side effect.
    pub async fn exists(&self) -> Result<bool> {
        if !tokio::fs::try_exists(&self.uri)
            .await
            .with_context(|| format!("checking vector table dir {}", self.uri.display()))?
        {
            return Ok(false);
        }
        let uri = self.uri.to_string_lossy().to_string();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to lancedb at {uri}"))?;
        let table_names = db
            .table_names()
            .execute()
            .await
            .context("listing vector tables")?;
        Ok(table_names.iter().any(|name| name == TABLE_NAME))
    }

    /// Upsert items into the table. Embeds via `embedder`, then transactionally
    /// merges rows by `id`. Existing rows not named by this call are retained.
    ///
    /// Returns the number of items written.
    pub async fn index(&self, embedder: &OllamaEmbedder, items: &[VectorItem]) -> Result<usize> {
        let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
        let vectors = embedder.embed_documents(&texts).await?;
        self.index_with_embeddings(items, &vectors).await
    }

    /// Lower-level: index with pre-computed embeddings. Lets callers reuse
    /// embeddings across operations (e.g., index + immediately rerank).
    pub async fn index_with_embeddings(
        &self,
        items: &[VectorItem],
        vectors: &[Vec<f32>],
    ) -> Result<usize> {
        if items.len() != vectors.len() {
            return Err(anyhow!(
                "index: items.len()={} != vectors.len()={}",
                items.len(),
                vectors.len()
            ));
        }
        if items.is_empty() {
            return Ok(0);
        }
        for v in vectors {
            if v.len() != self.dims {
                return Err(anyhow!(
                    "index: vector dim {} != table dim {}",
                    v.len(),
                    self.dims
                ));
            }
        }
        let mut ids = HashSet::with_capacity(items.len());
        if let Some(duplicate) = items
            .iter()
            .find_map(|item| (!ids.insert(item.id.as_str())).then_some(item.id.as_str()))
        {
            return Err(anyhow!(
                "index: duplicate id `{duplicate}` in one upsert batch"
            ));
        }
        tokio::fs::create_dir_all(&self.uri)
            .await
            .with_context(|| format!("creating vector table dir {}", self.uri.display()))?;
        let batch = build_record_batch(items, vectors, self.dims)?;
        let uri = self.uri.to_string_lossy().to_string();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to lancedb at {uri}"))?;
        let table_names = db
            .table_names()
            .execute()
            .await
            .context("listing vector tables")?;
        if table_names.iter().any(|name| name == TABLE_NAME) {
            let table = db
                .open_table(TABLE_NAME)
                .execute()
                .await
                .with_context(|| format!("opening vector table at {uri}"))?;
            let mut merge = table.merge_insert(&["id"]);
            merge
                .when_matched_update_all(None)
                .when_not_matched_insert_all();
            merge
                .execute(record_batch_reader(batch))
                .await
                .context(
                    "merging vector rows; run explicit namespace maintenance if the stored schema is incompatible",
                )?;
        } else {
            let table = db
                .create_table(TABLE_NAME, batch)
                .execute()
                .await
                .context("creating vector table")?;
            // Vector queries brute-force new fragments when no ANN index is
            // present. The FTS index is created once with the table and is
            // never replaced by a runtime upsert.
            table
                .create_index(&[FTS_COLUMN], Index::FTS(Default::default()))
                .replace(false)
                .execute()
                .await
                .context("creating FTS index on vector table")?;
        }
        Ok(items.len())
    }

    /// Delete rows with the supplied exact IDs. Missing namespaces and IDs are
    /// treated as successful no-ops so derived-index maintenance is idempotent.
    pub async fn delete_ids(&self, ids: &[String]) -> Result<u64> {
        if ids.is_empty() || !self.exists().await? {
            return Ok(0);
        }

        let uri = self.uri.to_string_lossy().to_string();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to lancedb at {uri}"))?;
        let table = db
            .open_table(TABLE_NAME)
            .execute()
            .await
            .with_context(|| format!("opening vector table at {uri}"))?;
        let mut deleted_rows = 0u64;
        let mut unique_ids = ids
            .iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        unique_ids.sort_unstable();
        for batch in unique_ids.chunks(DELETE_ID_BATCH_SIZE) {
            let predicate = format!(
                "id IN ({})",
                batch
                    .iter()
                    .map(|id| format!("'{}'", id.replace('\'', "''")))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let result = table
                .delete(&predicate)
                .await
                .with_context(|| format!("deleting {} vector table rows", batch.len()))?;
            deleted_rows = deleted_rows.saturating_add(result.num_deleted_rows);
        }
        Ok(deleted_rows)
    }

    /// Query the persisted table. Returns top-`limit` hits.
    pub async fn search(
        &self,
        embedder: &OllamaEmbedder,
        query: &str,
        mode: SearchMode,
        limit: usize,
    ) -> Result<Vec<VectorHit>> {
        let uri = self.uri.to_string_lossy().to_string();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to lancedb at {uri}"))?;
        let table = db
            .open_table(TABLE_NAME)
            .execute()
            .await
            .with_context(|| format!("opening vector table at {uri}"))?;

        match mode {
            SearchMode::Fts => {
                let batches = table
                    .query()
                    .full_text_search(FullTextSearchQuery::new(query.to_string()))
                    .limit(limit.max(1))
                    .select(Select::columns(&["id", "text", "metadata", "_score"]))
                    .execute()
                    .await
                    .context("FTS query")?
                    .try_collect::<Vec<_>>()
                    .await
                    .context("collecting FTS results")?;
                hits_from_batches(&batches, "_score")
            },
            SearchMode::Vector => {
                let qv = embedder.embed_query(query).await?;
                let batches = table
                    .query()
                    .nearest_to(qv.as_slice())
                    .context("building vector query")?
                    .column("embedding")
                    .limit(limit.max(1))
                    .select(Select::columns(&["id", "text", "metadata", "_distance"]))
                    .execute()
                    .await
                    .context("vector query")?
                    .try_collect::<Vec<_>>()
                    .await
                    .context("collecting vector results")?;
                hits_from_batches_distance(&batches)
            },
            SearchMode::Hybrid => {
                // Run both queries; reciprocal-rank fuse client-side. This
                // mirrors lancedb's hybrid scoring without depending on the
                // RRF feature gates that vary by version.
                let qv = embedder.embed_query(query).await?;
                let fts_batches = table
                    .query()
                    .full_text_search(FullTextSearchQuery::new(query.to_string()))
                    .limit(limit.max(1) * 2)
                    .select(Select::columns(&["id", "text", "metadata", "_score"]))
                    .execute()
                    .await
                    .context("hybrid FTS subquery")?
                    .try_collect::<Vec<_>>()
                    .await
                    .context("collecting hybrid FTS results")?;
                let vec_batches = table
                    .query()
                    .nearest_to(qv.as_slice())
                    .context("building hybrid vector subquery")?
                    .column("embedding")
                    .limit(limit.max(1) * 2)
                    .select(Select::columns(&["id", "text", "metadata", "_distance"]))
                    .execute()
                    .await
                    .context("hybrid vector subquery")?
                    .try_collect::<Vec<_>>()
                    .await
                    .context("collecting hybrid vector results")?;
                let fts_hits = hits_from_batches(&fts_batches, "_score")?;
                let vec_hits = hits_from_batches_distance(&vec_batches)?;
                Ok(reciprocal_rank_fuse(&fts_hits, &vec_hits, limit))
            },
        }
    }

    /// Rebuild the BM25 index after rows change. The index created with the
    /// table is not updated by a later merge, so notes search calls this
    /// whenever a note was written.
    pub async fn rebuild_fts(&self) -> Result<()> {
        if !self.exists().await? {
            return Ok(());
        }
        let uri = self.uri.to_string_lossy().to_string();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to lancedb at {uri}"))?;
        let table = db
            .open_table(TABLE_NAME)
            .execute()
            .await
            .with_context(|| format!("opening vector table at {uri}"))?;
        table
            .create_index(&[FTS_COLUMN], Index::FTS(Default::default()))
            .replace(true)
            .execute()
            .await
            .context("rebuilding FTS index")?;
        Ok(())
    }

    /// BM25 with automatic typo distance, plus nearest vectors inside
    /// `max_vector_distance`. `semantic` is set only for a vector hit that the
    /// keyword query did not already return. Pass `query_vector: None` to
    /// stay on keywords when embeddings are unavailable.
    pub async fn search_bm25_fuzzy_and_vector(
        &self,
        query: &str,
        query_vector: Option<&[f32]>,
        limit: usize,
        max_vector_distance: f32,
    ) -> Result<Vec<HybridAdmissionHit>> {
        let uri = self.uri.to_string_lossy().to_string();
        search_bm25_fuzzy_and_vector_uri(&uri, query, query_vector, limit, max_vector_distance)
            .await
    }

    /// Search the `items` table at `uri`. The table handle is kept for the
    /// process and checked out to the latest version on each query, so the
    /// full-text index stored with the table is reused.
    pub async fn search_bm25_fuzzy_and_vector_at(
        uri: &str,
        query: &str,
        query_vector: Option<&[f32]>,
        limit: usize,
        max_vector_distance: f32,
    ) -> Result<Vec<HybridAdmissionHit>> {
        search_bm25_fuzzy_and_vector_uri(uri, query, query_vector, limit, max_vector_distance).await
    }

    /// Hybrid-search only the supplied exact IDs. This is intended for
    /// derived indexes whose persisted namespace is broader than the caller's
    /// authorization or visibility projection; filtering happens inside both
    /// LanceDB subqueries before reciprocal-rank fusion.
    pub async fn search_hybrid_ids(
        &self,
        embedder: &OllamaEmbedder,
        query: &str,
        limit: usize,
        allowed_ids: &[String],
    ) -> Result<Vec<VectorHit>> {
        if allowed_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut unique_ids = allowed_ids
            .iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        unique_ids.sort_unstable();
        let predicate = format!(
            "id IN ({})",
            unique_ids
                .iter()
                .map(|id| format!("'{}'", id.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let uri = self.uri.to_string_lossy().to_string();
        let db = connect(&uri)
            .execute()
            .await
            .with_context(|| format!("connecting to lancedb at {uri}"))?;
        let table = db
            .open_table(TABLE_NAME)
            .execute()
            .await
            .with_context(|| format!("opening vector table at {uri}"))?;
        let qv = embedder.embed_query(query).await?;
        let limit = limit.max(1);
        let query = query.to_string();
        run_lancedb_search_at_scheduler_root(
            "filtered LanceDB hybrid vector-toolkit search",
            Box::new(move || {
                Box::pin(async move {
                    let fts_batches = table
                        .query()
                        .full_text_search(FullTextSearchQuery::new(query))
                        .only_if(predicate.clone())
                        .limit(limit * 2)
                        .select(Select::columns(&["id", "text", "metadata", "_score"]))
                        .execute()
                        .await
                        .context("filtered hybrid FTS subquery")?
                        .try_collect::<Vec<_>>()
                        .await
                        .context("collecting filtered hybrid FTS results")?;
                    let vec_batches = table
                        .query()
                        .nearest_to(qv.as_slice())
                        .context("building filtered hybrid vector subquery")?
                        .column("embedding")
                        .only_if(predicate)
                        .limit(limit * 2)
                        .select(Select::columns(&["id", "text", "metadata", "_distance"]))
                        .execute()
                        .await
                        .context("filtered hybrid vector subquery")?
                        .try_collect::<Vec<_>>()
                        .await
                        .context("collecting filtered hybrid vector results")?;
                    let fts_hits = hits_from_batches(&fts_batches, "_score")?;
                    let vec_hits = hits_from_batches_distance(&vec_batches)?;
                    Ok(reciprocal_rank_fuse(&fts_hits, &vec_hits, limit))
                })
            }),
        )
        .await
    }

    /// Stateless: embed `items` on the fly, score against `query` if given,
    /// return reordered items. No persistence. Honors `output` semantics:
    /// - `Ranked` requires `query`, returns top-`limit` reordered.
    /// - `Clusters` groups by cosine similarity ≥ `threshold`.
    /// - `Deduped` same as Clusters but flat-returns representatives.
    pub async fn rank(
        embedder: &OllamaEmbedder,
        items: &[VectorItem],
        query: Option<&str>,
        output: RankOutput,
        threshold: f32,
        limit: Option<usize>,
    ) -> Result<RankResult> {
        if items.is_empty() {
            return Ok(match output {
                RankOutput::Ranked => RankResult::Ranked(Vec::new()),
                RankOutput::Clusters => RankResult::Clusters(Vec::new()),
                RankOutput::Deduped => RankResult::Deduped(Vec::new()),
            });
        }

        let texts: Vec<String> = items.iter().map(|i| i.text.clone()).collect();
        let vecs = embedder.embed_documents(&texts).await?;

        // Cosine-similarity helpers; vectors arrive already L2-normalized for
        // common embedding models, but we re-normalize to be defensive.
        let normalized: Vec<Vec<f32>> = vecs.into_iter().map(normalize).collect();

        match output {
            RankOutput::Ranked => {
                let q = query.ok_or_else(|| anyhow!("rank(output=ranked) requires `query`"))?;
                let qv = embedder.embed_query(q).await?;
                let qn = normalize(qv);
                let mut scored: Vec<(usize, f32)> = normalized
                    .iter()
                    .enumerate()
                    .map(|(i, v)| (i, cosine_normalized(v, &qn)))
                    .collect();
                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                // Honor explicit `limit=0` as "no items" (e.g. caller only
                // wants the operation to happen for side effects). When
                // unspecified, default to all items.
                let take = limit.unwrap_or(items.len());
                let out: Vec<VectorHit> = scored
                    .into_iter()
                    .take(take)
                    .map(|(i, s)| VectorHit {
                        id: items[i].id.clone(),
                        score: s,
                        text: items[i].text.clone(),
                        metadata: items[i].metadata.clone(),
                    })
                    .collect();
                Ok(RankResult::Ranked(out))
            },
            RankOutput::Clusters | RankOutput::Deduped => {
                let qn_opt: Option<Vec<f32>> = if let Some(q) = query {
                    Some(normalize(embedder.embed_query(q).await?))
                } else {
                    None
                };
                let clusters = greedy_cluster(items, &normalized, threshold, qn_opt.as_deref());
                match output {
                    RankOutput::Clusters => Ok(RankResult::Clusters(clusters)),
                    RankOutput::Deduped => {
                        // Honor explicit limit=0; default = all reps.
                        let take = limit.unwrap_or(items.len());
                        let reps: Vec<VectorHit> = clusters
                            .into_iter()
                            .take(take)
                            .map(|c| c.representative)
                            .collect();
                        Ok(RankResult::Deduped(reps))
                    },
                    _ => unreachable!(),
                }
            },
        }
    }
}

fn search_tables() -> &'static tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Table>>>> {
    static TABLES: OnceLock<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<Table>>>>> =
        OnceLock::new();
    TABLES.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// One open `items` handle per URI. A later query checks out the latest
/// version instead of opening the dataset again, and uses the full-text
/// index already stored with that version.
async fn table_for_search(uri: &str) -> Result<Option<Arc<tokio::sync::Mutex<Table>>>> {
    if let Some(slot) = search_tables().lock().await.get(uri).cloned() {
        return Ok(Some(slot));
    }
    if !uri.contains("://") && !tokio::fs::try_exists(uri).await.unwrap_or(false) {
        return Ok(None);
    }
    let db = connect(uri)
        .execute()
        .await
        .with_context(|| format!("connecting to lancedb at {uri}"))?;
    let names = db.table_names().execute().await.context("listing tables")?;
    if !names.iter().any(|name| name == TABLE_NAME) {
        return Ok(None);
    }
    let table = db
        .open_table(TABLE_NAME)
        .execute()
        .await
        .with_context(|| format!("opening vector table at {uri}"))?;
    let slot = Arc::new(tokio::sync::Mutex::new(table));
    search_tables()
        .lock()
        .await
        .insert(uri.to_string(), slot.clone());
    Ok(Some(slot))
}

async fn search_bm25_fuzzy_and_vector_uri(
    uri: &str,
    query: &str,
    query_vector: Option<&[f32]>,
    limit: usize,
    max_vector_distance: f32,
) -> Result<Vec<HybridAdmissionHit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let Some(mut slot) = table_for_search(uri).await? else {
        return Ok(Vec::new());
    };
    let mut guard = slot.lock().await;
    if guard.checkout_latest().await.is_err() {
        drop(guard);
        search_tables().lock().await.remove(uri);
        let Some(fresh) = table_for_search(uri).await? else {
            return Ok(Vec::new());
        };
        slot = fresh;
        guard = slot.lock().await;
    }
    let table = &*guard;
    let take = limit.max(1).saturating_mul(4);
    let fts_query = FullTextSearchQuery::new_query(
        MatchQuery::new(query.to_string())
            .with_fuzziness(None)
            .with_operator(Operator::And)
            .into(),
    );
    let fts_batches = table
        .query()
        .full_text_search(fts_query)
        .limit(take)
        .select(Select::columns(&["id", "text", "metadata", "_score"]))
        .execute()
        .await
        .context("notes fuzzy FTS query")?
        .try_collect::<Vec<_>>()
        .await
        .context("collecting notes fuzzy FTS results")?;
    let fts_hits = hits_from_batches(&fts_batches, "_score")?;
    let mut fts_ids = fts_hits
        .iter()
        .map(|hit| hit.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let vector_hits = if let Some(qv) = query_vector {
        let batches = table
            .query()
            .nearest_to(qv)
            .context("building notes vector query")?
            .column("embedding")
            .limit(take)
            .select(Select::columns(&["id", "text", "metadata", "_distance"]))
            .execute()
            .await
            .context("notes vector query")?
            .try_collect::<Vec<_>>()
            .await
            .context("collecting notes vector results")?;
        hits_within_distance(&batches, max_vector_distance)?
    } else {
        Vec::new()
    };
    let fused = reciprocal_rank_fuse(&fts_hits, &vector_hits, take);
    Ok(fused
        .into_iter()
        .map(|hit| {
            let semantic = !fts_ids.contains(&hit.id);
            fts_ids.insert(hit.id.clone());
            HybridAdmissionHit {
                text: hit.text,
                id: hit.id,
                score: hit.score,
                semantic,
            }
        })
        .collect())
}

#[derive(Debug, Clone, Copy)]
pub enum RankOutput {
    Ranked,
    Clusters,
    Deduped,
}

#[derive(Debug, Clone, Serialize)]
pub struct VectorCluster {
    pub representative: VectorHit,
    pub members: Vec<VectorHit>,
}

#[derive(Debug, Clone, Serialize)]
pub enum RankResult {
    Ranked(Vec<VectorHit>),
    Clusters(Vec<VectorCluster>),
    Deduped(Vec<VectorHit>),
}

// =============================================================================
// Internal helpers
// =============================================================================

fn build_record_batch(
    items: &[VectorItem],
    vectors: &[Vec<f32>],
    dims: usize,
) -> Result<RecordBatch> {
    let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
    let metas: Vec<String> = items.iter().map(|i| i.metadata.to_string()).collect();
    let meta_refs: Vec<&str> = metas.iter().map(String::as_str).collect();

    let id_array: ArrayRef = Arc::new(StringArray::from(ids));
    let text_array: ArrayRef = Arc::new(StringArray::from(texts));
    let meta_array: ArrayRef = Arc::new(StringArray::from(meta_refs));

    let mut flat: Vec<f32> = Vec::with_capacity(vectors.len() * dims);
    for v in vectors {
        flat.extend(v.iter().copied());
    }
    let value_array = Float32Array::from(flat);
    let field = Arc::new(Field::new("item", DataType::Float32, true));
    let list = FixedSizeListArray::try_new(field, dims as i32, Arc::new(value_array), None)
        .context("constructing FixedSizeList for embeddings")?;
    let embedding_array: ArrayRef = Arc::new(list);

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("metadata", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dims as i32,
            ),
            false,
        ),
    ]));
    RecordBatch::try_new(
        schema,
        vec![id_array, text_array, meta_array, embedding_array],
    )
    .context("building vector record batch")
}

fn record_batch_reader(record_batch: RecordBatch) -> Box<dyn RecordBatchReader + Send> {
    let schema = record_batch.schema();
    Box::new(RecordBatchIterator::new(vec![Ok(record_batch)], schema))
}

fn hits_from_batches(batches: &[RecordBatch], score_col: &str) -> Result<Vec<VectorHit>> {
    let mut out = Vec::new();
    for batch in batches {
        let id_col = string_col(batch, "id")?;
        let text_col = string_col(batch, "text")?;
        let meta_col = string_col(batch, "metadata").ok();
        let score_col_arr = batch
            .column_by_name(score_col)
            .and_then(|c| c.as_any().downcast_ref::<arrow_array::Float32Array>());
        for row in 0..batch.num_rows() {
            let score = score_col_arr.map(|a| a.value(row)).unwrap_or(0.0);
            let meta_str = meta_col.as_ref().map(|c| c.value(row)).unwrap_or("");
            let metadata: Value = if meta_str.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(meta_str).unwrap_or(Value::Null)
            };
            out.push(VectorHit {
                id: id_col.value(row).to_string(),
                score,
                text: text_col.value(row).to_string(),
                metadata,
            });
        }
    }
    Ok(out)
}

fn hits_within_distance(batches: &[RecordBatch], max_distance: f32) -> Result<Vec<VectorHit>> {
    let mut out = Vec::new();
    for batch in batches {
        let id_col = string_col(batch, "id")?;
        let text_col = string_col(batch, "text")?;
        let meta_col = string_col(batch, "metadata").ok();
        let dist_col = batch
            .column_by_name("_distance")
            .and_then(|c| c.as_any().downcast_ref::<arrow_array::Float32Array>());
        for row in 0..batch.num_rows() {
            let dist = dist_col.map(|a| a.value(row)).unwrap_or(f32::MAX);
            if dist > max_distance {
                continue;
            }
            let score = 1.0 / (1.0 + dist);
            let meta_str = meta_col.as_ref().map(|c| c.value(row)).unwrap_or("");
            let metadata: Value = if meta_str.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(meta_str).unwrap_or(Value::Null)
            };
            out.push(VectorHit {
                id: id_col.value(row).to_string(),
                score,
                text: text_col.value(row).to_string(),
                metadata,
            });
        }
    }
    Ok(out)
}

fn hits_from_batches_distance(batches: &[RecordBatch]) -> Result<Vec<VectorHit>> {
    // lancedb returns `_distance` (lower = closer). Convert to similarity-ish
    // score: 1 / (1 + distance). Bounded (0, 1].
    let mut out = Vec::new();
    for batch in batches {
        let id_col = string_col(batch, "id")?;
        let text_col = string_col(batch, "text")?;
        let meta_col = string_col(batch, "metadata").ok();
        let dist_col = batch
            .column_by_name("_distance")
            .and_then(|c| c.as_any().downcast_ref::<arrow_array::Float32Array>());
        for row in 0..batch.num_rows() {
            let dist = dist_col.map(|a| a.value(row)).unwrap_or(0.0);
            let score = 1.0 / (1.0 + dist);
            let meta_str = meta_col.as_ref().map(|c| c.value(row)).unwrap_or("");
            let metadata: Value = if meta_str.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(meta_str).unwrap_or(Value::Null)
            };
            out.push(VectorHit {
                id: id_col.value(row).to_string(),
                score,
                text: text_col.value(row).to_string(),
                metadata,
            });
        }
    }
    Ok(out)
}

fn string_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| anyhow!("column `{name}` missing from result batch"))
}

const RRF_K: f32 = 60.0;

fn reciprocal_rank_fuse(fts: &[VectorHit], vec_: &[VectorHit], limit: usize) -> Vec<VectorHit> {
    use std::collections::HashMap;
    let mut by_id: HashMap<String, (VectorHit, f32)> = HashMap::new();
    for (rank, hit) in fts.iter().enumerate() {
        let contrib = 1.0 / (RRF_K + (rank as f32 + 1.0));
        by_id
            .entry(hit.id.clone())
            .and_modify(|(_, s)| *s += contrib)
            .or_insert((hit.clone(), contrib));
    }
    for (rank, hit) in vec_.iter().enumerate() {
        let contrib = 1.0 / (RRF_K + (rank as f32 + 1.0));
        by_id
            .entry(hit.id.clone())
            .and_modify(|(_, s)| *s += contrib)
            .or_insert((hit.clone(), contrib));
    }
    let mut fused: Vec<(VectorHit, f32)> = by_id.into_values().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused
        .into_iter()
        .take(limit.max(1))
        .map(|(mut hit, score)| {
            hit.score = score;
            hit
        })
        .collect()
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if norm.is_finite() && norm > 0.0 {
        for x in &mut v {
            *x = (f64::from(*x) / norm) as f32;
        }
    }
    v
}

fn cosine_normalized(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Greedy clustering: walk items in order, assign each to the first cluster
/// whose centroid (= representative vector) has cosine ≥ threshold; otherwise
/// open a new cluster.
fn greedy_cluster(
    items: &[VectorItem],
    vectors: &[Vec<f32>],
    threshold: f32,
    query_normalized: Option<&[f32]>,
) -> Vec<VectorCluster> {
    let mut clusters: Vec<(Vec<f32>, Vec<usize>)> = Vec::new();
    for (idx, vec) in vectors.iter().enumerate() {
        let mut assigned = false;
        for (centroid, members) in clusters.iter_mut() {
            if cosine_normalized(vec, centroid) >= threshold {
                members.push(idx);
                assigned = true;
                break;
            }
        }
        if !assigned {
            clusters.push((vec.clone(), vec![idx]));
        }
    }
    clusters
        .into_iter()
        .map(|(_, member_ixs)| {
            // Pick representative: highest query similarity if query supplied,
            // else first member.
            let rep_idx = if let Some(qn) = query_normalized {
                *member_ixs
                    .iter()
                    .max_by(|a, b| {
                        cosine_normalized(&vectors[**a], qn)
                            .partial_cmp(&cosine_normalized(&vectors[**b], qn))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .unwrap_or(&member_ixs[0])
            } else {
                member_ixs[0]
            };
            let rep_score = query_normalized
                .map(|qn| cosine_normalized(&vectors[rep_idx], qn))
                .unwrap_or(1.0);
            let representative = VectorHit {
                id: items[rep_idx].id.clone(),
                score: rep_score,
                text: items[rep_idx].text.clone(),
                metadata: items[rep_idx].metadata.clone(),
            };
            let members = member_ixs
                .into_iter()
                .map(|i| {
                    let score = query_normalized
                        .map(|qn| cosine_normalized(&vectors[i], qn))
                        .unwrap_or(1.0);
                    VectorHit {
                        id: items[i].id.clone(),
                        score,
                        text: items[i].text.clone(),
                        metadata: items[i].metadata.clone(),
                    }
                })
                .collect();
            VectorCluster {
                representative,
                members,
            }
        })
        .collect()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unit_vector_unchanged() {
        let v = vec![0.6_f32, 0.8_f32];
        let n = normalize(v);
        assert!((cosine_normalized(&n, &n) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = normalize(vec![1.0, 0.0]);
        let b = normalize(vec![0.0, 1.0]);
        assert!(cosine_normalized(&a, &b).abs() < 1e-5);
    }

    #[test]
    fn rrf_prefers_consensus_items() {
        let fts = vec![
            VectorHit {
                id: "a".into(),
                score: 10.0,
                text: "".into(),
                metadata: Value::Null,
            },
            VectorHit {
                id: "b".into(),
                score: 9.0,
                text: "".into(),
                metadata: Value::Null,
            },
        ];
        let vec_ = vec![
            VectorHit {
                id: "b".into(),
                score: 0.95,
                text: "".into(),
                metadata: Value::Null,
            },
            VectorHit {
                id: "c".into(),
                score: 0.90,
                text: "".into(),
                metadata: Value::Null,
            },
        ];
        let fused = reciprocal_rank_fuse(&fts, &vec_, 3);
        // `b` appears in both → fused score is higher than `a` (only in fts).
        assert_eq!(fused.first().map(|h| h.id.as_str()), Some("b"));
    }

    #[test]
    fn greedy_cluster_collapses_identical_vectors() {
        let items = vec![
            VectorItem {
                id: "1".into(),
                text: "x".into(),
                metadata: Value::Null,
            },
            VectorItem {
                id: "2".into(),
                text: "x".into(),
                metadata: Value::Null,
            },
            VectorItem {
                id: "3".into(),
                text: "y".into(),
                metadata: Value::Null,
            },
        ];
        let vecs = vec![
            normalize(vec![1.0, 0.0]),
            normalize(vec![1.0, 0.0]),
            normalize(vec![0.0, 1.0]),
        ];
        let clusters = greedy_cluster(&items, &vecs, 0.9, None);
        assert_eq!(clusters.len(), 2);
    }

    #[test]
    fn query_embedding_reuse_key_covers_query_and_model_contract() {
        let first = OllamaEmbedder::new(OllamaEmbedderConfig {
            model: "embed-a".to_string(),
            dims: 2,
            context_tokens: Some(8_192),
            batch_tokens: Some(512),
            ..Default::default()
        });
        let same = OllamaEmbedder::new(first.config().clone());
        let other_model = OllamaEmbedder::new(OllamaEmbedderConfig {
            model: "embed-b".to_string(),
            ..first.config().clone()
        });

        assert_eq!(
            first.query_embedding_key("same query"),
            same.query_embedding_key("same query")
        );
        assert_ne!(
            first.query_embedding_key("same query"),
            first.query_embedding_key("other query")
        );
        assert_ne!(
            first.query_embedding_key("same query"),
            other_model.query_embedding_key("same query")
        );
        assert_ne!(
            first.embedding_contract_id(),
            other_model.embedding_contract_id()
        );
        let execution_tuned = OllamaEmbedder::new(OllamaEmbedderConfig {
            num_parallel: 1,
            batch_size: 32,
            timeout: Duration::from_secs(120),
            ..first.config().clone()
        });
        assert_eq!(
            first.embedding_contract_id(),
            execution_tuned.embedding_contract_id(),
            "execution batching must not invalidate semantically compatible stored vectors"
        );
        let physical_ceiling_changed = OllamaEmbedder::new(OllamaEmbedderConfig {
            batch_tokens: Some(1024),
            ..first.config().clone()
        });
        assert_ne!(
            first.embedding_contract_id(),
            physical_ceiling_changed.embedding_contract_id(),
            "changing physical fragmentation must invalidate persisted vectors"
        );
    }

    #[test]
    fn long_embedding_inputs_are_semantically_chunked_and_pooled_one_for_one() {
        let long = format!("{} END", "A useful sentence with context. ".repeat(12));
        let prepared = prepare_embedding_inputs(&[long], Some(128), Some(128)).unwrap();
        assert_eq!(prepared.groups.len(), 1);
        assert!(prepared.inputs.len() > 1);
        assert!(prepared.inputs.iter().all(|fragment| fragment.len() <= 120));

        let fragment_count = prepared.inputs.len();
        let vectors = (0..fragment_count)
            .map(|index| {
                if index % 2 == 0 {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                }
            })
            .collect();
        let pooled =
            pool_embedding_fragments(vectors, &prepared.groups, &prepared.fragment_weights, 2)
                .unwrap();
        assert_eq!(pooled.len(), 1, "one input must still yield one vector");
        let norm = pooled[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn query_deadline_is_absolute_while_background_provider_budgets_refresh() {
        let started = tokio::time::Instant::now();
        let budget = Duration::from_millis(100);
        let query_deadline = started + budget;
        let later = started + Duration::from_millis(80);
        assert_eq!(
            embedding_provider_deadline(Some(query_deadline), later, budget),
            query_deadline,
            "every query fragment must consume the original caller SLA"
        );
        assert_eq!(
            embedding_provider_deadline(None, later, budget),
            later + budget,
            "each yieldable background provider call gets a fresh bounded budget"
        );
    }

    #[test]
    fn semantic_chunking_is_utf8_safe_and_short_inputs_are_byte_identical() {
        let short = "unchanged input".to_string();
        let unchanged =
            prepare_embedding_inputs(std::slice::from_ref(&short), Some(512), Some(512)).unwrap();
        assert_eq!(unchanged.inputs, vec![short]);
        assert_eq!(unchanged.groups, vec![0..1]);
        assert_eq!(unchanged.fragment_weights, vec![15]);

        let unicode = "🙂 東京 café — context ".repeat(30);
        let prepared = prepare_embedding_inputs(&[unicode], Some(96), Some(96)).unwrap();
        assert!(prepared.inputs.len() > 1);
        assert!(prepared
            .inputs
            .iter()
            .all(|fragment| { fragment.is_char_boundary(fragment.len()) && fragment.len() <= 88 }));
    }

    #[test]
    fn unsafe_logical_context_budget_fails_closed() {
        let error =
            prepare_embedding_inputs(&["text".to_string()], Some(16), Some(512)).unwrap_err();
        assert!(error
            .to_string()
            .contains("too small for safe semantic chunking"));
    }

    #[test]
    fn zero_logical_context_contract_fails_closed() {
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            model: "contract-probe".to_string(),
            dims: 2,
            batch_size: 1,
            context_tokens: Some(0),
            batch_tokens: Some(512),
            ..Default::default()
        });
        let error = embedder.validate_embedding_contract().unwrap_err();
        assert!(error.to_string().contains("logical context size"));
    }

    #[tokio::test]
    async fn malformed_contract_is_rejected_before_global_capacity_admission() {
        let _guard = crate::EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        crate::install_embedding_admission_capacity(1);
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            model: String::new(),
            num_parallel: 2,
            ..Default::default()
        });
        let error = embedder.validate_embedding_contract().unwrap_err();
        assert!(error.to_string().contains("model is not configured"));
        assert_eq!(
            crate::installed_embedding_admission_capacity(),
            1,
            "invalid local configuration must not mutate capacity authority"
        );
    }

    #[test]
    fn logical_context_chunking_keeps_two_multilingual_inputs_separate() {
        let prepared = prepare_embedding_inputs(
            &[
                "🙂 東京 café — context ".repeat(20),
                "नमस्ते दुनिया — दूसरा संदर्भ ".repeat(20),
            ],
            Some(96),
            Some(96),
        )
        .unwrap();
        assert_eq!(prepared.groups.len(), 2);
        assert!(prepared.groups[0].end <= prepared.groups[1].start);
        assert!(prepared.inputs.iter().all(|fragment| fragment.len() <= 88));
    }

    #[test]
    fn physical_batch_ceiling_fragments_inputs_even_when_context_is_larger() {
        let text = "x".repeat(2_000);
        let prepared = prepare_embedding_inputs(&[text.clone()], Some(8_192), Some(512)).unwrap();

        assert!(prepared.inputs.len() > 1);
        assert!(prepared.inputs.iter().all(|fragment| fragment.len() <= 504));
        assert_eq!(prepared.inputs.concat(), text);
        assert_eq!(prepared.groups, vec![0..prepared.inputs.len()]);
    }

    #[test]
    fn ollama_error_detail_is_bounded_but_preserves_provider_cause() {
        let cause = br#"{"error":"input is too large; increase the physical batch size"}"#;
        assert!(bounded_ollama_error_detail(cause).contains("physical batch size"));

        let oversized = vec![b'x'; MAX_OLLAMA_ERROR_DETAIL_CHARS + 50];
        let bounded = bounded_ollama_error_detail(&oversized);
        assert_eq!(bounded.chars().count(), MAX_OLLAMA_ERROR_DETAIL_CHARS + 1);
        assert!(bounded.ends_with('…'));
        assert_eq!(bounded_ollama_error_detail(b"  \n"), "empty response body");
    }

    #[test]
    fn fragment_pooling_weights_a_tiny_tail_less_than_full_context() {
        let pooled =
            pool_embedding_fragments(vec![vec![1.0, 0.0], vec![0.0, 1.0]], &[0..2], &[100, 10], 2)
                .unwrap();
        assert_eq!(pooled.len(), 1);
        assert!(
            pooled[0][0] > pooled[0][1] * 9.0,
            "a ten-times longer fragment must dominate the pooled direction"
        );
    }

    #[test]
    fn fragment_pooling_rejects_non_finite_provider_values() {
        let error = pool_embedding_fragments(
            vec![vec![1.0, f32::NAN], vec![0.0, 1.0]],
            &[0..2],
            &[100, 100],
            2,
        )
        .unwrap_err();
        assert!(error.to_string().contains("non-finite"));
    }

    #[test]
    fn fragment_pooling_rejects_zero_norm_even_for_one_fragment() {
        let error = pool_embedding_fragments(vec![vec![0.0, 0.0]], &[0..1], &[10], 2).unwrap_err();
        assert!(error.to_string().contains("zero norm"));
    }

    #[tokio::test]
    async fn optional_background_probe_signals_only_after_admission_and_holds_until_release() {
        let _guard = crate::EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        crate::install_embedding_admission_capacity(1);
        let blocking = crate::acquire_embedding_permit(EmbeddingPriority::Write).await;
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            model: "probe".to_string(),
            dims: 2,
            timeout: Duration::from_secs(1),
            batch_size: 1,
            num_parallel: 1,
            context_tokens: None,
            batch_tokens: None,
            ..Default::default()
        });
        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let probe = tokio::spawn(async move {
            embedder
                .embed_documents_background_with_admission_gate(
                    &["probe".to_string()],
                    acquired_tx,
                    release_rx,
                )
                .await
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut acquired_rx)
                .await
                .is_err()
        );
        drop(blocking);
        tokio::time::timeout(Duration::from_secs(1), &mut acquired_rx)
            .await
            .expect("probe should acquire after blocker drops")
            .expect("probe acquisition sender");
        assert_eq!(
            crate::embedding_admission_stats().active_background_reads,
            1
        );
        drop(release_tx);
        assert!(
            probe.await.unwrap().is_err(),
            "dropped release exits before HTTP"
        );
    }

    #[tokio::test]
    async fn query_admission_signal_is_bound_to_the_exact_query_loader() {
        let _guard = crate::EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        crate::reset_query_embedding_coalescer_for_tests();
        crate::install_embedding_admission_capacity(1);
        let blocking = crate::acquire_embedding_permit(EmbeddingPriority::Write).await;
        let embedder = OllamaEmbedder::new(OllamaEmbedderConfig {
            base_url: "http://127.0.0.1:1".to_string(),
            model: "probe".to_string(),
            dims: 2,
            timeout: Duration::from_secs(1),
            query_timeout: Duration::from_secs(1),
            batch_size: 1,
            num_parallel: 1,
            context_tokens: None,
            batch_tokens: None,
            ..Default::default()
        });
        let (wait_started_tx, wait_started_rx) = tokio::sync::oneshot::channel();
        let (coalesced_tx, coalesced_rx) = tokio::sync::oneshot::channel();
        let (acquired_tx, mut acquired_rx) = tokio::sync::oneshot::channel();
        let probed = {
            let embedder = embedder.clone();
            tokio::spawn(async move {
                embedder
                    .embed_query_with_contention_probe(
                        "unique-probed-query",
                        wait_started_tx,
                        coalesced_tx,
                        acquired_tx,
                    )
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), wait_started_rx)
            .await
            .expect("exact loader should register its read waiter")
            .expect("exact wait-started sender");
        let sibling = {
            let embedder = embedder.clone();
            tokio::spawn(async move { embedder.embed_query("unique-probed-query").await })
        };
        tokio::time::timeout(Duration::from_secs(1), coalesced_rx)
            .await
            .expect("same-key sibling should join the exact loader")
            .expect("exact reuse sender");
        let unrelated = {
            let embedder = embedder.clone();
            tokio::spawn(async move { embedder.embed_query("unrelated-query").await })
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut acquired_rx)
                .await
                .is_err()
        );
        drop(blocking);
        tokio::time::timeout(Duration::from_secs(1), &mut acquired_rx)
            .await
            .expect("exact query loader should eventually acquire")
            .expect("exact query signal sender");
        assert!(probed.await.unwrap().is_err());
        assert!(sibling.await.unwrap().is_err());
        assert!(unrelated.await.unwrap().is_err());
        crate::reset_query_embedding_coalescer_for_tests();
    }

    #[tokio::test]
    async fn vector_table_exists_and_delete_ids_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let table = VectorTable::at(dir.path().join("vectors"), 2);
        assert!(!table.exists().await.unwrap());

        let items = vec![
            VectorItem {
                id: "keep".to_string(),
                text: "keep this row".to_string(),
                metadata: Value::Null,
            },
            VectorItem {
                id: "remove'quoted".to_string(),
                text: "remove this row".to_string(),
                metadata: Value::Null,
            },
        ];
        table
            .index_with_embeddings(&items, &[vec![1.0, 0.0], vec![0.0, 1.0]])
            .await
            .unwrap();
        assert!(table.exists().await.unwrap());

        let removed_id = "remove'quoted".to_string();
        assert_eq!(
            table
                .delete_ids(&[removed_id.clone(), removed_id.clone()])
                .await
                .unwrap(),
            1
        );
        assert_eq!(table.delete_ids(&[removed_id]).await.unwrap(), 0);
    }
}
