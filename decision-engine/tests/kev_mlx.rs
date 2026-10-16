//! Opt-in local MLX models through the shared action and typed endpoints.
#![cfg(feature = "mlx")]
#[path = "fixtures/action.rs"]
mod fixture;
use serde_json::json;
#[actix_web::test]
async fn kev_mlx_answers_shared_actions_locally() {
    let Ok(dir) = std::env::var("KEV_MLX_DIR") else {
        return;
    };
    fixture::verify_local(
        json!({"adapter":"kev-mlx","model":"kev-mlx-eval","model_dir":dir}),
        "kev-mlx",
    )
    .await;
}
#[actix_web::test]
async fn laya_mlx_answers_shared_actions_locally() {
    let Ok(dir) = std::env::var("LAYA_MLX_DIR") else {
        return;
    };
    fixture::verify_local(
        json!({"adapter":"laya-mlx","model":"laya-multilingual-mlx","model_dir":dir}),
        "laya-mlx",
    )
    .await;
}
