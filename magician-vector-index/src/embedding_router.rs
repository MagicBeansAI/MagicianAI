//! Routed embedding seam: provider identity through magicllm router config.
//!
//! The embedding HTTP exchange always executes inside magicllm's
//! `OllamaProvider` (the single chokepoint). When a router is installed
//! (production bootstrap via `install_embedding_router`), the call resolves
//! through the profile bound to `embed_documents`/`embed_query`; when it is
//! not (offline tests, standalone use), an ad-hoc provider is constructed from
//! the caller's base URL exactly the way this crate used to resolve its
//! endpoint. Either way the wire body and error classification are identical,
//! so the migration is behavior-preserving rather than a hard cutover — but
//! the provider only ever arrives from config or from local fallback state,
//! never from a hand-built request in this crate.

use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};

use magicllm::LLMProvider;

use crate::vector_toolkit::OllamaHttpStatusError;

/// Operation keys carrying the routed embedding calls. These must stay bound
/// in `llm.router.operation_mapping` (see `op-embedding-local`).
pub const EMBED_DOCUMENTS_OPERATION: &str = "embed_documents";
pub const EMBED_QUERY_OPERATION: &str = "embed_query";

static EMBEDDING_ROUTER: OnceLock<RwLock<Option<Arc<magicllm::ConfiguredRouter>>>> =
    OnceLock::new();

fn embedding_router_cell() -> &'static RwLock<Option<Arc<magicllm::ConfiguredRouter>>> {
    EMBEDDING_ROUTER.get_or_init(|| RwLock::new(None))
}

/// Install the router used to resolve embedding provider identity. Follows the
/// crate's existing injection pattern (`set_default_ollama_embedding_policy`,
/// `install_hybrid_result_cache`): magician installs it at config load.
pub fn install_embedding_router(router: Arc<magicllm::ConfiguredRouter>) {
    if let Ok(mut guard) = embedding_router_cell().write() {
        *guard = Some(router);
    }
}

/// Drop an installed router (config reload that failed to produce a usable
/// router must not leave embeddings resolving through a stale one). Callers
/// fall back to the direct seam until a new router is installed.
pub fn uninstall_embedding_router() {
    if let Ok(mut guard) = embedding_router_cell().write() {
        *guard = None;
    }
}

/// The installed embedding router, when the runtime wired one.
pub(crate) fn installed_embedding_router() -> Option<Arc<magicllm::ConfiguredRouter>> {
    embedding_router_cell()
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

/// Map a magicllm error into the error shapes this crate's retry classifier
/// understands: `ProviderStatus` becomes `OllamaHttpStatusError` (so 429/5xx
/// stay retriable and 4xx stays terminal through the existing downcast), and
/// everything else keeps its typed identity for the widened classifier.
fn embed_error(error: magicllm::LLMError, base_url: &str) -> anyhow::Error {
    match error {
        magicllm::LLMError::ProviderStatus {
            provider,
            status,
            body,
        } => {
            let status_code = reqwest::StatusCode::from_u16(status)
                .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
            anyhow::Error::new(OllamaHttpStatusError::new(
                format!("{base_url} (via {provider})"),
                status_code,
                body.as_bytes(),
            ))
        },
        other => anyhow::Error::new(other).context(format!(
            "Ollama embedding request against {base_url} failed"
        )),
    }
}

/// The innermost embedding exchange: routed through the bound profile when a
/// router is installed, else through an ad-hoc provider for `base_url`. The
/// body is byte-identical on both paths: `truncate: false`, numeric-sentinel
/// keep-alive, `options{num_ctx, num_batch}` present only when configured.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn post_ollama_embed(
    operation: &str,
    base_url: &str,
    client: &reqwest::Client,
    model: &str,
    inputs: &[String],
    keep_alive: Option<&str>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
    timeout: Duration,
) -> Result<Vec<Vec<f32>>> {
    let request = magicllm::EmbeddingRequest {
        model: model.to_string(),
        inputs: inputs.to_vec(),
        truncate: false,
        keep_alive: keep_alive.map(magicllm::request_keep_alive_value),
        options: (context_tokens.is_some() || batch_tokens.is_some()).then_some(
            magicllm::EmbeddingOptions {
                context_tokens,
                batch_tokens,
            },
        ),
        // Exact millisecond deadline: callers pass a sub-second remainder
        // budget that second-quantization would silently round (up to ~1s
        // overrun or ~999ms early cut).
        timeout_ms: Some(timeout.as_millis().min(u64::MAX as u128) as u64),
        metadata: magicllm::RequestMetadata {
            operation: operation.to_string(),
            ..magicllm::RequestMetadata::default()
        },
    };
    if let Some(router) = installed_embedding_router() {
        return router
            .embed_for_operation(operation, request)
            .await
            .map_err(|error| embed_error(error, base_url))
            .map(|response| response.embeddings);
    }
    let provider = magicllm::providers::OllamaProvider::with_client(client.clone(), base_url);
    provider
        .embed(request)
        .await
        .map_err(|error| embed_error(error, base_url))
        .map(|response| response.embeddings)
}

/// Routed health probe (`GET {daemon}/api/tags` via the magicllm provider).
/// `Ok(None)` means no router is installed and the caller should use its
/// direct probe. Returned as a future so the caller owns the deadline (the
/// direct path's per-call `health_timeout` must bound this path too).
pub(crate) fn routed_health_check() -> Option<impl std::future::Future<Output = Result<bool>>> {
    let router = installed_embedding_router()?;
    Some(async move {
        router
            .router()
            .health_check_for_operation(EMBED_DOCUMENTS_OPERATION)
            .await
            .context("routed Ollama embedding health check failed")
    })
}
