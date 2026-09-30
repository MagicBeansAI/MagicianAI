//! The bearer-authenticating middleware — the enforcement seam.
//!
//! Design: `docs/archive/plans/2026-08-23-magician-auth-identity-workspace-design.md`
//! §7B/§7C, and the implementation plan's key decision 1. One `from_fn`
//! layer on the `/api/magician/v2` scope classifies the `Authorization:
//! Bearer` prefix, resolves it against the single store, and **engraves**
//! the proven `x-principal`/`x-workspace` in place — the exact pattern
//! `cloudflare_access::verify_access_middleware` already uses — so every
//! existing `api_scope::resolve_*` caller downstream reads verified values
//! without a single handler edit. These are internal compatibility headers;
//! client-supplied scope headers are never consulted.
//!
//! Behaviour matrix (the config decision, not a client choice):
//!
//! | Mode | Bearer | Result |
//! |---|---|---|
//! | `open` | valid | engrave the bearer-bound principal + workspace |
//! | `open` | absent, identity store empty | engrave the bootstrap anonymous/default scope |
//! | `open` | absent, any identity exists | `401` + `WWW-Authenticate: Bearer` |
//! | either | invalid | `401` + `WWW-Authenticate: Bearer` |
//! | `credentials` | valid | engrave from the session |
//! | `credentials` | absent | `401` + `WWW-Authenticate: Bearer` |
//!
//! Every general API bearer is bound to one workspace at mint. Terminal
//! `plt_` grants are admitted only by `/plane/mcp`, whose route-local resolver
//! enforces their tool allowlist. `X-Principal` and `X-Workspace` are ignored
//! as authority and overwritten before handlers run.
//! A revoked session
//! fails the *next* request; an already-upgraded SSE/WebSocket stream
//! finishes — matching the plane's grant semantics.

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::http::header::{self, HeaderName, HeaderValue};
use actix_web::middleware::Next;
use actix_web::web::Data;
use actix_web::{HttpMessage, HttpResponse};
use std::sync::Arc;

use super::sessions::{classify_token, TokenKind};
// The API CORS policy lives with the CORS layer in `magician_v2::cors`. This
// gate's 401s run as an `Err` short-circuit from the OUTERMOST wrap, so that
// inner layer never decorates them — the 401 has to carry the headers itself
// or browser clients see an opaque network error instead of the
// login-required body.
use super::store::AuthStore;
use super::{AuthError, BearerKind, ScopeRef};
use crate::config::{AuthConfig, AuthMode};
use crate::magician_v2::cors::{
    API_CORS_ALLOWED_HEADERS, API_CORS_ALLOWED_METHODS, API_CORS_EXPOSE_HEADERS,
    API_CORS_MAX_AGE_SECONDS,
};

/// Shared state the middleware (and the auth routes) read. Attached once at
/// startup; immutable afterwards.
pub struct AuthRuntime {
    pub store: Arc<AuthStore>,
    pub config: AuthConfig,
    /// In-flight social authorize flows (bounded, single-use state tickets).
    pub social: super::social::SocialFlows,
    /// HTTP transport for social flows — `ReqwestSocialHttp` in production,
    /// a fake in tests.
    pub social_http: std::sync::Arc<dyn super::social::SocialHttp>,
}

/// What the middleware stamped into request extensions: the authenticated
/// scope plus its provenance. Handlers that want the *proven* values (not
/// the engraved headers) read this; everything else keeps using
/// `api_scope::resolve_*` unchanged.
#[derive(Debug, Clone)]
pub struct AuthenticatedRequest {
    pub scope: ScopeRef,
    /// The login identity's name — may differ from the scope principal
    /// when the identity adopted `anonymous`. `None` for a runtime-minted
    /// bot token (`BearerKind::Bot`): the runtime is the minter and no
    /// person stands behind the bearer, so a handler that needs an identity
    /// (minting, ownership checks, social linking) refuses it rather than
    /// borrowing one.
    pub identity_name: Option<String>,
    pub bearer: BearerKind,
}

