//! HTTP API for the LLM dispatch queue viewer.
//!
//! Endpoints:
//! - `GET  /api/llm/queue/snapshot` — returns `QueueSnapshot` JSON.
//! - `POST /api/llm/queue/cancel/{job_id}` — cancels a pending job.
//! - `POST /api/llm/queue/resubmit/{job_id}` — resubmits a failed job
//!   (currently returns 501; callers must rebuild requests via Phase 5+).

use std::sync::Arc;

use actix_web::{web, HttpResponse, Responder};
use magicllm::{JobId, LlmDispatchQueue};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CancelRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// `GET /api/llm/queue/snapshot`
pub async fn snapshot_handler(queue: web::Data<Arc<LlmDispatchQueue>>) -> impl Responder {
    HttpResponse::Ok().json(queue.snapshot())
}

/// `POST /api/llm/queue/cancel/{job_id}`
pub async fn cancel_handler(
    queue: web::Data<Arc<LlmDispatchQueue>>,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> impl Responder {
    let job_id = JobId::from(path.into_inner());
    let reason = body
        .reason
        .clone()
        .unwrap_or_else(|| "viewer_cancel".to_string());
    let cancelled = queue.cancel_job(&job_id, reason);
    HttpResponse::Ok().json(serde_json::json!({ "cancelled": cancelled }))
}

/// `POST /api/llm/queue/resubmit/{job_id}`
pub async fn resubmit_handler(
    queue: web::Data<Arc<LlmDispatchQueue>>,
    path: web::Path<String>,
) -> impl Responder {
    let job_id = JobId::from(path.into_inner());
    match queue.resubmit_failed(&job_id).await {
        Ok(new_id) => HttpResponse::Ok().json(serde_json::json!({ "job_id": new_id })),
        Err(err) => HttpResponse::NotImplemented().json(serde_json::json!({
            "error": err.to_string()
        })),
    }
}

/// Register the routes under the supplied scope.
pub fn configure(cfg: &mut actix_web::web::ServiceConfig) {
    cfg.service(
        web::scope("/api/llm/queue")
            .route("/snapshot", web::get().to(snapshot_handler))
            .route("/cancel/{job_id}", web::post().to(cancel_handler))
            .route("/resubmit/{job_id}", web::post().to(resubmit_handler)),
    );
}
