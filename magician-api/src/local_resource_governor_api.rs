//! HTTP API for the local resource governor.
//!
//! Endpoint:
//! - `GET /api/local-resource-governor/snapshot` — returns the current
//!   process-local resource pressure snapshot. Live agent loops are hard-capped
//!   by `admit_agent_loop` (default 50; `0` is observe-only rollback). This
//!   endpoint itself does not admit or reject work.

use actix_web::{web, HttpResponse, Responder};

/// `GET /api/local-resource-governor/snapshot`
pub async fn snapshot_handler() -> impl Responder {
    let hol = magician_vector_index::retrieval_hol_snapshot();
    HttpResponse::Ok().json(
        magician::magician_v2::local_resource_governor::snapshot_with_retrieval_hol(
            magician_vector_index::memory_temperature::memory_temperature_overlay_max_lock_wait_ms(
            ),
            magician::magician_v2::local_resource_governor::RetrievalHolGauges {
                journal_lock_waiters: hol.journal_lock_waiters,
                journal_lock_max_wait_ms: hol.journal_lock_max_wait_ms,
                embedding_waiting_foreground: hol.embedding_waiting_foreground,
                embedding_active_foreground: hol.embedding_active_foreground,
                embedding_waiting_writes: hol.embedding_waiting_writes,
                lance_in_flight: hol.lance_in_flight,
                lance_waiters: hol.lance_waiters,
                lance_timeouts: hol.lance_timeouts as usize,
                lance_cancels: hol.lance_cancels as usize,
                read_snapshot_hits: hol.read_snapshot_hits as usize,
                read_snapshot_hydrates: hol.read_snapshot_hydrates as usize,
                journal_lock_acquisitions: hol.journal_lock_acquisitions as usize,
                result_cache_hits: hol.result_cache_hits as usize,
                result_cache_misses: hol.result_cache_misses as usize,
                result_cache_stores: hol.result_cache_stores as usize,
                result_cache_evictions: hol.result_cache_evictions as usize,
                result_cache_entries: hol.result_cache_entries,
                result_cache_bytes: hol.result_cache_bytes,
                result_cache_waiters: hol.result_cache_waiters,
                lance_table_pool_hits: hol.lance_table_pool_hits as usize,
                lance_table_pool_misses: hol.lance_table_pool_misses as usize,
                lance_table_pool_idle: hol.lance_table_pool_idle,
                embedding_oldest_write_wait_ms: hol.embedding_oldest_write_wait_ms,
                query_vector_cache_hits: hol.query_vector_cache_hits as usize,
                query_vector_cache_misses: hol.query_vector_cache_misses as usize,
                query_vector_cache_entries: hol.query_vector_cache_entries,
                query_vector_cache_bytes: hol.query_vector_cache_bytes,
                query_embed_batch_physical_calls: hol.query_embed_batch_physical_calls as usize,
                query_embed_batch_logical_queries: hol.query_embed_batch_logical_queries as usize,
                query_embed_batch_max_fill: hol.query_embed_batch_max_fill,
                query_embed_batch_waiters: hol.query_embed_batch_waiters,
                vector_search_mode: hol.vector_search_mode,
                ivf_present: usize::from(hol.ivf_present),
                ivf_disk_bytes: hol.ivf_disk_bytes as usize,
                ivf_generation: hol.ivf_generation as usize,
                ann_queries: hol.ann_queries as usize,
                ann_fallbacks: hol.ann_fallbacks as usize,
                ann_shadow_compares: hol.ann_shadow_compares as usize,
                ann_shadow_mismatches: hol.ann_shadow_mismatches as usize,
                ann_shadow_recall_milles: hol.ann_shadow_recall_milles,
            },
        ),
    )
}

/// Register the routes under the supplied scope.
pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(
        web::scope("/api/local-resource-governor")
            .route("/snapshot", web::get().to(snapshot_handler)),
    );
}
