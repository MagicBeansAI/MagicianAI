//! The API CORS layer for the `/api/magician/v2` and `/api/magician/v3`
//! scopes: one wildcard grant on every routed response, and every `OPTIONS`
//! answered here, before routing.
//!
//! Preflights cannot be left to a catch-all route. actix routing never falls
//! back out of a nested `web::scope`: `Scope::route` hoists its method guard
//! onto the resource, so an `OPTIONS` that fails the guard keeps scanning and
//! reaches a sibling `/{tail:.*}` catch-all — but a nested scope matches on its
//! prefix alone, commits, and answers 404 from its own default when no inner
//! resource accepts the method. Every browser origin that sent a credentialed
//! (bearer) request into `/notes`, `/apps`, `/workspace-storage`, `/tutor`,
//! `/social`, … therefore died at preflight with an opaque network error while
//! the flat routes beside them worked. Answering at the middleware covers
//! every scope shape, present and future.

use actix_web::{
    body::{BoxBody, EitherBody, MessageBody},
    dev::{ServiceRequest, ServiceResponse},
    http::{header, Method},
    middleware::Next,
    HttpResponse,
};

/// The API CORS policy. The auth gate's 401 short-circuits as an `Err` from
/// the OUTERMOST wrap, so this layer never decorates it — the gate carries
/// the same constants on that response itself so browser clients read the
/// login-required body instead of an opaque network error. One source of
/// truth for both surfaces so the values cannot drift apart.
pub const API_CORS_ALLOWED_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";
pub const API_CORS_ALLOWED_HEADERS: &str =
    "Authorization, Content-Type, If-Match, If-None-Match, X-Magician-Device-Id";
pub const API_CORS_EXPOSE_HEADERS: &str = "ETag, Location, X-App-Widget-Refresh-After";
pub const API_CORS_MAX_AGE_SECONDS: &str = "600";

/// Apps enrollment/review is a trusted owner/native capability channel, not a
/// general browser API. Same-origin requests need no CORS grant and the
/// handset exchange carries its one-time QR proof directly. Responses below
/// this prefix are never published to arbitrary web origins through the V2
/// compatibility wildcard — including their preflight, which a browser then
/// refuses for lack of a grant.
const APPS_OWNER_CHANNEL_PREFIX: &str = "/api/magician/v2/devices/apps-automation/";

fn is_apps_owner_channel(path: &str) -> bool {
    path.starts_with(APPS_OWNER_CHANNEL_PREFIX)
}

fn apply_policy(headers: &mut header::HeaderMap, apps_owner_channel: bool) {
    if apps_owner_channel {
        headers.remove(header::ACCESS_CONTROL_ALLOW_ORIGIN);
        headers.remove(header::ACCESS_CONTROL_ALLOW_METHODS);
        headers.remove(header::ACCESS_CONTROL_ALLOW_HEADERS);
        headers.remove(header::ACCESS_CONTROL_EXPOSE_HEADERS);
        headers.remove(header::ACCESS_CONTROL_MAX_AGE);
        return;
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static(API_CORS_ALLOWED_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static(API_CORS_ALLOWED_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static(API_CORS_EXPOSE_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        header::HeaderValue::from_static(API_CORS_MAX_AGE_SECONDS),
    );
}

/// The middleware. Wrapped innermost on the v2 and v3 scopes (first in code
/// order), beneath Cloudflare Access and the auth gate, both of which pass
/// `OPTIONS` through untouched: preflights never carry credentials by design.
pub async fn api_cors_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<EitherBody<B, BoxBody>>, actix_web::Error>
where
    B: MessageBody + 'static,
{
    let apps_owner_channel = is_apps_owner_channel(req.path());
    let mut res = if req.method() == Method::OPTIONS {
        req.into_response(HttpResponse::NoContent().finish())
            .map_into_right_body()
    } else {
        next.call(req).await?.map_into_left_body()
    };
    apply_policy(res.headers_mut(), apps_owner_channel);
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::{
        dev::Service as _, http::StatusCode, middleware::from_fn, test, web, App, HttpResponse,
        Scope,
    };

    const CORS_HEADERS: [header::HeaderName; 5] = [
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::ACCESS_CONTROL_MAX_AGE,
    ];

    /// The three route shapes the real scope mixes: a flat route, a nested
    /// scope, and the owner channel that must stay ungranted.
    fn routes() -> Scope {
        web::scope("/api/magician/v2")
            .route("/top", web::get().to(HttpResponse::Ok))
            .route(
                "/devices/apps-automation/x",
                web::get().to(HttpResponse::Ok),
            )
            .service(web::scope("/nested").route("/leaf", web::get().to(HttpResponse::Ok)))
    }

    async fn call(method: Method, uri: &str) -> ServiceResponse<impl MessageBody> {
        let app =
            test::init_service(App::new().service(routes().wrap(from_fn(api_cors_middleware))))
                .await;
        let mut req = test::TestRequest::default().method(method.clone()).uri(uri);
        if method == Method::OPTIONS {
            req = req
                .insert_header((header::ORIGIN, "tauri://localhost"))
                .insert_header((header::ACCESS_CONTROL_REQUEST_METHOD, "GET"))
                .insert_header((
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "authorization,content-type",
                ));
        }
        app.call(req.to_request()).await.unwrap()
    }

    fn assert_granted(res: &ServiceResponse<impl MessageBody>) {
        let headers = res.headers();
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_METHODS).unwrap(),
            API_CORS_ALLOWED_METHODS
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_ALLOW_HEADERS).unwrap(),
            API_CORS_ALLOWED_HEADERS
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_EXPOSE_HEADERS).unwrap(),
            API_CORS_EXPOSE_HEADERS
        );
        assert_eq!(
            headers.get(header::ACCESS_CONTROL_MAX_AGE).unwrap(),
            API_CORS_MAX_AGE_SECONDS
        );
    }

    fn assert_ungranted(res: &ServiceResponse<impl MessageBody>) {
        for name in CORS_HEADERS {
            assert!(
                res.headers().get(&name).is_none(),
                "{name} must not be published"
            );
        }
    }

    #[actix_web::test]
    async fn preflight_inside_a_nested_scope_is_answered() {
        let res = call(Method::OPTIONS, "/api/magician/v2/nested/leaf").await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_granted(&res);
    }

    #[actix_web::test]
    async fn preflight_for_an_unrouted_path_inside_a_nested_scope_is_answered() {
        // The nested scope's own default would answer 404 here; the browser
        // then reports an opaque network error rather than the real 404 the
        // actual request would have received.
        let res = call(Method::OPTIONS, "/api/magician/v2/nested/missing").await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_granted(&res);
    }

    #[actix_web::test]
    async fn preflight_on_a_flat_route_is_answered() {
        let res = call(Method::OPTIONS, "/api/magician/v2/top").await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_granted(&res);
    }

    #[actix_web::test]
    async fn apps_owner_channel_preflight_carries_no_grant() {
        let res = call(
            Method::OPTIONS,
            "/api/magician/v2/devices/apps-automation/x",
        )
        .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_ungranted(&res);
    }

    #[actix_web::test]
    async fn routed_responses_carry_the_grant() {
        let res = call(Method::GET, "/api/magician/v2/nested/leaf").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_granted(&res);
    }

    #[actix_web::test]
    async fn apps_owner_channel_responses_carry_no_grant() {
        let res = call(Method::GET, "/api/magician/v2/devices/apps-automation/x").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_ungranted(&res);
    }

    #[actix_web::test]
    async fn only_options_is_answered_before_routing() {
        let res = call(Method::GET, "/api/magician/v2/nested/missing").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_granted(&res);
    }
}