/// Optional app-host credential verifier, installed by the API composition
/// root. It resolves only canonical asset paths against the host's live
/// session registry. Keeping this seam here avoids an auth→app-runtime cycle.
pub struct ScriptedSurfaceAssetAuthenticator {
    verify: Arc<
        dyn Fn(
                &str,
                chrono::DateTime<chrono::Utc>,
            ) -> Option<crate::magician_v2::apps::authority::AuthenticatedAppScope>
            + Send
            + Sync,
    >,
}

impl ScriptedSurfaceAssetAuthenticator {
    pub fn new(
        verify: impl Fn(
                &str,
                chrono::DateTime<chrono::Utc>,
            ) -> Option<crate::magician_v2::apps::authority::AuthenticatedAppScope>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            verify: Arc::new(verify),
        }
    }
}

/// Request-local file-serving proof, deliberately not a general API identity.
/// Only the bearer gate can construct it, and only the asset handler uses it.
#[derive(Clone)]
pub struct AuthenticatedScriptedSurfaceAsset(
    crate::magician_v2::apps::authority::AuthenticatedAppScope,
);

impl AuthenticatedScriptedSurfaceAsset {
    pub fn scope(&self) -> &crate::magician_v2::apps::authority::AuthenticatedAppScope {
        &self.0
    }
}

/// Paths the middleware must not gate: the login route itself, OAuth
/// callbacks (their security is state + PKCE), social starts, and the
/// admin-secret-gated reconciler. The plane MCP door is also self-authenticating:
/// it must resolve both process-live and durable `plt_` grants, while this
/// middleware's unified store knows only the durable kind. Its handler performs
/// the stricter route-local resolution before doing any work. Exact matches
/// only, except the two provider-parameterised prefixes.
pub(crate) fn is_public_auth_path(path: &str) -> bool {
    path == "/api/magician/v2/auth/login"
        || path == "/api/magician/v2/auth/admin/orphaned-scopes"
        || path == "/api/magician/v2/plane/mcp"
        // The enrollment approve door authenticates itself with the same
        // MAGICIAN_ADMIN_SECRET as the orphaned-scopes reconciler — its
        // Authorization bearer IS the secret, never a mag_ token, so it
        // must bypass the unified bearer gate like its twin (otherwise the
        // middleware 401s the secret before the handler ever checks it).
        || path == "/api/magician/v2/chat/enroll/approve"
        // Device bootstrap doors authenticate outside the unified bearer
        // store: one-time enrollment secrets, or the ESP client's Cloudflare
        // service credential. They mint the bearer used by subsequent calls.
        || path == "/api/magician/v2/devices/pair"
        || path == "/api/magician/v2/devices/enrollment/exchange"
        || path == "/api/magician/v2/devices/apps-automation/enrollment/exchange"
        || (path.starts_with("/api/magician/v2/auth/social/") && path.ends_with("/start"))
        || path.starts_with("/api/magician/v2/auth/callback/")
        || path.starts_with("/api/magician/v2/auth/mcp/oauth/callback/")
}

/// `Authorization: Bearer <token>` — scheme match is case-insensitive per
/// RFC 7235; the token value is used verbatim.
pub fn bearer_token(headers: &actix_web::http::header::HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        if let Some((scheme, token)) = value.split_once(' ') {
            let token = token.trim();
            if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }

    // Browser WebSockets cannot set Authorization. Carry the same opaque
    // bearer in an auth-only subprotocol token, keeping credentials out of
    // URLs, browser history, and access-log query strings.
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .find_map(|protocol| {
            protocol
                .strip_prefix("magician-bearer.")
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_string)
        })
}

fn log_session_verification_rejection(req: &ServiceRequest, reason: &'static str) {
    if req.path() == "/api/magician/v2/auth/session" {
        tracing::warn!(
            path = req.path(),
            reason,
            "[AUTH] session verification rejected"
        );
    }
}

