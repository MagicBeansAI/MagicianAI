//! Aggregated `GET /health` for the runtime stack.
//!
//! Extends plain magician liveness with best-effort reachability of its two
//! dependencies — Magicutor and the desktop (Tauri) host gateway — so the iOS
//! health pill (`Magios/HealthViewModel`) can show real Magicutor/Tauri status
//! instead of always-orange dots.
//!
//! Response fields are ADDITIVE and backward compatible:
//! - `status` / `service` / `version` are preserved for existing consumers
//!   (iOS `VersionInfo`, the desktop tray's own health aggregator which reads
//!   `status`, and the unified-ui vite dev-proxy readiness probe).
//! - `magician` / `magicutor_status` / `tauri_status` are new; the iOS pill reads
//!   `magicutor_status`/`tauri_status == "healthy"` (`"offline"` otherwise).
//!
//! Both dependency probes are short-timeout and fail-soft: a slow or down
//! dependency yields `"offline"`, never a hung or failing health response.

use actix_web::{web, HttpResponse};
use serde_json::json;
use std::time::Duration;

use magician_media::media_rails::providers::{host_gateway_url_from_env, HostAutomationProvider};

/// How long each dependency probe may take before it is treated as offline.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Per-worker config for the aggregated health probe. The Magicutor base URL is
/// `magician_config.execution.magicutor_base_url`, cloned into app data at
/// server construction.
#[derive(Clone)]
pub struct ServiceHealthConfig {
    pub magicutor_base_url: String,
}

/// `GET /health` — magician answered (so it is alive) plus best-effort probes of
/// Magicutor (`{magicutor_base_url}/health`) and the desktop Tauri host gateway
/// (the proxy + bridge; only reachable when the desktop app is running and
/// `MAGICIAN_HOST_GATEWAY_URL` points at it, so in a server deployment it is
/// legitimately offline).
///
/// `cfg` is optional so a missing registration degrades to "magicutor offline"
/// rather than a 500.
pub async fn service_health_handler(cfg: Option<web::Data<ServiceHealthConfig>>) -> HttpResponse {
    let magicutor_ok = match cfg.as_ref() {
        Some(cfg) => probe_magicutor(&cfg.magicutor_base_url).await,
        None => false,
    };

    let tauri_ok = HostAutomationProvider::new(host_gateway_url_from_env())
        .gateway_available()
        .await;

    HttpResponse::Ok().json(json!({
        "status": "ok",
        "service": "magician",
        "version": env!("CARGO_PKG_VERSION"),
        "magician": "healthy",
        "magicutor_status": health_word(magicutor_ok),
        "tauri_status": health_word(tauri_ok),
    }))
}

fn health_word(ok: bool) -> &'static str {
    if ok {
        "healthy"
    } else {
        "offline"
    }
}

/// Best-effort GET `{base}/health` (Magicutor serves its own `/health`).
async fn probe_magicutor(base_url: &str) -> bool {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    match reqwest::Client::new()
        .get(&url)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use actix_web::body::to_bytes;
    use actix_web::http::StatusCode;

    /// With no Magicutor config and (in a test env) an unreachable host gateway,
    /// the handler must still return 200 with the additive fields, degrading each
    /// dependency to "offline" rather than hanging or erroring.
    #[actix_web::test]
    async fn health_is_fail_soft_and_additive() {
        let resp = service_health_handler(None).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = to_bytes(resp.into_body()).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Backward-compatible fields existing consumers rely on.
        assert_eq!(json["status"], "ok");
        assert_eq!(json["service"], "magician");
        assert!(json["version"].is_string());

        // New service fields; magicutor is deterministically offline with no cfg.
        assert_eq!(json["magician"], "healthy");
        assert_eq!(json["magicutor_status"], "offline");
        // tauri depends on the ambient gateway; assert only that it is reported.
        assert!(matches!(
            json["tauri_status"].as_str(),
            Some("healthy") | Some("offline")
        ));
    }

    #[test]
    fn health_word_maps_bool() {
        assert_eq!(health_word(true), "healthy");
        assert_eq!(health_word(false), "offline");
    }
}
