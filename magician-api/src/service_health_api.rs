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
//! - `version` is the Magician CORE version (`magician::VERSION`). Until
//!   2026-10-25 it was this crate's own version, so every app's "Magician
//!   backend" row showed the API crate (0.3.x) instead of Magician (0.7.x);
//!   that number moved to `api_version`.
//! - `versions` names each part of the stack: `magician`, `magician_api`, and
//!   `magicutor` / `desktop` as those services report themselves (`null` when
//!   offline or too old to say).
//! - `magician` / `magicutor_status` / `tauri_status`; the iOS pill reads
//!   `magicutor_status`/`tauri_status == "healthy"` (`"offline"` otherwise).
//!
//! - `tools.ripgrep` — whether the `grep` and `glob` agent tools have a
//!   ripgrep (`status`: `ok` / `missing`) and which one (`source`: `packaged`,
//!   `skillshub`, `override`, `system`, `missing`; see
//!   `runtime_core::process::resolve_ripgrep`). A missing one does not change
//!   `status`: the service is up, but file search will refuse.
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
    let magicutor_probe = async {
        match cfg.as_ref() {
            Some(cfg) => probe_magicutor(&cfg.magicutor_base_url).await,
            None => MagicutorProbe::default(),
        }
    };
    let host_gateway = HostAutomationProvider::new(host_gateway_url_from_env());
    let gateway_probe = host_gateway.gateway_probe();
    let (magicutor, gateway) = tokio::join!(magicutor_probe, gateway_probe);
    let (_, ripgrep) = runtime_core::process::resolve_ripgrep();

    HttpResponse::Ok().json(json!({
        "status": "ok",
        "service": "magician",
        "version": magician::VERSION,
        "api_version": env!("CARGO_PKG_VERSION"),
        "versions": {
            "magician": magician::VERSION,
            "magician_api": env!("CARGO_PKG_VERSION"),
            "magicutor": magicutor.version,
            "desktop": gateway.version,
        },
        "magician": "healthy",
        "magicutor_status": health_word(magicutor.ok),
        "tauri_status": health_word(gateway.available),
        "tools": {
            "ripgrep": {
                "status": if ripgrep == runtime_core::process::RipgrepSource::Missing { "missing" } else { "ok" },
                "source": ripgrep.as_str(),
            },
        },
    }))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct MagicutorProbe {
    ok: bool,
    version: Option<String>,
}

fn health_word(ok: bool) -> &'static str {
    if ok {
        "healthy"
    } else {
        "offline"
    }
}

/// Best-effort GET `{base}/health` (Magicutor serves its own `/health`, whose
/// JSON carries its `version`).
async fn probe_magicutor(base_url: &str) -> MagicutorProbe {
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    match reqwest::Client::new()
        .get(&url)
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            let version = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|body| reported_version(&body));
            MagicutorProbe { ok: true, version }
        },
        _ => MagicutorProbe::default(),
    }
}

/// A service's self-reported `version`, when it is a short plain string.
fn reported_version(body: &serde_json::Value) -> Option<String> {
    body.get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 40)
        .map(str::to_string)
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
        // `version` is Magician's own, not this API crate's.
        assert_eq!(json["version"], magician::VERSION);
        assert_eq!(json["api_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(json["versions"]["magician"], magician::VERSION);
        assert_eq!(json["versions"]["magician_api"], env!("CARGO_PKG_VERSION"));
        assert!(json["versions"]["magicutor"].is_null());

        // New service fields; magicutor is deterministically offline with no cfg.
        assert_eq!(json["magician"], "healthy");
        let (_, ripgrep) = runtime_core::process::resolve_ripgrep();
        assert_eq!(json["tools"]["ripgrep"]["source"], ripgrep.as_str());
        assert!(matches!(
            json["tools"]["ripgrep"]["status"].as_str(),
            Some("ok" | "missing")
        ));
        assert_eq!(json["magicutor_status"], "offline");
        // tauri depends on the ambient gateway; assert only that it is reported.
        assert!(matches!(
            json["tauri_status"].as_str(),
            Some("healthy") | Some("offline")
        ));
    }

    #[test]
    fn reported_version_takes_only_a_short_plain_string() {
        assert_eq!(
            reported_version(&json!({"version": "0.1.94"})).as_deref(),
            Some("0.1.94")
        );
        assert_eq!(reported_version(&json!({"version": " "})), None);
        assert_eq!(reported_version(&json!({"version": 3})), None);
        assert_eq!(reported_version(&json!({"version": "x".repeat(41)})), None);
        assert_eq!(reported_version(&json!({})), None);
    }

    #[test]
    fn health_word_maps_bool() {
        assert_eq!(health_word(true), "healthy");
        assert_eq!(health_word(false), "offline");
    }
}
