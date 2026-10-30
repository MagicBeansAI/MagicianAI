use actix_web::{web, HttpRequest, HttpResponse};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Mutex;
use uuid::Uuid;

use crate::bridge_protocol::ExtensionRequest;
use crate::server::bridge::{self, send_over_bridge_tracked, BridgeRoute, RoutedResponse};
use crate::types::Result;

/// The browser whose answer the last probe returned. A capture that names no
/// browser goes back to it: the probe and the capture describe the same tab,
/// and tab ids are only unique within one browser.
static LAST_PROBED_BROWSER: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

#[derive(Debug, Default, Deserialize)]
pub struct ContextualAssistProbeQuery {
    /// Optional browser selector (install id or label).
    #[serde(default)]
    pub browser: Option<String>,
}

/// The probe reads, and the capture returns, the owner's visible page: only
/// the desktop's own client (no website `Origin`; a trusted peer or the
/// configured token) may call them. See `bridge::owner_request_ok`.
fn owner_gate(req: &HttpRequest, cfg: Option<&web::Data<crate::MagicutorConfig>>) -> bool {
    let allowed = bridge::owner_request_ok(req, cfg.map(|cfg| cfg.get_ref()));
    if !allowed {
        tracing::warn!(
            "contextual assist refused {} from {:?} (untrusted peer, missing token or website Origin)",
            req.path(),
            req.peer_addr()
        );
    }
    allowed
}

fn refused(flag: &str) -> HttpResponse {
    let mut body = json!({ "ok": false, "reason": "forbidden" });
    body[flag] = json!(false);
    HttpResponse::Forbidden().json(body)
}

fn probe_request() -> ExtensionRequest {
    ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "probe_contextual_assist".to_string(),
        params: json!({}),
    }
}

/// Rank a probe answer: the browser whose window has OS focus is the one the
/// user is writing in; an eligible field beats an ineligible page.
fn probe_rank(result: &Result<RoutedResponse>) -> Option<u8> {
    let routed = result.as_ref().ok().filter(|r| r.response.success)?;
    let body = routed.response.result.as_ref();
    let flag = |key: &str| {
        body.and_then(|v| v.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Some(u8::from(flag("windowFocused")) * 2 + u8::from(flag("eligible")))
}

/// Pick the best answer; ties go to the earlier slot (the default browser).
fn best_probe(results: Vec<Result<RoutedResponse>>) -> Option<Result<RoutedResponse>> {
    let mut best: Option<(u8, Result<RoutedResponse>)> = None;
    let mut first_failure: Option<Result<RoutedResponse>> = None;
    for result in results {
        match probe_rank(&result) {
            Some(rank) if best.as_ref().map_or(true, |(top, _)| rank > *top) => {
                best = Some((rank, result));
            },
            Some(_) => {},
            None => {
                if first_failure.is_none() {
                    first_failure = Some(result);
                }
            },
        }
    }
    best.map(|(_, result)| result).or(first_failure)
}

/// Pull the active browser tab's contextual-writing eligibility through the
/// MV3 extension. This endpoint is intentionally request/response only; the
/// extension does not keep a realtime text-field buffer.
///
/// Several browsers can share the bridge. Without `?browser=` every connected
/// browser is probed and the one whose window has focus answers; the result
/// carries `browser` (the answering browser's fingerprint) so a follow-up
/// capture reaches the same browser.
pub async fn contextual_assist_probe(
    req: HttpRequest,
    query: Option<web::Query<ContextualAssistProbeQuery>>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !owner_gate(&req, cfg.as_ref()) {
        return refused("eligible");
    }
    let selector = query
        .and_then(|query| query.into_inner().browser)
        .map(|selector| selector.trim().to_string())
        .filter(|selector| !selector.is_empty());
    let browser_ids: Vec<String> = match selector.as_deref() {
        Some(selector) => match bridge::select_browser(Some(selector)).await {
            Ok(Some(browser_id)) => vec![browser_id],
            Ok(None) => Vec::new(),
            Err(error) => {
                return HttpResponse::BadGateway().json(json!({
                    "ok": false,
                    "eligible": false,
                    "reason": error.code(),
                    "error": error.to_string(),
                }))
            },
        },
        None => bridge::connected_browsers(None)
            .await
            .into_iter()
            .map(|browser| browser.id)
            .collect(),
    };
    let routes: Vec<BridgeRoute> = if browser_ids.is_empty() {
        // Nothing connected: the default route reports "Bridge not connected".
        vec![BridgeRoute::Default]
    } else {
        browser_ids.into_iter().map(BridgeRoute::Browser).collect()
    };
    let results = futures::future::join_all(
        routes
            .iter()
            .map(|route| send_over_bridge_tracked(route, probe_request())),
    )
    .await;

    match best_probe(results) {
        Some(Ok(routed)) if routed.response.success => {
            if let Ok(mut last) = LAST_PROBED_BROWSER.lock() {
                *last = Some(routed.browser_id.clone());
            }
            let mut result = routed
                .response
                .result
                .unwrap_or_else(|| json!({ "ok": true }));
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "browser".to_string(),
                    json!(bridge::browser_fingerprint(&routed.browser_id)),
                );
            }
            HttpResponse::Ok().json(result)
        },
        Some(Ok(routed)) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "eligible": false,
            "reason": "extension_error",
            "error": routed.response.error.unwrap_or_else(|| "extension bridge request failed".to_string()),
        })),
        Some(Err(error)) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "eligible": false,
            "reason": "bridge_unavailable",
            "error": error.to_string(),
        })),
        None => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "eligible": false,
            "reason": "bridge_unavailable",
            "error": "Bridge not connected",
        })),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualAssistCaptureTabRequest {
    #[serde(default)]
    pub tab_id: Option<i64>,
    /// Browser the tab belongs to (fingerprint, install id or label); the
    /// probe result's `browser`.
    #[serde(default, alias = "browser_id", alias = "browser")]
    pub browser_id: Option<String>,
}

