use actix_web::HttpResponse;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::bridge_protocol::ExtensionRequest;
use crate::server::bridge::send_over_bridge;

/// Pull the active browser tab's contextual-writing eligibility through the
/// MV3 extension. This endpoint is intentionally request/response only; the
/// extension does not keep a realtime text-field buffer.
pub async fn contextual_assist_probe() -> HttpResponse {
    let request = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "probe_contextual_assist".to_string(),
        params: json!({}),
    };

    match send_over_bridge(request).await {
        Ok(response) if response.success => {
            HttpResponse::Ok().json(response.result.unwrap_or_else(|| json!({ "ok": true })))
        },
        Ok(response) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "eligible": false,
            "reason": "extension_error",
            "error": response.error.unwrap_or_else(|| "extension bridge request failed".to_string()),
        })),
        Err(error) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "eligible": false,
            "reason": "bridge_unavailable",
            "error": error.to_string(),
        })),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistCaptureTabRequest {
    #[serde(default)]
    pub tab_id: Option<i64>,
}

/// Capture the active browser tab on demand through the MV3 extension. This is
/// used by Contextual Assist execution so browser writing help gets the visible
/// tab, not a broad desktop screenshot.
pub async fn contextual_assist_capture_tab(
    body: Option<actix_web::web::Json<ContextualAssistCaptureTabRequest>>,
) -> HttpResponse {
    let request = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "capture_contextual_assist_tab".to_string(),
        params: json!({
            "tabId": body.and_then(|body| body.tab_id),
        }),
    };

    match send_over_bridge(request).await {
        Ok(response) if response.success => {
            HttpResponse::Ok().json(response.result.unwrap_or_else(|| json!({ "ok": true })))
        },
        Ok(response) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "captured": false,
            "reason": "extension_error",
            "error": response.error.unwrap_or_else(|| "extension bridge request failed".to_string()),
        })),
        Err(error) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "captured": false,
            "reason": "bridge_unavailable",
            "error": error.to_string(),
        })),
    }
}