fn unauthorized_response() -> HttpResponse {
    HttpResponse::Unauthorized()
        .insert_header(("WWW-Authenticate", "Bearer"))
        // This gate short-circuits as an `Err` from the outermost wrap, so
        // the inner CORS layer never decorates the 401 — the headers are
        // carried here (same policy constants `cors::api_cors_middleware`
        // uses) so browser clients can read the login-required body instead
        // of an opaque network error. Preflights never reach this path (the
        // OPTIONS early-return above passes them through).
        .insert_header((header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"))
        .insert_header((
            header::ACCESS_CONTROL_ALLOW_METHODS,
            API_CORS_ALLOWED_METHODS,
        ))
        .insert_header((
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            API_CORS_ALLOWED_HEADERS,
        ))
        .insert_header((
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            API_CORS_EXPOSE_HEADERS,
        ))
        .insert_header((header::ACCESS_CONTROL_MAX_AGE, API_CORS_MAX_AGE_SECONDS))
        .json(serde_json::json!({
            "error": "authentication_required",
            "message": "A valid bearer token is required (log in via POST /auth/login)."
        }))
}

fn engrave(req: &mut ServiceRequest, scope: &ScopeRef) -> Result<(), actix_web::Error> {
    let principal = HeaderValue::from_str(scope.principal())
        .map_err(actix_web::error::ErrorInternalServerError)?;
    let workspace = HeaderValue::from_str(scope.workspace())
        .map_err(actix_web::error::ErrorInternalServerError)?;
    req.headers_mut()
        .insert(HeaderName::from_static("x-principal"), principal);
    req.headers_mut()
        .insert(HeaderName::from_static("x-workspace"), workspace);
    Ok(())
}

fn discard_caller_scope(req: &mut ServiceRequest) {
    req.headers_mut()
        .remove(HeaderName::from_static("x-principal"));
    req.headers_mut()
        .remove(HeaderName::from_static("x-workspace"));
}

/// The middleware. Registered as the outermost `from_fn` wrap on the v2
/// scope (after `verify_access_middleware` in code order, so it runs before
/// it in the request path).
pub async fn authenticate_request(
    mut req: ServiceRequest,
    next: Next<impl MessageBody>,
) -> Result<ServiceResponse<impl MessageBody>, actix_web::Error> {
    let runtime = req
        .app_data::<Data<AuthRuntime>>()
        .cloned()
        .ok_or_else(|| {
            actix_web::error::ErrorInternalServerError("auth runtime is not installed")
        })?;
    let mode = runtime.config.mode;

    if is_public_auth_path(req.path()) {
        // Public bootstrap routes have their own capabilities (password,
        // PKCE state, enrollment ticket, admin secret, or plane grant). They
        // have no authenticated API scope, so never let a compatibility
        // header survive into a handler by accident.
        discard_caller_scope(&mut req);
        return next.call(req).await;
    }
    // CORS preflights never carry credentials by design; gating them would
    // break every browser client the moment `credentials` mode is flipped.
    if req.method() == actix_web::http::Method::OPTIONS {
        discard_caller_scope(&mut req);
        return next.call(req).await;
    }

    let Some(token) = bearer_token(req.headers()) else {
        // A sandboxed page cannot send the owner's Authorization header. Its
        // unguessable, live asset-session path is a separate read-only
        // credential, scoped to this exact installation's reviewed files.
        // Never recover an explicitly supplied malformed/revoked bearer, and
        // never lend this proof to host-open, bridge, or lifecycle POSTs.
        if req.method() == actix_web::http::Method::GET
            && req.query_string().is_empty()
            && !req.headers().contains_key(header::AUTHORIZATION)
            && !req.headers().contains_key(header::SEC_WEBSOCKET_PROTOCOL)
        {
            if let Some(credential) = req
                .app_data::<Data<ScriptedSurfaceAssetAuthenticator>>()
                .and_then(|surfaces| (surfaces.verify)(req.path(), chrono::Utc::now()))
            {
                discard_caller_scope(&mut req);
                req.extensions_mut()
                    .insert(AuthenticatedScriptedSurfaceAsset(credential));
                return next.call(req).await;
            }
        }
        if mode == AuthMode::Credentials {
            log_session_verification_rejection(&req, "bearer_missing");
            return Err(actix_web::error::InternalError::from_response(
                "authentication required",
                unauthorized_response(),
            )
            .into());
        }
        // Open mode is a bootstrap posture, not a steady state: the
        // anonymous/default fallback exists only while this install has no
        // identity at all. The first login creates the owner (adopting
        // scopes/anonymous), and from that moment the same tree must not stay
        // reachable without a credential until an operator flips the config —
        // a forget-to-tighten window is the hole this closes. A store read
        // failure fails closed: if identities cannot be counted, the fallback
        // must not stand in for an answer.
        let identities_empty = runtime.store.identities_empty().map_err(|error| {
            actix_web::error::ErrorInternalServerError(format!("auth store failure: {error}"))
        })?;
        if !identities_empty {
            log_session_verification_rejection(&req, "bearer_missing_after_bootstrap");
            return Err(actix_web::error::InternalError::from_response(
                "authentication required",
                unauthorized_response(),
            )
            .into());
        }
        // Still single-user by construction: the caller cannot manufacture
        // another scope, and legacy scope headers stay non-authoritative.
        let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
        engrave(&mut req, &scope)?;
        return next.call(req).await;
    };

    // Bot daemons the runtime spawned present an in-memory `mag_bot_` token.
    // Handled before the store because nothing is persisted for them — there is
    // no row to look up, and `resolve_bearer` would simply miss. The scope is
    // read off the grant the runtime minted, so a bot cannot name its own.
    //
    // Unknown or revoked fails closed rather than falling through: a stale
    // token from a bot the runtime already stopped must not be retried against
    // the session store, where it would resolve to nothing and be reported as
    // an unrelated failure.
    //
    // Scope breadth is that of the bot's workspace — see the module docs on
    // `bot_tokens`; this arm engraves a scope and enforces no tool floor.
    //
    // The stamp is the same pair every store-resolved bearer gets. Engraving
    // the headers alone is not enough: handlers that read the *proven* caller
    // (`authenticated()`, `VerifiedRequestIdentity`) would see nothing and
    // treat the bot as anonymous — `/auth/session`, which the bot SDK calls
    // before its adapter starts, 401'd every bot on that gap.
    if matches!(classify_token(&token), TokenKind::Bot) {
        let Some(grant) = super::bot_tokens::bot_token_registry().resolve(&token) else {
            return Err(actix_web::error::InternalError::from_response(
                "unknown or revoked bot token",
                unauthorized_response(),
            )
            .into());
        };
        let scope = ScopeRef::from_bot_grant(&grant);
        engrave(&mut req, &scope)?;
        req.extensions_mut().insert(
            crate::magician_v2::cloudflare_access::VerifiedRequestIdentity::from_magician_bearer(
                scope.principal(),
                scope.workspace(),
                &token,
            ),
        );
        req.extensions_mut().insert(AuthenticatedRequest {
            scope,
            identity_name: None,
            bearer: BearerKind::Bot {
                bot_name: grant.bot_name,
            },
        });
        return next.call(req).await;
    }

    let mut authenticated: Option<AuthenticatedRequest> = None;
    let mut unowned: Option<(String, String)> = None;
    if let Some(bearer_identity) = runtime.store.resolve_bearer(&token).map_err(|error| {
        actix_web::error::ErrorInternalServerError(format!("auth store failure: {error}"))
    })? {
        // Durable terminal grants are capabilities for the self-authenticating
        // `/plane/mcp` door, not general-purpose API tokens. That exact route
        // bypasses this middleware and revalidates the grant plus its tool
        // allowlist. Accepting a `plt_` here would let a narrowly allowed
        // terminal call arbitrary REST handlers that know nothing about the
        // grant's capability floor.
        if matches!(&bearer_identity.kind, BearerKind::Grant { .. }) {
            return Err(actix_web::error::InternalError::from_response(
                "terminal grant is not a general API bearer",
                unauthorized_response(),
            )
            .into());
        }
        // A bearer whose identity no longer exists resolves to nothing —
        // unauthenticated, not an error.
        let identity = runtime
            .store
            .find_identity(&bearer_identity.identity)
            .map_err(|error| {
                actix_web::error::ErrorInternalServerError(format!(
                    "identity store failure: {error}"
                ))
            })?;
        if let Some(identity) = identity {
            let scopes_root = runtime.store.runtime_root().join("scopes");
            // The workspace comes only from the resolved bearer record. The
            // ownership check re-runs on every request so deleting a workspace
            // invalidates existing tokens for it without a separate sweep.
            match ScopeRef::from_session(&scopes_root, &identity, Some(&bearer_identity.workspace))
            {
                Ok(scope) => {
                    authenticated = Some(AuthenticatedRequest {
                        scope,
                        identity_name: Some(identity.name.clone()),
                        bearer: bearer_identity.kind,
                    });
                },
                Err(AuthError::UnownedWorkspace {
                    principal,
                    workspace,
                }) => {
                    unowned = Some((principal, workspace));
                },
                Err(error) => {
                    return Err(actix_web::error::ErrorInternalServerError(format!(
                        "authentication failed: {error}"
                    )));
                },
            }
        }
    }

    if let Some((principal, workspace)) = unowned {
        return Err(actix_web::error::InternalError::from_response(
            "unowned workspace selector",
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": "unowned_workspace",
                "message": format!("Principal {principal} does not own workspace {workspace}.")
            })),
        )
        .into());
    }

    match authenticated {
        Some(authenticated) => {
            engrave(&mut req, &authenticated.scope)?;
            req.extensions_mut().insert(
                crate::magician_v2::cloudflare_access::VerifiedRequestIdentity::from_magician_bearer(
                    authenticated.scope.principal(),
                    authenticated.scope.workspace(),
                    &token,
                ),
            );
            req.extensions_mut().insert(authenticated);
            next.call(req).await
        },
        None => {
            // An unrecognised bearer may be a paired-device token, whose
            // roster is owned by the inner identity middleware rather than
            // AuthStore. Only defer when the required device id is present;
            // that middleware rejects a bad pair before any handler runs.
            if req.headers().contains_key("x-magician-device-id")
                || req.headers().contains_key("x-magdroid-device")
            {
                return next.call(req).await;
            }
            log_session_verification_rejection(&req, "bearer_unknown_revoked_or_expired");
            Err(actix_web::error::InternalError::from_response(
                "authentication required",
                unauthorized_response(),
            )
            .into())
        },
    }
}

/// Convenience for handlers: the middleware-stamped authenticated request.
pub fn authenticated(req: &actix_web::HttpRequest) -> Option<AuthenticatedRequest> {
    req.extensions().get::<AuthenticatedRequest>().cloned()
}

/// The kind of bearer an authenticated request presented (used by logout,
/// which revokes sessions but not API tokens).
pub fn presented_bearer_kind(req: &actix_web::HttpRequest) -> Option<TokenKind> {
    bearer_token(req.headers()).map(|token| classify_token(&token))
}

#[cfg(test)]
mod tests {
    use super::is_public_auth_path;

    #[test]
    fn mcp_oauth_callback_is_public_but_nearby_management_routes_are_not() {
        assert!(is_public_auth_path(
            "/api/magician/v2/auth/mcp/oauth/callback/binding/flow"
        ));
        assert!(!is_public_auth_path(
            "/api/magician/v2/auth/mcp/oauth/callback"
        ));
        assert!(!is_public_auth_path(
            "/api/magician/v2/auth/mcp/oauth/status"
        ));
    }
}
