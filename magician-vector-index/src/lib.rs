//! `magician-vector-index` isolates the LanceDB / Arrow memory index from
//! the rest of `magician`. Modules previously living under
//! `magician::magician_v2::agents` have been moved here so that the heavy
//! transitive dependency graph (LanceDB pulls in ~100 crates) only has to be
//! compiled once and only by code that actually touches the derived index.
//!
//! Trait contracts (`storage_trait`, `definition_trait`) let magician keep
//! ownership of its concrete `AgentStorage` / `AgentDefinitionStore` types
//! while still allowing memory-index / memory-candidate logic to consume
//! them generically.

pub mod boundary_tags;
pub mod definition_trait;
pub mod embedding_router;
pub mod embedding_scheduler;
pub mod episode_candidates;
pub mod hol_stats;
pub mod hybrid_result_cache;
pub mod key_encoding;
mod lance_runtime;
pub mod lance_table_pool;
mod lancedb_scheduler;
pub mod memory_candidates;
pub mod memory_hot_projections;
pub mod memory_index;
pub mod memory_record;
pub mod memory_renderer;
pub mod memory_temperature;
pub mod memory_tier_health;
pub mod memory_tiers;
pub mod ollama_keep_alive;
pub mod query_embed_batcher;
pub mod query_vector_cache;
pub mod retrieval_scope;
pub mod storage_trait;
pub mod vector_search_mode;
pub mod vector_toolkit;

pub use embedding_router::{install_embedding_router, uninstall_embedding_router};
pub use embedding_scheduler::{
    acquire_embedding_permit, acquire_embedding_permit_with_capacity,
    acquire_embedding_permit_with_capacity_and_wait_signal, embedding_admission_stats,
    install_embedding_admission_capacity, installed_embedding_admission_capacity,
    query_embedding_coalescer_stats, reset_query_embedding_coalescer_for_tests,
    validate_embedding_admission_capacity, EmbeddingAdmissionStats, EmbeddingPermit,
    EmbeddingPriority, QueryEmbeddingCoalescerStats, EMBEDDING_ADMISSION_TEST_LOCK,
};
pub use hol_stats::{retrieval_hol_snapshot, RetrievalHolSnapshot};
pub use hybrid_result_cache::{
    hybrid_result_cache_enabled, hybrid_result_cache_settings, install_hybrid_result_cache,
    invalidate_hybrid_results_for_lancedb_dir, invalidate_hybrid_results_for_root,
    published_hybrid_index_generation, reset_hybrid_result_cache_for_tests,
    HybridResultCacheSettings, HYBRID_RESULT_CACHE_TEST_LOCK, MEMORY_HYBRID_SCORE_CONTRACT_VERSION,
};
pub use lance_runtime::{
    build_lance_runtime, build_lance_runtime_with_threads, configured_lance_runtime_mode,
    lance_runtime_mode, lance_runtime_mode_label, set_lance_runtime_handle,
    should_build_lance_runtime, LanceRuntimeMode, LANCE_THREAD_NAME, LANCE_WORKER_THREADS,
};
pub use lance_table_pool::{
    install_lance_table_pool, invalidate_lance_table_pool, lance_table_pool_enabled,
    reset_lance_table_pool_for_tests, LanceTablePoolSettings, LANCE_TABLE_POOL_TEST_LOCK,
};
pub use memory_index::{
    configure_lance_search_concurrency, keep_embedding_model_resident,
    resolve_lance_search_concurrency, DEFAULT_LANCE_SEARCH_CONCURRENCY,
};
pub use memory_record::V3MemoryTierRecord;
pub use ollama_keep_alive::{
    set_default_ollama_embedding_policy, set_default_ollama_keep_alive, DEFAULT_OLLAMA_KEEP_ALIVE,
};
pub use query_embed_batcher::{
    install_query_embed_batch, query_embed_batch_enabled, query_embed_batch_settings,
    reset_query_embed_batch_for_tests, QueryEmbedBatchSettings, QUERY_EMBED_BATCH_TEST_LOCK,
};
pub use query_vector_cache::{
    clear_query_vector_cache, install_query_vector_cache, query_vector_cache_enabled,
    query_vector_cache_settings, reset_query_vector_cache_for_tests, QueryVectorCacheSettings,
    QUERY_VECTOR_CACHE_TEST_LOCK,
};
pub use retrieval_scope::{
    label_for_item, label_from_metadata, label_from_token, stamp_engagement_scope, ContextLabel,
    RetrievalScope, ScopeDecision, ENGAGEMENT_SCOPE_KEY, ENGAGEMENT_TOKEN_PREFIX,
    MEETING_TOKEN_PREFIX, NEUTRAL_TOKEN,
};
pub use vector_search_mode::{
    install_vector_search, recall_at_k, reset_vector_search_for_tests,
    served_hybrid_scoring_contract, vector_search_mode, vector_search_mode_label,
    vector_search_ranking_epoch, vector_search_settings, vector_search_snapshot, VectorSearchMode,
    VectorSearchPlan, VectorSearchRecall, VectorSearchSettings,
    DEFAULT_VECTOR_SEARCH_CANDIDATE_MULTIPLIER, DEFAULT_VECTOR_SEARCH_MIN_ROWS,
    DEFAULT_VECTOR_SEARCH_NPROBES, MAX_VECTOR_SEARCH_CANDIDATE_MULTIPLIER, VECTOR_SEARCH_TEST_LOCK,
};
pub use vector_toolkit::{
    normalize_url, OllamaEmbedder, OllamaEmbedderConfig, RankOutput, RankResult, SearchMode,
    VectorCluster, VectorHit, VectorItem, VectorTable, DEFAULT_OLLAMA_URL,
};
