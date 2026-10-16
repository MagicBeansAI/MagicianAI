use actix_web::{web, HttpResponse, Responder};

use super::bridge::bridge_route;
use super::cdp_proxy::{
    cdp_bind_thread_tab, cdp_browser_ws, cdp_clear_thread, cdp_list, cdp_page_ws, cdp_version,
};
use super::contextual_assist::{contextual_assist_capture_tab, contextual_assist_probe};
use super::page_signals;
use super::trace_capture;

/// `GET /trace/drain/{thread_id}` — drain network traces captured for a
/// magician thread on the CDP proxy. Returns an empty array when no traces
/// are pending. Magician calls this at inner-loop boundaries and folds the
/// events into its `TraceManager` with `capture_source = "cdp_proxy"`.
async fn drain_thread_traces(path: web::Path<String>) -> HttpResponse {
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
async fn drain_thread_auth(path: web::Path<String>) -> HttpResponse {
    let thread_id = path.into_inner();
    let events = trace_capture::drain_captured_auth(&thread_id);
    HttpResponse::Ok().json(serde_json::json!({
        "thread_id": thread_id,
        "events": events,
    }))
}

/// `GET /ambient/page/drain/{thread_id}` — drain passive page identity/change
/// signals buffered for a magician thread.
async fn drain_thread_page_signals(path: web::Path<String>) -> HttpResponse {
    let thread_id = path.into_inner();
    let signals = page_signals::drain(&thread_id);
    HttpResponse::Ok().json(serde_json::json!({
        "thread_id": thread_id,
        "page_signals": signals,
    }))
}

/// Health check endpoint for the Magicutor CDP proxy service.
async fn health_check() -> impl Responder {
    let extension_connected = super::bridge::is_connected().await;
    HttpResponse::Ok().json(serde_json::json!({
        "status": "healthy",
        "service": "magicutor",
        "version": env!("CARGO_PKG_VERSION"),
        "mode": "cdp_proxy",
        "extension_connected": extension_connected,
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

    #[actix_web::test]
    async fn health_reports_extension_connection_state() {
        let app = test::init_service(App::new().configure(configure_routes)).await;
        let request = test::TestRequest::get().uri("/health").to_request();
        let response: serde_json::Value = test::call_and_read_body_json(&app, request).await;
        assert_eq!(response["status"], "healthy");
        assert!(response["extension_connected"].is_boolean());
    }
}
