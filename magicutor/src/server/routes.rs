use actix_web::{web, HttpRequest, HttpResponse, Responder};

use super::bridge::bridge_route;
use super::cdp_proxy::{
    cdp_bind_thread_tab, cdp_browser_ws, cdp_clear_thread, cdp_list, cdp_page_ws,
    cdp_pin_thread_browser, cdp_start_browser_choice, cdp_thread_browser_info, cdp_version,
};
use super::contextual_assist::{contextual_assist_capture_tab, contextual_assist_probe};
use super::page_signals;
use super::trace_capture;

/// Drains hand out captured traffic (and, for auth, unredacted credentials):
/// Magician's own client only — no website Origin, token or trusted peer.
fn drain_request_ok(req: &HttpRequest, cfg: Option<&web::Data<crate::MagicutorConfig>>) -> bool {
    super::bridge::owner_request_ok(req, cfg.map(|cfg| cfg.get_ref()))
}

/// `GET /trace/drain/{thread_id}` — drain network traces captured for a
/// magician thread on the CDP proxy. Returns an empty array when no traces
/// are pending. Magician calls this at inner-loop boundaries and folds the
/// events into its `TraceManager` with `capture_source = "cdp_proxy"`.
async fn drain_thread_traces(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !drain_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let traces = trace_capture::drain(&thread_id);
    HttpResponse::Ok().json(serde_json::json!({
        "thread_id": thread_id,
        "traces": traces,
    }))
}

/// `GET /auth/drain/{thread_id}` — one-shot drain of unredacted auth material
/// captured for a CDP session. The response is consumed immediately by
/// Magician's encrypted secret store and must never be persisted as a trace.
async fn drain_thread_auth(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !drain_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let events = trace_capture::drain_captured_auth(&thread_id);
    HttpResponse::Ok().json(serde_json::json!({
        "thread_id": thread_id,
        "events": events,
    }))
}

/// `GET /ambient/page/drain/{thread_id}` — drain passive page identity/change
/// signals buffered for a magician thread.
async fn drain_thread_page_signals(
    req: HttpRequest,
    path: web::Path<String>,
    cfg: Option<web::Data<crate::MagicutorConfig>>,
) -> HttpResponse {
    if !drain_request_ok(&req, cfg.as_ref()) {
        return HttpResponse::Forbidden().finish();
    }
    let thread_id = path.into_inner();
    let signals = page_signals::drain(&thread_id);
    HttpResponse::Ok().json(serde_json::json!({
        "thread_id": thread_id,
        "page_signals": signals,
    }))
}

#[derive(Debug, Default, serde::Deserialize)]
struct HealthQuery {
    /// The caller's own browser id; its entry is marked `this_browser`.
    #[serde(default)]
    browser_id: Option<String>,
}

/// Health check endpoint for the Magicutor CDP proxy service. Lists every
/// browser connected over the bridge (slot 1 is the default browser) by label
/// and fingerprint — never by install id — so the extension panel can show
/// its own slot (`?browser_id=` marks it) and a full bridge.
async fn health_check(
    cfg: Option<web::Data<crate::MagicutorConfig>>,
    query: Option<web::Query<HealthQuery>>,
) -> impl Responder {
    let this_browser = query.and_then(|query| query.into_inner().browser_id);
    let browsers = super::bridge::connected_browsers(this_browser.as_deref()).await;
    let max_browsers = cfg
        .map(|cfg| cfg.bridge.max_connections.max(1))
        .unwrap_or(super::bridge::MAX_BRIDGE_CONNECTIONS);
    HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "service": "magicutor",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "cdp_proxy",
        "extension_connected": !browsers.is_empty(),
        "browsers": browsers,
        "max_browsers": max_browsers,
    }))
}

