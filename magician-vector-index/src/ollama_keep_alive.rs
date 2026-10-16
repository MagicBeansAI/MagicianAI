use std::sync::{OnceLock, RwLock};

pub const DEFAULT_OLLAMA_KEEP_ALIVE: &str = "10m";
pub const DEFAULT_OLLAMA_EMBEDDING_KEEP_ALIVE: &str = "-1";

static CONFIGURED_EMBEDDING_BASE_URL: OnceLock<RwLock<Option<String>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_KEEP_ALIVE: OnceLock<RwLock<Option<String>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_NUM_PARALLEL: OnceLock<RwLock<Option<u32>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_MAX_LOADED_MODELS: OnceLock<RwLock<Option<u32>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_QUERY_TIMEOUT_MS: OnceLock<RwLock<Option<u64>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_WRITE_TIMEOUT_MS: OnceLock<RwLock<Option<u64>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_MODEL: OnceLock<RwLock<Option<String>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_CONTEXT_TOKENS: OnceLock<RwLock<Option<u32>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_BATCH_TOKENS: OnceLock<RwLock<Option<u32>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_BATCH_SIZE: OnceLock<RwLock<Option<usize>>> = OnceLock::new();
static CONFIGURED_EMBEDDING_DIMENSIONS: OnceLock<RwLock<Option<usize>>> = OnceLock::new();

pub fn set_default_ollama_keep_alive(value: Option<String>) {
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_KEEP_ALIVE
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = value.and_then(|value| normalize_keep_alive(&value));
    }
}

/// Install the embedding contract loaded from Magician configuration.
pub fn set_default_ollama_embedding_policy(
    base_url: Option<String>,
    keep_alive: Option<String>,
    num_parallel: Option<u32>,
    max_loaded_models: Option<u32>,
    query_timeout_ms: Option<u64>,
    write_timeout_ms: Option<u64>,
    model: Option<String>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
    batch_size: Option<usize>,
    dimensions: Option<usize>,
) {
    crate::embedding_scheduler::install_embedding_admission_capacity(
        num_parallel.unwrap_or(1).max(1) as usize,
    );
    crate::hybrid_result_cache::clear_live_embedding_contract();
    crate::query_vector_cache::clear_query_vector_cache();
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_BASE_URL
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = base_url
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_KEEP_ALIVE
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = keep_alive.and_then(|value| normalize_keep_alive(&value));
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_NUM_PARALLEL
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = num_parallel.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_MAX_LOADED_MODELS
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = max_loaded_models.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_QUERY_TIMEOUT_MS
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = query_timeout_ms.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_WRITE_TIMEOUT_MS
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = write_timeout_ms.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_MODEL
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_CONTEXT_TOKENS
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = context_tokens.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_BATCH_TOKENS
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = batch_tokens.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_BATCH_SIZE
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = batch_size.filter(|value| *value > 0);
    }
    if let Ok(mut guard) = CONFIGURED_EMBEDDING_DIMENSIONS
        .get_or_init(|| RwLock::new(None))
        .write()
    {
        *guard = dimensions.filter(|value| *value > 0);
    }
}

pub(crate) fn normalize_keep_alive(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// Numeric-sentinel keep-alive encoding (`-1`/`0` as JSON numbers) now lives in
// magicllm (`magicllm::request_keep_alive_value`) so the generation and
// embedding paths share one encoder.

pub(crate) fn default_embedding_base_url() -> Option<String> {
    CONFIGURED_EMBEDDING_BASE_URL
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

pub(crate) fn default_embedding_query_timeout_ms() -> Option<u64> {
    CONFIGURED_EMBEDDING_QUERY_TIMEOUT_MS
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
}

pub(crate) fn default_embedding_num_parallel() -> Option<u32> {
    CONFIGURED_EMBEDDING_NUM_PARALLEL
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
}

pub(crate) fn default_embedding_max_loaded_models() -> Option<u32> {
    CONFIGURED_EMBEDDING_MAX_LOADED_MODELS
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
}

pub(crate) fn default_embedding_write_timeout_ms() -> Option<u64> {
    CONFIGURED_EMBEDDING_WRITE_TIMEOUT_MS
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
}

pub(crate) fn default_embedding_model() -> String {
    CONFIGURED_EMBEDDING_MODEL
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_default()
}

pub(crate) fn default_embedding_dimensions() -> usize {
    CONFIGURED_EMBEDDING_DIMENSIONS
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
        .unwrap_or_default()
}

pub(crate) fn default_embedding_context_tokens() -> Option<u32> {
    CONFIGURED_EMBEDDING_CONTEXT_TOKENS
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
}

pub(crate) fn default_embedding_batch_tokens() -> Option<u32> {
    CONFIGURED_EMBEDDING_BATCH_TOKENS
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
}

pub(crate) fn default_embedding_batch_size() -> usize {
    CONFIGURED_EMBEDDING_BATCH_SIZE
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| *guard)
        .unwrap_or_default()
}

pub(crate) fn default_keep_alive() -> Option<String> {
    CONFIGURED_EMBEDDING_KEEP_ALIVE
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| guard.clone())
        .or_else(|| Some(DEFAULT_OLLAMA_EMBEDDING_KEEP_ALIVE.to_string()))
}
