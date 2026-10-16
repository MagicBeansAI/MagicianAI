//! Read-only diagnostics for the logical-context chunking framework.

use actix_web::{web, HttpResponse, Responder};
use serde::Serialize;

use magician::magician_v2::llm_chunking::global_chunk_adapter_registry;

#[derive(Debug, Serialize)]
struct ChunkAdapterInventoryResponse {
    status: &'static str,
    adapter_count: usize,
    adapters: Vec<magician::magician_v2::llm_chunking::ChunkAdapterInventoryEntry>,
}

/// `GET /api/magician/v2/llm/chunking/adapters`
///
/// This endpoint exposes registration contracts only. It cannot enable a
/// profile, invoke a provider, or write a chunked result.
pub async fn adapter_inventory_handler() -> impl Responder {
    match global_chunk_adapter_registry().read() {
        Ok(registry) => {
            let adapters = registry.inventory();
            HttpResponse::Ok().json(ChunkAdapterInventoryResponse {
                status: "registered",
                adapter_count: adapters.len(),
                adapters,
            })
        },
        Err(_) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "logical chunk adapter registry lock is poisoned"
        })),
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/llm/chunking/adapters",
        web::get().to(adapter_inventory_handler),
    );
}