/// Where a capture goes: the named browser, else the browser that answered
/// the last probe, else the default browser. A tab id from a probe whose
/// browser has since disconnected is refused, since in another browser the
/// same id names a different tab.
async fn capture_route(
    selector: Option<&str>,
    has_tab_id: bool,
) -> std::result::Result<BridgeRoute, String> {
    if let Some(selector) = selector {
        return match bridge::select_browser(Some(selector)).await {
            Ok(Some(browser_id)) => Ok(BridgeRoute::Browser(browser_id)),
            Ok(None) => Ok(BridgeRoute::Default),
            Err(error) => Err(error.to_string()),
        };
    }
    let last_probed = LAST_PROBED_BROWSER
        .lock()
        .ok()
        .and_then(|last| (*last).clone());
    let Some(browser_id) = last_probed else {
        return Ok(BridgeRoute::Default);
    };
    match bridge::select_browser(Some(&browser_id)).await {
        Ok(Some(connected)) if connected == browser_id => Ok(BridgeRoute::Browser(connected)),
        _ if has_tab_id => Err(format!(
            "browser [{}] that answered the probe is no longer connected",
            bridge::browser_fingerprint(&browser_id)
        )),
        _ => Ok(BridgeRoute::Default),
    }
}

/// Capture the active browser tab on demand through the MV3 extension. This is
/// used by Contextual Assist execution so browser writing help gets the visible
/// tab, not a broad desktop screenshot.
pub async fn contextual_assist_capture_tab(
    req: HttpRequest,
    body: Option<actix_web::web::Json<ContextualAssistCaptureTabRequest>>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !owner_gate(&req, cfg.as_ref()) {
        return refused("captured");
    }
    let body = body.map(|body| body.into_inner());
    let tab_id = body.as_ref().and_then(|body| body.tab_id);
    let selector = body
        .as_ref()
        .and_then(|body| body.browser_id.as_deref())
        .map(str::trim)
        .filter(|selector| !selector.is_empty());
    let route = match capture_route(selector, tab_id.is_some()).await {
        Ok(route) => route,
        Err(error) => {
            return HttpResponse::BadGateway().json(json!({
                "ok": false,
                "captured": false,
                "reason": "bridge_unavailable",
                "error": error,
            }))
        },
    };
    let request = ExtensionRequest {
        request_id: Uuid::new_v4().to_string(),
        action: "capture_contextual_assist_tab".to_string(),
        params: json!({
            "tabId": tab_id,
        }),
    };

    match send_over_bridge_tracked(&route, request).await {
        Ok(routed) if routed.response.success => {
            let mut result = routed
                .response
                .result
                .unwrap_or_else(|| json!({ "ok": true }));
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "browser".to_string(),
                    json!(bridge::browser_fingerprint(&routed.browser_id)),
                );
            }
            HttpResponse::Ok().json(result)
        },
        Ok(routed) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "captured": false,
            "reason": "extension_error",
            "error": routed.response.error.unwrap_or_else(|| "extension bridge request failed".to_string()),
        })),
        Err(error) => HttpResponse::BadGateway().json(json!({
            "ok": false,
            "captured": false,
            "reason": "bridge_unavailable",
            "error": error.to_string(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge_protocol::ExtensionResponse;
    use crate::types::ExecutionError;

    fn answer(browser_id: &str, body: Value) -> Result<RoutedResponse> {
        Ok(RoutedResponse {
            browser_id: browser_id.to_string(),
            response: ExtensionResponse::success("probe".to_string(), body),
        })
    }

    fn picked(result: Option<Result<RoutedResponse>>) -> String {
        result.unwrap().unwrap().browser_id
    }

    async fn assist_app() -> impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    > {
        let mut config = crate::MagicutorConfig::default();
        config.bridge.auth_token.clear();
        actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(config))
                .route(
                    "/contextual-assist/probe",
                    web::post().to(contextual_assist_probe),
                )
                .route(
                    "/contextual-assist/capture-tab",
                    web::post().to(contextual_assist_capture_tab),
                ),
        )
        .await
    }

    #[actix_web::test]
    async fn a_website_cannot_probe_or_capture_the_owners_page() {
        use actix_web::http::StatusCode;
        let app = assist_app().await;
        for uri in ["/contextual-assist/probe", "/contextual-assist/capture-tab"] {
            let request = actix_web::test::TestRequest::post()
                .uri(uri)
                .peer_addr("127.0.0.1:5000".parse().unwrap())
                .insert_header(("origin", "https://evil.example"))
                .to_request();
            let response = actix_web::test::call_service(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{uri} from a website"
            );
            assert!(response
                .headers()
                .get("access-control-allow-origin")
                .is_none());

            let request = actix_web::test::TestRequest::post()
                .uri(uri)
                .peer_addr("192.168.1.20:5000".parse().unwrap())
                .to_request();
            let response = actix_web::test::call_service(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{uri} from the LAN"
            );

            // The desktop's reqwest client: loopback, no Origin. It passes the
            // gate (and then finds no browser connected: 502).
            let request = actix_web::test::TestRequest::post()
                .uri(uri)
                .peer_addr("127.0.0.1:5000".parse().unwrap())
                .to_request();
            let response = actix_web::test::call_service(&app, request).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_GATEWAY,
                "{uri} from the desktop"
            );
        }
    }

    #[test]
    fn the_focused_browser_answers_a_multi_browser_probe() {
        let results = vec![
            answer(
                "chrome",
                json!({ "ok": true, "eligible": true, "windowFocused": false }),
            ),
            answer(
                "comet",
                json!({ "ok": true, "eligible": false, "windowFocused": true }),
            ),
            answer(
                "dia",
                json!({ "ok": true, "eligible": true, "windowFocused": true }),
            ),
        ];
        assert_eq!(picked(best_probe(results)), "dia");
    }

    #[test]
    fn ties_go_to_the_default_browser_and_failures_only_when_nothing_answered() {
        let results = vec![
            Err(ExecutionError::BridgeError("gone".to_string())),
            answer("chrome", json!({ "ok": true, "eligible": false })),
            answer("comet", json!({ "ok": true, "eligible": false })),
        ];
        assert_eq!(picked(best_probe(results)), "chrome");
        let failed = best_probe(vec![Err(ExecutionError::BridgeError("gone".to_string()))]);
        assert!(matches!(failed, Some(Err(_))));
    }
}