/// Configure all HTTP routes
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health_check))
        .service(web::resource("/bridge/native").route(web::get().to(bridge_route)))
        // CDP-compatible proxy — lets `agent-browser connect 3003` work via the bridge
        .route("/json/version", web::get().to(cdp_version))
        .route("/json", web::get().to(cdp_list))
        .route("/json/list", web::get().to(cdp_list))
        .service(web::resource("/devtools/browser/{id}").route(web::get().to(cdp_browser_ws)))
        .service(web::resource("/devtools/page/{tab_id}").route(web::get().to(cdp_page_ws)))
        .service(
            web::resource("/cdp/threads/{thread_id}").route(web::delete().to(cdp_clear_thread)),
        )
        .service(
            web::resource("/cdp/threads/{thread_id}/bind-tab")
                .route(web::post().to(cdp_bind_thread_tab)),
        )
        .service(
            web::resource("/cdp/threads/{thread_id}/browser")
                .route(web::put().to(cdp_pin_thread_browser))
                .route(web::get().to(cdp_thread_browser_info)),
        )
        .service(
            web::resource("/cdp/threads/{thread_id}/choose")
                .route(web::post().to(cdp_start_browser_choice)),
        )
        // Owner-authenticated pairing management (bridge token).
        .service(
            web::resource("/bridge/browsers")
                .route(web::get().to(super::bridge::bridge_browsers_list)),
        )
        .service(
            web::resource("/bridge/browsers/{fingerprint}/accept")
                .route(web::post().to(super::bridge::bridge_browser_accept)),
        )
        .service(
            web::resource("/bridge/browsers/{fingerprint}/revoke")
                .route(web::post().to(super::bridge::bridge_browser_revoke)),
        )
        .service(
            web::resource("/contextual-assist/probe")
                .route(web::post().to(contextual_assist_probe)),
        )
        .service(
            web::resource("/contextual-assist/capture-tab")
                .route(web::post().to(contextual_assist_capture_tab)),
        )
        // API mining: drain network traces buffered for a magician thread.
        .service(
            web::resource("/trace/drain/{thread_id}").route(web::get().to(drain_thread_traces)),
        )
        .service(web::resource("/auth/drain/{thread_id}").route(web::get().to(drain_thread_auth)))
        .service(
            web::resource("/ambient/page/drain/{thread_id}")
                .route(web::get().to(drain_thread_page_signals)),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{test, App};

    /// Magicutor sends no CORS headers: a website's script cannot read any
    /// response (the extension reads them through its host permissions, the
    /// desktop and Magician send no Origin). Preflights get no grant either.
    #[actix_web::test]
    async fn no_route_grants_cors_to_a_website() {
        let app = test::init_service(App::new().configure(configure_routes)).await;
        for request in [
            test::TestRequest::get()
                .uri("/health")
                .insert_header(("origin", "https://evil.example"))
                .to_request(),
            test::TestRequest::get()
                .uri("/json/version")
                .insert_header(("origin", "https://evil.example"))
                .to_request(),
            test::TestRequest::default()
                .method(actix_web::http::Method::OPTIONS)
                .uri("/contextual-assist/capture-tab")
                .insert_header(("origin", "https://evil.example"))
                .insert_header(("access-control-request-method", "POST"))
                .to_request(),
        ] {
            let response = test::call_service(&app, request).await;
            assert!(response
                .headers()
                .get("access-control-allow-origin")
                .is_none());
            assert!(response
                .headers()
                .get("access-control-allow-credentials")
                .is_none());
        }
    }

    #[actix_web::test]
    async fn health_names_browsers_without_ids_or_keys() {
        let app = test::init_service(App::new().configure(configure_routes)).await;
        let request = test::TestRequest::get().uri("/health").to_request();
        let response: serde_json::Value = test::call_and_read_body_json(&app, request).await;
        let rendered = response.to_string();
        assert!(!rendered.contains("browser_key"));
        for browser in response["browsers"].as_array().unwrap() {
            assert!(browser.get("id").is_none());
            assert!(browser.get("fingerprint").is_some());
        }
    }

    #[actix_web::test]
    async fn health_reports_extension_connection_state() {
        let app = test::init_service(App::new().configure(configure_routes)).await;
        let request = test::TestRequest::get().uri("/health").to_request();
        let response: serde_json::Value = test::call_and_read_body_json(&app, request).await;
        assert_eq!(response["status"], "healthy");
        assert!(response["extension_connected"].is_boolean());
        assert!(response["browsers"].is_array());
        assert_eq!(
            response["max_browsers"],
            serde_json::json!(super::super::bridge::MAX_BRIDGE_CONNECTIONS)
        );
    }
}
