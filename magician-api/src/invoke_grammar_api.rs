//! Lane invoke-grammar catalog API (plan workstream 1.2,
//! docs/plans/2026-08-26-platform-layering-and-app-extraction-plan.md).
//!
//! Publishes the server-side invoke grammar for every conversational
//! product lane (tutor, app_copilot, brainstorm, vibedev) as one versioned,
//! content-hashed JSON catalog, generated from the same constants the
//! backend parsers match against — never hand-maintained vocabulary. Read-
//! only, stateless, and cache-friendly; the agreement test
//! (`magician/tests/invoke_grammar_agreement.rs`, plan 0.4) asserts the
//! published content against the parser vocabularies by calling the same
//! builder this handler serves.
//!
//! Route:
//! ```text
//! GET /api/magician/v2/chat/invoke-grammar
//!     -> { "version": 1, "etag": "<sha256-hex>",
//!          "leading_invoke_required": true,
//!          "lanes": { "tutor": …, "app_copilot": …,
//!                     "brainstorm": …, "vibedev": … } }
//! ```
//!
//! Wire contract is **additive-only**: lanes and fields are never removed
//! or retyped. `version` bumps only on schema changes; vocabulary changes
//! move the `etag` (sha256 over the serialized lane set) so clients can
//! detect drift without re-diffing content. Clients keep their local
//! parsers until the later client-consumption step — this endpoint is the
//! server truth that step will consume.

use actix_web::{web, HttpResponse, Result};

use magician::magician_v2::chat::invoke_catalog::invoke_grammar_catalog;

/// GET /api/magician/v2/chat/invoke-grammar — the versioned, content-hashed
/// projection of the invoke grammar the backend parsers accept.
pub async fn get_invoke_grammar_handler() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(invoke_grammar_catalog()))
}

/// Mount the invoke-grammar catalog route. A single exact route rather
/// than a `/chat` scope: the v2 chain already registers sibling
/// `/chat/…` routes as exact resources, and a scope would swallow them.
pub fn configure_invoke_grammar_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/chat/invoke-grammar",
        web::get().to(get_invoke_grammar_handler),
    );
}

#[cfg(test)]
mod tests {
    use actix_web::{test, App};

    use super::configure_invoke_grammar_routes;

    #[actix_web::test]
    async fn serves_versioned_catalog_with_stable_etag() {
        let app = test::init_service(App::new().configure(configure_invoke_grammar_routes)).await;

        let first: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/chat/invoke-grammar")
                .to_request(),
        )
        .await;
        let second: serde_json::Value = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/chat/invoke-grammar")
                .to_request(),
        )
        .await;

        assert_eq!(
            first, second,
            "the catalog is a pure function of the grammar"
        );
        assert_eq!(first["version"].as_u64(), Some(1));
        assert!(first["leading_invoke_required"].as_bool().unwrap_or(false));
        let etag = first["etag"].as_str().expect("etag is a string");
        assert_eq!(etag.len(), 64, "etag should be a 64-char sha256 hex string");
        assert!(etag.bytes().all(|byte| byte.is_ascii_hexdigit()));

        // One probe per lane: the wire keys exist and carry the parser
        // constants' vocabulary (exact agreement is asserted by the
        // backend-side agreement test; this pins the served JSON shape).
        assert_eq!(first["lanes"]["tutor"]["markers"][0], "@tutor");
        assert_eq!(first["lanes"]["app_copilot"]["markers"][0], "@copilot");
        assert_eq!(first["lanes"]["brainstorm"]["markers"][0], "@brainstorm");
        assert_eq!(first["lanes"]["vibedev"]["markers"][0], "@vibedev");
        assert_eq!(first["lanes"]["tutor"]["quick_flags"][0], "#quick");
        assert_eq!(first["lanes"]["vibedev"]["quick_flags"][0], "#discuss");
    }
}
