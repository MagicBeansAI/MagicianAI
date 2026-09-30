//! Opt-in local ONNX models through the shared action and typed endpoints.
#[path = "fixtures/action.rs"]
mod fixture;
use serde_json::json;
#[actix_web::test]
async fn laya_onnx_answers_shared_actions_locally() {
    let Ok(dir) = std::env::var("LAYA_MODEL_DIR") else {
        return;
    };
    fixture::verify_local(
        json!({"adapter":"laya-onnx","model":"laya-multilingual-int8","model_dir":dir}),
        "laya-onnx",
    )
    .await;
}
#[actix_web::test]
async fn kev_onnx_answers_shared_actions_locally() {
    let Ok(dir) = std::env::var("KEV_MODEL_DIR") else {
        return;
    };
    let threads = std::env::var("KEV_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(magician_decision::adapters::kev_onnx::default_threads);
    fixture::verify_local(
        json!({"adapter":"kev-onnx","model":"kev-0.8b-q8f32","model_dir":dir,"threads":threads}),
        "kev-onnx",
    )
    .await;
}
